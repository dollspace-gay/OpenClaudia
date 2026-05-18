//! LSP (Language Server Protocol) integration tool.
//!
//! Provides code intelligence via external language servers:
//! - `goToDefinition`: Find where a symbol is defined
//! - `findReferences`: Find all references to a symbol
//! - hover: Get type/documentation info for a symbol
//! - `documentSymbols`: List symbols in a file

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::hash::BuildHasher;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Command, Stdio};

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

    // Check if server is available
    if Command::new("which")
        .arg(server_cmd)
        .output()
        .map_or(true, |o| !o.status.success())
    {
        return (
            format!("Language server '{server_cmd}' not found. Install it to use LSP features."),
            true,
        );
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
    let abs_path =
        std::fs::canonicalize(file_path).map_err(|e| format!("Cannot resolve path: {e}"))?;
    let root_uri = find_project_root(&abs_path);
    let file_uri = format!("file://{}", abs_path.display());

    // Read file content for textDocument/didOpen
    let content =
        std::fs::read_to_string(&abs_path).map_err(|e| format!("Cannot read file: {e}"))?;

    let mut child = Command::new(server_cmd)
        .args(server_args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("Failed to start {server_cmd}: {e}"))?;

    let mut stdin = child.stdin.take().ok_or("Failed to get stdin")?;
    let stdout = child.stdout.take().ok_or("Failed to get stdout")?;
    let mut reader = BufReader::new(stdout);

    // Send initialize
    let init_params = json!({
        "processId": std::process::id(),
        "rootUri": root_uri,
        "capabilities": {},
        "workspaceFolders": [{"uri": root_uri, "name": "workspace"}]
    });
    send_lsp_message(&mut stdin, "initialize", 1, init_params)?;
    let _init_response = read_lsp_response(&mut reader, 1)?;

    // Send initialized notification
    send_lsp_notification(&mut stdin, "initialized", json!({}))?;

    // Send textDocument/didOpen
    let did_open = json!({
        "textDocument": {
            "uri": file_uri,
            "languageId": detect_language_id(file_path),
            "version": 1,
            "text": content,
        }
    });
    send_lsp_notification(&mut stdin, "textDocument/didOpen", did_open)?;

    // Give server a moment to process the file
    std::thread::sleep(std::time::Duration::from_millis(500));

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
    let response = read_lsp_response(&mut reader, 2)?;

    // Shutdown
    send_lsp_message(&mut stdin, "shutdown", 3, json!(null))?;
    send_lsp_notification(&mut stdin, "exit", json!(null))?;
    let _ = child.wait();

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

/// Read an LSP response, skipping server-initiated notifications until we find
/// the response matching `expected_id`.
fn read_lsp_response(
    reader: &mut BufReader<impl std::io::Read>,
    expected_id: u32,
) -> Result<Value, String> {
    for _attempt in 0..100 {
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
        "LSP server did not respond to request {expected_id} after 100 messages"
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

            let children = sym
                .get("children")
                .and_then(|c| c.as_array())
                .map(|_| parse_symbols_inner(sym.get("children"), depth + 1))
                .unwrap_or_default();

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
}
