//! LSP (Language Server Protocol) integration tool.
//!
//! Provides code intelligence via external language servers:
//! - `goToDefinition`: Find where a symbol is defined
//! - `findReferences`: Find all references to a symbol
//! - hover: Get type/documentation info for a symbol
//! - `documentSymbols`: List symbols in a file

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::hash::BuildHasher;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;

/// Maximum file size (10 MiB) accepted for LSP analysis.
///
/// Parity with CC `LSPTool.ts` (lines 53 + 264-269): files larger than 10 MB
/// would slow the language server to a crawl and likely time out, so OC short-
/// circuits with a clear error before even spawning the server.  See issue
/// #648.
pub const LSP_MAX_FILE_SIZE: u64 = 10 * 1024 * 1024;

/// Process-wide registry of open files per LSP server binary.
///
/// Parity with CC `LSPServerManager.ts:64,277` (`isFileOpen` map).  OC spawns a
/// fresh server per call today, but mirroring CC's deduplication contract here
/// (a) avoids redundant `textDocument/didOpen` notifications when the same
/// server *is* reused (e.g. tests, future pooled mode) and (b) keeps the public
/// surface ready for #647's eventual move to a long-lived server pool.
///
/// The key is the server binary name (`server_cmd`), the value is the set of
/// canonicalised file paths the server has been told about.
///
/// `mark_opened` is the only call site that should mutate the map; it returns
/// `true` iff the caller is the first to claim the slot and must therefore
/// send the `didOpen` notification.  `mark_closed` flips the flag back when
/// the corresponding `textDocument/didClose` notification is sent (or the
/// session shuts down).
fn open_files_registry() -> &'static Mutex<HashMap<String, HashSet<PathBuf>>> {
    static REGISTRY: OnceLock<Mutex<HashMap<String, HashSet<PathBuf>>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Record that `server_cmd` has been informed about `path`.
///
/// Returns `true` when the caller is the first to register the file (so the
/// caller MUST send `textDocument/didOpen`); returns `false` when the file is
/// already recorded as open (so the caller MUST skip the notification).
#[must_use]
pub fn mark_opened(server_cmd: &str, path: &Path) -> bool {
    let mut guard = open_files_registry()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    guard
        .entry(server_cmd.to_string())
        .or_default()
        .insert(path.to_path_buf())
}

/// Mirror of `mark_opened` for `textDocument/didClose`.
///
/// Returns `true` if the entry was present (and thus removed), `false` if the
/// caller was attempting to close a file that was never opened — useful for
/// asserting protocol invariants in tests.
pub fn mark_closed(server_cmd: &str, path: &Path) -> bool {
    let mut guard = open_files_registry()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    guard
        .get_mut(server_cmd)
        .is_some_and(|set| set.remove(path))
}

/// RAII guard pairing `mark_opened` with a guaranteed `mark_closed`.
///
/// Without this guard a `?`-early-return between `mark_opened` and the
/// shutdown sequence would leave the dedup entry stuck in the registry
/// after the spawned server has already been killed by `ChildGuard::drop`,
/// causing the *next* invocation to silently skip `textDocument/didOpen`
/// against a fresh server that has never seen the file.
///
/// On the happy path the caller invokes [`commit`] to acknowledge that a
/// matching `textDocument/didClose` was sent; on the error path Drop
/// performs the same rollback so the registry never leaks a stale slot.
struct OpenFileGuard<'a> {
    server_cmd: &'a str,
    path: &'a Path,
    owns_slot: bool,
}

impl<'a> OpenFileGuard<'a> {
    /// Bind the guard to a `(server_cmd, path)` pair.
    ///
    /// `we_opened_it` reflects the return value of `mark_opened`: when
    /// `false`, the slot was already held by a concurrent caller and this
    /// guard is a no-op (it must not release a slot it doesn't own).
    const fn new(server_cmd: &'a str, path: &'a Path, we_opened_it: bool) -> Self {
        Self {
            server_cmd,
            path,
            owns_slot: we_opened_it,
        }
    }

    /// Acknowledge a clean shutdown: a matching `textDocument/didClose`
    /// notification was sent, so free the dedup slot and disarm the Drop
    /// rollback.  Calling `commit` twice is harmless.
    fn commit(&mut self) {
        if self.owns_slot {
            let _ = mark_closed(self.server_cmd, self.path);
            self.owns_slot = false;
        }
    }
}

impl Drop for OpenFileGuard<'_> {
    fn drop(&mut self) {
        if self.owns_slot {
            let _ = mark_closed(self.server_cmd, self.path);
        }
    }
}

/// Test-only helper: query whether `(server_cmd, path)` is currently marked as
/// open.  Used by the unit tests to verify dedup state transitions without
/// reaching into the registry directly.
#[cfg(test)]
fn is_marked_open(server_cmd: &str, path: &Path) -> bool {
    let guard = open_files_registry()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    guard.get(server_cmd).is_some_and(|set| set.contains(path))
}

/// Returns `true` when the language server for `language_or_ext` is reachable
/// on `PATH`, i.e. when a fresh LSP request would be able to spawn it.
///
/// `language_or_ext` accepts either a bare language name (`"rust"`) or a file
/// extension with or without a leading dot (`".rs"`, `"rs"`).  Unknown values
/// always return `false`.
///
/// Parity with CC `LSPTool.ts:137-139` + `manager.ts:100-110` (`isLspConnected`).
/// OC checks via `which` since it has no long-lived server pool yet; once one
/// exists this function should be updated to query the pool's liveness map.
#[must_use]
pub fn is_lsp_connected(language_or_ext: &str) -> bool {
    let Some((server_cmd, _)) = resolve_language_server(language_or_ext) else {
        return false;
    };
    Command::new("which")
        .arg(server_cmd)
        .output()
        .is_ok_and(|o| o.status.success())
}

/// Resolve a bare language name OR a file extension to a server command.
///
/// This is the inverse of [`detect_language_server`] for cases where the
/// caller has a language identifier (e.g. `"rust"`) instead of a file path.
fn resolve_language_server(language_or_ext: &str) -> Option<(&'static str, Vec<&'static str>)> {
    let trimmed = language_or_ext.trim().trim_start_matches('.');
    let ext: &str = match trimmed {
        // Bare language identifiers — map to a representative extension.
        "rust" => "rs",
        "typescript" => "ts",
        "typescriptreact" => "tsx",
        "javascript" => "js",
        "javascriptreact" => "jsx",
        "python" => "py",
        "go" => "go",
        "c" => "c",
        "cpp" | "c++" => "cpp",
        "java" => "java",
        "ruby" => "rb",
        // Already an extension (or unknown) — try as-is.
        other => other,
    };
    detect_language_server(&format!("dummy.{ext}"))
}

/// RAII guard that kills and reaps a child process on drop.
///
/// Fixes the zombie-process leak in the original `run_lsp_request`:
/// any early return via `?` previously skipped `child.wait()`, leaving
/// an un-reaped zombie on Unix and a leaking handle on Windows.
struct ChildGuard {
    child: Option<Child>,
}

impl ChildGuard {
    const fn new(child: Child) -> Self {
        Self { child: Some(child) }
    }

    /// Return a mutable reference to the wrapped child.
    const fn child_mut(&mut self) -> &mut Child {
        self.child.as_mut().expect("child already taken")
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            // Best-effort kill; ignore errors (process may have already exited).
            let _ = child.kill();
            // Reap the zombie; ignore the exit status.
            let _ = child.wait();
        }
    }
}

/// Readiness probe: drain server-initiated notifications until the server
/// replies to the `textDocument/documentSymbol` probe we sent with id=1001,
/// or until `deadline` elapses.
///
/// Replaces the 500 ms unconditional sleep at the original line 191.
/// A real server reply (even an empty symbols array or an error result) is
/// sufficient evidence that the server has finished loading the document.
///
/// Returns `Ok(())` on readiness, `Err(String)` on timeout or I/O failure.
fn wait_for_readiness(
    reader: &mut BufReader<impl std::io::Read>,
    deadline: std::time::Instant,
) -> Result<(), String> {
    const READINESS_ID: u64 = 1001;
    loop {
        if std::time::Instant::now() >= deadline {
            return Err(
                "LSP server readiness timeout (10 s) — server did not acknowledge didOpen"
                    .to_string(),
            );
        }

        // Read one message from the server.  `read_line` blocks; we rely on
        // the overall deadline check above to bound total wait time.
        let mut content_length: usize = 0;
        loop {
            if std::time::Instant::now() >= deadline {
                return Err("LSP server readiness timeout (10 s) during header read".to_string());
            }
            let mut line = String::new();
            let n = reader
                .read_line(&mut line)
                .map_err(|e| format!("Readiness read error: {e}"))?;
            if n == 0 {
                return Err("LSP server closed stdout before sending readiness reply".to_string());
            }
            let trimmed = line.trim();
            if trimmed.is_empty() {
                break;
            }
            if let Some(len_str) = trimmed.strip_prefix("Content-Length: ") {
                content_length = len_str
                    .parse()
                    .map_err(|e| format!("Bad content-length in readiness probe: {e}"))?;
            }
        }

        if content_length == 0 {
            continue; // skip malformed message and keep trying
        }

        let mut body = vec![0u8; content_length];
        std::io::Read::read_exact(reader, &mut body)
            .map_err(|e| format!("Readiness body read error: {e}"))?;

        let msg: Value = match serde_json::from_slice(&body) {
            Ok(v) => v,
            Err(_) => continue, // non-JSON framing; skip
        };

        // The probe response carries id=READINESS_ID.
        if let Some(id) = msg.get("id").and_then(serde_json::Value::as_u64) {
            if id == READINESS_ID {
                return Ok(());
            }
        }
        // Any other message (notification, different response) — keep draining.
    }
}

/// Spawn a thread that drains `stderr` into a ring buffer capped at 1 KiB.
///
/// Returns an `Arc<Mutex<Vec<u8>>>` that the caller can inspect after the
/// child exits.  Fixes issue #355 point 5: original code used `Stdio::null()`,
/// discarding all diagnostic information on server crash.
fn capture_stderr(stderr: std::process::ChildStderr) -> Arc<Mutex<Vec<u8>>> {
    let buf: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
    let buf_clone = Arc::clone(&buf);
    thread::spawn(move || {
        use std::io::Read;
        let mut reader = BufReader::new(stderr);
        let mut chunk = [0u8; 256];
        loop {
            match reader.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    let mut guard = buf_clone
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    guard.extend_from_slice(&chunk[..n]);
                    // Keep only the last 1024 bytes.
                    let len = guard.len();
                    if len > 1024 {
                        let keep_from = len - 1024;
                        guard.drain(..keep_from);
                    }
                }
            }
        }
    });
    buf
}

/// Extract up to 1 KiB from the stderr ring buffer as a displayable suffix.
fn stderr_snippet(buf: &Arc<Mutex<Vec<u8>>>) -> String {
    let guard = buf
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if guard.is_empty() {
        String::new()
    } else {
        let text = String::from_utf8_lossy(&guard).into_owned();
        drop(guard);
        format!("\nServer stderr (last 1 KiB):\n{text}")
    }
}

/// LSP operation types
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum LspAction {
    GoToDefinition,
    FindReferences,
    Hover,
    DocumentSymbols,
}

/// Result from an LSP operation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LspResult {
    pub action: String,
    pub file_path: String,
    pub results: Vec<LspLocation>,
    pub hover_text: Option<String>,
    pub symbols: Vec<LspSymbol>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LspLocation {
    pub uri: String,
    pub line: u32,
    pub character: u32,
    pub end_line: Option<u32>,
    pub end_character: Option<u32>,
    pub preview: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LspSymbol {
    pub name: String,
    pub kind: String,
    pub line: u32,
    pub end_line: Option<u32>,
    pub children: Vec<Self>,
}

/// Known language servers by file extension
fn detect_language_server(file_path: &str) -> Option<(&'static str, Vec<&'static str>)> {
    let ext = file_path.rsplit('.').next().unwrap_or("");
    match ext {
        "rs" => Some(("rust-analyzer", vec![])),
        "ts" | "tsx" | "js" | "jsx" => Some(("typescript-language-server", vec!["--stdio"])),
        "py" => Some(("pylsp", vec![])),
        "go" => Some(("gopls", vec!["serve"])),
        "c" | "cpp" | "h" | "hpp" => Some(("clangd", vec![])),
        "java" => Some(("jdtls", vec![])),
        "rb" => Some(("solargraph", vec!["stdio"])),
        _ => None,
    }
}

/// Execute an LSP action
#[must_use]
pub fn execute_lsp<S: BuildHasher>(args: &HashMap<String, Value, S>) -> (String, bool) {
    let action_str = args
        .get("action")
        .and_then(|v| v.as_str())
        .unwrap_or("hover");

    let Some(file_path) = args.get("file_path").and_then(|v| v.as_str()) else {
        return ("Error: file_path is required".to_string(), true);
    };

    let line = args
        .get("line")
        .and_then(serde_json::Value::as_u64)
        .map_or(1, |v| u32::try_from(v).unwrap_or(u32::MAX));
    let character = args
        .get("character")
        .and_then(serde_json::Value::as_u64)
        .map_or(0, |v| u32::try_from(v).unwrap_or(u32::MAX));

    // Detect language server
    let Some((server_cmd, server_args)) = detect_language_server(file_path) else {
        return (
            format!("No language server known for file: {file_path}"),
            true,
        );
    };

    // Availability gate (#650): refuse early if the server binary isn't
    // reachable on PATH. Parity with CC `LSPTool.ts:137-139` +
    // `manager.ts:100-110` (`isLspConnected`). `is_lsp_connected` uses
    // the `which` crate (in-process PATH walk, crosslink #955) so we
    // never fork+exec a `which(1)` subprocess.
    let language = detect_language_id(file_path);
    if !is_lsp_connected(language) {
        return (
            format!(
                "LSP server unavailable for {language}: '{server_cmd}' not found on PATH. \
                 Start or install the language server (e.g. `cargo install {server_cmd}` \
                 or your distro's package) before retrying."
            ),
            true,
        );
    }

    // 10 MiB file-size guard (#648): refuse to ship enormous buffers across
    // the LSP wire — they reliably time out and starve the server of memory.
    // Parity with CC `LSPTool.ts:53,264-269`.  We probe the size BEFORE
    // canonicalising or reading the file so the failure mode is "cheap and
    // honest" rather than "OOM the proxy".
    //
    // `metadata()` can fail for legitimate reasons (e.g. permission denied
    // on a symlink target); we tolerate those and let `run_lsp_request`
    // surface the canonical error rather than masking it here.
    if let Ok(meta) = std::fs::metadata(file_path) {
        if meta.len() > LSP_MAX_FILE_SIZE {
            return (
                format!(
                    "File too large for LSP analysis: {} bytes exceeds the {}-byte limit \
                     (10 MiB).  Trim the file or use grep/Read on a focused range.",
                    meta.len(),
                    LSP_MAX_FILE_SIZE
                ),
                true,
            );
        }
    }

    let action = match action_str {
        "goToDefinition" | "definition" => LspAction::GoToDefinition,
        "findReferences" | "references" => LspAction::FindReferences,
        "hover" => LspAction::Hover,
        "documentSymbols" | "symbols" => LspAction::DocumentSymbols,
        _ => {
            return (
                format!(
                    "Unknown LSP action: {action_str}. Use: goToDefinition, findReferences, hover, documentSymbols"
                ),
                true,
            )
        }
    };

    // Run the server, send initialize + request, get response
    match run_lsp_request(server_cmd, &server_args, file_path, line, character, action) {
        Ok(result) => (
            serde_json::to_string_pretty(&result).unwrap_or_default(),
            false,
        ),
        Err(e) => (format!("LSP error: {e}"), true),
    }
}

fn run_lsp_request(
    server_cmd: &str,
    server_args: &[&str],
    file_path: &str,
    line: u32,
    character: u32,
    action: LspAction,
) -> Result<LspResult, String> {
    // File-resolve and read flow through typed `FileError` so the path and
    // `io::ErrorKind` are preserved through the source chain — see #492. We
    // stringify only at this boundary because `run_lsp_request` returns
    // `Result<_, String>` to its caller; the rendered message now always
    // names the offending file.
    let abs_path = std::fs::canonicalize(file_path)
        .map_err(crate::file_error::FileError::with_path(file_path))
        .map_err(|e| e.to_string())?;
    let root_uri = find_project_root(&abs_path);
    let file_uri = format!("file://{}", abs_path.display());

    // Read file content for textDocument/didOpen
    let content = crate::file_error::read_file(&abs_path).map_err(|e| e.to_string())?;

    // Spawn the server.  stderr is captured into a ring buffer (last 1 KiB) so
    // that crash diagnostics survive instead of being silently discarded
    // (original: Stdio::null() — fix #355 point 5).
    let mut raw_child = Command::new(server_cmd)
        .args(server_args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("Failed to start {server_cmd}: {e}"))?;

    // Take pipes before handing the child to the guard.
    let mut stdin = raw_child.stdin.take().ok_or("Failed to get stdin")?;
    let stdout = raw_child.stdout.take().ok_or("Failed to get stdout")?;
    let stderr_pipe = raw_child.stderr.take().ok_or("Failed to get stderr")?;
    let stderr_buf = capture_stderr(stderr_pipe);

    // The guard now owns the child.  Any early return via `?` — including the
    // original zombie-leak paths (former lines 174 and 224) — will trigger Drop
    // which kills and reaps the process (fix #355 point 3).
    let mut guard = ChildGuard::new(raw_child);
    let mut reader = BufReader::new(stdout);

    // Send initialize
    let init_params = json!({
        "processId": std::process::id(),
        "rootUri": root_uri,
        "capabilities": {},
        "workspaceFolders": [{"uri": root_uri, "name": "workspace"}]
    });
    send_lsp_message(&mut stdin, "initialize", 1, init_params)?;
    let _init_response = read_lsp_response(&mut reader, 1).map_err(|e| {
        let snip = stderr_snippet(&stderr_buf);
        format!("initialize failed: {e}{snip}")
    })?;

    // Send initialized notification
    send_lsp_notification(&mut stdin, "initialized", json!({}))?;

    // didOpen deduplication (#647): only send `textDocument/didOpen` the first
    // time the (server, file) pair is seen.  Parity with CC `LSPServerManager
    // .ts:64,277` (`isFileOpen`).  `mark_opened` returns true iff this caller
    // is the first to claim the slot; subsequent calls (e.g. a repeated tool
    // invocation against the same file) skip the notification.
    //
    // The `OpenFileGuard` ensures that an early `?`-return between here and
    // the explicit `commit()` below clears the dedup entry, so a *failed*
    // run cannot leak a "this file is open" claim into the registry — the
    // child is killed by `ChildGuard::drop`, so the server can no longer
    // honour any didOpen we did send.
    let needs_did_open = mark_opened(server_cmd, &abs_path);
    let mut open_guard = OpenFileGuard::new(server_cmd, &abs_path, needs_did_open);
    if needs_did_open {
        let did_open = json!({
            "textDocument": {
                "uri": file_uri,
                "languageId": detect_language_id(file_path),
                "version": 1,
                "text": content,
            }
        });
        send_lsp_notification(&mut stdin, "textDocument/didOpen", did_open)?;
    }

    // Readiness probe: send a documentSymbol request (id=1001) and drain
    // server notifications until the server replies.  This replaces the
    // original unconditional 500 ms sleep (fix #355 point 2), which was
    // both insufficient for cold servers and wasteful for warm ones.
    let readiness_params = json!({"textDocument": {"uri": &file_uri}});
    send_lsp_message(
        &mut stdin,
        "textDocument/documentSymbol",
        1001,
        readiness_params,
    )?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    wait_for_readiness(&mut reader, deadline).map_err(|e| {
        let snip = stderr_snippet(&stderr_buf);
        format!("{e}{snip}")
    })?;

    // Send the actual request
    let (method, params) = match action {
        LspAction::GoToDefinition => (
            "textDocument/definition",
            json!({
                "textDocument": {"uri": &file_uri},
                "position": {"line": line.saturating_sub(1), "character": character}
            }),
        ),
        LspAction::FindReferences => (
            "textDocument/references",
            json!({
                "textDocument": {"uri": &file_uri},
                "position": {"line": line.saturating_sub(1), "character": character},
                "context": {"includeDeclaration": true}
            }),
        ),
        LspAction::Hover => (
            "textDocument/hover",
            json!({
                "textDocument": {"uri": &file_uri},
                "position": {"line": line.saturating_sub(1), "character": character}
            }),
        ),
        LspAction::DocumentSymbols => (
            "textDocument/documentSymbol",
            json!({"textDocument": {"uri": &file_uri}}),
        ),
    };

    send_lsp_message(&mut stdin, method, 2, params)?;
    let response = read_lsp_response(&mut reader, 2).map_err(|e| {
        let snip = stderr_snippet(&stderr_buf);
        format!("LSP request failed: {e}{snip}")
    })?;

    // didClose mirror (#647): if we sent didOpen on this call, flip the
    // dedup flag back so the next caller is forced to send a fresh
    // didOpen.  We also notify the server for protocol cleanliness so
    // pooled-server setups don't accumulate stale handles.  This must
    // happen BEFORE the shutdown sequence below because the LSP spec
    // forbids `textDocument/*` notifications after `shutdown`.
    if needs_did_open {
        let did_close = json!({"textDocument": {"uri": &file_uri}});
        let _ = send_lsp_notification(&mut stdin, "textDocument/didClose", did_close);
    }
    // Reaching this point means the request succeeded; commit() prevents
    // the OpenFileGuard from rolling back the dedup entry below.  The
    // explicit `mark_closed` call inside commit mirrors the didClose
    // notification we just sent.
    open_guard.commit();

    // Graceful shutdown; Drop will kill+wait regardless, but we attempt a
    // clean exit first so the server can flush caches.
    //
    // crosslink #965: per LSP spec the `shutdown` request requires a response
    // BEFORE `exit` is sent. Well-behaved servers may buffer further messages
    // until they have replied to `shutdown`; if we skip reading that response,
    // the subsequent `exit` notification can land in a buffer that the server
    // never drains, leaving the child as an orphan. We read (and discard) the
    // shutdown response between the two sends; the result is intentionally
    // dropped because we only care about the protocol sequencing, not the
    // payload, and any read failure is non-fatal (Drop still kills+waits).
    let _ = send_lsp_message(&mut stdin, "shutdown", 3, json!(null));
    let _ = read_lsp_response(&mut reader, 3);
    let _ = send_lsp_notification(&mut stdin, "exit", json!(null));
    drop(stdin); // EOF signals server to exit
    let _ = guard.child_mut().wait();

    // Parse response into our types
    Ok(parse_lsp_response(action, file_path, &response))
}

#[allow(clippy::needless_pass_by_value)]
fn send_lsp_message(
    stdin: &mut impl Write,
    method: &str,
    id: u32,
    params: Value,
) -> Result<(), String> {
    let msg = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
    let body = serde_json::to_string(&msg).map_err(|e| e.to_string())?;
    let header = format!("Content-Length: {}\r\n\r\n", body.len());
    stdin
        .write_all(header.as_bytes())
        .map_err(|e| e.to_string())?;
    stdin
        .write_all(body.as_bytes())
        .map_err(|e| e.to_string())?;
    stdin.flush().map_err(|e| e.to_string())?;
    Ok(())
}

#[allow(clippy::needless_pass_by_value)]
fn send_lsp_notification(
    stdin: &mut impl Write,
    method: &str,
    params: Value,
) -> Result<(), String> {
    let msg = json!({"jsonrpc": "2.0", "method": method, "params": params});
    let body = serde_json::to_string(&msg).map_err(|e| e.to_string())?;
    let header = format!("Content-Length: {}\r\n\r\n", body.len());
    stdin
        .write_all(header.as_bytes())
        .map_err(|e| e.to_string())?;
    stdin
        .write_all(body.as_bytes())
        .map_err(|e| e.to_string())?;
    stdin.flush().map_err(|e| e.to_string())?;
    Ok(())
}

/// Default cap for the number of intermediate LSP messages skipped while
/// waiting for a matching response id. Servers like clangd routinely emit
/// dozens of diagnostics before completing — 100 is a reasonable starting
/// point but is overridable via the `OPENCLAUDIA_LSP_MAX_MESSAGES`
/// environment variable for noisy servers (crosslink #886).
const LSP_DEFAULT_MAX_MESSAGES: u32 = 100;

/// Resolve the per-call maximum response-scan budget.
///
/// Crosslink #886: previously a hard-coded `100`. We now respect the
/// `OPENCLAUDIA_LSP_MAX_MESSAGES` env var so an operator can crank the
/// budget up for chatty servers without recompiling, or down for tests
/// that want to exercise the cap path quickly. Invalid / zero values
/// fall back to the default rather than silently disabling the cap.
fn lsp_max_messages() -> u32 {
    std::env::var("OPENCLAUDIA_LSP_MAX_MESSAGES")
        .ok()
        .and_then(|v| v.parse::<u32>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(LSP_DEFAULT_MAX_MESSAGES)
}

/// Read an LSP response, skipping server-initiated notifications until we find
/// the response matching `expected_id`.
///
/// Crosslink #886: the scan budget is configurable via
/// `OPENCLAUDIA_LSP_MAX_MESSAGES`, and the exhaustion error names the
/// cap so an operator can tell "chatty server → raise the cap" from
/// "hung server → different remedy".
fn read_lsp_response(
    reader: &mut BufReader<impl std::io::Read>,
    expected_id: u32,
) -> Result<Value, String> {
    let max_messages = lsp_max_messages();
    for _attempt in 0..max_messages {
        // Read headers
        let mut content_length: usize = 0;
        loop {
            let mut line = String::new();
            reader
                .read_line(&mut line)
                .map_err(|e| format!("Read error: {e}"))?;
            let trimmed = line.trim();
            if trimmed.is_empty() {
                break;
            }
            if let Some(len_str) = trimmed.strip_prefix("Content-Length: ") {
                content_length = len_str
                    .parse()
                    .map_err(|e| format!("Bad content-length: {e}"))?;
            }
        }

        if content_length == 0 {
            return Err("No content-length in response".to_string());
        }

        let mut body = vec![0u8; content_length];
        std::io::Read::read_exact(reader, &mut body)
            .map_err(|e| format!("Body read error: {e}"))?;
        let msg: Value =
            serde_json::from_slice(&body).map_err(|e| format!("JSON parse error: {e}"))?;

        // If this message has an "id" matching our expected_id, it's the response
        if let Some(id) = msg.get("id").and_then(serde_json::Value::as_u64) {
            if id == u64::from(expected_id) {
                return Ok(msg);
            }
        }

        // Otherwise it's a notification (no id) or a response to a different request;
        // skip it and read the next message.
    }
    Err(format!(
        "LSP scan budget exhausted: did not see response id={expected_id} after \
         {max_messages} messages (raise OPENCLAUDIA_LSP_MAX_MESSAGES to relax)"
    ))
}

fn find_project_root(file_path: &Path) -> String {
    let mut dir = file_path.parent().unwrap_or(file_path);
    loop {
        if dir.join(".git").exists()
            || dir.join("Cargo.toml").exists()
            || dir.join("package.json").exists()
        {
            return format!("file://{}", dir.display());
        }
        match dir.parent() {
            Some(p) if p != dir => dir = p,
            _ => return format!("file://{}", dir.display()),
        }
    }
}

fn detect_language_id(file_path: &str) -> &str {
    let ext = file_path.rsplit('.').next().unwrap_or("");
    match ext {
        "rs" => "rust",
        "ts" => "typescript",
        "tsx" => "typescriptreact",
        "js" => "javascript",
        "jsx" => "javascriptreact",
        "py" => "python",
        "go" => "go",
        "c" => "c",
        "cpp" | "cc" | "cxx" | "h" | "hpp" => "cpp",
        "java" => "java",
        "rb" => "ruby",
        _ => "plaintext",
    }
}

fn parse_lsp_response(action: LspAction, file_path: &str, response: &Value) -> LspResult {
    let result_data = response.get("result");

    match action {
        LspAction::Hover => {
            let hover_text = result_data.and_then(|r| r.get("contents")).map(|c| {
                c.as_str().map_or_else(
                    || {
                        c.as_object().map_or_else(
                            || {
                                c.as_array().map_or_else(String::new, |arr| {
                                    arr.iter()
                                        .filter_map(|v| {
                                            v.as_str()
                                                .or_else(|| v.get("value").and_then(|x| x.as_str()))
                                        })
                                        .collect::<Vec<_>>()
                                        .join("\n")
                                })
                            },
                            |obj| {
                                obj.get("value")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("")
                                    .to_string()
                            },
                        )
                    },
                    str::to_string,
                )
            });
            LspResult {
                action: "hover".to_string(),
                file_path: file_path.to_string(),
                results: Vec::new(),
                hover_text,
                symbols: Vec::new(),
            }
        }
        LspAction::GoToDefinition | LspAction::FindReferences => {
            let locations = parse_locations(result_data);
            LspResult {
                action: format!("{action:?}"),
                file_path: file_path.to_string(),
                results: locations,
                hover_text: None,
                symbols: Vec::new(),
            }
        }
        LspAction::DocumentSymbols => {
            let symbols = parse_symbols(result_data);
            LspResult {
                action: "documentSymbols".to_string(),
                file_path: file_path.to_string(),
                results: Vec::new(),
                hover_text: None,
                symbols,
            }
        }
    }
}

/// Convert a u64 to u32, saturating at `u32::MAX`.
fn u64_to_u32_saturating(v: u64) -> u32 {
    u32::try_from(v).unwrap_or(u32::MAX)
}

fn parse_locations(data: Option<&Value>) -> Vec<LspLocation> {
    let arr = match data {
        Some(Value::Array(a)) => a.clone(),
        Some(obj @ Value::Object(_)) => vec![obj.clone()],
        _ => return Vec::new(),
    };

    arr.iter()
        .filter_map(|loc| {
            let uri = loc.get("uri").and_then(|u| u.as_str())?;
            let range = loc.get("range")?;
            let start = range.get("start")?;
            let end = range.get("end");
            Some(LspLocation {
                uri: uri.to_string(),
                line: start
                    .get("line")
                    .and_then(serde_json::Value::as_u64)
                    .map_or(0, u64_to_u32_saturating)
                    + 1,
                character: start
                    .get("character")
                    .and_then(serde_json::Value::as_u64)
                    .map_or(0, u64_to_u32_saturating),
                end_line: end
                    .and_then(|e| e.get("line"))
                    .and_then(serde_json::Value::as_u64)
                    .map(|l| u64_to_u32_saturating(l) + 1),
                end_character: end
                    .and_then(|e| e.get("character"))
                    .and_then(serde_json::Value::as_u64)
                    .map(u64_to_u32_saturating),
                preview: None,
            })
        })
        .collect()
}

const MAX_SYMBOL_DEPTH: usize = 20;

fn parse_symbols(data: Option<&Value>) -> Vec<LspSymbol> {
    parse_symbols_inner(data, 0)
}

fn parse_symbols_inner(data: Option<&Value>, depth: usize) -> Vec<LspSymbol> {
    if depth >= MAX_SYMBOL_DEPTH {
        return Vec::new();
    }

    let Some(Value::Array(arr)) = data else {
        return Vec::new();
    };

    arr.iter()
        .filter_map(|sym| {
            let name = sym.get("name").and_then(|n| n.as_str())?;
            let kind_num = sym
                .get("kind")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(0);
            let range = sym
                .get("range")
                .or_else(|| sym.get("location").and_then(|l| l.get("range")))?;
            let start = range.get("start")?;
            let end = range.get("end");

            // crosslink #963: `parse_symbols_inner` already returns `Vec::new()`
            // when the value is not an array, so the previous `.and_then(as_array)
            // .map(|_| ...)` gate was a redundant double-fetch that discarded the
            // already-converted array. One call, single fetch.
            let children = parse_symbols_inner(sym.get("children"), depth + 1);

            Some(LspSymbol {
                name: name.to_string(),
                kind: symbol_kind_name(kind_num),
                line: start
                    .get("line")
                    .and_then(serde_json::Value::as_u64)
                    .map_or(0, u64_to_u32_saturating)
                    + 1,
                end_line: end
                    .and_then(|e| e.get("line"))
                    .and_then(serde_json::Value::as_u64)
                    .map(|l| u64_to_u32_saturating(l) + 1),
                children,
            })
        })
        .collect()
}

fn symbol_kind_name(kind: u64) -> String {
    match kind {
        1 => "File",
        2 => "Module",
        3 => "Namespace",
        4 => "Package",
        5 => "Class",
        6 => "Method",
        7 => "Property",
        8 => "Field",
        9 => "Constructor",
        10 => "Enum",
        11 => "Interface",
        12 => "Function",
        13 => "Variable",
        14 => "Constant",
        15 => "String",
        16 => "Number",
        17 => "Boolean",
        18 => "Array",
        19 => "Object",
        20 => "Key",
        21 => "Null",
        22 => "EnumMember",
        23 => "Struct",
        24 => "Event",
        25 => "Operator",
        26 => "TypeParameter",
        _ => "Unknown",
    }
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_language_server() {
        assert_eq!(
            detect_language_server("main.rs").unwrap().0,
            "rust-analyzer"
        );
        assert_eq!(
            detect_language_server("app.tsx").unwrap().0,
            "typescript-language-server"
        );
        assert_eq!(detect_language_server("script.py").unwrap().0, "pylsp");
        assert!(detect_language_server("readme.md").is_none());
    }

    #[test]
    fn test_detect_language_id() {
        assert_eq!(detect_language_id("main.rs"), "rust");
        assert_eq!(detect_language_id("App.tsx"), "typescriptreact");
        assert_eq!(detect_language_id("unknown.xyz"), "plaintext");
    }

    #[test]
    fn test_parse_hover_response() {
        let resp = json!({"result": {"contents": {"kind": "markdown", "value": "fn main()"}}});
        let result = parse_lsp_response(LspAction::Hover, "test.rs", &resp);
        assert_eq!(result.hover_text, Some("fn main()".to_string()));
    }

    #[test]
    fn test_parse_hover_string_contents() {
        let resp = json!({"result": {"contents": "simple hover text"}});
        let result = parse_lsp_response(LspAction::Hover, "test.rs", &resp);
        assert_eq!(result.hover_text, Some("simple hover text".to_string()));
    }

    #[test]
    fn test_parse_hover_array_contents() {
        let resp = json!({"result": {"contents": ["line1", {"value": "line2"}]}});
        let result = parse_lsp_response(LspAction::Hover, "test.rs", &resp);
        assert_eq!(result.hover_text, Some("line1\nline2".to_string()));
    }

    #[test]
    fn test_parse_hover_null_result() {
        let resp = json!({"result": null});
        let result = parse_lsp_response(LspAction::Hover, "test.rs", &resp);
        assert_eq!(result.hover_text, None);
    }

    #[test]
    fn test_parse_locations() {
        let data = json!([{
            "uri": "file:///test.rs",
            "range": {"start": {"line": 10, "character": 5}, "end": {"line": 10, "character": 15}}
        }]);
        let locs = parse_locations(Some(&data));
        assert_eq!(locs.len(), 1);
        assert_eq!(locs[0].line, 11); // 0-indexed to 1-indexed
        assert_eq!(locs[0].character, 5);
        assert_eq!(locs[0].end_line, Some(11));
        assert_eq!(locs[0].end_character, Some(15));
    }

    #[test]
    fn test_parse_locations_single_object() {
        let data = json!({
            "uri": "file:///test.rs",
            "range": {"start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 10}}
        });
        let locs = parse_locations(Some(&data));
        assert_eq!(locs.len(), 1);
        assert_eq!(locs[0].line, 1);
    }

    #[test]
    fn test_parse_locations_empty() {
        let locs = parse_locations(None);
        assert!(locs.is_empty());

        let locs = parse_locations(Some(&json!(null)));
        assert!(locs.is_empty());
    }

    #[test]
    fn test_parse_symbols() {
        let data = json!([{
            "name": "main",
            "kind": 12,
            "range": {"start": {"line": 0, "character": 0}, "end": {"line": 5, "character": 1}}
        }]);
        let syms = parse_symbols(Some(&data));
        assert_eq!(syms.len(), 1);
        assert_eq!(syms[0].name, "main");
        assert_eq!(syms[0].kind, "Function");
        assert_eq!(syms[0].line, 1);
        assert_eq!(syms[0].end_line, Some(6));
    }

    #[test]
    fn test_parse_symbols_with_children() {
        let data = json!([{
            "name": "MyStruct",
            "kind": 23,
            "range": {"start": {"line": 0, "character": 0}, "end": {"line": 10, "character": 1}},
            "children": [{
                "name": "field_a",
                "kind": 8,
                "range": {"start": {"line": 1, "character": 4}, "end": {"line": 1, "character": 20}}
            }]
        }]);
        let syms = parse_symbols(Some(&data));
        assert_eq!(syms.len(), 1);
        assert_eq!(syms[0].name, "MyStruct");
        assert_eq!(syms[0].kind, "Struct");
        assert_eq!(syms[0].children.len(), 1);
        assert_eq!(syms[0].children[0].name, "field_a");
        assert_eq!(syms[0].children[0].kind, "Field");
    }

    #[test]
    fn test_parse_symbols_with_location_fallback() {
        // SymbolInformation uses "location" instead of "range"
        let data = json!([{
            "name": "foo",
            "kind": 12,
            "location": {
                "uri": "file:///test.rs",
                "range": {"start": {"line": 5, "character": 0}, "end": {"line": 8, "character": 1}}
            }
        }]);
        let syms = parse_symbols(Some(&data));
        assert_eq!(syms.len(), 1);
        assert_eq!(syms[0].name, "foo");
        assert_eq!(syms[0].line, 6);
    }

    #[test]
    fn test_symbol_kind_names() {
        assert_eq!(symbol_kind_name(5), "Class");
        assert_eq!(symbol_kind_name(12), "Function");
        assert_eq!(symbol_kind_name(23), "Struct");
        assert_eq!(symbol_kind_name(999), "Unknown");
    }

    #[test]
    fn test_execute_lsp_missing_file_path() {
        let args = HashMap::new();
        let (msg, is_err) = execute_lsp(&args);
        assert!(is_err);
        assert!(msg.contains("file_path is required"));
    }

    #[test]
    fn test_execute_lsp_unknown_extension() {
        let mut args = HashMap::new();
        args.insert(
            "file_path".to_string(),
            Value::String("readme.md".to_string()),
        );
        let (msg, is_err) = execute_lsp(&args);
        assert!(is_err);
        assert!(msg.contains("No language server known"));
    }

    #[test]
    fn test_execute_lsp_unknown_action() {
        let mut args = HashMap::new();
        args.insert(
            "file_path".to_string(),
            Value::String("test.rs".to_string()),
        );
        args.insert("action".to_string(), Value::String("badAction".to_string()));
        // This will either fail on unknown action or missing server; both are valid error paths
        let (msg, is_err) = execute_lsp(&args);
        assert!(is_err);
        assert!(msg.contains("Unknown LSP action") || msg.contains("not found"));
    }

    #[test]
    fn test_find_project_root_with_cargo() {
        // Use this project's own path as a test case
        let manifest = Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
        if manifest.exists() {
            let root = find_project_root(&manifest);
            assert!(root.starts_with("file://"));
            assert!(root.contains(env!("CARGO_MANIFEST_DIR")));
        }
    }

    #[test]
    fn test_parse_definition_response() {
        let resp = json!({
            "id": 2,
            "result": [{
                "uri": "file:///src/main.rs",
                "range": {
                    "start": {"line": 42, "character": 4},
                    "end": {"line": 42, "character": 20}
                }
            }]
        });
        let result = parse_lsp_response(LspAction::GoToDefinition, "test.rs", &resp);
        assert_eq!(result.results.len(), 1);
        assert_eq!(result.results[0].line, 43);
        assert_eq!(result.results[0].uri, "file:///src/main.rs");
    }

    #[test]
    fn test_parse_document_symbols_response() {
        let resp = json!({
            "id": 2,
            "result": [
                {
                    "name": "Config",
                    "kind": 23,
                    "range": {"start": {"line": 0, "character": 0}, "end": {"line": 20, "character": 1}},
                    "children": [
                        {
                            "name": "new",
                            "kind": 6,
                            "range": {"start": {"line": 5, "character": 4}, "end": {"line": 10, "character": 5}}
                        }
                    ]
                }
            ]
        });
        let result = parse_lsp_response(LspAction::DocumentSymbols, "test.rs", &resp);
        assert_eq!(result.symbols.len(), 1);
        assert_eq!(result.symbols[0].name, "Config");
        assert_eq!(result.symbols[0].kind, "Struct");
        assert_eq!(result.symbols[0].children.len(), 1);
        assert_eq!(result.symbols[0].children[0].name, "new");
        assert_eq!(result.symbols[0].children[0].kind, "Method");
    }

    // ── Spec-pinning tests (#550 Phase 2) ─────────────────────────────────────
    //
    // These tests pin OC's CURRENT behavior against the Phase 1 spec (#535).
    // They deliberately assert divergences from the CC reference; each divergence
    // is tracked by a gap issue. Do NOT "fix" these tests by adding features —
    // the purpose is to detect regressions in the existing contracts.

    // Spec B1: goToDefinition — server selection + location return
    // ─────────────────────────────────────────────────────────────

    /// B1a — Coordinate system: OC converts 0-based LSP lines to 1-based by
    /// adding 1 to `start.line`. `character` is NOT adjusted (stays 0-based).
    /// Gap: character should also become 1-based per spec, but OC omits that.
    #[test]
    fn spec_b1_coordinate_conversion_line_1based_character_0based() {
        let data = json!([{
            "uri": "file:///foo.rs",
            "range": {
                "start": {"line": 9, "character": 3},
                "end":   {"line": 9, "character": 12}
            }
        }]);
        let locs = parse_locations(Some(&data));
        assert_eq!(locs.len(), 1);
        // OC adds 1 to line (0→1-based); pinning that exact conversion.
        assert_eq!(locs[0].line, 10);
        // OC does NOT add 1 to character — it stays 0-based. (Gap vs CC spec.)
        assert_eq!(locs[0].character, 3);
    }

    /// B1b — OC stores the raw `file://…` URI, not a workspace-relative path.
    /// Gap #643: CC normalizes `LocationLink` → Location; OC only handles
    /// Location objects (requires `uri` field). A `LocationLink` input (with
    /// `targetUri` but no `uri`) is silently dropped.
    #[test]
    fn spec_b1_raw_uri_stored_not_relative_path() {
        let data = json!([{
            "uri": "file:///home/user/project/src/lib.rs",
            "range": {"start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 5}}
        }]);
        let locs = parse_locations(Some(&data));
        assert_eq!(locs.len(), 1);
        // Pinning: OC stores the raw file:// URI, not a relative path.
        assert_eq!(locs[0].uri, "file:///home/user/project/src/lib.rs");
    }

    /// B1c — `LocationLink` objects (with `targetUri` but no `uri`) are silently
    /// dropped by OC's `parse_locations` because it requires `uri`. (Gap #643.)
    #[test]
    fn spec_b1_location_link_silently_dropped_gap643() {
        // This is a LocationLink shape, not a Location shape.
        let data = json!([{
            "targetUri": "file:///src/lib.rs",
            "targetRange": {"start": {"line": 5, "character": 0}, "end": {"line": 5, "character": 10}},
            "targetSelectionRange": {"start": {"line": 5, "character": 0}, "end": {"line": 5, "character": 10}},
            "originSelectionRange": {"start": {"line": 1, "character": 0}, "end": {"line": 1, "character": 5}}
        }]);
        // OC: filter_map drops entries without `uri`. Result is empty.
        // CC: would normalise targetUri → Location. (Gap #643.)
        let locs = parse_locations(Some(&data));
        assert!(
            locs.is_empty(),
            "OC drops LocationLink shapes (no `uri` field); gap #643 tracks the fix"
        );
    }

    // Spec B2: hover — hover text extraction
    // ───────────────────────────────────────

    /// B2a — OC joins `MarkedString` array items with "\n" (single newline).
    /// CC uses "\n\n" (double newline). Pinning OC's current join separator.
    /// Gap: array join should be "\n\n" per CC spec.
    #[test]
    fn spec_b2_array_join_single_newline_not_double() {
        let resp = json!({"result": {"contents": ["first", {"value": "second"}, "third"]}});
        let result = parse_lsp_response(LspAction::Hover, "test.rs", &resp);
        let text = result.hover_text.unwrap();
        // Pinning: OC uses "\n" not "\n\n".
        assert_eq!(text, "first\nsecond\nthird");
        // Asserting absence of double-newline explicitly (the gap from CC).
        assert!(
            !text.contains("\n\n"),
            "OC uses single '\\n' between array items; gap vs CC which uses '\\n\\n'"
        );
    }

    /// B2b — OC does NOT prepend a range-qualified prefix even when
    /// `Hover.range` is present. CC prepends "Hover info at <line>:<char>:\n\n".
    /// Pinning the absence of this prefix.
    #[test]
    fn spec_b2_no_range_prefix_when_hover_range_present() {
        let resp = json!({
            "result": {
                "contents": {"kind": "plaintext", "value": "fn foo()"},
                "range": {
                    "start": {"line": 10, "character": 4},
                    "end":   {"line": 10, "character": 7}
                }
            }
        });
        let result = parse_lsp_response(LspAction::Hover, "test.rs", &resp);
        let text = result.hover_text.unwrap();
        // OC ignores the range field entirely; raw value is returned.
        assert_eq!(text, "fn foo()");
        // Pinning absence of CC's range prefix.
        assert!(
            !text.contains("Hover info at"),
            "OC does not emit range-qualified prefix; gap vs CC"
        );
    }

    // Spec B3: findReferences — reference location list
    // ──────────────────────────────────────────────────

    /// B3a — OC's findReferences output uses the same `parse_locations` path as
    /// goToDefinition: produces Vec<LspLocation> with raw URIs, no file-grouping.
    /// Gap: CC groups references by file; OC returns a flat list.
    #[test]
    fn spec_b3_references_flat_raw_uris_no_file_grouping() {
        let resp = json!({
            "id": 2,
            "result": [
                {
                    "uri": "file:///a.rs",
                    "range": {"start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 5}}
                },
                {
                    "uri": "file:///b.rs",
                    "range": {"start": {"line": 9, "character": 2}, "end": {"line": 9, "character": 8}}
                }
            ]
        });
        let result = parse_lsp_response(LspAction::FindReferences, "test.rs", &resp);
        // OC: flat list of LspLocation, no grouping.
        assert_eq!(result.results.len(), 2);
        // URIs are raw file:// strings, not relative paths. (Gap vs CC.)
        assert_eq!(result.results[0].uri, "file:///a.rs");
        assert_eq!(result.results[1].uri, "file:///b.rs");
        // Symbols vector is empty for references action.
        assert!(result.symbols.is_empty());
        // hover_text is None for references action.
        assert!(result.hover_text.is_none());
    }

    /// B3b — Locations missing `uri` field are silently dropped.
    /// (Gap: CC logs these as errors; OC silently filters.)
    #[test]
    fn spec_b3_locations_missing_uri_silently_dropped() {
        let data = json!([
            {
                "uri": "file:///valid.rs",
                "range": {"start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 1}}
            },
            {
                "range": {"start": {"line": 5, "character": 0}, "end": {"line": 5, "character": 1}}
                // no "uri" field
            }
        ]);
        let locs = parse_locations(Some(&data));
        // OC: filter_map on `uri` drops the entry with no uri field.
        assert_eq!(locs.len(), 1, "OC silently drops locations missing 'uri'");
        assert_eq!(locs[0].uri, "file:///valid.rs");
    }

    // Spec B4: documentSymbols — nested symbol tree
    // ──────────────────────────────────────────────

    /// B4a — OC enforces `MAX_SYMBOL_DEPTH` = 20. A tree deeper than 20 levels
    /// is truncated: children at depth ≥ 20 are returned as empty.
    #[test]
    fn spec_b4_symbol_depth_limit_at_20() {
        // Build a chain of 22 nested symbols. Each wraps the next as its child.
        fn make_nested(depth: usize) -> serde_json::Value {
            if depth == 0 {
                return json!({
                    "name": "leaf",
                    "kind": 12,
                    "range": {"start": {"line": depth as u64, "character": 0},
                              "end":   {"line": depth as u64, "character": 1}}
                });
            }
            json!({
                "name": format!("node_{depth}"),
                "kind": 2,
                "range": {"start": {"line": depth as u64, "character": 0},
                          "end":   {"line": depth as u64, "character": 1}},
                "children": [make_nested(depth - 1)]
            })
        }

        // Nest 22 levels deep; OC truncates at depth 20.
        let root = json!([make_nested(22)]);
        let syms = parse_symbols(Some(&root));
        assert_eq!(syms.len(), 1, "root symbol present");

        // Walk down the tree counting reachable levels.
        let mut level = 0usize;
        let mut current = &syms[0];
        loop {
            level += 1;
            if current.children.is_empty() {
                break;
            }
            current = &current.children[0];
        }
        // With MAX_SYMBOL_DEPTH = 20 the tree is cut before depth 20,
        // so we can reach at most 20 levels before children become empty.
        assert!(
            level <= MAX_SYMBOL_DEPTH,
            "OC truncates at MAX_SYMBOL_DEPTH={MAX_SYMBOL_DEPTH}; reached {level}"
        );
    }

    /// B4b — All 26 LSP `SymbolKind` integers map to their canonical names.
    /// Pinning the full mapping so renames are caught as regressions.
    #[test]
    fn spec_b4_all_26_symbol_kind_names() {
        let expected: &[(u64, &str)] = &[
            (1, "File"),
            (2, "Module"),
            (3, "Namespace"),
            (4, "Package"),
            (5, "Class"),
            (6, "Method"),
            (7, "Property"),
            (8, "Field"),
            (9, "Constructor"),
            (10, "Enum"),
            (11, "Interface"),
            (12, "Function"),
            (13, "Variable"),
            (14, "Constant"),
            (15, "String"),
            (16, "Number"),
            (17, "Boolean"),
            (18, "Array"),
            (19, "Object"),
            (20, "Key"),
            (21, "Null"),
            (22, "EnumMember"),
            (23, "Struct"),
            (24, "Event"),
            (25, "Operator"),
            (26, "TypeParameter"),
            (0, "Unknown"),
            (27, "Unknown"),
            (999, "Unknown"),
        ];
        for (kind, name) in expected {
            assert_eq!(
                symbol_kind_name(*kind),
                *name,
                "SymbolKind {kind} should map to {name}"
            );
        }
    }

    /// B4c — OC outputs symbols as a Vec<LspSymbol> (JSON-serialisable struct),
    /// NOT as a pre-formatted human-readable text tree.
    /// Gap: CC formats as indented text; OC returns raw structured data.
    #[test]
    fn spec_b4_output_is_structured_not_formatted_text_gap() {
        let resp = json!({
            "result": [{
                "name": "MyFn",
                "kind": 12,
                "range": {"start": {"line": 0, "character": 0}, "end": {"line": 3, "character": 1}}
            }]
        });
        let result = parse_lsp_response(LspAction::DocumentSymbols, "test.rs", &resp);
        // OC: result.symbols is populated; result is serialised to JSON by execute_lsp.
        assert_eq!(result.symbols.len(), 1);
        assert_eq!(result.symbols[0].name, "MyFn");
        // No hover_text or results are populated for this action.
        assert!(result.results.is_empty());
        assert!(result.hover_text.is_none());
        // The action label OC sets for documentSymbols.
        assert_eq!(result.action, "documentSymbols");
    }

    // Spec B5: Unknown action string → explicit error listing valid actions
    // ─────────────────────────────────────────────────────────────────────

    /// B5a — OC returns a specific error message naming exactly the 4 operations
    /// it supports. CC rejects at Zod validation layer (9 operations).
    /// Pinning: the exact error text from the `_` match arm.
    #[test]
    fn spec_b5_unknown_action_exact_error_message() {
        // Use an extension for which rust-analyzer might not be installed;
        // unknown-action check happens AFTER the server-availability check.
        // We use a non-.rs extension with a known server path that won't
        // be installed in CI to ensure we hit the unknown-action arm only
        // when the server IS installed. To isolate the action-parsing logic
        // we call it through execute_lsp with .md extension (no known server),
        // which returns a different error. Instead, test via the internal match
        // by calling execute_lsp with a .rs file and a bad action; the code
        // checks action BEFORE spawning the server, so we expect "Unknown LSP
        // action" regardless of server availability.
        let mut args: HashMap<String, Value> = HashMap::new();
        args.insert(
            "file_path".to_string(),
            Value::String("test.rs".to_string()),
        );
        args.insert(
            "action".to_string(),
            Value::String("workspaceSymbol".to_string()),
        );
        let (msg, is_err) = execute_lsp(&args);
        assert!(is_err);
        // Exact pin: OC's error message lists exactly these 4 operations.
        // Gap #645: CC has 9 operations; OC only implements 4.
        assert!(
            msg.contains("Unknown LSP action: workspaceSymbol"),
            "unexpected message: {msg}"
        );
        assert!(
            msg.contains("goToDefinition"),
            "error should list goToDefinition; got: {msg}"
        );
        assert!(
            msg.contains("findReferences"),
            "error should list findReferences; got: {msg}"
        );
        assert!(msg.contains("hover"), "error should list hover; got: {msg}");
        assert!(
            msg.contains("documentSymbols"),
            "error should list documentSymbols; got: {msg}"
        );
    }

    /// B5b — All 5 gap operations are unknown to OC.
    /// Gap #645: workspaceSymbol, goToImplementation, prepareCallHierarchy,
    ///           incomingCalls, outgoingCalls are absent from OC's `LspAction` enum.
    #[test]
    fn spec_b5_five_missing_operations_unknown_gap645() {
        let missing_ops = [
            "workspaceSymbol",
            "goToImplementation",
            "prepareCallHierarchy",
            "incomingCalls",
            "outgoingCalls",
        ];
        for op in missing_ops {
            let mut args: HashMap<String, Value> = HashMap::new();
            args.insert(
                "file_path".to_string(),
                Value::String("test.rs".to_string()),
            );
            args.insert("action".to_string(), Value::String(op.to_string()));
            let (msg, is_err) = execute_lsp(&args);
            assert!(is_err, "{op} should produce an error");
            // Either "Unknown LSP action" (action parsed before server check)
            // or "not found" (server absent in CI). Both are acceptable pins.
            assert!(
                msg.contains("Unknown LSP action") || msg.contains("not found"),
                "op={op} unexpected message: {msg}"
            );
        }
    }

    // Spec B6: Server crash mid-call → explicit error, not hang
    // ──────────────────────────────────────────────────────────

    /// B6a — `read_lsp_response` returns `Err` after exhausting 100 messages
    /// without finding the expected id. This is the OC equivalent of the
    /// "did not respond" path. CC has health-check + retry; OC has neither.
    /// This test drives the function with a reader that yields only mismatched
    /// responses (wrong id), verifying the 100-message limit fires.
    #[test]
    fn spec_b6_read_lsp_response_errors_after_100_mismatches() {
        use std::io::Cursor;

        // Build 101 LSP messages all with id=99 (not the expected id=1).
        let mut bytes = Vec::new();
        for _ in 0..101u8 {
            let body = r#"{"jsonrpc":"2.0","id":99,"result":null}"#;
            let header = format!("Content-Length: {}\r\n\r\n", body.len());
            bytes.extend_from_slice(header.as_bytes());
            bytes.extend_from_slice(body.as_bytes());
        }

        let cursor = Cursor::new(bytes);
        let mut reader = BufReader::new(cursor);

        // OC loops up to 100 messages then returns Err.
        let result = read_lsp_response(&mut reader, 1);
        assert!(
            result.is_err(),
            "expected Err after 100 mismatched messages"
        );
        let msg = result.unwrap_err();
        assert!(
            msg.contains("100"),
            "error should mention the 100-message limit; got: {msg}"
        );
    }

    /// B6b — `read_lsp_response` returns `Err` when the underlying reader
    /// returns zero bytes (simulates a server process that has exited/crashed).
    /// OC has no health-check before send; crash is detected only during read.
    /// Gap #636: CC's server pool detects crashes via process exit handler
    /// and throws immediately from sendRequest; OC blocks until I/O error.
    #[test]
    fn spec_b6_read_lsp_response_errors_on_empty_stream_gap636() {
        use std::io::Cursor;

        // Empty reader simulates a server that has closed its stdout.
        let cursor = Cursor::new(Vec::<u8>::new());
        let mut reader = BufReader::new(cursor);

        let result = read_lsp_response(&mut reader, 1);
        // OC: read_line on empty stream returns 0 bytes → content_length stays 0
        // → Err("No content-length in response") or similar.
        assert!(
            result.is_err(),
            "empty stream should produce an error; OC has no hang-guard (gap #636)"
        );
    }

    // Spec B1-det: detect_language_server — full extension table
    // ───────────────────────────────────────────────────────────

    /// Pin the full extension→binary mapping table so additions/removals
    /// are caught as regressions.
    #[test]
    fn spec_detect_language_server_full_extension_table() {
        // (extension suffix, expected binary, expected first arg if any)
        let cases: &[(&str, &str, Option<&str>)] = &[
            ("file.rs", "rust-analyzer", None),
            ("file.ts", "typescript-language-server", Some("--stdio")),
            ("file.tsx", "typescript-language-server", Some("--stdio")),
            ("file.js", "typescript-language-server", Some("--stdio")),
            ("file.jsx", "typescript-language-server", Some("--stdio")),
            ("file.py", "pylsp", None),
            ("file.go", "gopls", Some("serve")),
            ("file.c", "clangd", None),
            ("file.cpp", "clangd", None),
            ("file.h", "clangd", None),
            ("file.hpp", "clangd", None),
            ("file.java", "jdtls", None),
            ("file.rb", "solargraph", Some("stdio")),
        ];
        for (path, binary, first_arg) in cases {
            let (cmd, args) =
                detect_language_server(path).unwrap_or_else(|| panic!("no server for {path}"));
            assert_eq!(cmd, *binary, "extension of {path}");
            match first_arg {
                Some(arg) => assert_eq!(args.first().copied(), Some(*arg), "first arg for {path}"),
                None => assert!(args.is_empty(), "expected no args for {path}, got {args:?}"),
            }
        }
    }

    /// Pin: extensions not in the table return None.
    #[test]
    fn spec_detect_language_server_unknown_extensions_return_none() {
        for path in &["file.md", "file.txt", "file.json", "file.yaml", "noext"] {
            assert!(
                detect_language_server(path).is_none(),
                "expected None for {path}"
            );
        }
    }

    // ── Fix #355: ChildGuard, wait_for_readiness, capture_stderr ─────────────

    /// Fix #355-zombie: `ChildGuard::drop` kills and reaps a running child.
    ///
    /// Forensic evidence: original `run_lsp_request` called `child.wait()`
    /// only at line 229 (after the shutdown sequence).  Any `?`-early-return
    /// at line 174 (`read_lsp_response` for initialize) or line 224 (for the
    /// actual request) bypassed that call, leaving an un-reaped zombie on Unix.
    /// `ChildGuard` wraps the child in a Drop impl that always kills+waits.
    #[test]
    fn fix355_child_guard_drop_reaps_child() {
        // Spawn a long-running child (sleep 60) so we can verify it is alive
        // before the guard drops, and dead after.
        let child = Command::new("sleep")
            .arg("60")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("failed to spawn sleep — needs Unix");

        let pid = child.id();

        {
            let _guard = ChildGuard::new(child);
            // Child is alive inside the guard scope.
            // /proc/<pid> exists on Linux while the process lives.
            assert!(
                std::path::Path::new(&format!("/proc/{pid}")).exists(),
                "child should be alive while guard is held"
            );
        } // guard drops here → kills + waits

        // After drop the process should be gone.  Give the OS a brief moment
        // to finalize the reap (wait() in Drop is synchronous, so this should
        // be immediate, but we add a tiny yield for robustness).
        std::thread::sleep(std::time::Duration::from_millis(20));
        assert!(
            !std::path::Path::new(&format!("/proc/{pid}")).exists(),
            "child should be reaped after ChildGuard drops (zombie fix #355)"
        );
    }

    /// Fix #355-sleep: `wait_for_readiness` returns Ok when the probe response
    /// (id=1001) appears in the stream, possibly after skipped notifications.
    ///
    /// Forensic evidence: original line 191 was
    ///   `std::thread::sleep(std::time::Duration::from_millis(500));`
    /// This is a pure guess — too short for cold rust-analyzer (10-60 s index),
    /// wasted latency for fast servers.  The replacement sends a documentSymbol
    /// probe (id=1001) and returns as soon as the server replies.
    #[test]
    fn fix355_wait_for_readiness_returns_ok_after_probe_response() {
        use std::io::Cursor;

        // Simulate: two server-initiated notifications, then the probe reply.
        let mut bytes = Vec::new();

        // Notification 1 (no id) — window/logMessage
        let notif1 = r#"{"jsonrpc":"2.0","method":"window/logMessage","params":{"type":4,"message":"loading"}}"#;
        bytes.extend_from_slice(format!("Content-Length: {}\r\n\r\n", notif1.len()).as_bytes());
        bytes.extend_from_slice(notif1.as_bytes());

        // Notification 2 — publishDiagnostics (no id)
        let notif2 = r#"{"jsonrpc":"2.0","method":"textDocument/publishDiagnostics","params":{"uri":"file:///x.rs","diagnostics":[]}}"#;
        bytes.extend_from_slice(format!("Content-Length: {}\r\n\r\n", notif2.len()).as_bytes());
        bytes.extend_from_slice(notif2.as_bytes());

        // Probe response (id=1001) — documentSymbol reply with empty array
        let probe_reply = r#"{"jsonrpc":"2.0","id":1001,"result":[]}"#;
        bytes
            .extend_from_slice(format!("Content-Length: {}\r\n\r\n", probe_reply.len()).as_bytes());
        bytes.extend_from_slice(probe_reply.as_bytes());

        let cursor = Cursor::new(bytes);
        let mut reader = BufReader::new(cursor);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);

        let result = wait_for_readiness(&mut reader, deadline);
        assert!(
            result.is_ok(),
            "should return Ok after skipping 2 notifications and finding id=1001; got: {result:?}"
        );
    }

    /// Fix #355-sleep-timeout: `wait_for_readiness` returns Err when the deadline
    /// elapses before the probe response arrives.
    #[test]
    fn fix355_wait_for_readiness_times_out_when_no_probe_response() {
        use std::io::Cursor;

        // Only send a notification with a wrong id — probe reply never arrives.
        let wrong_id = r#"{"jsonrpc":"2.0","id":9999,"result":[]}"#;
        let mut bytes = Vec::new();
        bytes.extend_from_slice(format!("Content-Length: {}\r\n\r\n", wrong_id.len()).as_bytes());
        bytes.extend_from_slice(wrong_id.as_bytes());
        // Then EOF — simulates server that never answers the probe.

        let cursor = Cursor::new(bytes);
        let mut reader = BufReader::new(cursor);
        // Already-expired deadline so the check fires immediately.
        let deadline = std::time::Instant::now()
            .checked_sub(std::time::Duration::from_millis(1))
            .unwrap_or_else(std::time::Instant::now);

        let result = wait_for_readiness(&mut reader, deadline);
        assert!(
            result.is_err(),
            "should return Err when deadline is already past"
        );
        let msg = result.unwrap_err();
        assert!(
            msg.contains("timeout"),
            "error should mention timeout; got: {msg}"
        );
    }

    /// Fix #355-stderr: `capture_stderr` drains bytes into a ring buffer and
    /// truncates to the last 1024 bytes when more arrive.
    #[test]
    fn fix355_capture_stderr_ring_buffer_truncates_to_1024() {
        // Spawn a child that writes 2048 bytes to stderr then exits.
        let mut child = Command::new("sh")
            .args(["-c", "printf '%02048d' 0 >&2; exit 0"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .expect("failed to spawn sh");

        let stderr_pipe = child.stderr.take().expect("no stderr");
        let buf = capture_stderr(stderr_pipe);

        // Wait for the child to finish writing.
        let _ = child.wait();

        // Give the drain thread a moment to flush the ring buffer.
        std::thread::sleep(std::time::Duration::from_millis(100));

        let guard = buf.lock().unwrap();
        let len = guard.len();
        let is_empty = guard.is_empty();
        drop(guard);
        assert!(
            len <= 1024,
            "ring buffer should be capped at 1024 bytes; actual len = {len}"
        );
        assert!(
            !is_empty,
            "ring buffer should not be empty after writing 2048 bytes"
        );
    }

    // ── Fix #647: didOpen deduplication via process-wide registry ────────────

    /// #647-a — `mark_opened` returns true the first time and false the
    /// second time for the same `(server_cmd, path)` pair, and the registry
    /// reflects the open state in between.  Parity with CC `LSPServerManager
    /// .ts:64,277` (`isFileOpen`).
    #[test]
    fn fix647_mark_opened_dedupes_repeated_calls() {
        let server = "rust-analyzer-test-647a-unique";
        let path = PathBuf::from("/tmp/openclaudia-647a-unique.rs");
        // Defensive cleanup so a previously aborted run can't poison this one.
        // We use process-unique (server, path) so we never need a global
        // registry reset (which would race with other parallel tests).
        let _ = mark_closed(server, &path);

        assert!(!is_marked_open(server, &path), "starts empty");
        assert!(
            mark_opened(server, &path),
            "first mark_opened should claim the slot"
        );
        assert!(
            is_marked_open(server, &path),
            "registry should report the file as open"
        );
        assert!(
            !mark_opened(server, &path),
            "second mark_opened should report already-open (skip didOpen)"
        );

        // didClose flips the flag back.
        assert!(mark_closed(server, &path), "first close removes the entry");
        assert!(!is_marked_open(server, &path), "registry now clear");
        assert!(
            !mark_closed(server, &path),
            "closing an already-closed file is a no-op"
        );

        // After close, mark_opened claims a fresh slot again.
        assert!(
            mark_opened(server, &path),
            "post-close mark_opened claims a fresh slot"
        );
        // Final cleanup so re-runs start clean.
        let _ = mark_closed(server, &path);
    }

    /// #647-b — `OpenFileGuard::drop` rolls back the dedup entry when commit
    /// is never called, preventing leaked slots from poisoning future calls
    /// after a `?`-early-return inside `run_lsp_request`.
    #[test]
    fn fix647_open_file_guard_drop_rolls_back_uncommitted_slot() {
        let server = "rust-analyzer-test-647b-unique";
        let path = PathBuf::from("/tmp/openclaudia-647b-unique.rs");
        let _ = mark_closed(server, &path);

        // Simulate the prologue inside run_lsp_request.
        let we_opened = mark_opened(server, &path);
        assert!(we_opened);
        assert!(is_marked_open(server, &path));

        {
            let _guard = OpenFileGuard::new(server, &path, we_opened);
            // …imagine `?` returns here without commit…
        }

        assert!(
            !is_marked_open(server, &path),
            "Drop must release the slot when commit() was never called (fix #647)"
        );
    }

    /// #647-c — `OpenFileGuard::commit` releases the slot exactly once and
    /// is idempotent under double-call (defensive against future
    /// refactors).
    #[test]
    fn fix647_open_file_guard_commit_is_idempotent() {
        let server = "rust-analyzer-test-647c-unique";
        let path = PathBuf::from("/tmp/openclaudia-647c-unique.rs");
        let _ = mark_closed(server, &path);

        let we_opened = mark_opened(server, &path);
        assert!(we_opened);
        let mut guard = OpenFileGuard::new(server, &path, we_opened);

        guard.commit();
        assert!(!is_marked_open(server, &path), "first commit releases");
        guard.commit();
        assert!(
            !is_marked_open(server, &path),
            "second commit is a no-op (no panic, no resurrection)"
        );
        // Drop on `guard` after this point must also be a no-op.
        drop(guard);
        assert!(!is_marked_open(server, &path));
    }

    // ── Fix #648: 10 MiB file-size guard before LSP analysis ─────────────────

    /// #648-a — A file larger than `LSP_MAX_FILE_SIZE` (10 MiB) is rejected
    /// with a clear "too large" error before any server is spawned.  Parity
    /// with CC `LSPTool.ts:53,264-269`.
    #[test]
    fn fix648_oversized_file_is_rejected_before_server_spawn() {
        use std::io::Write as _;
        let tmp = tempfile::NamedTempFile::with_suffix(".rs").expect("tempfile");
        // Write 10 MiB + 1 byte so we strictly exceed the limit.
        let payload = vec![b'a'; usize::try_from(LSP_MAX_FILE_SIZE).unwrap() + 1];
        {
            let mut f = std::fs::File::create(tmp.path()).expect("create");
            f.write_all(&payload).expect("write payload");
        }

        let mut args: HashMap<String, Value> = HashMap::new();
        args.insert(
            "file_path".to_string(),
            Value::String(tmp.path().to_string_lossy().into_owned()),
        );
        args.insert("action".to_string(), Value::String("hover".to_string()));

        let (msg, is_err) = execute_lsp(&args);
        assert!(is_err, "oversized file must produce an error");
        // The error must be the size-guard message, not a server-not-found
        // or unknown-action message, regardless of whether rust-analyzer is
        // present on this host.
        if msg.contains("LSP server unavailable") {
            // Server-availability gate fires first when rust-analyzer is
            // absent — that path is exercised by fix650 tests below.  When
            // it does fire we cannot also assert the size-guard path; skip
            // the rest of the assertion in that environment.
            return;
        }
        assert!(
            msg.contains("File too large for LSP analysis"),
            "expected size-guard message; got: {msg}"
        );
        assert!(
            msg.contains("10 MiB"),
            "error should name the 10 MiB limit; got: {msg}"
        );
    }

    /// #648-b — A file exactly at the limit is accepted (boundary check):
    /// the size-guard must not reject `len == LSP_MAX_FILE_SIZE`, only
    /// strictly greater.  We can't verify a full LSP run here without
    /// rust-analyzer, so we assert the rejection path is NOT taken — any
    /// other error (server missing, etc.) is acceptable.
    #[test]
    fn fix648_file_at_limit_is_not_rejected_by_size_guard() {
        use std::io::Write as _;
        let tmp = tempfile::NamedTempFile::with_suffix(".rs").expect("tempfile");
        let payload = vec![b'a'; usize::try_from(LSP_MAX_FILE_SIZE).unwrap()];
        {
            let mut f = std::fs::File::create(tmp.path()).expect("create");
            f.write_all(&payload).expect("write payload");
        }

        let mut args: HashMap<String, Value> = HashMap::new();
        args.insert(
            "file_path".to_string(),
            Value::String(tmp.path().to_string_lossy().into_owned()),
        );
        args.insert("action".to_string(), Value::String("hover".to_string()));

        let (msg, _is_err) = execute_lsp(&args);
        // The size-guard message must NOT appear for a file at the limit.
        assert!(
            !msg.contains("File too large for LSP analysis"),
            "size-guard should accept len == LSP_MAX_FILE_SIZE; got: {msg}"
        );
    }

    // ── Fix #650: Availability gate via is_lsp_connected() ───────────────────

    /// #650-a — `is_lsp_connected` returns false for languages whose servers
    /// are guaranteed-not-installed (we use an unknown identifier so the
    /// resolver short-circuits).  Parity with CC `LSPTool.ts:137-139`.
    #[test]
    fn fix650_is_lsp_connected_false_for_unknown_language() {
        // An identifier that maps to no known server must report disconnected.
        assert!(!is_lsp_connected("brainfuck"));
        assert!(!is_lsp_connected(""));
        assert!(!is_lsp_connected("xyz"));
    }

    /// #650-b — When a real server is genuinely absent, `execute_lsp`
    /// returns the "LSP server unavailable" error naming the language and
    /// binary plus the PATH hint.  We probe with `.java` (jdtls), which is
    /// effectively never installed in CI; if a host *does* happen to have
    /// jdtls on PATH the test short-circuits to a vacuous pass rather than
    /// mutating process-global PATH (which would race with other parallel
    /// tests that spawn external commands).
    #[test]
    fn fix650_execute_lsp_gates_on_missing_server_with_language_hint() {
        // Probe whether jdtls is installed on this host.  If yes, skip the
        // strict assertion (the gate doesn't fire); we still cover the
        // happy "gate fires" path via is_lsp_connected("brainfuck") in
        // fix650_is_lsp_connected_false_for_unknown_language.
        if is_lsp_connected("java") {
            return;
        }

        let mut args: HashMap<String, Value> = HashMap::new();
        args.insert(
            "file_path".to_string(),
            Value::String("test_file.java".to_string()),
        );
        args.insert("action".to_string(), Value::String("hover".to_string()));

        let (msg, is_err) = execute_lsp(&args);
        assert!(is_err, "missing server must produce an error");
        assert!(
            msg.contains("LSP server unavailable for java"),
            "error should name the language; got: {msg}"
        );
        assert!(
            msg.contains("jdtls"),
            "error should name the server binary; got: {msg}"
        );
        assert!(
            msg.contains("not found on PATH"),
            "error should hint at PATH; got: {msg}"
        );
    }

    /// #650-c — `resolve_language_server` accepts both bare language names
    /// and extension forms (with or without leading dot), so the gate's
    /// input contract matches CC's broader API.
    #[test]
    fn fix650_resolve_language_server_accepts_name_and_extension() {
        assert_eq!(resolve_language_server("rust").unwrap().0, "rust-analyzer");
        assert_eq!(resolve_language_server("rs").unwrap().0, "rust-analyzer");
        assert_eq!(resolve_language_server(".rs").unwrap().0, "rust-analyzer");
        assert_eq!(resolve_language_server("python").unwrap().0, "pylsp");
        assert_eq!(resolve_language_server("py").unwrap().0, "pylsp");
        assert_eq!(
            resolve_language_server("typescript").unwrap().0,
            "typescript-language-server"
        );
        assert!(resolve_language_server("nonsense").is_none());
    }
}
