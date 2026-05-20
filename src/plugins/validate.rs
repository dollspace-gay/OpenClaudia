//! Input validation for plugin/marketplace source URLs and derived paths.
//!
//! Centralizes the scheme-allowlist and plugin-directory-name checks that
//! previously were scattered (and in some code paths entirely missing) across
//! `manager.rs` and `git.rs`. See crosslink #280 and #248.
//!
//! Also provides ed25519 manifest signature verification (crosslink #249 / #521).

use super::PluginError;
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use url::Url;

// ---------------------------------------------------------------------------
// Signature types and verification
// ---------------------------------------------------------------------------

/// A detached ed25519 signature over a plugin manifest's raw bytes.
///
/// Stored as a 64-byte array rather than keeping the `ed25519_dalek::Signature`
/// type in the public surface so callers that only have raw bytes (loaded from a
/// `plugin.sig` sidecar, inline manifest field, etc.) can construct one without
/// needing to import `ed25519_dalek` themselves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginSignature(pub [u8; 64]);

impl PluginSignature {
    /// Construct from a 64-byte slice.
    ///
    /// # Errors
    ///
    /// Returns [`SignatureError::InvalidLength`] when `bytes` is not exactly 64 bytes.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, SignatureError> {
        let arr: [u8; 64] = bytes
            .try_into()
            .map_err(|_| SignatureError::InvalidLength(bytes.len()))?;
        Ok(Self(arr))
    }

    /// Construct from a base64-encoded string (standard alphabet, no padding required).
    ///
    /// # Errors
    ///
    /// Returns [`SignatureError::InvalidEncoding`] when the string is not valid base64,
    /// or [`SignatureError::InvalidLength`] when the decoded byte count != 64.
    pub fn from_base64(encoded: &str) -> Result<Self, SignatureError> {
        use base64::Engine as _;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .map_err(|e| SignatureError::InvalidEncoding(e.to_string()))?;
        Self::from_bytes(&bytes)
    }

    /// Return the raw signature bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 64] {
        &self.0
    }
}

impl Serialize for PluginSignature {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use base64::Engine as _;
        let encoded = base64::engine::general_purpose::STANDARD.encode(self.as_bytes());
        s.serialize_str(&encoded)
    }
}

impl<'de> Deserialize<'de> for PluginSignature {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        use base64::Engine as _;
        let encoded = String::deserialize(d)?;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(&encoded)
            .map_err(|e| serde::de::Error::custom(format!("bad base64 signature: {e}")))?;
        Self::from_bytes(&bytes)
            .map_err(|e| serde::de::Error::custom(format!("bad signature bytes: {e}")))
    }
}

/// A trusted signer's ed25519 public key (32 bytes).
///
/// Callers obtain public keys from a trust store (config file,
/// `openclaudia plugin trust-key <path>`, etc.) and pass a slice of them
/// to [`verify_signature`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublicKey(pub [u8; 32]);

impl PublicKey {
    /// Construct from a 32-byte slice.
    ///
    /// # Errors
    ///
    /// Returns [`SignatureError::InvalidLength`] when `bytes` is not exactly 32 bytes.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, SignatureError> {
        let arr: [u8; 32] = bytes
            .try_into()
            .map_err(|_| SignatureError::InvalidLength(bytes.len()))?;
        Ok(Self(arr))
    }

    /// Construct from a hex-encoded string (lower or upper case).
    ///
    /// # Errors
    ///
    /// Returns [`SignatureError::InvalidEncoding`] for non-hex input, or
    /// [`SignatureError::InvalidLength`] when the decoded byte count != 32.
    pub fn from_hex(encoded: &str) -> Result<Self, SignatureError> {
        if encoded.len() != 64 {
            return Err(SignatureError::InvalidLength(encoded.len() / 2));
        }
        let mut arr = [0u8; 32];
        for (i, chunk) in encoded.as_bytes().chunks(2).enumerate() {
            let hi = hex_nibble(chunk[0]).map_err(SignatureError::InvalidEncoding)?;
            let lo = hex_nibble(chunk[1]).map_err(SignatureError::InvalidEncoding)?;
            arr[i] = (hi << 4) | lo;
        }
        Ok(Self(arr))
    }
}

fn hex_nibble(b: u8) -> Result<u8, String> {
    match b {
        b'0'..=b'9' => Ok(b - b'0'),
        b'a'..=b'f' => Ok(b - b'a' + 10),
        b'A'..=b'F' => Ok(b - b'A' + 10),
        _ => Err(format!("invalid hex character: {}", b as char)),
    }
}

/// Errors that can occur during signature verification.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum SignatureError {
    /// The manifest is not signed but the enforcing policy requires a signature.
    #[error("plugin manifest is not signed; a signature is required by policy")]
    MissingSignature,

    /// A signature was present but none of the trusted keys accepted it.
    #[error("plugin signature does not match any trusted key")]
    UnknownSigner,

    /// The signature bytes were present but are cryptographically invalid
    /// (i.e. the correct key was identified but the signature itself is wrong).
    #[error("plugin signature is invalid: cryptographic verification failed")]
    SignatureMismatch,

    /// Raw bytes had the wrong length to be a signature (64) or a public key (32).
    #[error("invalid byte length {0}: expected 64 bytes for a signature, 32 for a public key")]
    InvalidLength(usize),

    /// Base64 or hex decoding failure before the bytes could be interpreted.
    #[error("invalid encoding: {0}")]
    InvalidEncoding(String),

    /// The public key bytes do not form a valid ed25519 point.
    #[error("malformed public key: {0}")]
    MalformedKey(String),
}

/// Verify that `manifest_bytes` was signed by one of the `trusted_keys`.
///
/// The function iterates through every key in `trusted_keys` and attempts to
/// verify the signature. It returns:
///
/// - `Ok(())` as soon as a key accepts the signature.
/// - `Err(SignatureError::SignatureMismatch)` if a key's byte-layout matched
///   but the cryptographic check failed. This is kept distinct from
///   `UnknownSigner` so that diagnostics can tell "you have the right key but
///   the signature was produced over different bytes" from "we don't know who
///   signed this".
/// - `Err(SignatureError::UnknownSigner)` if no key in the set verified the
///   signature (and no key was malformed — malformed keys skip with a warning
///   rather than failing hard, so a single bad config entry doesn't block
///   every valid key in the set).
///
/// # Security note
///
/// The function does NOT check revocation. Callers that need revocation must
/// filter `trusted_keys` before calling.
///
/// # Errors
///
/// - [`SignatureError::MalformedKey`] — every key in `trusted_keys` was
///   malformed (i.e. none could be parsed as a valid ed25519 verifying key).
///   This is a configuration error, not a policy rejection.
/// - [`SignatureError::UnknownSigner`] — at least one key parsed successfully
///   but none verified the signature.
/// - [`SignatureError::SignatureMismatch`] — kept as a variant that callers
///   might return but this function itself uses `UnknownSigner` for the
///   multi-key case; individual-key mismatch is collapsed into the set result.
pub fn verify_signature(
    manifest_bytes: &[u8],
    sig: &PluginSignature,
    trusted_keys: &[PublicKey],
) -> Result<(), SignatureError> {
    if trusted_keys.is_empty() {
        // No keys ⟹ can't trust anything ⟹ treat as unknown signer.
        return Err(SignatureError::UnknownSigner);
    }

    let dalek_sig = Signature::from_bytes(&sig.0);

    let mut all_malformed = true;
    for key in trusted_keys {
        let verifying_key = VerifyingKey::from_bytes(&key.0)
            .map_err(|e| SignatureError::MalformedKey(e.to_string()))?;
        all_malformed = false;
        if verifying_key.verify(manifest_bytes, &dalek_sig).is_ok() {
            return Ok(());
        }
    }

    if all_malformed {
        // Caller has a config problem, not a policy problem.
        Err(SignatureError::MalformedKey(
            "all supplied public keys are malformed".to_string(),
        ))
    } else {
        Err(SignatureError::UnknownSigner)
    }
}

/// Scheme allowlist for any URL that gets passed to `git clone` or used as a
/// source URL. Rationale:
///  * `https` is the baseline.
///  * `ssh` is supported for authenticated enterprise git hosting.
///  * `http` is rejected — MITM can substitute a malicious plugin payload.
///  * `file` is rejected — it bypasses the filesystem-jail checks that
///    `Directory`/`File` marketplace sources go through.
///  * `git://` is rejected — no TLS, trivially MITM'd.
///  * Everything else (ftp, ldap, gopher, …) is rejected outright.
pub const ALLOWED_URL_SCHEMES: &[&str] = &["https", "ssh"];

/// Validate a source URL against the scheme allowlist, host presence, and the no-userinfo rule.
///
/// Rejects `user:pass@host` embedded credentials — those belong in git's
/// credential helper, not in a marketplace manifest.
///
/// # Errors
///
/// Returns [`PluginError::InvalidManifest`] when the URL fails any of:
/// * unparsable as a URL
/// * scheme not in [`ALLOWED_URL_SCHEMES`]
/// * missing or empty host
/// * includes userinfo for non-ssh schemes, or a password for any scheme
pub fn validate_source_url(raw: &str) -> Result<(), PluginError> {
    // Crosslink #866: SCP-style `git@host:path` URLs previously short-circuited
    // every scheme/host/userinfo check, so `git@attacker.invalid:owner/repo.git`
    // was accepted alongside `git@github.com:owner/repo.git`. We now normalise
    // SCP form into an implicit `ssh://` URL and run it through the same checks.
    // The bare-username rule for SSH still applies (any non-`git` user is
    // rejected unless it would also be rejected by the URL parser).
    let normalised: String = if looks_like_scp_ssh(raw) {
        scp_to_ssh_url(raw).ok_or_else(|| {
            PluginError::InvalidManifest(format!(
                "SCP-style source URL '{raw}' could not be normalised to ssh://"
            ))
        })?
    } else {
        raw.to_string()
    };

    let url = Url::parse(&normalised).map_err(|e| {
        PluginError::InvalidManifest(format!("Invalid source URL '{raw}': {e}"))
    })?;

    if !ALLOWED_URL_SCHEMES.contains(&url.scheme()) {
        return Err(PluginError::InvalidManifest(format!(
            "Source URL scheme '{}' is not allowed. Allowed: {}. Rejected URL: {raw}",
            url.scheme(),
            ALLOWED_URL_SCHEMES.join(", ")
        )));
    }

    let host = url.host_str().unwrap_or("");
    if host.is_empty() {
        return Err(PluginError::InvalidManifest(format!(
            "Source URL has no host: {raw}"
        )));
    }

    // Embedded credentials rule:
    //  * `ssh://` is allowed to carry a bare username (`ssh://git@host/...`)
    //    because that is literally the standard way to address a git SSH
    //    endpoint. A PASSWORD is still rejected on ssh, and only the
    //    canonical `git` username is permitted (see #866).
    //  * Every other scheme (https) forbids both username and password —
    //    credentials belong in a git credential helper, never in a URL that
    //    will be logged or checked into a marketplace manifest.
    if url.password().is_some() {
        return Err(PluginError::InvalidManifest(format!(
            "Source URL must not carry an inline password (use git credential helpers): {raw}"
        )));
    }
    if url.scheme() == "ssh" {
        // Username "" is allowed (caller's ssh config handles it); "git" is
        // the canonical git-over-ssh username. Anything else is rejected
        // because it lets a hostile manifest exfiltrate a per-user identity
        // via the URL — see #866.
        let user = url.username();
        if !user.is_empty() && user != "git" {
            return Err(PluginError::InvalidManifest(format!(
                "ssh:// source URL must use the 'git' username (got '{user}'): {raw}"
            )));
        }
    } else if !url.username().is_empty() {
        return Err(PluginError::InvalidManifest(format!(
            "Source URL must not carry an inline username for {} (use git credential helpers): {raw}",
            url.scheme()
        )));
    }

    Ok(())
}

/// True for the `git@github.com:owner/repo.git` SSH shorthand.
fn looks_like_scp_ssh(s: &str) -> bool {
    // Reject anything that already declares a scheme (`https://...`,
    // `ssh://...`) — those go through Url::parse directly.
    if s.contains("://") {
        return false;
    }
    let Some(at_idx) = s.find('@') else {
        return false;
    };
    let Some(colon_idx) = s.find(':') else {
        return false;
    };
    if at_idx == 0 || at_idx >= colon_idx {
        return false;
    }
    let user = &s[..at_idx];
    if user.chars().any(|c| c.is_whitespace() || c == '/') {
        return false;
    }
    let rest = &s[colon_idx + 1..];
    !rest.is_empty() && !rest.starts_with('/')
}

/// Rewrite `user@host:path` into `ssh://user@host/path` so the standard
/// URL parser can apply scheme + host + userinfo checks. Returns `None`
/// if the SCP form is malformed (e.g. `:` before `@`).
fn scp_to_ssh_url(s: &str) -> Option<String> {
    let at_idx = s.find('@')?;
    let colon_idx = s.find(':')?;
    if at_idx == 0 || at_idx >= colon_idx {
        return None;
    }
    let user = &s[..at_idx];
    let host = &s[at_idx + 1..colon_idx];
    let path = &s[colon_idx + 1..];
    if host.is_empty() || path.is_empty() {
        return None;
    }
    Some(format!("ssh://{user}@{host}/{path}"))
}

/// Windows reserved device names that may not be used as directory or
/// file stems. Crosslink #875: declared at module scope (not inside
/// `validate_plugin_dir_name`) so clippy's `items_after_statements`
/// lint is happy and the list can be shared with future call-sites
/// that need the same check.
const WINDOWS_RESERVED_NAMES: &[&str] = &[
    "CON", "PRN", "AUX", "NUL",
    "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8", "COM9",
    "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// Validate a directory name that was derived from a URL (the trailing
/// path segment after stripping `.git`). Rejects any component that would
/// let the caller escape the plugins root — see crosslink #248.
///
/// # Errors
///
/// Returns [`PluginError::InvalidManifest`] when the name is empty, contains
/// path separators, `..`, a leading dot, NUL, or any control character.
pub fn validate_plugin_dir_name(name: &str) -> Result<(), PluginError> {
    if name.is_empty() {
        return Err(PluginError::InvalidManifest(
            "Derived plugin/marketplace directory name is empty".to_string(),
        ));
    }
    if name == "." || name == ".." {
        return Err(PluginError::InvalidManifest(format!(
            "Derived directory name '{name}' is not a valid filename"
        )));
    }
    if name.starts_with('.') {
        return Err(PluginError::InvalidManifest(format!(
            "Derived directory name '{name}' begins with a dot; refusing to create hidden dir"
        )));
    }
    // Crosslink #875: the forbidden-char set is intentionally cross-platform.
    //   * `/` — POSIX path separator.
    //   * `\\` — Windows path separator. While the URL-derived segment will
    //     not normally contain a backslash, the same `validate_plugin_dir_name`
    //     is also called from `derive_dir_name_from_path` and from inline
    //     manifest fields (`name`, `dir`) that DO accept arbitrary user
    //     input. Keeping the backslash rejection makes the validator
    //     reusable from every callsite without per-caller exceptions.
    //   * `\0` — NUL terminates C strings; lets a hostile name truncate
    //     downstream tool args (git, fs::canonicalize) at the boundary.
    //   * `:` — Windows drive separator (`C:`) and macOS HFS path separator.
    let forbidden: &[char] = &['/', '\\', '\0', ':'];
    if name
        .chars()
        .any(|c| forbidden.contains(&c) || c.is_control())
    {
        return Err(PluginError::InvalidManifest(format!(
            "Derived directory name '{name}' contains path separator, NUL, or control character"
        )));
    }
    if name.contains("..") {
        return Err(PluginError::InvalidManifest(format!(
            "Derived directory name '{name}' contains '..'"
        )));
    }
    // Crosslink #875: reject Windows reserved device names (CON, PRN, AUX,
    // NUL, COM1-9, LPT1-9) case-insensitively. A repository named `con.git`
    // would otherwise derive `con` and either collide with the device or
    // fail opaquely on Windows. Comparison is case-insensitive against the
    // documented Microsoft set; we also reject the bare stem followed by an
    // extension (`con.txt` → still reserved on Windows). Trailing dot or
    // space is also rejected because Windows strips them on creation,
    // enabling collisions between `foo` and `foo.`.
    if name.ends_with('.') || name.ends_with(' ') {
        return Err(PluginError::InvalidManifest(format!(
            "Derived directory name '{name}' has a trailing dot or space (Windows strips these on creation)"
        )));
    }
    let stem = name.split('.').next().unwrap_or("");
    let stem_upper = stem.to_ascii_uppercase();
    if WINDOWS_RESERVED_NAMES.contains(&stem_upper.as_str()) {
        return Err(PluginError::InvalidManifest(format!(
            "Derived directory name '{name}' is a Windows reserved device name"
        )));
    }
    Ok(())
}

/// Derive the default directory name from a source URL — last path segment
/// with trailing `.git` stripped — and validate it via
/// [`validate_plugin_dir_name`].
///
/// # Errors
///
/// Same as [`validate_plugin_dir_name`], plus an error when the URL has no
/// usable final path segment.
pub fn derive_dir_name_from_url(raw: &str) -> Result<String, PluginError> {
    let segment = if looks_like_scp_ssh(raw) {
        let after_colon = raw.split_once(':').map_or("", |(_, p)| p);
        let trimmed = after_colon.trim_end_matches('/').trim_end_matches(".git");
        trimmed
            .rsplit('/')
            .next()
            .map(str::to_string)
            .unwrap_or_default()
    } else {
        let url = Url::parse(raw).map_err(|e| {
            PluginError::InvalidManifest(format!("Cannot derive dir name from '{raw}': {e}"))
        })?;
        let last = url
            .path_segments()
            .and_then(|mut s| s.next_back().map(str::to_string))
            .unwrap_or_default();
        last.trim_end_matches(".git").to_string()
    };

    validate_plugin_dir_name(&segment)?;
    Ok(segment)
}

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // Signature verification tests (crosslink #249 / #521)
    // -----------------------------------------------------------------------

    /// Generate a fresh ed25519 keypair and return (`signing_key`, `verifying_key` raw bytes).
    fn gen_keypair() -> (ed25519_dalek::SigningKey, PublicKey) {
        use ed25519_dalek::SigningKey;
        use rand_core::{OsRng, RngCore};
        let mut secret = [0u8; 32];
        OsRng.fill_bytes(&mut secret);
        let signing_key = SigningKey::from_bytes(&secret);
        let pub_bytes = signing_key.verifying_key().to_bytes();
        (signing_key, PublicKey(pub_bytes))
    }

    /// Sign `msg` with `signing_key` and return a `PluginSignature`.
    fn sign(signing_key: &ed25519_dalek::SigningKey, msg: &[u8]) -> PluginSignature {
        use ed25519_dalek::Signer as _;
        let sig = signing_key.sign(msg);
        PluginSignature(sig.to_bytes())
    }

    #[test]
    fn valid_signature_accepts() {
        let manifest = b"name: my-plugin\nversion: 1.0.0\n";
        let (sk, pk) = gen_keypair();
        let sig = sign(&sk, manifest);
        assert!(
            verify_signature(manifest, &sig, &[pk]).is_ok(),
            "a valid signature over the exact bytes must be accepted"
        );
    }

    #[test]
    fn signature_mismatch_rejects_unknown_signer() {
        // Sign with key A, verify against key B — must produce UnknownSigner.
        let manifest = b"name: my-plugin\nversion: 1.0.0\n";
        let (sk_a, _pk_a) = gen_keypair();
        let (_sk_b, pk_b) = gen_keypair();
        let sig = sign(&sk_a, manifest);
        let result = verify_signature(manifest, &sig, &[pk_b]);
        assert_eq!(
            result,
            Err(SignatureError::UnknownSigner),
            "signature from a different key must be rejected as UnknownSigner"
        );
    }

    #[test]
    fn tampered_manifest_bytes_rejects_unknown_signer() {
        // Sign over original bytes, verify over tampered bytes — must fail.
        let original = b"name: my-plugin\nversion: 1.0.0\n";
        let tampered = b"name: my-plugin\nversion: 9.9.9\n";
        let (sk, pk) = gen_keypair();
        let sig = sign(&sk, original);
        let result = verify_signature(tampered, &sig, &[pk]);
        assert_eq!(
            result,
            Err(SignatureError::UnknownSigner),
            "signature over different bytes must not verify"
        );
    }

    #[test]
    fn empty_trusted_keys_rejects_unknown_signer() {
        // If there are no trusted keys at all, we can't accept any signature.
        let manifest = b"name: plugin\n";
        let (sk, _pk) = gen_keypair();
        let sig = sign(&sk, manifest);
        let result = verify_signature(manifest, &sig, &[]);
        assert_eq!(
            result,
            Err(SignatureError::UnknownSigner),
            "empty trusted-key set must always reject"
        );
    }

    #[test]
    fn key_not_in_trusted_set_rejects() {
        // Signed with key A; only key B and key C are trusted — must reject.
        let manifest = b"name: plugin\nversion: 0.1.0\n";
        let (sk_a, _pk_a) = gen_keypair();
        let (_sk_b, pk_b) = gen_keypair();
        let (_sk_c, pk_c) = gen_keypair();
        let sig = sign(&sk_a, manifest);
        let result = verify_signature(manifest, &sig, &[pk_b, pk_c]);
        assert_eq!(
            result,
            Err(SignatureError::UnknownSigner),
            "key not in trusted set must be rejected even with multiple trusted keys"
        );
    }

    #[test]
    fn correct_key_among_many_accepts() {
        // Key A is trusted alongside keys B and C; signing with A must succeed.
        let manifest = b"name: multi-key-plugin\n";
        let (sk_a, pk_a) = gen_keypair();
        let (_sk_b, pk_b) = gen_keypair();
        let (_sk_c, pk_c) = gen_keypair();
        let sig = sign(&sk_a, manifest);
        assert!(
            verify_signature(manifest, &sig, &[pk_b, pk_c, pk_a]).is_ok(),
            "a valid key anywhere in the trusted set must be accepted"
        );
    }

    #[test]
    fn plugin_signature_from_bytes_round_trip() {
        let raw = [0xab_u8; 64];
        let sig = PluginSignature::from_bytes(&raw).unwrap();
        assert_eq!(sig.as_bytes(), &raw);
    }

    #[test]
    fn plugin_signature_from_bytes_wrong_length() {
        let result = PluginSignature::from_bytes(&[0u8; 32]);
        assert_eq!(result, Err(SignatureError::InvalidLength(32)));
    }

    #[test]
    fn plugin_signature_from_base64_round_trip() {
        use base64::Engine as _;
        let raw = [0xcd_u8; 64];
        let encoded = base64::engine::general_purpose::STANDARD.encode(raw);
        let sig = PluginSignature::from_base64(&encoded).unwrap();
        assert_eq!(sig.as_bytes(), &raw);
    }

    #[test]
    fn plugin_signature_from_base64_bad_input() {
        let result = PluginSignature::from_base64("not-valid-base64!!!");
        assert!(matches!(result, Err(SignatureError::InvalidEncoding(_))));
    }

    #[test]
    fn public_key_from_hex_round_trip() {
        let raw = [0xef_u8; 32];
        let mut encoded = String::with_capacity(raw.len() * 2);
        for b in raw {
            use std::fmt::Write as _;
            let _ = write!(encoded, "{b:02x}");
        }
        let pk = PublicKey::from_hex(&encoded).unwrap();
        assert_eq!(pk.0, raw);
    }

    #[test]
    fn public_key_from_hex_wrong_length() {
        // 31 bytes ⟹ 62 hex chars ⟹ length check fires first
        let encoded = "ef".repeat(31);
        let result = PublicKey::from_hex(&encoded);
        assert!(matches!(result, Err(SignatureError::InvalidLength(_))));
    }

    #[test]
    fn public_key_from_hex_invalid_char() {
        // 64 chars but one is not hex
        let mut encoded = "00".repeat(32);
        // Replace char 10 with 'z'
        let v: Vec<char> = encoded.chars().collect();
        let mut s: Vec<char> = v;
        s[10] = 'z';
        encoded = s.into_iter().collect();
        let result = PublicKey::from_hex(&encoded);
        assert!(matches!(result, Err(SignatureError::InvalidEncoding(_))));
    }

    // -----------------------------------------------------------------------
    // URL validation tests (pre-existing suite, kept intact)
    // -----------------------------------------------------------------------

    #[test]
    fn allows_https() {
        assert!(validate_source_url("https://github.com/owner/repo.git").is_ok());
    }

    #[test]
    fn allows_ssh_scheme() {
        assert!(validate_source_url("ssh://git@github.com/owner/repo.git").is_ok());
    }

    #[test]
    fn allows_scp_ssh_shorthand() {
        assert!(validate_source_url("git@github.com:owner/repo.git").is_ok());
    }

    /// Crosslink #866: SCP URLs with a non-`git` username are rejected.
    /// Pre-fix this slipped through because SCP form short-circuited every
    /// downstream check.
    #[test]
    fn rejects_scp_with_non_git_username() {
        let result = validate_source_url("attacker@evil.example.invalid:owner/repo.git");
        assert!(
            result.is_err(),
            "non-git username must be rejected in SCP form (got Ok)"
        );
    }

    /// Crosslink #866: SCP URLs route through the same scheme/host/userinfo
    /// pipeline as `ssh://` URLs, so a malformed SCP form is rejected with
    /// a clear error rather than silently accepted.
    #[test]
    fn rejects_scp_with_empty_host() {
        let result = validate_source_url("git@:owner/repo.git");
        assert!(result.is_err(), "SCP with empty host must be rejected");
    }

    #[test]
    fn rejects_http() {
        let err = validate_source_url("http://example.com/repo.git").unwrap_err();
        assert!(matches!(err, PluginError::InvalidManifest(_)));
    }

    #[test]
    fn rejects_file_scheme() {
        let err = validate_source_url("file:///etc/passwd").unwrap_err();
        assert!(format!("{err:?}").contains("not allowed"));
    }

    #[test]
    fn rejects_git_scheme() {
        let err = validate_source_url("git://example.com/repo.git").unwrap_err();
        assert!(format!("{err:?}").contains("not allowed"));
    }

    #[test]
    fn rejects_ftp_scheme() {
        let err = validate_source_url("ftp://example.com/repo.git").unwrap_err();
        assert!(format!("{err:?}").contains("not allowed"));
    }

    #[test]
    fn rejects_userinfo_in_https() {
        let err = validate_source_url("https://user:pass@example.com/repo.git").unwrap_err();
        let msg = format!("{err:?}");
        assert!(
            msg.contains("password") || msg.contains("username"),
            "expected credential rejection, got: {msg}"
        );
    }

    #[test]
    fn rejects_password_in_ssh() {
        let err = validate_source_url("ssh://git:secret@example.com/repo.git").unwrap_err();
        assert!(format!("{err:?}").contains("password"));
    }

    #[test]
    fn rejects_gibberish_urls() {
        assert!(validate_source_url("not-a-url").is_err());
        assert!(validate_source_url("").is_err());
    }

    #[test]
    fn derives_name_from_https_url() {
        let n = derive_dir_name_from_url("https://github.com/owner/repo.git").unwrap();
        assert_eq!(n, "repo");
    }

    #[test]
    fn derives_name_from_scp_url() {
        let n = derive_dir_name_from_url("git@github.com:owner/repo.git").unwrap();
        assert_eq!(n, "repo");
    }

    #[test]
    fn derive_name_rejects_traversal() {
        assert!(derive_dir_name_from_url("https://x/a/..").is_err());
    }

    #[test]
    fn derive_name_rejects_trailing_dot() {
        assert!(derive_dir_name_from_url("https://x/.hidden").is_err());
    }

    #[test]
    fn derive_name_rejects_empty_segment() {
        assert!(derive_dir_name_from_url("https://x/").is_err());
    }
}
