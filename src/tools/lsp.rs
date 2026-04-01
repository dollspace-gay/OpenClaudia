//! LSP (Language Server Protocol) integration tool.
//!
//! Provides code intelligence via external language servers:
//! - goToDefinition: Find where a symbol is defined
//! - findReferences: Find all references to a symbol
//! - hover: Get type/documentation info for a symbol
//! - documentSymbols: List symbols in a file

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Command, Stdio};

/// LSP operation types
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
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
    pub children: Vec<LspSymbol>,
}

/// Known language servers by file extension
fn detect_language_server(file_path: &str) -> Option<(&'static str, Vec<&'static str>)> {
    let ext = file_path.rsplit('.').next().unwrap_or("");
    match ext {
        "rs" => Some(("rust-analyzer", vec![])),
        "ts" | "tsx" | "js" | "jsx" => {
            Some(("typescript-language-server", vec!["--stdio"]))
        }
        "py" => Some(("pylsp", vec![])),
        "go" => Some(("gopls", vec!["serve"])),
        "c" | "cpp" | "h" | "hpp" => Some(("clangd", vec![])),
        "java" => Some(("jdtls", vec![])),
        "rb" => Some(("solargraph", vec!["stdio"])),
        _ => None,
    }
}

/// Execute an LSP action
pub fn execute_lsp(args: &HashMap<String, Value>) -> (String, bool) {
    let action_str = args
        .get("action")
        .and_then(|v| v.as_str())
        .unwrap_or("hover");

    let file_path = match args.get("file_path").and_then(|v| v.as_str()) {
        Some(p) => p,
        None => return ("Error: file_path is required".to_string(), true),
    };

    let line = args
        .get("line")
        .and_then(|v| v.as_u64())
        .unwrap_or(1) as u32;
    let character = args
        .get("character")
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as u32;

    // Detect language server
    let (server_cmd, server_args) = match detect_language_server(file_path) {
        Some(s) => s,
        None => {
            return (
                format!("No language server known for file: {}", file_path),
                true,
            )
        }
    };

    // Check if server is available
    if Command::new("which")
        .arg(server_cmd)
        .output()
        .map(|o| !o.status.success())
        .unwrap_or(true)
    {
        return (
            format!(
                "Language server '{}' not found. Install it to use LSP features.",
                server_cmd
            ),
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
                    "Unknown LSP action: {}. Use: goToDefinition, findReferences, hover, documentSymbols",
                    action_str
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
        Err(e) => (format!("LSP error: {}", e), true),
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
        std::fs::canonicalize(file_path).map_err(|e| format!("Cannot resolve path: {}", e))?;
    let root_uri = find_project_root(&abs_path);
    let file_uri = format!("file://{}", abs_path.display());

    // Read file content for textDocument/didOpen
    let content =
        std::fs::read_to_string(&abs_path).map_err(|e| format!("Cannot read file: {}", e))?;

    let mut child = Command::new(server_cmd)
        .args(server_args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("Failed to start {}: {}", server_cmd, e))?;

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
    parse_lsp_response(action, file_path, &response)
}

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
                .map_err(|e| format!("Read error: {}", e))?;
            let trimmed = line.trim();
            if trimmed.is_empty() {
                break;
            }
            if let Some(len_str) = trimmed.strip_prefix("Content-Length: ") {
                content_length = len_str
                    .parse()
                    .map_err(|e| format!("Bad content-length: {}", e))?;
            }
        }

        if content_length == 0 {
            return Err("No content-length in response".to_string());
        }

        let mut body = vec![0u8; content_length];
        std::io::Read::read_exact(reader, &mut body)
            .map_err(|e| format!("Body read error: {}", e))?;
        let msg: Value =
            serde_json::from_slice(&body).map_err(|e| format!("JSON parse error: {}", e))?;

        // If this message has an "id" matching our expected_id, it's the response
        if let Some(id) = msg.get("id").and_then(|v| v.as_u64()) {
            if id == u64::from(expected_id) {
                return Ok(msg);
            }
        }

        // Otherwise it's a notification (no id) or a response to a different request;
        // skip it and read the next message.
    }
    Err(format!("LSP server did not respond to request {} after 100 messages", expected_id))
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
        "cpp" | "cc" | "cxx" => "cpp",
        "h" | "hpp" => "cpp",
        "java" => "java",
        "rb" => "ruby",
        _ => "plaintext",
    }
}

fn parse_lsp_response(
    action: LspAction,
    file_path: &str,
    response: &Value,
) -> Result<LspResult, String> {
    let result_data = response.get("result");

    match action {
        LspAction::Hover => {
            let hover_text = result_data
                .and_then(|r| r.get("contents"))
                .map(|c| {
                    if let Some(s) = c.as_str() {
                        s.to_string()
                    } else if let Some(obj) = c.as_object() {
                        obj.get("value")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string()
                    } else if let Some(arr) = c.as_array() {
                        arr.iter()
                            .filter_map(|v| {
                                v.as_str()
                                    .or_else(|| v.get("value").and_then(|x| x.as_str()))
                            })
                            .collect::<Vec<_>>()
                            .join("\n")
                    } else {
                        String::new()
                    }
                });
            Ok(LspResult {
                action: "hover".to_string(),
                file_path: file_path.to_string(),
                results: Vec::new(),
                hover_text,
                symbols: Vec::new(),
            })
        }
        LspAction::GoToDefinition | LspAction::FindReferences => {
            let locations = parse_locations(result_data);
            Ok(LspResult {
                action: format!("{:?}", action),
                file_path: file_path.to_string(),
                results: locations,
                hover_text: None,
                symbols: Vec::new(),
            })
        }
        LspAction::DocumentSymbols => {
            let symbols = parse_symbols(result_data);
            Ok(LspResult {
                action: "documentSymbols".to_string(),
                file_path: file_path.to_string(),
                results: Vec::new(),
                hover_text: None,
                symbols,
            })
        }
    }
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
                    .and_then(|l| l.as_u64())
                    .unwrap_or(0) as u32
                    + 1,
                character: start
                    .get("character")
                    .and_then(|c| c.as_u64())
                    .unwrap_or(0) as u32,
                end_line: end
                    .and_then(|e| e.get("line"))
                    .and_then(|l| l.as_u64())
                    .map(|l| l as u32 + 1),
                end_character: end
                    .and_then(|e| e.get("character"))
                    .and_then(|c| c.as_u64())
                    .map(|c| c as u32),
                preview: None,
            })
        })
        .collect()
}

fn parse_symbols(data: Option<&Value>) -> Vec<LspSymbol> {
    let arr = match data {
        Some(Value::Array(a)) => a,
        _ => return Vec::new(),
    };

    arr.iter()
        .filter_map(|sym| {
            let name = sym.get("name").and_then(|n| n.as_str())?;
            let kind_num = sym.get("kind").and_then(|k| k.as_u64()).unwrap_or(0);
            let range = sym
                .get("range")
                .or_else(|| sym.get("location").and_then(|l| l.get("range")))?;
            let start = range.get("start")?;
            let end = range.get("end");

            let children = sym
                .get("children")
                .and_then(|c| c.as_array())
                .map(|_| parse_symbols(sym.get("children")))
                .unwrap_or_default();

            Some(LspSymbol {
                name: name.to_string(),
                kind: symbol_kind_name(kind_num),
                line: start
                    .get("line")
                    .and_then(|l| l.as_u64())
                    .unwrap_or(0) as u32
                    + 1,
                end_line: end
                    .and_then(|e| e.get("line"))
                    .and_then(|l| l.as_u64())
                    .map(|l| l as u32 + 1),
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
        let result = parse_lsp_response(LspAction::Hover, "test.rs", &resp).unwrap();
        assert_eq!(result.hover_text, Some("fn main()".to_string()));
    }

    #[test]
    fn test_parse_hover_string_contents() {
        let resp = json!({"result": {"contents": "simple hover text"}});
        let result = parse_lsp_response(LspAction::Hover, "test.rs", &resp).unwrap();
        assert_eq!(result.hover_text, Some("simple hover text".to_string()));
    }

    #[test]
    fn test_parse_hover_array_contents() {
        let resp = json!({"result": {"contents": ["line1", {"value": "line2"}]}});
        let result = parse_lsp_response(LspAction::Hover, "test.rs", &resp).unwrap();
        assert_eq!(result.hover_text, Some("line1\nline2".to_string()));
    }

    #[test]
    fn test_parse_hover_null_result() {
        let resp = json!({"result": null});
        let result = parse_lsp_response(LspAction::Hover, "test.rs", &resp).unwrap();
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
        args.insert(
            "action".to_string(),
            Value::String("badAction".to_string()),
        );
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
        let result =
            parse_lsp_response(LspAction::GoToDefinition, "test.rs", &resp).unwrap();
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
        let result =
            parse_lsp_response(LspAction::DocumentSymbols, "test.rs", &resp).unwrap();
        assert_eq!(result.symbols.len(), 1);
        assert_eq!(result.symbols[0].name, "Config");
        assert_eq!(result.symbols[0].kind, "Struct");
        assert_eq!(result.symbols[0].children.len(), 1);
        assert_eq!(result.symbols[0].children[0].name, "new");
        assert_eq!(result.symbols[0].children[0].kind, "Method");
    }
}
