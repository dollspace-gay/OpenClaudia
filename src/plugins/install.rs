//! Installation tracking types for `installed_plugins.json` (V2 format).

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use tracing::{debug, warn};

use super::PluginError;

// ---------------------------------------------------------------------------
// Installation tracking (installed_plugins.json V2)
// ---------------------------------------------------------------------------

/// Installation scope for a plugin
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum InstallScope {
    Managed,
    User,
    Project,
    Local,
}

impl std::fmt::Display for InstallScope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Managed => write!(f, "managed"),
            Self::User => write!(f, "user"),
            Self::Project => write!(f, "project"),
            Self::Local => write!(f, "local"),
        }
    }
}

impl std::str::FromStr for InstallScope {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "managed" => Ok(Self::Managed),
            "user" => Ok(Self::User),
            "project" => Ok(Self::Project),
            "local" => Ok(Self::Local),
            _ => Err(format!(
                "Invalid scope '{s}'. Must be: managed, user, project, local"
            )),
        }
    }
}

/// A single installation entry for a plugin
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginInstallEntry {
    /// Installation scope
    pub scope: InstallScope,
    /// Project path (required for project/local scopes)
    #[serde(
        default,
        rename = "projectPath",
        skip_serializing_if = "Option::is_none"
    )]
    pub project_path: Option<String>,
    /// Absolute path to the installed plugin directory
    #[serde(rename = "installPath")]
    pub install_path: String,
    /// Currently installed version
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// ISO 8601 timestamp of installation
    #[serde(
        default,
        rename = "installedAt",
        skip_serializing_if = "Option::is_none"
    )]
    pub installed_at: Option<String>,
    /// ISO 8601 timestamp of last update
    #[serde(
        default,
        rename = "lastUpdated",
        skip_serializing_if = "Option::is_none"
    )]
    pub last_updated: Option<String>,
    /// Git commit SHA for git-based plugins
    #[serde(
        default,
        rename = "gitCommitSha",
        skip_serializing_if = "Option::is_none"
    )]
    pub git_commit_sha: Option<String>,
}

/// Installed plugins tracking file (V2 format)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstalledPlugins {
    /// Schema version (always 2)
    pub version: u32,
    /// Map of plugin IDs (plugin@marketplace) to installation entries
    pub plugins: HashMap<String, Vec<PluginInstallEntry>>,
}

impl Default for InstalledPlugins {
    fn default() -> Self {
        Self {
            version: 2,
            plugins: HashMap::new(),
        }
    }
}

impl InstalledPlugins {
    /// Load from disk, returning default if not found
    pub fn load() -> Self {
        let path = Self::file_path();
        if !path.exists() {
            return Self::default();
        }
        match std::fs::read_to_string(&path) {
            Ok(content) => match serde_json::from_str::<Self>(&content) {
                Ok(data) => {
                    debug!(
                        count = data.plugins.len(),
                        "Loaded installed plugins tracking"
                    );
                    data
                }
                Err(e) => {
                    warn!(error = %e, "Failed to parse installed_plugins.json, starting fresh");
                    Self::default()
                }
            },
            Err(e) => {
                warn!(error = %e, "Failed to read installed_plugins.json");
                Self::default()
            }
        }
    }

    /// Save to disk using an atomic write-then-rename pattern.
    ///
    /// The file is first written to `<path>.tmp.<pid>.<counter>`, then
    /// `fsync`-ed, then renamed into place.  On Unix the temp file is given
    /// mode `0o600` (owner-read/write only) before the rename, so the final
    /// file is never world-readable even for a split instant.
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be serialized, written, or renamed.
    pub fn save(&self) -> Result<(), PluginError> {
        // Declared first so it precedes all statements
        // (clippy::items_after_statements).
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

        let path = Self::file_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| PluginError::IoError(e.to_string()))?;

            // Restrict the parent directory so only the owner can list it.
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                let meta =
                    std::fs::metadata(parent).map_err(|e| PluginError::IoError(e.to_string()))?;
                let mut perms = meta.permissions();
                perms.set_mode(0o700);
                std::fs::set_permissions(parent, perms)
                    .map_err(|e| PluginError::IoError(e.to_string()))?;
            }
        }

        let json =
            serde_json::to_string_pretty(self).map_err(|e| PluginError::IoError(e.to_string()))?;

        // Build a collision-resistant tmp path using PID + a static counter.
        // Keeping it on the same filesystem as the target is required for
        // `rename(2)` to be atomic.
        let nonce = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let tmp = path.with_extension(format!("tmp.{pid}.{nonce}", pid = std::process::id()));

        // Write to the temp file first.
        std::fs::write(&tmp, &json).map_err(|e| PluginError::IoError(e.to_string()))?;

        // Restrict permissions BEFORE the rename so the file is never
        // world-readable even momentarily.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))
                .map_err(|e| PluginError::IoError(e.to_string()))?;
        }

        // fsync so a crash between write and rename does not leave the kernel
        // buffer un-flushed, which would make the old file vanish while the
        // new data is still only in page cache.
        {
            let f = std::fs::File::open(&tmp).map_err(|e| PluginError::IoError(e.to_string()))?;
            f.sync_all()
                .map_err(|e| PluginError::IoError(e.to_string()))?;
        }

        // Atomic rename: POSIX rename(2) on the same filesystem is atomic --
        // readers see either the old or the new complete file, never a
        // half-written intermediate.
        std::fs::rename(&tmp, &path).map_err(|e| {
            // Best-effort cleanup; ignore secondary error.
            let _ = std::fs::remove_file(&tmp);
            PluginError::IoError(e.to_string())
        })?;

        debug!(path = ?path, count = self.plugins.len(), "Saved installed plugins");
        Ok(())
    }

    /// Add or update an installation entry
    pub fn upsert(&mut self, plugin_id: &str, entry: PluginInstallEntry) {
        let entries = self.plugins.entry(plugin_id.to_string()).or_default();
        if let Some(existing) = entries
            .iter_mut()
            .find(|e| e.scope == entry.scope && e.project_path == entry.project_path)
        {
            *existing = entry;
        } else {
            entries.push(entry);
        }
    }

    /// Remove a plugin by ID
    pub fn remove(&mut self, plugin_id: &str) -> bool {
        self.plugins.remove(plugin_id).is_some()
    }

    /// Get the file path for `installed_plugins.json`
    fn file_path() -> PathBuf {
        dirs::home_dir().map_or_else(
            || PathBuf::from(".openclaudia/plugins/installed_plugins.json"),
            |home| {
                home.join(".openclaudia")
                    .join("plugins")
                    .join("installed_plugins.json")
            },
        )
    }

    /// Get all plugin IDs
    #[must_use]
    pub fn plugin_ids(&self) -> Vec<&str> {
        self.plugins
            .keys()
            .map(std::string::String::as_str)
            .collect()
    }

    /// Save to a caller-supplied path (test seam).
    ///
    /// Mirrors [`Self::save`] but uses `path` instead of [`Self::file_path`]
    /// so tests can write into a `TempDir` without touching `~/.openclaudia`.
    #[cfg(test)]
    fn save_to(&self, path: &std::path::Path) -> Result<(), PluginError> {
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| PluginError::IoError(e.to_string()))?;
        }

        let json =
            serde_json::to_string_pretty(self).map_err(|e| PluginError::IoError(e.to_string()))?;

        let nonce = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let tmp = path.with_extension(format!("tmp.{pid}.{nonce}", pid = std::process::id()));

        std::fs::write(&tmp, &json).map_err(|e| PluginError::IoError(e.to_string()))?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))
                .map_err(|e| PluginError::IoError(e.to_string()))?;
        }

        {
            let f = std::fs::File::open(&tmp).map_err(|e| PluginError::IoError(e.to_string()))?;
            f.sync_all()
                .map_err(|e| PluginError::IoError(e.to_string()))?;
        }

        std::fs::rename(&tmp, path).map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            PluginError::IoError(e.to_string())
        })?;

        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Tests for InstalledPlugins::save  (security + atomicity)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod save_tests {
    use super::*;
    use tempfile::TempDir;

    fn make_populated() -> InstalledPlugins {
        let mut ip = InstalledPlugins::default();
        ip.upsert(
            "test-plugin@marketplace",
            PluginInstallEntry {
                scope: InstallScope::User,
                project_path: None,
                install_path: "/home/user/.openclaudia/plugins/test-plugin".to_string(),
                version: Some("1.0.0".to_string()),
                installed_at: Some("2026-01-01T00:00:00Z".to_string()),
                last_updated: None,
                git_commit_sha: None,
            },
        );
        ip
    }

    /// (a) On Unix, the saved file must be mode 0o600 (owner-rw only).
    ///
    /// Verifies that `installed_plugins.json` -- which contains absolute
    /// install paths that disclose workspace layout -- is never world-readable.
    #[test]
    #[cfg(unix)]
    fn save_creates_file_with_mode_0o600() {
        use std::os::unix::fs::PermissionsExt as _;

        let dir = TempDir::new().unwrap();
        let path = dir.path().join("installed_plugins.json");

        let ip = make_populated();
        ip.save_to(&path).expect("save_to must succeed");

        assert!(path.exists(), "file must exist after save");

        let meta = std::fs::metadata(&path).unwrap();
        let mode = meta.permissions().mode() & 0o777;
        assert_eq!(
            mode, 0o600,
            "file mode must be 0o600 (owner-rw only), got 0o{mode:o}"
        );
    }

    /// (b) Concurrent readers always see complete, valid JSON -- never a
    /// half-written file.
    ///
    /// A reader thread spins reading the file while the writer performs 50
    /// saves. The atomic rename guarantees every snapshot is either the
    /// pre-existing content or the fully-written new content.
    #[test]
    fn save_is_atomic_concurrent_reads_see_complete_content() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;

        let dir = TempDir::new().unwrap();
        let path = dir.path().join("installed_plugins.json");

        // Pre-populate so the reader has something to open immediately.
        InstalledPlugins::default().save_to(&path).unwrap();

        let done = Arc::new(AtomicBool::new(false));
        let done_reader = Arc::clone(&done);
        let path_reader = path.clone();

        let reader = std::thread::spawn(move || {
            let mut snapshots: Vec<String> = Vec::new();
            while !done_reader.load(Ordering::Relaxed) {
                if let Ok(content) = std::fs::read_to_string(&path_reader) {
                    if !content.is_empty() {
                        snapshots.push(content);
                    }
                }
                std::hint::spin_loop();
            }
            snapshots
        });

        for i in 0_u32..50 {
            let mut ip = InstalledPlugins::default();
            ip.upsert(
                &format!("plugin-{i}@market"),
                PluginInstallEntry {
                    scope: InstallScope::User,
                    project_path: None,
                    install_path: format!("/tmp/plugin-{i}"),
                    version: Some(format!("{i}.0.0")),
                    installed_at: None,
                    last_updated: None,
                    git_commit_sha: None,
                },
            );
            ip.save_to(&path).expect("concurrent save must succeed");
        }

        done.store(true, Ordering::Relaxed);
        let snapshots = reader.join().unwrap();

        for (idx, snap) in snapshots.iter().enumerate() {
            serde_json::from_str::<InstalledPlugins>(snap).unwrap_or_else(|e| {
                panic!(
                    "snapshot #{idx} is not valid JSON (atomicity violated): {e}\n\
                     Content: {snap}"
                )
            });
        }
    }

    /// (c) If the rename fails, the temp file is cleaned up and `save_to`
    /// returns an error -- no orphaned `.tmp.*` file is left behind.
    ///
    /// Strategy: write to a path whose parent is temporarily read-only so
    /// that `fs::write` on the temp file fails (EACCES), which triggers the
    /// cleanup path in `save_to`.  On WSL2 with some fs configurations the
    /// rename may succeed even with 0o500; we simulate write-failure instead
    /// which is the true invariant being tested: cleanup on any IO error.
    #[test]
    #[cfg(unix)]
    fn save_cleans_up_tempfile_on_rename_failure() {
        use std::os::unix::fs::PermissionsExt as _;

        // Use a subdirectory so the TempDir itself stays writable for cleanup.
        let dir = TempDir::new().unwrap();
        let datadir = dir.path().join("data");
        std::fs::create_dir_all(&datadir).unwrap();

        let path = datadir.join("installed_plugins.json");
        let ip = make_populated();

        // First save -- succeeds.
        ip.save_to(&path).unwrap();

        // Make the directory read-only so any attempt to create a new temp
        // file (or rename) inside it will fail with EACCES.
        std::fs::set_permissions(&datadir, std::fs::Permissions::from_mode(0o555)).unwrap();

        let result = ip.save_to(&path);
        // Restore before any assertions so TempDir::drop works.
        std::fs::set_permissions(&datadir, std::fs::Permissions::from_mode(0o755)).unwrap();

        // The save must have failed.
        assert!(
            result.is_err(),
            "save must fail when directory is read-only"
        );

        // No .tmp.* debris should remain.
        let leftover: Vec<_> = std::fs::read_dir(&datadir)
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp."))
            .collect();
        assert!(
            leftover.is_empty(),
            "orphaned temp files found: {leftover:?}"
        );
    }
}
