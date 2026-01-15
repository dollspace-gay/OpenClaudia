//! Tool definitions and execution for OpenClaudia
//!
//! Implements the core tools that make OpenClaudia an agent:
//! - Bash: Execute shell commands
//! - Read: Read file contents
//! - Write: Write/create files
//! - Edit: Make targeted edits to files
//!
//! Stateful mode adds memory tools:
//! - memory_save: Store information in archival memory
//! - memory_search: Search archival memory
//! - memory_update: Update existing memory
//! - core_memory_update: Update core memory sections
//!
use crate::memory::{MemoryDb, SECTION_PERSONA, SECTION_PROJECT_INFO, SECTION_USER_PREFS};
use crate::web::{self, WebConfig};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::runtime::Handle;

/// Track if we've shown the chainlink install message (only show once per session)
static CHAINLINK_INSTALL_SHOWN: AtomicBool = AtomicBool::new(false);

/// Tool call from the model
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    #[serde(rename = "type")]
    pub call_type: String,
    pub function: FunctionCall,
}

/// Function call details
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionCall {
    pub name: String,
    pub arguments: String,
}

/// Result of executing a tool
#[derive(Debug, Clone)]
pub struct ToolResult {
    pub tool_call_id: String,
    pub content: String,
    pub is_error: bool,
}

/// Get all tool definitions for the API request (OpenAI function format)
pub fn get_tool_definitions() -> Value {
    json!([
        {
            "type": "function",
            "function": {
                "name": "bash",
                "description": "Execute a bash shell command and return the output. On Windows, Git Bash is used so standard Unix commands (ls, grep, find, cat, etc.) work normally. Use this for running commands, installing packages, git operations, file exploration, etc.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "command": {
                            "type": "string",
                            "description": "The bash command to execute. Unix-style commands work on all platforms."
                        }
                    },
                    "required": ["command"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "read_file",
                "description": "Read the contents of a file. Returns the file content as text.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": {
                            "type": "string",
                            "description": "The path to the file to read"
                        }
                    },
                    "required": ["path"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "write_file",
                "description": "Write content to a file. Creates the file if it doesn't exist, overwrites if it does.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": {
                            "type": "string",
                            "description": "The path to the file to write"
                        },
                        "content": {
                            "type": "string",
                            "description": "The content to write to the file"
                        }
                    },
                    "required": ["path", "content"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "edit_file",
                "description": "Make a targeted edit to a file by replacing old_string with new_string. The old_string must match exactly.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": {
                            "type": "string",
                            "description": "The path to the file to edit"
                        },
                        "old_string": {
                            "type": "string",
                            "description": "The exact string to find and replace"
                        },
                        "new_string": {
                            "type": "string",
                            "description": "The string to replace it with"
                        }
                    },
                    "required": ["path", "old_string", "new_string"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "list_files",
                "description": "List files and directories at a given path. Returns a list of entries.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": {
                            "type": "string",
                            "description": "The directory path to list (defaults to current directory)"
                        }
                    },
                    "required": []
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "chainlink",
                "description": "Task management tool for tracking issues and work. Commands: 'create \"title\" -p priority' (create issue), 'close ID' (close issue), 'comment ID \"text\"' (add comment), 'label ID label' (add label), 'list' (show open issues), 'show ID' (show issue details), 'subissue ID \"title\"' (create subissue), 'session start/end/work ID' (session management).",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "args": {
                            "type": "string",
                            "description": "The chainlink command arguments (e.g., 'create \"Fix bug\" -p high' or 'close 5')"
                        }
                    },
                    "required": ["args"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "web_fetch",
                "description": "Fetch the content of a web page and return it as markdown. Handles JavaScript rendering and bypasses most bot detection. Use this to read documentation, articles, or any web content.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "url": {
                            "type": "string",
                            "description": "The URL to fetch (must be a valid http:// or https:// URL)"
                        }
                    },
                    "required": ["url"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "web_search",
                "description": "Search the web and return relevant results. Requires TAVILY_API_KEY or BRAVE_API_KEY environment variable to be set. Returns titles, snippets, and URLs.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "query": {
                            "type": "string",
                            "description": "The search query"
                        },
                        "limit": {
                            "type": "integer",
                            "description": "Maximum number of results to return (default: 5)"
                        }
                    },
                    "required": ["query"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "web_browser",
                "description": "Fetch a web page using a full headless Chrome browser. Use this as a fallback when web_fetch fails due to complex JavaScript, authentication, or strict bot protection. Requires the 'browser' feature to be enabled at build time.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "url": {
                            "type": "string",
                            "description": "The URL to fetch (must be a valid http:// or https:// URL)"
                        }
                    },
                    "required": ["url"]
                }
            }
        }
    ])
}

/// Execute a tool call and return the result (non-stateful mode)
///
/// This is a convenience wrapper around `execute_tool_with_memory` for
/// when memory tools are not needed. Memory-related tool calls will
/// return an error indicating stateful mode is required.
pub fn execute_tool(tool_call: &ToolCall) -> ToolResult {
    execute_tool_with_memory(tool_call, None)
}

/// Find Git Bash on Windows
#[cfg(windows)]
fn find_git_bash() -> Option<std::path::PathBuf> {
    // Common Git Bash locations on Windows
    let paths = [
        r"C:\Program Files\Git\bin\bash.exe",
        r"C:\Program Files (x86)\Git\bin\bash.exe",
        r"C:\Git\bin\bash.exe",
    ];

    for path in &paths {
        let p = std::path::PathBuf::from(path);
        if p.exists() {
            return Some(p);
        }
    }

    // Try to find via 'where git' and derive bash path
    if let Ok(output) = Command::new("where").arg("git").output() {
        if output.status.success() {
            let git_path = String::from_utf8_lossy(&output.stdout);
            if let Some(first_line) = git_path.lines().next() {
                // git.exe is usually in cmd/ or bin/, bash is in bin/
                let git_dir = std::path::Path::new(first_line.trim())
                    .parent()
                    .and_then(|p| p.parent());
                if let Some(git_root) = git_dir {
                    let bash = git_root.join("bin").join("bash.exe");
                    if bash.exists() {
                        return Some(bash);
                    }
                }
            }
        }
    }

    None
}

/// Execute a bash command
fn execute_bash(args: &HashMap<String, Value>) -> (String, bool) {
    let command = match args.get("command").and_then(|v| v.as_str()) {
        Some(cmd) => cmd,
        None => return ("Missing 'command' argument".to_string(), true),
    };

    // On Windows, use Git Bash explicitly (not WSL bash)
    // On Unix, use system bash
    #[cfg(windows)]
    let output = {
        match find_git_bash() {
            Some(git_bash) => Command::new(git_bash)
                .args(["-c", command])
                .output(),
            None => Command::new("bash")
                .args(["-c", command])
                .output(),
        }
    };

    #[cfg(not(windows))]
    let output = Command::new("bash")
        .args(["-c", command])
        .output();

    match output {
        Ok(output) => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);

            let mut result = String::new();
            if !stdout.is_empty() {
                result.push_str(&stdout);
            }
            if !stderr.is_empty() {
                if !result.is_empty() {
                    result.push('\n');
                }
                result.push_str("stderr: ");
                result.push_str(&stderr);
            }
            if result.is_empty() {
                result = "(command completed with no output)".to_string();
            }

            // Truncate if too long
            if result.len() > 50000 {
                result = format!("{}...\n(output truncated, {} total chars)",
                    &result[..50000], result.len());
            }

            (result, !output.status.success())
        }
        Err(e) => (format!("Failed to execute command: {}", e), true),
    }
}

/// Read a file's contents
fn execute_read_file(args: &HashMap<String, Value>) -> (String, bool) {
    let path = match args.get("path").and_then(|v| v.as_str()) {
        Some(p) => p,
        None => return ("Missing 'path' argument".to_string(), true),
    };

    match fs::read_to_string(path) {
        Ok(content) => {
            // Add line numbers
            let numbered: Vec<String> = content
                .lines()
                .enumerate()
                .map(|(i, line)| format!("{:4}| {}", i + 1, line))
                .collect();

            let result = numbered.join("\n");

            // Truncate if too long
            if result.len() > 100000 {
                (format!("{}...\n(file truncated, {} total chars)",
                    &result[..100000], result.len()), false)
            } else {
                (result, false)
            }
        }
        Err(e) => (format!("Failed to read file '{}': {}", path, e), true),
    }
}

/// Write content to a file
fn execute_write_file(args: &HashMap<String, Value>) -> (String, bool) {
    let path = match args.get("path").and_then(|v| v.as_str()) {
        Some(p) => p,
        None => return ("Missing 'path' argument".to_string(), true),
    };

    let content = match args.get("content").and_then(|v| v.as_str()) {
        Some(c) => c,
        None => return ("Missing 'content' argument".to_string(), true),
    };

    // Create parent directories if needed
    if let Some(parent) = Path::new(path).parent() {
        if !parent.as_os_str().is_empty() {
            if let Err(e) = fs::create_dir_all(parent) {
                return (format!("Failed to create directories: {}", e), true);
            }
        }
    }

    match fs::write(path, content) {
        Ok(()) => (format!("Successfully wrote {} bytes to '{}'", content.len(), path), false),
        Err(e) => (format!("Failed to write file '{}': {}", path, e), true),
    }
}

/// Edit a file by replacing text
fn execute_edit_file(args: &HashMap<String, Value>) -> (String, bool) {
    let path = match args.get("path").and_then(|v| v.as_str()) {
        Some(p) => p,
        None => return ("Missing 'path' argument".to_string(), true),
    };

    let old_string = match args.get("old_string").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return ("Missing 'old_string' argument".to_string(), true),
    };

    let new_string = match args.get("new_string").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return ("Missing 'new_string' argument".to_string(), true),
    };

    // Read the file
    let content = match fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) => return (format!("Failed to read file '{}': {}", path, e), true),
    };

    // Check if old_string exists
    if !content.contains(old_string) {
        return (format!("Could not find the specified text in '{}'. Make sure old_string matches exactly.", path), true);
    }

    // Count occurrences
    let count = content.matches(old_string).count();
    if count > 1 {
        return (format!("Found {} occurrences of the text. Please provide a more specific old_string that matches uniquely.", count), true);
    }

    // Make the replacement
    let new_content = content.replacen(old_string, new_string, 1);

    // Write back
    match fs::write(path, &new_content) {
        Ok(()) => (format!("Successfully edited '{}'. Replaced {} chars with {} chars.",
            path, old_string.len(), new_string.len()), false),
        Err(e) => (format!("Failed to write file '{}': {}", path, e), true),
    }
}

/// List files in a directory
fn execute_list_files(args: &HashMap<String, Value>) -> (String, bool) {
    let path = args.get("path")
        .and_then(|v| v.as_str())
        .unwrap_or(".");

    match fs::read_dir(path) {
        Ok(entries) => {
            let mut items: Vec<String> = Vec::new();
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                let file_type = entry.file_type().map(|ft| {
                    if ft.is_dir() { "/" } else { "" }
                }).unwrap_or("");
                items.push(format!("{}{}", name, file_type));
            }
            items.sort();
            (items.join("\n"), false)
        }
        Err(e) => (format!("Failed to list directory '{}': {}", path, e), true),
    }
}

/// Execute chainlink command for task management
/// Uses Git Bash on Windows (which has access to Windows PATH)
fn execute_chainlink(args: &HashMap<String, Value>) -> (String, bool) {
    let cmd_args = match args.get("args").and_then(|v| v.as_str()) {
        Some(a) => a,
        None => return ("Missing 'args' argument".to_string(), true),
    };

    // Use Git Bash to run chainlink (same approach as execute_bash)
    #[cfg(windows)]
    let output = {
        match find_git_bash() {
            Some(git_bash) => Command::new(git_bash)
                .args(["-c", &format!("chainlink {}", cmd_args)])
                .output(),
            None => Command::new("bash")
                .args(["-c", &format!("chainlink {}", cmd_args)])
                .output(),
        }
    };

    #[cfg(not(windows))]
    let output = Command::new("bash")
        .args(["-c", &format!("chainlink {}", cmd_args)])
        .output();

    match output {
        Ok(output) => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);

            // Check if chainlink wasn't found
            if !output.status.success() &&
               (stderr.contains("command not found") || stderr.contains("not recognized")) {
                let show_install_help = !CHAINLINK_INSTALL_SHOWN.swap(true, Ordering::Relaxed);
                if show_install_help {
                    return (
                        "Chainlink not found. Chainlink is a lightweight issue tracking tool designed to integrate with AI agents.\n\n\
                        Install from: https://github.com/dollspace-gay/chainlink".to_string(),
                        true
                    );
                } else {
                    return ("Chainlink not available.".to_string(), true);
                }
            }

            let mut result = stdout.to_string();
            if !stderr.is_empty() {
                if !result.is_empty() {
                    result.push('\n');
                }
                if !output.status.success() {
                    result.push_str("Error: ");
                }
                result.push_str(&stderr);
            }
            if result.is_empty() {
                result = "(chainlink command completed)".to_string();
            }

            (result.trim().to_string(), !output.status.success())
        }
        Err(e) => (format!("Failed to execute chainlink: {}", e), true),
    }
}

/// Parse tool calls from a streaming response delta
/// Returns accumulated tool calls when complete
#[derive(Default, Debug)]
pub struct ToolCallAccumulator {
    pub tool_calls: Vec<PartialToolCall>,
}

#[derive(Default, Debug, Clone)]
pub struct PartialToolCall {
    pub index: usize,
    pub id: String,
    pub call_type: String,
    pub function_name: String,
    pub function_arguments: String,
}

impl ToolCallAccumulator {
    pub fn new() -> Self {
        Self { tool_calls: Vec::new() }
    }

    /// Process a delta from streaming response
    pub fn process_delta(&mut self, delta: &Value) {
        if let Some(tool_calls) = delta.get("tool_calls").and_then(|v| v.as_array()) {
            for tc in tool_calls {
                let index = tc.get("index").and_then(|v| v.as_u64()).unwrap_or(0) as usize;

                // Ensure we have enough slots
                while self.tool_calls.len() <= index {
                    self.tool_calls.push(PartialToolCall::default());
                }

                let partial = &mut self.tool_calls[index];
                partial.index = index;

                if let Some(id) = tc.get("id").and_then(|v| v.as_str()) {
                    partial.id = id.to_string();
                }
                if let Some(t) = tc.get("type").and_then(|v| v.as_str()) {
                    partial.call_type = t.to_string();
                }
                if let Some(func) = tc.get("function") {
                    if let Some(name) = func.get("name").and_then(|v| v.as_str()) {
                        partial.function_name = name.to_string();
                    }
                    if let Some(args) = func.get("arguments").and_then(|v| v.as_str()) {
                        partial.function_arguments.push_str(args);
                    }
                }
            }
        }
    }

    /// Convert accumulated partials to complete tool calls
    pub fn finalize(&self) -> Vec<ToolCall> {
        self.tool_calls
            .iter()
            .filter(|tc| !tc.id.is_empty() && !tc.function_name.is_empty())
            .map(|tc| ToolCall {
                id: tc.id.clone(),
                call_type: if tc.call_type.is_empty() { "function".to_string() } else { tc.call_type.clone() },
                function: FunctionCall {
                    name: tc.function_name.clone(),
                    arguments: tc.function_arguments.clone(),
                },
            })
            .collect()
    }

    /// Check if we have any tool calls
    pub fn has_tool_calls(&self) -> bool {
        self.tool_calls.iter().any(|tc| !tc.id.is_empty())
    }

    /// Clear the accumulator
    pub fn clear(&mut self) {
        self.tool_calls.clear();
    }
}

// === Memory Tools (Stateful Mode) ===

/// Get memory tool definitions for stateful mode
pub fn get_memory_tool_definitions() -> Value {
    json!([
        {
            "type": "function",
            "function": {
                "name": "memory_save",
                "description": "Save important information to archival memory for long-term storage. Use this to remember facts, decisions, patterns, and anything worth preserving across sessions.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "content": {
                            "type": "string",
                            "description": "The information to save to memory"
                        },
                        "tags": {
                            "type": "array",
                            "items": { "type": "string" },
                            "description": "Optional tags for categorizing the memory (e.g., ['architecture', 'decision'])"
                        }
                    },
                    "required": ["content"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "memory_search",
                "description": "Search archival memory for relevant information. Use this to recall previously saved facts, decisions, or context.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "query": {
                            "type": "string",
                            "description": "The search query to find relevant memories"
                        },
                        "limit": {
                            "type": "integer",
                            "description": "Maximum number of results to return (default: 10)"
                        }
                    },
                    "required": ["query"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "memory_update",
                "description": "Update an existing memory entry by ID. Use this to correct or expand previously saved information.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "id": {
                            "type": "integer",
                            "description": "The ID of the memory to update"
                        },
                        "content": {
                            "type": "string",
                            "description": "The new content for the memory"
                        }
                    },
                    "required": ["id", "content"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "core_memory_update",
                "description": "Update a core memory section. Core memory is always present in context. Sections: 'persona' (your identity/role), 'project_info' (project knowledge), 'user_preferences' (user's preferences).",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "section": {
                            "type": "string",
                            "enum": ["persona", "project_info", "user_preferences"],
                            "description": "The core memory section to update"
                        },
                        "content": {
                            "type": "string",
                            "description": "The new content for this section"
                        }
                    },
                    "required": ["section", "content"]
                }
            }
        }
    ])
}

/// Get all tool definitions, optionally including memory tools
pub fn get_all_tool_definitions(stateful: bool) -> Value {
    let mut tools = get_tool_definitions();

    if stateful {
        if let (Some(base_arr), Some(memory_arr)) = (
            tools.as_array_mut(),
            get_memory_tool_definitions().as_array().cloned()
        ) {
            base_arr.extend(memory_arr);
        }
    }

    tools
}

/// Execute a tool call, with optional memory database for stateful mode
pub fn execute_tool_with_memory(tool_call: &ToolCall, memory_db: Option<&MemoryDb>) -> ToolResult {
    let args: HashMap<String, Value> = serde_json::from_str(&tool_call.function.arguments)
        .unwrap_or_default();

    let (content, is_error) = match tool_call.function.name.as_str() {
        // Standard tools
        "bash" => execute_bash(&args),
        "read_file" => execute_read_file(&args),
        "write_file" => execute_write_file(&args),
        "edit_file" => execute_edit_file(&args),
        "list_files" => execute_list_files(&args),
        "chainlink" => execute_chainlink(&args),

        // Memory tools (require stateful mode)
        "memory_save" => {
            if let Some(db) = memory_db {
                execute_memory_save(&args, db)
            } else {
                ("Memory tools require stateful mode (--stateful flag)".to_string(), true)
            }
        }
        "memory_search" => {
            if let Some(db) = memory_db {
                execute_memory_search(&args, db)
            } else {
                ("Memory tools require stateful mode (--stateful flag)".to_string(), true)
            }
        }
        "memory_update" => {
            if let Some(db) = memory_db {
                execute_memory_update(&args, db)
            } else {
                ("Memory tools require stateful mode (--stateful flag)".to_string(), true)
            }
        }
        "core_memory_update" => {
            if let Some(db) = memory_db {
                execute_core_memory_update(&args, db)
            } else {
                ("Memory tools require stateful mode (--stateful flag)".to_string(), true)
            }
        }

        // Web tools
        "web_fetch" => execute_web_fetch(&args),
        "web_search" => execute_web_search(&args),
        "web_browser" => execute_web_browser(&args),

        _ => (format!("Unknown tool: {}", tool_call.function.name), true),
    };

    ToolResult {
        tool_call_id: tool_call.id.clone(),
        content,
        is_error,
    }
}

/// Save content to archival memory
fn execute_memory_save(args: &HashMap<String, Value>, db: &MemoryDb) -> (String, bool) {
    let content = match args.get("content").and_then(|v| v.as_str()) {
        Some(c) => c,
        None => return ("Missing 'content' argument".to_string(), true),
    };

    let tags: Vec<String> = args
        .get("tags")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    match db.memory_save(content, &tags) {
        Ok(id) => (format!("Memory saved with ID {}. Tags: {:?}", id, tags), false),
        Err(e) => (format!("Failed to save memory: {}", e), true),
    }
}

/// Search archival memory
fn execute_memory_search(args: &HashMap<String, Value>, db: &MemoryDb) -> (String, bool) {
    let query = match args.get("query").and_then(|v| v.as_str()) {
        Some(q) => q,
        None => return ("Missing 'query' argument".to_string(), true),
    };

    let limit = args
        .get("limit")
        .and_then(|v| v.as_u64())
        .unwrap_or(10) as usize;

    match db.memory_search(query, limit) {
        Ok(memories) => {
            if memories.is_empty() {
                return ("No memories found matching query.".to_string(), false);
            }

            let mut result = format!("Found {} memories:\n\n", memories.len());
            for mem in memories {
                result.push_str(&format!(
                    "[ID {}] ({})\n{}\nTags: {:?}\n\n",
                    mem.id, mem.updated_at, mem.content, mem.tags
                ));
            }
            (result, false)
        }
        Err(e) => (format!("Failed to search memory: {}", e), true),
    }
}

/// Update an existing memory
fn execute_memory_update(args: &HashMap<String, Value>, db: &MemoryDb) -> (String, bool) {
    let id = match args.get("id").and_then(|v| v.as_i64()) {
        Some(id) => id,
        None => return ("Missing 'id' argument".to_string(), true),
    };

    let content = match args.get("content").and_then(|v| v.as_str()) {
        Some(c) => c,
        None => return ("Missing 'content' argument".to_string(), true),
    };

    match db.memory_update(id, content) {
        Ok(true) => (format!("Memory {} updated successfully.", id), false),
        Ok(false) => (format!("Memory {} not found.", id), true),
        Err(e) => (format!("Failed to update memory: {}", e), true),
    }
}

/// Update a core memory section
fn execute_core_memory_update(args: &HashMap<String, Value>, db: &MemoryDb) -> (String, bool) {
    let section = match args.get("section").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return ("Missing 'section' argument".to_string(), true),
    };

    // Validate section name
    if ![SECTION_PERSONA, SECTION_PROJECT_INFO, SECTION_USER_PREFS].contains(&section) {
        return (format!("Invalid section '{}'. Must be: {}, {}, or {}",
            section, SECTION_PERSONA, SECTION_PROJECT_INFO, SECTION_USER_PREFS), true);
    }

    let content = match args.get("content").and_then(|v| v.as_str()) {
        Some(c) => c,
        None => return ("Missing 'content' argument".to_string(), true),
    };

    match db.update_core_memory(section, content) {
        Ok(()) => (format!("Core memory section '{}' updated.", section), false),
        Err(e) => (format!("Failed to update core memory: {}", e), true),
    }
}

// === Web Tools ===

/// Fetch a URL using Jina Reader
fn execute_web_fetch(args: &HashMap<String, Value>) -> (String, bool) {
    let url = match args.get("url").and_then(|v| v.as_str()) {
        Some(u) => u,
        None => return ("Missing 'url' argument".to_string(), true),
    };

    // Validate URL format
    if !url.starts_with("http://") && !url.starts_with("https://") {
        return ("Invalid URL: must start with http:// or https://".to_string(), true);
    }

    // Use tokio runtime to execute async function
    let result = match Handle::try_current() {
        Ok(handle) => {
            // We're in an async context, use block_in_place
            tokio::task::block_in_place(|| {
                handle.block_on(web::fetch_url(url))
            })
        }
        Err(_) => {
            // Create a new runtime for sync context
            match tokio::runtime::Runtime::new() {
                Ok(rt) => rt.block_on(web::fetch_url(url)),
                Err(e) => return (format!("Failed to create runtime: {}", e), true),
            }
        }
    };

    match result {
        Ok(fetch_result) => {
            let mut output = String::new();
            if let Some(title) = fetch_result.title {
                output.push_str(&format!("# {}\n\n", title));
            }
            output.push_str(&format!("URL: {}\n\n", fetch_result.url));
            output.push_str(&fetch_result.content);

            // Truncate if too long
            if output.len() > 50000 {
                output = format!("{}...\n\n(content truncated, {} total chars)",
                    &output[..50000], output.len());
            }

            (output, false)
        }
        Err(e) => (format!("Failed to fetch URL: {}", e), true),
    }
}

/// Search the web using Tavily or Brave API
fn execute_web_search(args: &HashMap<String, Value>) -> (String, bool) {
    let query = match args.get("query").and_then(|v| v.as_str()) {
        Some(q) => q,
        None => return ("Missing 'query' argument".to_string(), true),
    };

    let limit = args
        .get("limit")
        .and_then(|v| v.as_u64())
        .unwrap_or(5) as usize;

    // Load web config from environment
    let config = WebConfig::from_env();

    if !config.has_search_provider() {
        return (
            "No search API configured. Set TAVILY_API_KEY or BRAVE_API_KEY environment variable.".to_string(),
            true
        );
    }

    // Use tokio runtime to execute async function
    let result = match Handle::try_current() {
        Ok(handle) => {
            tokio::task::block_in_place(|| {
                handle.block_on(web::search_web(query, &config, limit))
            })
        }
        Err(_) => {
            match tokio::runtime::Runtime::new() {
                Ok(rt) => rt.block_on(web::search_web(query, &config, limit)),
                Err(e) => return (format!("Failed to create runtime: {}", e), true),
            }
        }
    };

    match result {
        Ok(results) => (web::format_search_results(&results), false),
        Err(e) => (format!("Search failed: {}", e), true),
    }
}

/// Fetch a URL using headless Chrome browser
/// Fallback for when Jina Reader fails on complex sites
fn execute_web_browser(args: &HashMap<String, Value>) -> (String, bool) {
    let url = match args.get("url").and_then(|v| v.as_str()) {
        Some(u) => u,
        None => return ("Missing 'url' argument".to_string(), true),
    };

    // Validate URL format
    if !url.starts_with("http://") && !url.starts_with("https://") {
        return ("Invalid URL: must start with http:// or https://".to_string(), true);
    }

    match web::fetch_with_browser(url) {
        Ok(fetch_result) => {
            let mut output = String::new();
            if let Some(title) = fetch_result.title {
                output.push_str(&format!("# {}\n\n", title));
            }
            output.push_str(&format!("URL: {}\n\n", fetch_result.url));
            output.push_str(&fetch_result.content);

            // Truncate if too long
            if output.len() > 50000 {
                output = format!("{}...\n\n(content truncated, {} total chars)",
                    &output[..50000], output.len());
            }

            (output, false)
        }
        Err(e) => (format!("Browser fetch failed: {}", e), true),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tool_definitions() {
        let tools = get_tool_definitions();
        assert!(tools.is_array());
        let arr = tools.as_array().unwrap();
        assert!(arr.len() >= 4);
    }

    #[test]
    fn test_bash_execution() {
        let mut args = HashMap::new();
        args.insert("command".to_string(), json!("echo hello"));
        let (output, is_error) = execute_bash(&args);
        assert!(!is_error);
        assert!(output.contains("hello"));
    }

    #[test]
    fn test_list_files() {
        let args = HashMap::new();
        let (output, is_error) = execute_list_files(&args);
        assert!(!is_error);
        assert!(!output.is_empty());
    }

    #[test]
    fn test_tool_call_accumulator() {
        let mut acc = ToolCallAccumulator::new();

        // Simulate streaming deltas
        acc.process_delta(&json!({
            "tool_calls": [{
                "index": 0,
                "id": "call_123",
                "type": "function",
                "function": {
                    "name": "bash",
                    "arguments": "{\"com"
                }
            }]
        }));

        acc.process_delta(&json!({
            "tool_calls": [{
                "index": 0,
                "function": {
                    "arguments": "mand\": \"ls\"}"
                }
            }]
        }));

        let calls = acc.finalize();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].function.name, "bash");
        assert_eq!(calls[0].function.arguments, "{\"command\": \"ls\"}");
    }
}
