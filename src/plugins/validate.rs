//! Input validation for plugin/marketplace source URLs and derived paths.
//!
//! Centralizes the scheme-allowlist and plugin-directory-name checks that
//! previously were scattered (and in some code paths entirely missing) across
//! `manager.rs` and `git.rs`. See crosslink #280 and #248.

use super::PluginError;
use url::Url;

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

/// Validate a source URL against the scheme allowlist, host presence, and
/// the no-userinfo rule (reject `user:pass@host` embedded credentials —
/// those belong in git's credential helper, not in a marketplace manifest).
///
/// # Errors
///
/// Returns [`PluginError::InvalidManifest`] when the URL fails any of:
/// * unparsable as a URL
/// * scheme not in [`ALLOWED_URL_SCHEMES`]
/// * missing or empty host
/// * includes userinfo for non-ssh schemes, or a password for any scheme
pub fn validate_source_url(raw: &str) -> Result<(), PluginError> {
    // SCP-style `git@host:path` is *not* a standard URL and is commonly used
    // for git over SSH. Accept it by treating it as implicit ssh:// — but
    // only when it matches the exact `user@host:path` shape with no scheme.
    if looks_like_scp_ssh(raw) {
        return Ok(());
    }

    let url = Url::parse(raw)
        .map_err(|e| PluginError::InvalidManifest(format!("Invalid source URL '{raw}': {e}")))?;

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
    //    endpoint. A PASSWORD is still rejected on ssh.
    //  * Every other scheme (https) forbids both username and password —
    //    credentials belong in a git credential helper, never in a URL that
    //    will be logged or checked into a marketplace manifest.
    if url.password().is_some() {
        return Err(PluginError::InvalidManifest(format!(
            "Source URL must not carry an inline password (use git credential helpers): {raw}"
        )));
    }
    if url.scheme() != "ssh" && !url.username().is_empty() {
        return Err(PluginError::InvalidManifest(format!(
            "Source URL must not carry an inline username for {} (use git credential helpers): {raw}",
            url.scheme()
        )));
    }

    Ok(())
}

/// True for the `git@github.com:owner/repo.git` SSH shorthand.
fn looks_like_scp_ssh(s: &str) -> bool {
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
    let forbidden: &[char] = &['/', '\\', '\0', ':'];
    if name.chars().any(|c| forbidden.contains(&c) || c.is_control()) {
        return Err(PluginError::InvalidManifest(format!(
            "Derived directory name '{name}' contains path separator, NUL, or control character"
        )));
    }
    if name.contains("..") {
        return Err(PluginError::InvalidManifest(format!(
            "Derived directory name '{name}' contains '..'"
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
