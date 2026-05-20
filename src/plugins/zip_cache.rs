//! Offline plugin distribution via on-disk archive cache (crosslink
//! #656, CC parity with `zipCache.ts`).
//!
//! Background: CC supports installing plugins from a pre-downloaded
//! archive in `~/.claude/plugins/cache/<sha256>.zip` so the install
//! works on air-gapped machines and survives marketplace outages. The
//! cache key is the SHA-256 of the archive bytes; the manifest carries
//! that hash so the host can resolve, verify, and extract without
//! touching the network.
//!
//! This module ships the schema and the integrity-check half of that
//! contract: the cache index, lookup, write, and verify-on-read paths.
//! The actual `.zip` extraction is deliberately deferred until a `zip`
//! crate is added to the workspace (tracked in #656's runtime
//! follow-up); the cache file format is stable so today's writes will
//! be readable by the extracting consumer.
//!
//! On-disk layout (under `~/.openclaudia/plugins/cache/`):
//!
//! ```text
//! cache/
//!   index.json            ← one entry per cached archive
//!   <sha256>.zip          ← raw archive bytes
//! ```
//!
//! `index.json` is a flat JSON map keyed by sha256. Storing the sha256
//! in the filename redundantly is intentional: a corrupt index lets us
//! rebuild from `ls`.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::PathBuf;
use thiserror::Error;

/// Errors surfaced by the zip-cache module. Returned as a single typed
/// error so callers can distinguish "cache miss" (recoverable, do the
/// online install) from "cache present but corrupt" (must repair).
#[derive(Debug, Error)]
pub enum ZipCacheError {
    /// The requested sha256 is not present in the cache.
    #[error("cache miss: no archive with sha256 {0}")]
    Missing(String),
    /// The archive on disk was found but its bytes hash to a different
    /// sha256. The caller must treat this as tampering and refuse.
    #[error("integrity check failed: archive {sha256} hashes to {actual}")]
    IntegrityMismatch {
        /// The sha256 the caller asked for.
        sha256: String,
        /// The sha256 actually computed off-disk.
        actual: String,
    },
    /// Filesystem error reading or writing the cache.
    #[error("cache I/O error: {0}")]
    Io(#[from] std::io::Error),
    /// Index file failed to deserialize.
    #[error("cache index corrupt: {0}")]
    Index(#[from] serde_json::Error),
}

/// One cached archive entry.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CacheEntry {
    /// SHA-256 of the archive bytes (hex, lowercase). Doubles as the
    /// filename (`<sha256>.zip`).
    pub sha256: String,
    /// Plugin id this archive provides. Matches
    /// [`crate::plugins::Plugin::id`] post-install.
    pub plugin_id: String,
    /// Semver string from the originating manifest, when known.
    pub version: Option<String>,
    /// Wall-clock seconds since UNIX epoch the entry was written. Used
    /// by `/maintain` to age out stale entries.
    pub installed_at_unix: u64,
}

/// Filename of the index file at the cache root.
pub const INDEX_FILENAME: &str = "index.json";

/// File extension applied to cached archives. Plain `.zip` to match
/// CC's on-disk convention; extracting consumers don't have to guess.
pub const ARCHIVE_EXTENSION: &str = "zip";

/// On-disk cache of archives.
#[derive(Debug)]
pub struct ZipCache {
    root: PathBuf,
}

impl ZipCache {
    /// Bind to `root` (created on demand). `root` is conventionally
    /// `~/.openclaudia/plugins/cache/`.
    #[must_use]
    pub const fn new(root: PathBuf) -> Self {
        Self { root }
    }

    /// Path to the cache index file. Public so /doctor can surface it.
    #[must_use]
    pub fn index_path(&self) -> PathBuf {
        self.root.join(INDEX_FILENAME)
    }

    /// Path the archive for `sha256` would occupy on disk. Returns the
    /// path even when the archive isn't yet present so callers can
    /// `fs::write` to it for fresh adds.
    #[must_use]
    pub fn archive_path(&self, sha256: &str) -> PathBuf {
        self.root.join(format!("{sha256}.{ARCHIVE_EXTENSION}"))
    }

    /// Read the index file. Returns an empty map when the index doesn't
    /// yet exist (first-run behaviour).
    ///
    /// # Errors
    ///
    /// Returns [`ZipCacheError::Io`] for any FS error other than
    /// `NotFound`, and [`ZipCacheError::Index`] when the index file is
    /// present but doesn't deserialize.
    pub fn read_index(&self) -> Result<BTreeMap<String, CacheEntry>, ZipCacheError> {
        let path = self.index_path();
        match std::fs::read_to_string(&path) {
            Ok(s) => Ok(serde_json::from_str(&s)?),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(BTreeMap::new()),
            Err(e) => Err(ZipCacheError::Io(e)),
        }
    }

    /// Write the index file, creating parent dirs if needed.
    ///
    /// # Errors
    ///
    /// Returns [`ZipCacheError::Io`] on any filesystem failure, or
    /// [`ZipCacheError::Index`] if serialization fails (which would
    /// indicate a logic error in `CacheEntry`'s derive).
    pub fn write_index(&self, entries: &BTreeMap<String, CacheEntry>) -> Result<(), ZipCacheError> {
        std::fs::create_dir_all(&self.root)?;
        let body = serde_json::to_string_pretty(entries)?;
        std::fs::write(self.index_path(), body)?;
        Ok(())
    }

    /// Insert one archive into the cache: write `bytes` to disk,
    /// upsert `entry` into the index, and atomically flush both. The
    /// caller is responsible for filling `entry.sha256` with the
    /// archive's actual hash (which must match `bytes` — verified
    /// here).
    ///
    /// # Errors
    ///
    /// Returns [`ZipCacheError::IntegrityMismatch`] when
    /// `entry.sha256` disagrees with the computed hash of `bytes`, and
    /// [`ZipCacheError::Io`] for any filesystem error.
    pub fn put(&self, entry: CacheEntry, bytes: &[u8]) -> Result<(), ZipCacheError> {
        let actual = sha256_hex(bytes);
        if actual != entry.sha256 {
            return Err(ZipCacheError::IntegrityMismatch {
                sha256: entry.sha256,
                actual,
            });
        }
        std::fs::create_dir_all(&self.root)?;
        std::fs::write(self.archive_path(&entry.sha256), bytes)?;
        let mut idx = self.read_index().unwrap_or_default();
        idx.insert(entry.sha256.clone(), entry);
        self.write_index(&idx)?;
        Ok(())
    }

    /// Read an archive out of the cache, verifying integrity. The
    /// expected `sha256` MUST be supplied by the caller (typically out
    /// of the install manifest) so a swap-on-disk attack cannot silently
    /// substitute a different archive under the same id.
    ///
    /// # Errors
    ///
    /// * [`ZipCacheError::Missing`] when the archive isn't cached.
    /// * [`ZipCacheError::IntegrityMismatch`] when the bytes on disk
    ///   don't hash to the expected sha256.
    /// * [`ZipCacheError::Io`] on filesystem failure.
    pub fn get_verified(&self, sha256: &str) -> Result<Vec<u8>, ZipCacheError> {
        let path = self.archive_path(sha256);
        let bytes = match std::fs::read(&path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(ZipCacheError::Missing(sha256.to_string()));
            }
            Err(e) => return Err(ZipCacheError::Io(e)),
        };
        let actual = sha256_hex(&bytes);
        if actual != sha256 {
            return Err(ZipCacheError::IntegrityMismatch {
                sha256: sha256.to_string(),
                actual,
            });
        }
        Ok(bytes)
    }

    /// True iff `sha256` is currently present on disk (does NOT
    /// re-verify the bytes — use [`Self::get_verified`] for that).
    #[must_use]
    pub fn contains(&self, sha256: &str) -> bool {
        self.archive_path(sha256).is_file()
    }
}

/// Compute the lowercase-hex SHA-256 of `bytes`. Shared helper so the
/// cache's write path and the verify path can't disagree on hex casing.
#[must_use]
pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let digest = hasher.finalize();
    let mut out = String::with_capacity(digest.len() * 2);
    for b in digest {
        use std::fmt::Write as _;
        let _ = write!(out, "{b:02x}");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn fresh() -> (TempDir, ZipCache) {
        let tmp = TempDir::new().unwrap();
        let cache = ZipCache::new(tmp.path().to_path_buf());
        (tmp, cache)
    }

    fn entry(sha: &str) -> CacheEntry {
        CacheEntry {
            sha256: sha.into(),
            plugin_id: "demo".into(),
            version: Some("1.0.0".into()),
            installed_at_unix: 42,
        }
    }

    #[test]
    fn put_then_get_round_trips() {
        let (_tmp, cache) = fresh();
        let bytes = b"PK\x03\x04 fake zip bytes";
        let sha = sha256_hex(bytes);
        cache.put(entry(&sha), bytes).expect("put succeeds");

        let read = cache.get_verified(&sha).expect("get succeeds");
        assert_eq!(read, bytes);
        assert!(cache.contains(&sha));
    }

    #[test]
    fn get_missing_archive_returns_missing_error() {
        let (_tmp, cache) = fresh();
        let err = cache
            .get_verified("00".repeat(32).as_str())
            .expect_err("missing must error");
        assert!(matches!(err, ZipCacheError::Missing(_)));
    }

    #[test]
    fn put_with_wrong_sha_rejects() {
        let (_tmp, cache) = fresh();
        let bytes = b"some bytes";
        let bad = entry("0".repeat(64).as_str());
        let err = cache.put(bad, bytes).expect_err("mismatch must error");
        match err {
            ZipCacheError::IntegrityMismatch { actual, .. } => {
                assert_eq!(actual, sha256_hex(bytes));
            }
            other => panic!("expected IntegrityMismatch, got {other:?}"),
        }
    }

    #[test]
    fn get_detects_tampered_archive_on_disk() {
        let (_tmp, cache) = fresh();
        let bytes = b"genuine";
        let sha = sha256_hex(bytes);
        cache.put(entry(&sha), bytes).unwrap();
        // Overwrite the file with different bytes — same filename, different hash.
        std::fs::write(cache.archive_path(&sha), b"tampered").unwrap();
        let err = cache.get_verified(&sha).expect_err("tamper must be caught");
        assert!(matches!(err, ZipCacheError::IntegrityMismatch { .. }));
    }

    #[test]
    fn missing_index_reads_as_empty_map() {
        let (_tmp, cache) = fresh();
        let idx = cache.read_index().expect("missing index → empty map");
        assert!(idx.is_empty());
    }

    #[test]
    fn index_round_trips() {
        let (_tmp, cache) = fresh();
        let mut idx = BTreeMap::new();
        let e = entry("a".repeat(64).as_str());
        idx.insert(e.sha256.clone(), e.clone());
        cache.write_index(&idx).unwrap();
        let read = cache.read_index().unwrap();
        assert_eq!(read.get(&e.sha256), Some(&e));
    }

    #[test]
    fn sha256_hex_is_lowercase_64_chars() {
        let h = sha256_hex(b"");
        assert_eq!(h.len(), 64);
        assert!(h
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
        // Spot-check against a well-known fixture: SHA-256 of "".
        assert_eq!(
            h,
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }
}
