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
use base64::Engine;
use crate::config::AppConfig;
use crate::memory::{MemoryDb, SECTION_PERSONA, SECTION_PROJECT_INFO, SECTION_USER_PREFS};
use crate::subagent;
use crate::web::{self, WebConfig};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use tokio::runtime::Handle;
use uuid::Uuid;

/// Background shell process with captured output
struct BackgroundShell {
    stdout_buffer: Arc<Mutex<Vec<String>>>,
    stderr_buffer: Arc<Mutex<Vec<String>>>,
    command: String,
    finished: Arc<AtomicBool>,
    exit_status: Arc<Mutex<Option<i32>>>,
}

/// Manager for background shell processes
struct BackgroundShellManager {
    shells: Mutex<HashMap<String, BackgroundShell>>,
}

impl BackgroundShellManager {
    fn new() -> Self {
        Self {
            shells: Mutex::new(HashMap::new()),
        }
    }

    /// Spawn a new background shell and return its ID
    fn spawn(&self, command: &str) -> Result<String, String> {
        let shell_id = Uuid::new_v4().to_string()[..8].to_string();
        // IMPORTANT: Set current_dir to ensure bash runs in the same directory as the process
        let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));

        #[cfg(windows)]
        let child = {
            match find_git_bash() {
                Some(git_bash) => Command::new(git_bash)
                    .args(["-c", command])
                    .current_dir(&cwd)
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped())
                    .spawn(),
                None => Command::new("bash")
                    .args(["-c", command])
                    .current_dir(&cwd)
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped())
                    .spawn(),
            }
        };

        #[cfg(not(windows))]
        let child = Command::new("bash")
            .args(["-c", command])
            .current_dir(&cwd)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn();

        let mut child = child.map_err(|e| format!("Failed to spawn background shell: {}", e))?;

        let stdout_buffer = Arc::new(Mutex::new(Vec::new()));
        let stderr_buffer = Arc::new(Mutex::new(Vec::new()));
        let finished = Arc::new(AtomicBool::new(false));
        let exit_status = Arc::new(Mutex::new(None));

        // Spawn thread to read stdout
        if let Some(stdout) = child.stdout.take() {
            let buffer = Arc::clone(&stdout_buffer);
            let finished_clone = Arc::clone(&finished);
            thread::spawn(move || {
                let reader = BufReader::new(stdout);
                for line in reader.lines().map_while(Result::ok) {
                    if let Ok(mut buf) = buffer.lock() {
                        buf.push(line);
                    }
                }
                finished_clone.store(true, Ordering::SeqCst);
            });
        }

        // Spawn thread to read stderr
        if let Some(stderr) = child.stderr.take() {
            let buffer = Arc::clone(&stderr_buffer);
            thread::spawn(move || {
                let reader = BufReader::new(stderr);
                for line in reader.lines().map_while(Result::ok) {
                    if let Ok(mut buf) = buffer.lock() {
                        buf.push(line);
                    }
                }
            });
        }

        // Spawn thread to wait for process and capture exit status
        let exit_status_clone = Arc::clone(&exit_status);
        let finished_clone = Arc::clone(&finished);
        let mut child_for_wait = child;
        thread::spawn(move || {
            if let Ok(status) = child_for_wait.wait() {
                if let Ok(mut es) = exit_status_clone.lock() {
                    *es = status.code();
                }
                finished_clone.store(true, Ordering::SeqCst);
            }
        });

        let shell = BackgroundShell {
            stdout_buffer,
            stderr_buffer,
            command: command.to_string(),
            finished,
            exit_status,
        };

        if let Ok(mut shells) = self.shells.lock() {
            shells.insert(shell_id.clone(), shell);
        }

        Ok(shell_id)
    }

    /// Get output from a background shell (returns new output since last call)
    fn get_output(&self, shell_id: &str) -> Result<(String, bool, Option<i32>), String> {
        let shells = self.shells.lock().map_err(|_| "Failed to lock shells")?;
        let shell = shells
            .get(shell_id)
            .ok_or_else(|| format!("Shell '{}' not found", shell_id))?;

        let mut output = String::new();

        // Get stdout lines
        if let Ok(mut buf) = shell.stdout_buffer.lock() {
            if !buf.is_empty() {
                output.push_str(&buf.join("\n"));
                buf.clear();
            }
        }

        // Get stderr lines
        if let Ok(mut buf) = shell.stderr_buffer.lock() {
            if !buf.is_empty() {
                if !output.is_empty() {
                    output.push('\n');
                }
                output.push_str("stderr:\n");
                output.push_str(&buf.join("\n"));
                buf.clear();
            }
        }

        let is_running = !shell.finished.load(Ordering::SeqCst);
        let exit_code = shell.exit_status.lock().ok().and_then(|es| *es);

        Ok((output, is_running, exit_code))
    }

    /// Kill a background shell
    fn kill(&self, shell_id: &str) -> Result<String, String> {
        let mut shells = self.shells.lock().map_err(|_| "Failed to lock shells")?;

        if let Some(shell) = shells.remove(shell_id) {
            shell.finished.store(true, Ordering::SeqCst);
            Ok(format!(
                "Shell '{}' terminated (command: {})",
                shell_id, shell.command
            ))
        } else {
            Err(format!("Shell '{}' not found", shell_id))
        }
    }

    /// List all background shells
    fn list(&self) -> Vec<(String, String, bool)> {
        if let Ok(shells) = self.shells.lock() {
            shells
                .iter()
                .map(|(id, shell)| {
                    (
                        id.clone(),
                        shell.command.clone(),
                        !shell.finished.load(Ordering::SeqCst),
                    )
                })
                .collect()
        } else {
            Vec::new()
        }
    }
}

/// Global background shell manager
static BACKGROUND_SHELLS: std::sync::LazyLock<BackgroundShellManager> =
    std::sync::LazyLock::new(BackgroundShellManager::new);

/// Track if we've shown the chainlink install message (only show once per session)
static CHAINLINK_INSTALL_SHOWN: AtomicBool = AtomicBool::new(false);

/// Todo item for task tracking
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TodoItem {
    pub content: String,
    pub status: String,
    #[serde(rename = "activeForm")]
    pub active_form: String,
}

/// Global todo list storage
static TODO_LIST: std::sync::LazyLock<Mutex<Vec<TodoItem>>> =
    std::sync::LazyLock::new(|| Mutex::new(Vec::new()));

/// Tracks which files have been read in the current session.
/// edit_file will fail if the file hasn't been read first.
static READ_TRACKER: std::sync::LazyLock<ReadFileTracker> =
    std::sync::LazyLock::new(ReadFileTracker::new);

struct ReadFileTracker {
    read_files: Mutex<std::collections::HashSet<std::path::PathBuf>>,
}

impl ReadFileTracker {
    fn new() -> Self {
        Self {
            read_files: Mutex::new(std::collections::HashSet::new()),
        }
    }

    /// Mark a file as having been read
    fn mark_read(&self, path: &Path) {
        if let Ok(canonical) = std::fs::canonicalize(path) {
            if let Ok(mut set) = self.read_files.lock() {
                set.insert(canonical);
            }
        } else {
            // If we can't canonicalize, use the path as-is
            if let Ok(mut set) = self.read_files.lock() {
                set.insert(path.to_path_buf());
            }
        }
    }

    /// Check if a file has been read
    fn has_been_read(&self, path: &Path) -> bool {
        let check_path = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        if let Ok(set) = self.read_files.lock() {
            set.contains(&check_path)
        } else {
            false
        }
    }

    /// Clear tracking (called on new session)
    #[allow(dead_code)]
    fn clear(&self) {
        if let Ok(mut set) = self.read_files.lock() {
            set.clear();
        }
    }
}

/// Reset the read tracker - used for testing
/// In production, this is called at the start of each new session
#[doc(hidden)]
pub fn reset_read_tracker() {
    READ_TRACKER.clear();
}

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
                "description": "Execute a bash shell command and return the output. On Windows, Git Bash is used so standard Unix commands (ls, grep, find, cat, etc.) work normally. Use this for running commands, installing packages, git operations, file exploration, etc. Use run_in_background for long-running commands.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "command": {
                            "type": "string",
                            "description": "The bash command to execute. Unix-style commands work on all platforms."
                        },
                        "run_in_background": {
                            "type": "boolean",
                            "description": "If true, run the command in the background and return a shell_id. Use bash_output to retrieve output later."
                        }
                    },
                    "required": ["command"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "bash_output",
                "description": "Retrieve output from a background shell. Returns new output since last check, along with status (running/finished) and exit code if finished.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "shell_id": {
                            "type": "string",
                            "description": "The shell ID returned from a bash command with run_in_background=true"
                        }
                    },
                    "required": ["shell_id"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "kill_shell",
                "description": "Terminate a background shell process.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "shell_id": {
                            "type": "string",
                            "description": "The shell ID to terminate"
                        }
                    },
                    "required": ["shell_id"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "read_file",
                "description": "Read the contents of a file. Returns the file content as text with line numbers. Supports images (PNG, JPG, GIF, WebP) via base64 encoding, PDFs via pdftotext extraction, and Jupyter notebooks (.ipynb) with formatted cell output.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": {
                            "type": "string",
                            "description": "The path to the file to read"
                        },
                        "offset": {
                            "type": "integer",
                            "description": "Line number to start reading from (1-indexed). Defaults to 1."
                        },
                        "limit": {
                            "type": "integer",
                            "description": "Maximum number of lines to read. Defaults to reading entire file."
                        },
                        "pages": {
                            "type": "string",
                            "description": "Page range for PDF files (e.g., '1-5', '3', '10-20'). Required for PDFs with more than 10 pages."
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
                "description": "Search the web and return relevant results. Uses DuckDuckGo by default (free, no API key). Falls back to Tavily or Brave API if configured. Returns titles, snippets, and URLs.",
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
        },
        {
            "type": "function",
            "function": {
                "name": "todo_write",
                "description": "Create and manage a structured task list. Use this as a fallback when chainlink is unavailable. Helps track progress and show the user what you're working on. Only one task should be 'in_progress' at a time.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "todos": {
                            "type": "array",
                            "description": "The complete todo list (replaces existing list)",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "content": {
                                        "type": "string",
                                        "description": "Task description in imperative form (e.g., 'Fix the bug')"
                                    },
                                    "status": {
                                        "type": "string",
                                        "enum": ["pending", "in_progress", "completed"],
                                        "description": "Task status"
                                    },
                                    "activeForm": {
                                        "type": "string",
                                        "description": "Task in present continuous form (e.g., 'Fixing the bug')"
                                    }
                                },
                                "required": ["content", "status", "activeForm"]
                            }
                        }
                    },
                    "required": ["todos"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "todo_read",
                "description": "Read the current todo list. Returns all tasks with their status.",
                "parameters": {
                    "type": "object",
                    "properties": {},
                    "required": []
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "notebook_edit",
                "description": "Edit a Jupyter notebook (.ipynb file). Supports replacing cell contents, inserting new cells, and deleting cells. The notebook must be read with read_file before editing.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "notebook_path": {
                            "type": "string",
                            "description": "The absolute path to the .ipynb file to edit"
                        },
                        "cell_number": {
                            "type": "integer",
                            "description": "The 0-indexed cell number to operate on"
                        },
                        "new_source": {
                            "type": "string",
                            "description": "The new source content for the cell. For delete mode, this can be empty."
                        },
                        "cell_type": {
                            "type": "string",
                            "enum": ["code", "markdown"],
                            "description": "The type of cell. Required when inserting a new cell."
                        },
                        "edit_mode": {
                            "type": "string",
                            "enum": ["replace", "insert", "delete"],
                            "description": "The edit operation: 'replace' (default) overwrites cell source, 'insert' adds a new cell at the index, 'delete' removes the cell."
                        }
                    },
                    "required": ["notebook_path", "cell_number", "new_source"]
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

    // Check if this should run in background
    let run_in_background = args
        .get("run_in_background")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    if run_in_background {
        // Spawn background shell and return shell_id
        match BACKGROUND_SHELLS.spawn(command) {
            Ok(shell_id) => {
                (format!("Background shell started with ID: {}\nUse bash_output with this shell_id to retrieve output.", shell_id), false)
            }
            Err(e) => (e, true),
        }
    } else {
        // Run synchronously (original behavior)
        // On Windows, use Git Bash explicitly (not WSL bash)
        // On Unix, use system bash
        // IMPORTANT: Set current_dir to ensure bash runs in the same directory as the process
        let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));

        #[cfg(windows)]
        let output = {
            match find_git_bash() {
                Some(git_bash) => Command::new(git_bash)
                    .args(["-c", command])
                    .current_dir(&cwd)
                    .output(),
                None => Command::new("bash")
                    .args(["-c", command])
                    .current_dir(&cwd)
                    .output(),
            }
        };

        #[cfg(not(windows))]
        let output = Command::new("bash")
            .args(["-c", command])
            .current_dir(&cwd)
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
                    result = format!(
                        "{}...\n(output truncated, {} total chars)",
                        &result[..50000],
                        result.len()
                    );
                }

                (result, !output.status.success())
            }
            Err(e) => (format!("Failed to execute command: {}", e), true),
        }
    }
}

/// Retrieve output from a background shell
fn execute_bash_output(args: &HashMap<String, Value>) -> (String, bool) {
    // If no shell_id provided, list all background shells
    let shell_id = match args.get("shell_id").and_then(|v| v.as_str()) {
        Some(id) => id,
        None => {
            let shells = BACKGROUND_SHELLS.list();
            if shells.is_empty() {
                return ("No background shells running.".to_string(), false);
            }
            let mut result = format!("Background shells ({}):\n", shells.len());
            for (id, command, is_running) in shells {
                let status = if is_running { "running" } else { "finished" };
                let cmd_preview = if command.len() > 50 {
                    format!("{}...", &command[..50])
                } else {
                    command
                };
                result.push_str(&format!("  {} [{}]: {}\n", id, status, cmd_preview));
            }
            return (result, false);
        }
    };

    match BACKGROUND_SHELLS.get_output(shell_id) {
        Ok((output, is_running, exit_code)) => {
            let status = if is_running {
                "running".to_string()
            } else {
                match exit_code {
                    Some(code) => format!("finished (exit code: {})", code),
                    None => "finished".to_string(),
                }
            };

            let result = if output.is_empty() {
                format!("Status: {}\n(no new output)", status)
            } else {
                format!("Status: {}\n\n{}", status, output)
            };

            (result, false)
        }
        Err(e) => (e, true),
    }
}

/// Kill a background shell
fn execute_kill_shell(args: &HashMap<String, Value>) -> (String, bool) {
    let shell_id = match args.get("shell_id").and_then(|v| v.as_str()) {
        Some(id) => id,
        None => return ("Missing 'shell_id' argument".to_string(), true),
    };

    match BACKGROUND_SHELLS.kill(shell_id) {
        Ok(msg) => (msg, false),
        Err(e) => (e, true),
    }
}

/// Detect file type from extension
fn detect_file_type(path: &str) -> FileType {
    let lower = path.to_lowercase();
    if lower.ends_with(".png") {
        FileType::Image("image/png")
    } else if lower.ends_with(".jpg") || lower.ends_with(".jpeg") {
        FileType::Image("image/jpeg")
    } else if lower.ends_with(".gif") {
        FileType::Image("image/gif")
    } else if lower.ends_with(".webp") {
        FileType::Image("image/webp")
    } else if lower.ends_with(".pdf") {
        FileType::Pdf
    } else if lower.ends_with(".ipynb") {
        FileType::Notebook
    } else {
        FileType::Text
    }
}

/// Supported file types for read_file
enum FileType {
    Text,
    Image(&'static str), // mime type
    Pdf,
    Notebook,
}

/// Read an image file, base64-encode it, and return a structured result
fn read_image_file(path: &str, mime_type: &str) -> (String, bool) {
    match fs::read(path) {
        Ok(bytes) => {
            let file_size = bytes.len();
            let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
            let filename = Path::new(path)
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| path.to_string());

            let result = format!(
                "[Image: {} ({} bytes, {}) - base64 data included for vision-capable models]\n{}",
                filename, file_size, mime_type, b64
            );
            (result, false)
        }
        Err(e) => (format!("Failed to read image file '{}': {}", path, e), true),
    }
}

/// Parse a page range string like "1-5", "3", or "10-20"
/// Returns (first_page, last_page) as 1-indexed values
fn parse_page_range(pages: &str) -> Result<(u32, u32), String> {
    let pages = pages.trim();
    if let Some((start, end)) = pages.split_once('-') {
        let start: u32 = start
            .trim()
            .parse()
            .map_err(|_| format!("Invalid page range start: '{}'", start.trim()))?;
        let end: u32 = end
            .trim()
            .parse()
            .map_err(|_| format!("Invalid page range end: '{}'", end.trim()))?;
        if start == 0 || end == 0 {
            return Err("Page numbers must be 1 or greater".to_string());
        }
        if start > end {
            return Err(format!(
                "Invalid page range: start ({}) > end ({})",
                start, end
            ));
        }
        Ok((start, end))
    } else {
        let page: u32 = pages
            .parse()
            .map_err(|_| format!("Invalid page number: '{}'", pages))?;
        if page == 0 {
            return Err("Page numbers must be 1 or greater".to_string());
        }
        Ok((page, page))
    }
}

/// Read a PDF file using pdftotext
fn read_pdf_file(path: &str, pages: Option<&str>) -> (String, bool) {
    // Check if pdftotext is available
    let check = Command::new("which").arg("pdftotext").output();
    match check {
        Ok(output) if !output.status.success() => {
            return (
                "pdftotext is not installed. Install it with:\n  \
                 Ubuntu/Debian: sudo apt install poppler-utils\n  \
                 macOS: brew install poppler\n  \
                 Fedora: sudo dnf install poppler-utils"
                    .to_string(),
                true,
            );
        }
        Err(_) => {
            return (
                "Could not check for pdftotext. Ensure poppler-utils is installed.".to_string(),
                true,
            );
        }
        _ => {}
    }

    // If no pages specified, check total page count first
    if pages.is_none() {
        // Use pdftotext on the whole file but first count pages with pdfinfo if available
        let info_output = Command::new("pdfinfo").arg(path).output();
        if let Ok(info) = info_output {
            if info.status.success() {
                let info_text = String::from_utf8_lossy(&info.stdout);
                for line in info_text.lines() {
                    if line.starts_with("Pages:") {
                        if let Some(count_str) = line.split(':').nth(1) {
                            if let Ok(count) = count_str.trim().parse::<u32>() {
                                if count > 10 {
                                    return (
                                        format!(
                                            "PDF has {} pages. For large PDFs (>10 pages), you must specify \
                                             a page range using the 'pages' parameter (e.g., '1-5', '3', '10-20').",
                                            count
                                        ),
                                        true,
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    // Build pdftotext command
    let mut cmd = Command::new("pdftotext");
    if let Some(pages_str) = pages {
        match parse_page_range(pages_str) {
            Ok((first, last)) => {
                cmd.arg("-f").arg(first.to_string());
                cmd.arg("-l").arg(last.to_string());
            }
            Err(e) => return (format!("Invalid pages parameter: {}", e), true),
        }
    }
    cmd.arg(path);
    cmd.arg("-"); // Output to stdout

    match cmd.output() {
        Ok(output) => {
            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                return (
                    format!("pdftotext failed for '{}': {}", path, stderr),
                    true,
                );
            }
            let text = String::from_utf8_lossy(&output.stdout).to_string();
            if text.trim().is_empty() {
                (
                    format!("PDF '{}' produced no extractable text (may be image-based).", path),
                    false,
                )
            } else {
                (text, false)
            }
        }
        Err(e) => (format!("Failed to run pdftotext on '{}': {}", path, e), true),
    }
}

/// Read a Jupyter notebook (.ipynb) and format cells for display
fn read_notebook_file(path: &str) -> (String, bool) {
    let content = match fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) => return (format!("Failed to read notebook '{}': {}", path, e), true),
    };

    let notebook: Value = match serde_json::from_str(&content) {
        Ok(v) => v,
        Err(e) => {
            return (
                format!("Failed to parse notebook '{}' as JSON: {}", path, e),
                true,
            )
        }
    };

    let cells = match notebook.get("cells").and_then(|c| c.as_array()) {
        Some(c) => c,
        None => return ("Notebook has no 'cells' array.".to_string(), true),
    };

    let mut output = String::new();
    for (i, cell) in cells.iter().enumerate() {
        let cell_type = cell
            .get("cell_type")
            .and_then(|t| t.as_str())
            .unwrap_or("unknown");

        // Get source - can be a string or array of strings
        let source = match cell.get("source") {
            Some(Value::Array(arr)) => arr
                .iter()
                .filter_map(|v| v.as_str())
                .collect::<Vec<_>>()
                .join(""),
            Some(Value::String(s)) => s.clone(),
            _ => String::new(),
        };

        output.push_str(&format!("Cell {} ({}):\n```\n{}\n```\n", i, cell_type, source));

        // For code cells, include text outputs (skip binary/image outputs)
        if cell_type == "code" {
            if let Some(outputs) = cell.get("outputs").and_then(|o| o.as_array()) {
                for out in outputs {
                    let output_type = out.get("output_type").and_then(|t| t.as_str());
                    match output_type {
                        Some("stream") => {
                            if let Some(text) = out.get("text") {
                                let text_str = match text {
                                    Value::Array(arr) => arr
                                        .iter()
                                        .filter_map(|v| v.as_str())
                                        .collect::<Vec<_>>()
                                        .join(""),
                                    Value::String(s) => s.clone(),
                                    _ => continue,
                                };
                                output.push_str(&format!("Output:\n{}\n", text_str));
                            }
                        }
                        Some("execute_result") | Some("display_data") => {
                            // Only include text/plain data, skip images and other binary
                            if let Some(data) = out.get("data") {
                                if let Some(text_plain) = data.get("text/plain") {
                                    let text_str = match text_plain {
                                        Value::Array(arr) => arr
                                            .iter()
                                            .filter_map(|v| v.as_str())
                                            .collect::<Vec<_>>()
                                            .join(""),
                                        Value::String(s) => s.clone(),
                                        _ => continue,
                                    };
                                    output.push_str(&format!("Output:\n{}\n", text_str));
                                }
                            }
                        }
                        Some("error") => {
                            if let Some(traceback) = out.get("traceback").and_then(|t| t.as_array())
                            {
                                let tb: Vec<&str> =
                                    traceback.iter().filter_map(|v| v.as_str()).collect();
                                output.push_str(&format!("Error:\n{}\n", tb.join("\n")));
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
        output.push('\n');
    }

    (output, false)
}

/// Read a file's contents
fn execute_read_file(args: &HashMap<String, Value>) -> (String, bool) {
    let path = match args.get("path").and_then(|v| v.as_str()) {
        Some(p) => p,
        None => return ("Missing 'path' argument".to_string(), true),
    };

    // Track that this file has been read (for edit_file and notebook_edit enforcement)
    READ_TRACKER.mark_read(Path::new(path));

    // Detect file type and dispatch accordingly
    match detect_file_type(path) {
        FileType::Image(mime_type) => read_image_file(path, mime_type),
        FileType::Pdf => {
            let pages = args.get("pages").and_then(|v| v.as_str());
            read_pdf_file(path, pages)
        }
        FileType::Notebook => read_notebook_file(path),
        FileType::Text => read_text_file(path, args),
    }
}

/// Read a plain text file with optional offset/limit
fn read_text_file(path: &str, args: &HashMap<String, Value>) -> (String, bool) {
    // Get optional offset (1-indexed line number to start from)
    let offset = args
        .get("offset")
        .and_then(|v| v.as_u64())
        .map(|n| n.saturating_sub(1) as usize) // Convert to 0-indexed
        .unwrap_or(0);

    // Get optional limit (max lines to read)
    let limit = args
        .get("limit")
        .and_then(|v| v.as_u64())
        .map(|n| n as usize);

    match fs::read_to_string(path) {
        Ok(content) => {
            let lines: Vec<&str> = content.lines().collect();
            let total_lines = lines.len();

            // Apply offset and limit
            let selected_lines: Vec<(usize, &str)> = lines
                .into_iter()
                .enumerate()
                .skip(offset)
                .take(limit.unwrap_or(usize::MAX))
                .collect();

            // Add line numbers (original line numbers, not relative)
            let numbered: Vec<String> = selected_lines
                .iter()
                .map(|(i, line)| format!("{:4}| {}", i + 1, line))
                .collect();

            let result = numbered.join("\n");

            // Add context about what was shown
            let context = if offset > 0 || limit.is_some() {
                let shown_start = offset + 1;
                let shown_end = offset + selected_lines.len();
                format!(
                    "\n(showing lines {}-{} of {} total)",
                    shown_start, shown_end, total_lines
                )
            } else {
                String::new()
            };

            // Truncate if too long
            if result.len() > 100000 {
                (
                    format!(
                        "{}...\n(file truncated, {} total chars){}",
                        &result[..100000],
                        result.len(),
                        context
                    ),
                    false,
                )
            } else {
                (format!("{}{}", result, context), false)
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

    // Blast radius check
    if let Err(msg) = crate::guardrails::check_file_access(path) {
        return (msg, true);
    }

    // Read existing content for diff tracking
    let old_lines = fs::read_to_string(path)
        .map(|c| c.lines().count() as u32)
        .unwrap_or(0);
    let new_lines = content.lines().count() as u32;

    // Create parent directories if needed
    if let Some(parent) = Path::new(path).parent() {
        if !parent.as_os_str().is_empty() {
            if let Err(e) = fs::create_dir_all(parent) {
                return (format!("Failed to create directories: {}", e), true);
            }
        }
    }

    match fs::write(path, content) {
        Ok(()) => {
            // Record diff stats
            crate::guardrails::record_file_modification(path, new_lines, old_lines);

            let mut result = format!("Successfully wrote {} bytes to '{}'", content.len(), path);
            if let Some(warning) = crate::guardrails::check_diff_thresholds() {
                result.push_str(&format!("\n\nWarning: {}", warning.message));
            }
            (result, false)
        }
        Err(e) => (format!("Failed to write file '{}': {}", path, e), true),
    }
}

/// Edit a file by replacing text
fn execute_edit_file(args: &HashMap<String, Value>) -> (String, bool) {
    let path = match args.get("path").and_then(|v| v.as_str()) {
        Some(p) => p,
        None => return ("Missing 'path' argument".to_string(), true),
    };

    // ENFORCE: Must read file before editing
    // This prevents the model from making edits based on hallucinated file contents
    if !READ_TRACKER.has_been_read(Path::new(path)) {
        return (
            format!(
                "You must read '{}' before editing it. Use read_file first to see the actual contents.",
                path
            ),
            true,
        );
    }

    // Blast radius check
    if let Err(msg) = crate::guardrails::check_file_access(path) {
        return (msg, true);
    }

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
        return (
            format!(
                "Could not find the specified text in '{}'. Make sure old_string matches exactly.",
                path
            ),
            true,
        );
    }

    // Count occurrences
    let count = content.matches(old_string).count();
    if count > 1 {
        return (format!("Found {} occurrences of the text. Please provide a more specific old_string that matches uniquely.", count), true);
    }

    // Track diff: lines removed vs added
    let lines_removed = old_string.lines().count() as u32;
    let lines_added = new_string.lines().count() as u32;

    // Make the replacement
    let new_content = content.replacen(old_string, new_string, 1);

    // Write back
    match fs::write(path, &new_content) {
        Ok(()) => {
            // Record diff stats
            crate::guardrails::record_file_modification(path, lines_added, lines_removed);

            let mut result = format!(
                "Successfully edited '{}'. Replaced {} chars with {} chars.",
                path,
                old_string.len(),
                new_string.len()
            );
            if let Some(warning) = crate::guardrails::check_diff_thresholds() {
                result.push_str(&format!("\n\nWarning: {}", warning.message));
            }
            (result, false)
        }
        Err(e) => (format!("Failed to write file '{}': {}", path, e), true),
    }
}

/// Split source text into a JSON array of line strings for notebook cell source format.
/// Each line except possibly the last ends with '\n'.
fn source_to_line_array(source: &str) -> Value {
    if source.is_empty() {
        return json!([]);
    }
    let lines: Vec<&str> = source.split('\n').collect();
    let mut result: Vec<Value> = Vec::with_capacity(lines.len());
    for (i, line) in lines.iter().enumerate() {
        if i < lines.len() - 1 {
            // Not the last line: append \n
            result.push(json!(format!("{}\n", line)));
        } else {
            // Last line: include as-is (no trailing \n unless empty)
            if !line.is_empty() {
                result.push(json!(*line));
            }
        }
    }
    result.into()
}

/// Edit a Jupyter notebook cell
fn execute_notebook_edit(args: &HashMap<String, Value>) -> (String, bool) {
    let notebook_path = match args.get("notebook_path").and_then(|v| v.as_str()) {
        Some(p) => p,
        None => return ("Missing 'notebook_path' argument".to_string(), true),
    };

    let cell_number = match args.get("cell_number").and_then(|v| v.as_u64()) {
        Some(n) => n as usize,
        None => return ("Missing 'cell_number' argument".to_string(), true),
    };

    let new_source = match args.get("new_source").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return ("Missing 'new_source' argument".to_string(), true),
    };

    let cell_type = args.get("cell_type").and_then(|v| v.as_str());
    let edit_mode = args
        .get("edit_mode")
        .and_then(|v| v.as_str())
        .unwrap_or("replace");

    // Validate edit_mode
    if !["replace", "insert", "delete"].contains(&edit_mode) {
        return (
            format!(
                "Invalid edit_mode '{}'. Must be 'replace', 'insert', or 'delete'.",
                edit_mode
            ),
            true,
        );
    }

    // Enforce read-before-edit
    if !READ_TRACKER.has_been_read(Path::new(notebook_path)) {
        return (
            format!(
                "You must read '{}' before editing it. Use read_file first to see the actual contents.",
                notebook_path
            ),
            true,
        );
    }

    // Blast radius check
    if let Err(msg) = crate::guardrails::check_file_access(notebook_path) {
        return (msg, true);
    }

    // Read and parse the notebook
    let content = match fs::read_to_string(notebook_path) {
        Ok(c) => c,
        Err(e) => {
            return (
                format!("Failed to read notebook '{}': {}", notebook_path, e),
                true,
            )
        }
    };

    let mut notebook: Value = match serde_json::from_str(&content) {
        Ok(v) => v,
        Err(e) => {
            return (
                format!(
                    "Failed to parse notebook '{}' as JSON: {}",
                    notebook_path, e
                ),
                true,
            )
        }
    };

    let cells = match notebook.get_mut("cells").and_then(|c| c.as_array_mut()) {
        Some(c) => c,
        None => return ("Notebook has no 'cells' array.".to_string(), true),
    };

    match edit_mode {
        "replace" => {
            if cell_number >= cells.len() {
                return (
                    format!(
                        "Cell number {} is out of bounds. Notebook has {} cells (0-indexed).",
                        cell_number,
                        cells.len()
                    ),
                    true,
                );
            }

            // Update the cell's source
            cells[cell_number]["source"] = source_to_line_array(new_source);

            // Optionally update cell_type if provided
            if let Some(ct) = cell_type {
                cells[cell_number]["cell_type"] = json!(ct);
            }
        }
        "insert" => {
            let ct = match cell_type {
                Some(ct) => ct,
                None => {
                    return (
                        "cell_type is required when inserting a new cell. Use 'code' or 'markdown'."
                            .to_string(),
                        true,
                    )
                }
            };

            if cell_number > cells.len() {
                return (
                    format!(
                        "Cell number {} is out of bounds for insertion. Notebook has {} cells (valid range: 0-{}).",
                        cell_number,
                        cells.len(),
                        cells.len()
                    ),
                    true,
                );
            }

            // Create a new cell
            let mut new_cell = json!({
                "cell_type": ct,
                "metadata": {},
                "source": source_to_line_array(new_source)
            });

            // Code cells have an outputs array and execution_count
            if ct == "code" {
                new_cell["outputs"] = json!([]);
                new_cell["execution_count"] = Value::Null;
            }

            cells.insert(cell_number, new_cell);
        }
        "delete" => {
            if cell_number >= cells.len() {
                return (
                    format!(
                        "Cell number {} is out of bounds. Notebook has {} cells (0-indexed).",
                        cell_number,
                        cells.len()
                    ),
                    true,
                );
            }

            cells.remove(cell_number);
        }
        _ => unreachable!(),
    }

    // Write back with pretty formatting
    let old_lines = content.lines().count() as u32;
    match serde_json::to_string_pretty(&notebook) {
        Ok(pretty) => {
            let new_lines = pretty.lines().count() as u32;
            match fs::write(notebook_path, &pretty) {
                Ok(()) => {
                    crate::guardrails::record_file_modification(
                        notebook_path,
                        new_lines,
                        old_lines,
                    );
                    let action = match edit_mode {
                        "replace" => format!("Replaced cell {} contents", cell_number),
                        "insert" => format!("Inserted new {} cell at position {}", cell_type.unwrap_or("unknown"), cell_number),
                        "delete" => format!("Deleted cell {}", cell_number),
                        _ => unreachable!(),
                    };
                    let mut result = format!(
                        "Successfully edited '{}'. {}. Notebook now has {} cells.",
                        notebook_path,
                        action,
                        notebook.get("cells").and_then(|c| c.as_array()).map(|a| a.len()).unwrap_or(0)
                    );
                    if let Some(warning) = crate::guardrails::check_diff_thresholds() {
                        result.push_str(&format!("\n\nWarning: {}", warning.message));
                    }
                    (result, false)
                }
                Err(e) => (
                    format!("Failed to write notebook '{}': {}", notebook_path, e),
                    true,
                ),
            }
        }
        Err(e) => (
            format!("Failed to serialize notebook: {}", e),
            true,
        ),
    }
}

/// List files in a directory
fn execute_list_files(args: &HashMap<String, Value>) -> (String, bool) {
    let path = args.get("path").and_then(|v| v.as_str()).unwrap_or(".");

    match fs::read_dir(path) {
        Ok(entries) => {
            let mut items: Vec<String> = Vec::new();
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                let file_type = entry
                    .file_type()
                    .map(|ft| if ft.is_dir() { "/" } else { "" })
                    .unwrap_or("");
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
            if !output.status.success()
                && (stderr.contains("command not found") || stderr.contains("not recognized"))
            {
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
        Self {
            tool_calls: Vec::new(),
        }
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
                call_type: if tc.call_type.is_empty() {
                    "function".to_string()
                } else {
                    tc.call_type.clone()
                },
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

// ==========================================================================
// Anthropic Streaming Tool Accumulator
// ==========================================================================

/// Content block types from Anthropic streaming responses
#[derive(Debug, Clone)]
pub enum AnthropicContentBlock {
    /// Text content block
    Text(String),
    /// Tool use content block
    ToolUse {
        id: String,
        name: String,
        input_json: String,
    },
}

/// Accumulates tool_use content blocks from Anthropic streaming responses.
///
/// When the Anthropic API receives tool definitions, it returns structured
/// `tool_use` content blocks instead of XML in text. This accumulator
/// processes the streaming events to collect those blocks.
///
/// Anthropic streaming event sequence for tool_use:
/// 1. `content_block_start` with `type: "tool_use"`, `id`, `name`
/// 2. `content_block_delta` with `type: "input_json_delta"`, `partial_json`
/// 3. `content_block_stop`
/// 4. `message_delta` with `stop_reason: "tool_use"`
#[derive(Debug)]
pub struct AnthropicToolAccumulator {
    /// Accumulated content blocks (text + tool_use)
    pub blocks: Vec<AnthropicContentBlock>,
    /// The stop reason from message_delta
    pub stop_reason: Option<String>,
}

impl Default for AnthropicToolAccumulator {
    fn default() -> Self {
        Self::new()
    }
}

impl AnthropicToolAccumulator {
    pub fn new() -> Self {
        Self {
            blocks: Vec::new(),
            stop_reason: None,
        }
    }

    /// Process a streaming SSE event from the Anthropic API.
    /// Returns any text that should be printed to the terminal.
    pub fn process_event(&mut self, event: &Value) -> Option<String> {
        let event_type = event.get("type").and_then(|t| t.as_str())?;

        match event_type {
            "content_block_start" => {
                let block = event.get("content_block")?;
                let block_type = block.get("type").and_then(|t| t.as_str())?;

                match block_type {
                    "text" => {
                        self.blocks.push(AnthropicContentBlock::Text(String::new()));
                    }
                    "tool_use" => {
                        let id = block
                            .get("id")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        let name = block
                            .get("name")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        self.blocks.push(AnthropicContentBlock::ToolUse {
                            id,
                            name,
                            input_json: String::new(),
                        });
                    }
                    _ => {}
                }
                None
            }
            "content_block_delta" => {
                let delta = event.get("delta")?;
                let delta_type = delta.get("type").and_then(|t| t.as_str())?;

                match delta_type {
                    "text_delta" => {
                        let text = delta.get("text").and_then(|t| t.as_str()).unwrap_or("");
                        // Append to last text block
                        if let Some(AnthropicContentBlock::Text(ref mut s)) = self.blocks.last_mut()
                        {
                            s.push_str(text);
                        }
                        Some(text.to_string())
                    }
                    "input_json_delta" => {
                        let json_chunk = delta
                            .get("partial_json")
                            .and_then(|t| t.as_str())
                            .unwrap_or("");
                        // Append to last tool_use block's input
                        if let Some(AnthropicContentBlock::ToolUse {
                            ref mut input_json, ..
                        }) = self.blocks.last_mut()
                        {
                            input_json.push_str(json_chunk);
                        }
                        None
                    }
                    _ => None,
                }
            }
            "message_delta" => {
                if let Some(delta) = event.get("delta") {
                    if let Some(reason) = delta.get("stop_reason").and_then(|r| r.as_str()) {
                        self.stop_reason = Some(reason.to_string());
                    }
                }
                None
            }
            _ => None,
        }
    }

    /// Check if the model requested tool use
    pub fn has_tool_use(&self) -> bool {
        self.stop_reason.as_deref() == Some("tool_use")
            && self
                .blocks
                .iter()
                .any(|b| matches!(b, AnthropicContentBlock::ToolUse { .. }))
    }

    /// Get concatenated text from all text blocks
    pub fn get_text(&self) -> String {
        self.blocks
            .iter()
            .filter_map(|b| match b {
                AnthropicContentBlock::Text(s) => Some(s.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("")
    }

    /// Convert accumulated tool_use blocks to ToolCall format for execution
    pub fn finalize_tool_calls(&self) -> Vec<ToolCall> {
        self.blocks
            .iter()
            .filter_map(|b| match b {
                AnthropicContentBlock::ToolUse {
                    id,
                    name,
                    input_json,
                } => Some(ToolCall {
                    id: id.clone(),
                    call_type: "function".to_string(),
                    function: FunctionCall {
                        name: name.clone(),
                        arguments: input_json.clone(),
                    },
                }),
                _ => None,
            })
            .collect()
    }

    /// Convert to OpenAI-format tool_calls JSON for storage in chat_session.
    /// This allows `convert_messages_to_anthropic` to handle the back-conversion.
    pub fn to_openai_tool_calls_json(&self) -> Vec<serde_json::Value> {
        self.blocks
            .iter()
            .filter_map(|b| match b {
                AnthropicContentBlock::ToolUse {
                    id,
                    name,
                    input_json,
                } => Some(serde_json::json!({
                    "id": id,
                    "type": "function",
                    "function": {
                        "name": name,
                        "arguments": input_json
                    }
                })),
                _ => None,
            })
            .collect()
    }

    /// Clear the accumulator for reuse
    pub fn clear(&mut self) {
        self.blocks.clear();
        self.stop_reason = None;
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

/// Get all tool definitions, optionally including memory and subagent tools
pub fn get_all_tool_definitions(stateful: bool, subagents: bool) -> Value {
    let mut tools = get_tool_definitions();

    if stateful {
        if let (Some(base_arr), Some(memory_arr)) = (
            tools.as_array_mut(),
            get_memory_tool_definitions().as_array().cloned(),
        ) {
            base_arr.extend(memory_arr);
        }
    }

    if subagents {
        if let (Some(base_arr), Some(subagent_arr)) = (
            tools.as_array_mut(),
            subagent::get_subagent_tool_definitions()
                .as_array()
                .cloned(),
        ) {
            base_arr.extend(subagent_arr);
        }
    }

    tools
}

/// Execute a tool call, with optional memory database for stateful mode
pub fn execute_tool_with_memory(tool_call: &ToolCall, memory_db: Option<&MemoryDb>) -> ToolResult {
    let args: HashMap<String, Value> =
        serde_json::from_str(&tool_call.function.arguments).unwrap_or_default();

    let (content, is_error) = match tool_call.function.name.as_str() {
        // Standard tools
        "bash" => execute_bash(&args),
        "bash_output" => execute_bash_output(&args),
        "kill_shell" => execute_kill_shell(&args),
        "read_file" => execute_read_file(&args),
        "write_file" => execute_write_file(&args),
        "edit_file" => execute_edit_file(&args),
        "notebook_edit" => execute_notebook_edit(&args),
        "list_files" => execute_list_files(&args),
        "chainlink" => execute_chainlink(&args),

        // Memory tools (require stateful mode)
        "memory_save" => {
            if let Some(db) = memory_db {
                execute_memory_save(&args, db)
            } else {
                (
                    "Memory tools require stateful mode (--stateful flag)".to_string(),
                    true,
                )
            }
        }
        "memory_search" => {
            if let Some(db) = memory_db {
                execute_memory_search(&args, db)
            } else {
                (
                    "Memory tools require stateful mode (--stateful flag)".to_string(),
                    true,
                )
            }
        }
        "memory_update" => {
            if let Some(db) = memory_db {
                execute_memory_update(&args, db)
            } else {
                (
                    "Memory tools require stateful mode (--stateful flag)".to_string(),
                    true,
                )
            }
        }
        "core_memory_update" => {
            if let Some(db) = memory_db {
                execute_core_memory_update(&args, db)
            } else {
                (
                    "Memory tools require stateful mode (--stateful flag)".to_string(),
                    true,
                )
            }
        }

        // Web tools
        "web_fetch" => execute_web_fetch(&args),
        "web_search" => execute_web_search(&args),
        "web_browser" => execute_web_browser(&args),

        // Todo tools (fallback for chainlink)
        "todo_write" => execute_todo_write(&args),
        "todo_read" => execute_todo_read(),

        // Subagent tools (require config - return error if called without it)
        "task" | "agent_output" => (
            "Subagent tools require configuration context. Use execute_tool_full() instead."
                .to_string(),
            true,
        ),

        _ => (format!("Unknown tool: {}", tool_call.function.name), true),
    };

    ToolResult {
        tool_call_id: tool_call.id.clone(),
        content,
        is_error,
    }
}

/// Execute a tool call with full context (memory + config for subagents)
pub fn execute_tool_full(
    tool_call: &ToolCall,
    memory_db: Option<&MemoryDb>,
    app_config: Option<&AppConfig>,
) -> ToolResult {
    let args: HashMap<String, Value> =
        serde_json::from_str(&tool_call.function.arguments).unwrap_or_default();

    // Check for subagent tools first (they need config)
    let (content, is_error) = match tool_call.function.name.as_str() {
        "task" => {
            if let Some(config) = app_config {
                subagent::execute_task_tool(&args, config)
            } else {
                (
                    "Task tool requires application configuration".to_string(),
                    true,
                )
            }
        }
        "agent_output" => subagent::execute_agent_output_tool(&args),
        // For all other tools, delegate to the existing function
        _ => {
            let result = execute_tool_with_memory(tool_call, memory_db);
            return result;
        }
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
        Ok(id) => (
            format!("Memory saved with ID {}. Tags: {:?}", id, tags),
            false,
        ),
        Err(e) => (format!("Failed to save memory: {}", e), true),
    }
}

/// Search archival memory
fn execute_memory_search(args: &HashMap<String, Value>, db: &MemoryDb) -> (String, bool) {
    let query = match args.get("query").and_then(|v| v.as_str()) {
        Some(q) => q,
        None => return ("Missing 'query' argument".to_string(), true),
    };

    let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(10) as usize;

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
        return (
            format!(
                "Invalid section '{}'. Must be: {}, {}, or {}",
                section, SECTION_PERSONA, SECTION_PROJECT_INFO, SECTION_USER_PREFS
            ),
            true,
        );
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
        return (
            "Invalid URL: must start with http:// or https://".to_string(),
            true,
        );
    }

    // Use tokio runtime to execute async function
    let result = match Handle::try_current() {
        Ok(handle) => {
            // We're in an async context, use block_in_place
            tokio::task::block_in_place(|| handle.block_on(web::fetch_url(url)))
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
                output = format!(
                    "{}...\n\n(content truncated, {} total chars)",
                    &output[..50000],
                    output.len()
                );
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

    let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(5) as usize;

    // Load web config from environment
    // Falls back to DuckDuckGo with headless browser if no API keys configured
    let config = WebConfig::from_env();

    // Use tokio runtime to execute async function
    let result = match Handle::try_current() {
        Ok(handle) => {
            tokio::task::block_in_place(|| handle.block_on(web::search_web(query, &config, limit)))
        }
        Err(_) => match tokio::runtime::Runtime::new() {
            Ok(rt) => rt.block_on(web::search_web(query, &config, limit)),
            Err(e) => return (format!("Failed to create runtime: {}", e), true),
        },
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
        return (
            "Invalid URL: must start with http:// or https://".to_string(),
            true,
        );
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
                output = format!(
                    "{}...\n\n(content truncated, {} total chars)",
                    &output[..50000],
                    output.len()
                );
            }

            (output, false)
        }
        Err(e) => (format!("Browser fetch failed: {}", e), true),
    }
}

// === Todo Tools (Chainlink fallback) ===

/// Write/update the todo list
fn execute_todo_write(args: &HashMap<String, Value>) -> (String, bool) {
    let todos_value = match args.get("todos") {
        Some(v) => v,
        None => return ("Missing 'todos' argument".to_string(), true),
    };

    let todos_array = match todos_value.as_array() {
        Some(arr) => arr,
        None => return ("'todos' must be an array".to_string(), true),
    };

    let mut new_todos: Vec<TodoItem> = Vec::new();
    let mut in_progress_count = 0;

    for (i, item) in todos_array.iter().enumerate() {
        let content = match item.get("content").and_then(|v| v.as_str()) {
            Some(c) => c.to_string(),
            None => return (format!("Todo {} missing 'content' field", i), true),
        };

        let status = match item.get("status").and_then(|v| v.as_str()) {
            Some(s) => {
                if !["pending", "in_progress", "completed"].contains(&s) {
                    return (
                        format!(
                            "Todo {} has invalid status '{}'. Must be: pending, in_progress, completed",
                            i, s
                        ),
                        true,
                    );
                }
                if s == "in_progress" {
                    in_progress_count += 1;
                }
                s.to_string()
            }
            None => return (format!("Todo {} missing 'status' field", i), true),
        };

        let active_form = match item.get("activeForm").and_then(|v| v.as_str()) {
            Some(a) => a.to_string(),
            None => return (format!("Todo {} missing 'activeForm' field", i), true),
        };

        new_todos.push(TodoItem {
            content,
            status,
            active_form,
        });
    }

    // Warn if more than one task is in_progress
    let warning = if in_progress_count > 1 {
        format!(
            "\nWarning: {} tasks marked as in_progress. Best practice is to have only one.",
            in_progress_count
        )
    } else {
        String::new()
    };

    // Update the global todo list
    match TODO_LIST.lock() {
        Ok(mut list) => {
            *list = new_todos.clone();
        }
        Err(e) => return (format!("Failed to update todo list: {}", e), true),
    }

    // Format output
    let completed = new_todos.iter().filter(|t| t.status == "completed").count();
    let in_progress = new_todos
        .iter()
        .filter(|t| t.status == "in_progress")
        .count();
    let pending = new_todos.iter().filter(|t| t.status == "pending").count();

    let mut output = format!(
        "Todo list updated: {} total ({} completed, {} in progress, {} pending){}",
        new_todos.len(),
        completed,
        in_progress,
        pending,
        warning
    );

    // Show current in-progress task if any
    if let Some(current) = new_todos.iter().find(|t| t.status == "in_progress") {
        output.push_str(&format!("\n\nCurrently: {}", current.active_form));
    }

    (output, false)
}

/// Read the current todo list
fn execute_todo_read() -> (String, bool) {
    let todos = match TODO_LIST.lock() {
        Ok(list) => list.clone(),
        Err(e) => return (format!("Failed to read todo list: {}", e), true),
    };

    if todos.is_empty() {
        return ("No todos in list.".to_string(), false);
    }

    let mut output = String::new();
    for (i, todo) in todos.iter().enumerate() {
        let status_icon = match todo.status.as_str() {
            "completed" => "[x]",
            "in_progress" => "[>]",
            "pending" => "[ ]",
            _ => "[?]",
        };
        output.push_str(&format!("{}. {} {}\n", i + 1, status_icon, todo.content));
    }

    // Summary
    let completed = todos.iter().filter(|t| t.status == "completed").count();
    let in_progress = todos.iter().filter(|t| t.status == "in_progress").count();
    let pending = todos.iter().filter(|t| t.status == "pending").count();

    output.push_str(&format!(
        "\n({} completed, {} in progress, {} pending)",
        completed, in_progress, pending
    ));

    (output, false)
}

/// Get the current todo list (for external use)
pub fn get_todo_list() -> Vec<TodoItem> {
    TODO_LIST.lock().map(|l| l.clone()).unwrap_or_default()
}

/// Clear the todo list
pub fn clear_todo_list() {
    if let Ok(mut list) = TODO_LIST.lock() {
        list.clear();
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

        // Extract tool names for specific checks
        let tool_names: Vec<&str> = arr
            .iter()
            .filter_map(|t| t["function"]["name"].as_str())
            .collect();

        // Verify all core tools are present
        let required = vec![
            "bash",
            "bash_output",
            "kill_shell",
            "read_file",
            "write_file",
            "edit_file",
            "list_files",
            "chainlink",
            "web_fetch",
            "web_search",
            "todo_write",
            "todo_read",
            "notebook_edit",
        ];
        for name in &required {
            assert!(
                tool_names.contains(name),
                "Missing required tool '{}'. Found: {:?}",
                name,
                tool_names
            );
        }

        // Each tool must have valid structure
        for tool in arr {
            let func = tool.get("function").expect("Tool missing 'function'");
            assert!(
                func.get("name").and_then(|n| n.as_str()).is_some(),
                "Tool missing name"
            );
            assert!(
                func.get("description").and_then(|d| d.as_str()).is_some(),
                "Tool missing description"
            );
            assert!(func.get("parameters").is_some(), "Tool missing parameters");
        }
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
        assert!(!is_error, "list_files should succeed for cwd");
        assert!(!output.is_empty(), "cwd should contain files");
        // Running in the project root, Cargo.toml must be present
        assert!(
            output.contains("Cargo.toml"),
            "Project root should contain Cargo.toml, got: {}",
            output
        );
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

    #[test]
    fn test_anthropic_accumulator_text_only() {
        let mut acc = AnthropicToolAccumulator::new();

        acc.process_event(
            &json!({"type": "content_block_start", "content_block": {"type": "text"}}),
        );
        let text1 = acc.process_event(&json!({"type": "content_block_delta", "delta": {"type": "text_delta", "text": "Hello "}}));
        let text2 = acc.process_event(&json!({"type": "content_block_delta", "delta": {"type": "text_delta", "text": "world"}}));
        acc.process_event(&json!({"type": "content_block_stop"}));
        acc.process_event(&json!({"type": "message_delta", "delta": {"stop_reason": "end_turn"}}));

        assert_eq!(text1, Some("Hello ".to_string()));
        assert_eq!(text2, Some("world".to_string()));
        assert!(!acc.has_tool_use());
        assert_eq!(acc.get_text(), "Hello world");
        assert_eq!(acc.stop_reason.as_deref(), Some("end_turn"));
    }

    #[test]
    fn test_anthropic_accumulator_tool_use() {
        let mut acc = AnthropicToolAccumulator::new();

        // Text block
        acc.process_event(
            &json!({"type": "content_block_start", "content_block": {"type": "text"}}),
        );
        acc.process_event(&json!({"type": "content_block_delta", "delta": {"type": "text_delta", "text": "Reading file..."}}));
        acc.process_event(&json!({"type": "content_block_stop"}));

        // Tool use block
        acc.process_event(&json!({
            "type": "content_block_start",
            "content_block": {"type": "tool_use", "id": "toolu_abc123", "name": "read_file"}
        }));
        acc.process_event(&json!({"type": "content_block_delta", "delta": {"type": "input_json_delta", "partial_json": "{\"path\":"}}));
        acc.process_event(&json!({"type": "content_block_delta", "delta": {"type": "input_json_delta", "partial_json": " \"test.txt\"}"}}));
        acc.process_event(&json!({"type": "content_block_stop"}));

        // Stop with tool_use
        acc.process_event(&json!({"type": "message_delta", "delta": {"stop_reason": "tool_use"}}));

        assert!(acc.has_tool_use());
        assert_eq!(acc.get_text(), "Reading file...");

        let tools = acc.finalize_tool_calls();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].id, "toolu_abc123");
        assert_eq!(tools[0].function.name, "read_file");
        assert_eq!(tools[0].function.arguments, "{\"path\": \"test.txt\"}");
    }

    #[test]
    fn test_anthropic_accumulator_multiple_tools() {
        let mut acc = AnthropicToolAccumulator::new();

        // First tool
        acc.process_event(&json!({
            "type": "content_block_start",
            "content_block": {"type": "tool_use", "id": "toolu_001", "name": "bash"}
        }));
        acc.process_event(&json!({"type": "content_block_delta", "delta": {"type": "input_json_delta", "partial_json": "{\"command\": \"ls\"}"}}));
        acc.process_event(&json!({"type": "content_block_stop"}));

        // Second tool
        acc.process_event(&json!({
            "type": "content_block_start",
            "content_block": {"type": "tool_use", "id": "toolu_002", "name": "read_file"}
        }));
        acc.process_event(&json!({"type": "content_block_delta", "delta": {"type": "input_json_delta", "partial_json": "{\"path\": \"Cargo.toml\"}"}}));
        acc.process_event(&json!({"type": "content_block_stop"}));

        acc.process_event(&json!({"type": "message_delta", "delta": {"stop_reason": "tool_use"}}));

        assert!(acc.has_tool_use());
        let tools = acc.finalize_tool_calls();
        assert_eq!(tools.len(), 2);
        assert_eq!(tools[0].function.name, "bash");
        assert_eq!(tools[1].function.name, "read_file");
    }

    #[test]
    fn test_anthropic_accumulator_openai_conversion() {
        let mut acc = AnthropicToolAccumulator::new();

        acc.process_event(&json!({
            "type": "content_block_start",
            "content_block": {"type": "tool_use", "id": "toolu_xyz", "name": "edit_file"}
        }));
        acc.process_event(&json!({"type": "content_block_delta", "delta": {"type": "input_json_delta", "partial_json": "{\"path\": \"a.rs\"}"}}));
        acc.process_event(&json!({"type": "content_block_stop"}));
        acc.process_event(&json!({"type": "message_delta", "delta": {"stop_reason": "tool_use"}}));

        let openai_calls = acc.to_openai_tool_calls_json();
        assert_eq!(openai_calls.len(), 1);
        assert_eq!(openai_calls[0]["id"], "toolu_xyz");
        assert_eq!(openai_calls[0]["function"]["name"], "edit_file");
        assert_eq!(
            openai_calls[0]["function"]["arguments"],
            "{\"path\": \"a.rs\"}"
        );
    }

    #[test]
    fn test_anthropic_accumulator_clear() {
        let mut acc = AnthropicToolAccumulator::new();

        acc.process_event(
            &json!({"type": "content_block_start", "content_block": {"type": "text"}}),
        );
        acc.process_event(&json!({"type": "content_block_delta", "delta": {"type": "text_delta", "text": "hello"}}));
        acc.process_event(&json!({"type": "message_delta", "delta": {"stop_reason": "end_turn"}}));

        assert_eq!(acc.blocks.len(), 1);
        assert!(acc.stop_reason.is_some());

        acc.clear();
        assert!(acc.blocks.is_empty());
        assert!(acc.stop_reason.is_none());
    }

    // === File type detection tests ===

    #[test]
    fn test_detect_file_type_images() {
        assert!(matches!(detect_file_type("photo.png"), FileType::Image("image/png")));
        assert!(matches!(detect_file_type("photo.PNG"), FileType::Image("image/png")));
        assert!(matches!(detect_file_type("photo.jpg"), FileType::Image("image/jpeg")));
        assert!(matches!(detect_file_type("photo.jpeg"), FileType::Image("image/jpeg")));
        assert!(matches!(detect_file_type("photo.JPEG"), FileType::Image("image/jpeg")));
        assert!(matches!(detect_file_type("anim.gif"), FileType::Image("image/gif")));
        assert!(matches!(detect_file_type("modern.webp"), FileType::Image("image/webp")));
    }

    #[test]
    fn test_detect_file_type_pdf() {
        assert!(matches!(detect_file_type("document.pdf"), FileType::Pdf));
        assert!(matches!(detect_file_type("DOCUMENT.PDF"), FileType::Pdf));
    }

    #[test]
    fn test_detect_file_type_notebook() {
        assert!(matches!(detect_file_type("analysis.ipynb"), FileType::Notebook));
        assert!(matches!(detect_file_type("test.IPYNB"), FileType::Notebook));
    }

    #[test]
    fn test_detect_file_type_text() {
        assert!(matches!(detect_file_type("main.rs"), FileType::Text));
        assert!(matches!(detect_file_type("README.md"), FileType::Text));
        assert!(matches!(detect_file_type("config.yaml"), FileType::Text));
        assert!(matches!(detect_file_type("data.csv"), FileType::Text));
    }

    // === Page range parsing tests ===

    #[test]
    fn test_parse_page_range_single() {
        assert_eq!(parse_page_range("3").unwrap(), (3, 3));
        assert_eq!(parse_page_range("1").unwrap(), (1, 1));
        assert_eq!(parse_page_range("100").unwrap(), (100, 100));
    }

    #[test]
    fn test_parse_page_range_range() {
        assert_eq!(parse_page_range("1-5").unwrap(), (1, 5));
        assert_eq!(parse_page_range("10-20").unwrap(), (10, 20));
        assert_eq!(parse_page_range(" 3 - 7 ").unwrap(), (3, 7));
    }

    #[test]
    fn test_parse_page_range_invalid() {
        assert!(parse_page_range("0").is_err());
        assert!(parse_page_range("5-3").is_err());
        assert!(parse_page_range("abc").is_err());
        assert!(parse_page_range("1-abc").is_err());
        assert!(parse_page_range("0-5").is_err());
    }

    // === Notebook source formatting tests ===

    #[test]
    fn test_source_to_line_array_multiline() {
        let result = source_to_line_array("line1\nline2\nline3");
        let arr = result.as_array().unwrap();
        assert_eq!(arr.len(), 3);
        assert_eq!(arr[0], json!("line1\n"));
        assert_eq!(arr[1], json!("line2\n"));
        assert_eq!(arr[2], json!("line3"));
    }

    #[test]
    fn test_source_to_line_array_single_line() {
        let result = source_to_line_array("hello world");
        let arr = result.as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0], json!("hello world"));
    }

    #[test]
    fn test_source_to_line_array_empty() {
        let result = source_to_line_array("");
        let arr = result.as_array().unwrap();
        assert_eq!(arr.len(), 0);
    }

    #[test]
    fn test_source_to_line_array_trailing_newline() {
        let result = source_to_line_array("line1\nline2\n");
        let arr = result.as_array().unwrap();
        // "line1\nline2\n" splits into ["line1", "line2", ""]
        // line1 -> "line1\n", line2 -> "line2\n", "" -> skipped (empty last)
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0], json!("line1\n"));
        assert_eq!(arr[1], json!("line2\n"));
    }

    // === Notebook reading tests ===

    #[test]
    fn test_read_notebook_file() {
        let dir = tempfile::tempdir().unwrap();
        let nb_path = dir.path().join("test.ipynb");
        let notebook = json!({
            "cells": [
                {
                    "cell_type": "markdown",
                    "metadata": {},
                    "source": ["# Title\n", "Some text"]
                },
                {
                    "cell_type": "code",
                    "metadata": {},
                    "source": ["print('hello')"],
                    "outputs": [
                        {
                            "output_type": "stream",
                            "name": "stdout",
                            "text": ["hello\n"]
                        }
                    ],
                    "execution_count": 1
                }
            ],
            "metadata": {},
            "nbformat": 4,
            "nbformat_minor": 5
        });
        fs::write(&nb_path, serde_json::to_string_pretty(&notebook).unwrap()).unwrap();

        let (output, is_error) = read_notebook_file(nb_path.to_str().unwrap());
        assert!(!is_error, "read_notebook_file should succeed: {}", output);
        assert!(output.contains("Cell 0 (markdown)"));
        assert!(output.contains("# Title"));
        assert!(output.contains("Cell 1 (code)"));
        assert!(output.contains("print('hello')"));
        assert!(output.contains("Output:"));
        assert!(output.contains("hello"));
    }

    // === Notebook edit tests ===

    #[test]
    fn test_notebook_edit_replace() {
        let dir = tempfile::tempdir().unwrap();
        let nb_path = dir.path().join("test.ipynb");
        let notebook = json!({
            "cells": [
                {
                    "cell_type": "code",
                    "metadata": {},
                    "source": ["old code"],
                    "outputs": [],
                    "execution_count": null
                }
            ],
            "metadata": {},
            "nbformat": 4,
            "nbformat_minor": 5
        });
        fs::write(&nb_path, serde_json::to_string_pretty(&notebook).unwrap()).unwrap();

        // Mark as read first
        READ_TRACKER.mark_read(&nb_path);

        let mut args = HashMap::new();
        args.insert("notebook_path".to_string(), json!(nb_path.to_str().unwrap()));
        args.insert("cell_number".to_string(), json!(0));
        args.insert("new_source".to_string(), json!("new code\nline 2"));

        let (output, is_error) = execute_notebook_edit(&args);
        assert!(!is_error, "notebook_edit replace should succeed: {}", output);
        assert!(output.contains("Replaced cell 0"));

        // Verify the file was updated
        let content = fs::read_to_string(&nb_path).unwrap();
        let updated: Value = serde_json::from_str(&content).unwrap();
        let source = updated["cells"][0]["source"].as_array().unwrap();
        assert_eq!(source[0], json!("new code\n"));
        assert_eq!(source[1], json!("line 2"));
    }

    #[test]
    fn test_notebook_edit_insert() {
        let dir = tempfile::tempdir().unwrap();
        let nb_path = dir.path().join("test.ipynb");
        let notebook = json!({
            "cells": [
                {
                    "cell_type": "code",
                    "metadata": {},
                    "source": ["existing"],
                    "outputs": [],
                    "execution_count": null
                }
            ],
            "metadata": {},
            "nbformat": 4,
            "nbformat_minor": 5
        });
        fs::write(&nb_path, serde_json::to_string_pretty(&notebook).unwrap()).unwrap();

        READ_TRACKER.mark_read(&nb_path);

        let mut args = HashMap::new();
        args.insert("notebook_path".to_string(), json!(nb_path.to_str().unwrap()));
        args.insert("cell_number".to_string(), json!(0));
        args.insert("new_source".to_string(), json!("# New markdown cell"));
        args.insert("cell_type".to_string(), json!("markdown"));
        args.insert("edit_mode".to_string(), json!("insert"));

        let (output, is_error) = execute_notebook_edit(&args);
        assert!(!is_error, "notebook_edit insert should succeed: {}", output);
        assert!(output.contains("Inserted new markdown cell"));

        // Verify - should now have 2 cells
        let content = fs::read_to_string(&nb_path).unwrap();
        let updated: Value = serde_json::from_str(&content).unwrap();
        let cells = updated["cells"].as_array().unwrap();
        assert_eq!(cells.len(), 2);
        assert_eq!(cells[0]["cell_type"], json!("markdown"));
        assert_eq!(cells[1]["cell_type"], json!("code"));
    }

    #[test]
    fn test_notebook_edit_delete() {
        let dir = tempfile::tempdir().unwrap();
        let nb_path = dir.path().join("test.ipynb");
        let notebook = json!({
            "cells": [
                {
                    "cell_type": "code",
                    "metadata": {},
                    "source": ["cell 0"],
                    "outputs": [],
                    "execution_count": null
                },
                {
                    "cell_type": "code",
                    "metadata": {},
                    "source": ["cell 1"],
                    "outputs": [],
                    "execution_count": null
                }
            ],
            "metadata": {},
            "nbformat": 4,
            "nbformat_minor": 5
        });
        fs::write(&nb_path, serde_json::to_string_pretty(&notebook).unwrap()).unwrap();

        READ_TRACKER.mark_read(&nb_path);

        let mut args = HashMap::new();
        args.insert("notebook_path".to_string(), json!(nb_path.to_str().unwrap()));
        args.insert("cell_number".to_string(), json!(0));
        args.insert("new_source".to_string(), json!(""));
        args.insert("edit_mode".to_string(), json!("delete"));

        let (output, is_error) = execute_notebook_edit(&args);
        assert!(!is_error, "notebook_edit delete should succeed: {}", output);
        assert!(output.contains("Deleted cell 0"));

        // Verify - should now have 1 cell
        let content = fs::read_to_string(&nb_path).unwrap();
        let updated: Value = serde_json::from_str(&content).unwrap();
        let cells = updated["cells"].as_array().unwrap();
        assert_eq!(cells.len(), 1);
        assert_eq!(cells[0]["source"].as_array().unwrap()[0], json!("cell 1"));
    }

    #[test]
    fn test_notebook_edit_requires_read_first() {
        let mut args = HashMap::new();
        args.insert(
            "notebook_path".to_string(),
            json!("/tmp/nonexistent_unread_notebook.ipynb"),
        );
        args.insert("cell_number".to_string(), json!(0));
        args.insert("new_source".to_string(), json!("test"));

        let (output, is_error) = execute_notebook_edit(&args);
        assert!(is_error, "Should fail without reading first");
        assert!(output.contains("must read"));
    }

    #[test]
    fn test_notebook_edit_out_of_bounds() {
        let dir = tempfile::tempdir().unwrap();
        let nb_path = dir.path().join("test.ipynb");
        let notebook = json!({
            "cells": [
                {
                    "cell_type": "code",
                    "metadata": {},
                    "source": ["only cell"],
                    "outputs": [],
                    "execution_count": null
                }
            ],
            "metadata": {},
            "nbformat": 4,
            "nbformat_minor": 5
        });
        fs::write(&nb_path, serde_json::to_string_pretty(&notebook).unwrap()).unwrap();

        READ_TRACKER.mark_read(&nb_path);

        let mut args = HashMap::new();
        args.insert("notebook_path".to_string(), json!(nb_path.to_str().unwrap()));
        args.insert("cell_number".to_string(), json!(5));
        args.insert("new_source".to_string(), json!("test"));

        let (output, is_error) = execute_notebook_edit(&args);
        assert!(is_error, "Should fail for out-of-bounds cell");
        assert!(output.contains("out of bounds"));
    }

    #[test]
    fn test_notebook_edit_insert_requires_cell_type() {
        let dir = tempfile::tempdir().unwrap();
        let nb_path = dir.path().join("test.ipynb");
        let notebook = json!({
            "cells": [],
            "metadata": {},
            "nbformat": 4,
            "nbformat_minor": 5
        });
        fs::write(&nb_path, serde_json::to_string_pretty(&notebook).unwrap()).unwrap();

        READ_TRACKER.mark_read(&nb_path);

        let mut args = HashMap::new();
        args.insert("notebook_path".to_string(), json!(nb_path.to_str().unwrap()));
        args.insert("cell_number".to_string(), json!(0));
        args.insert("new_source".to_string(), json!("test"));
        args.insert("edit_mode".to_string(), json!("insert"));
        // No cell_type provided

        let (output, is_error) = execute_notebook_edit(&args);
        assert!(is_error, "Should fail without cell_type for insert");
        assert!(output.contains("cell_type is required"));
    }

    // === Image reading test ===

    #[test]
    fn test_read_image_file() {
        let dir = tempfile::tempdir().unwrap();
        let img_path = dir.path().join("test.png");
        // Write some fake PNG bytes
        let fake_png = vec![0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];
        fs::write(&img_path, &fake_png).unwrap();

        let (output, is_error) = read_image_file(img_path.to_str().unwrap(), "image/png");
        assert!(!is_error, "read_image_file should succeed");
        assert!(output.contains("[Image: test.png"));
        assert!(output.contains("image/png"));
        assert!(output.contains("8 bytes"));
        // Check that base64 data is present
        let b64 = base64::engine::general_purpose::STANDARD.encode(&fake_png);
        assert!(output.contains(&b64));
    }

    // === Insert code cell has outputs field ===

    #[test]
    fn test_notebook_edit_insert_code_cell_has_outputs() {
        let dir = tempfile::tempdir().unwrap();
        let nb_path = dir.path().join("test.ipynb");
        let notebook = json!({
            "cells": [],
            "metadata": {},
            "nbformat": 4,
            "nbformat_minor": 5
        });
        fs::write(&nb_path, serde_json::to_string_pretty(&notebook).unwrap()).unwrap();

        READ_TRACKER.mark_read(&nb_path);

        let mut args = HashMap::new();
        args.insert("notebook_path".to_string(), json!(nb_path.to_str().unwrap()));
        args.insert("cell_number".to_string(), json!(0));
        args.insert("new_source".to_string(), json!("x = 1"));
        args.insert("cell_type".to_string(), json!("code"));
        args.insert("edit_mode".to_string(), json!("insert"));

        let (output, is_error) = execute_notebook_edit(&args);
        assert!(!is_error, "insert code cell should succeed: {}", output);

        let content = fs::read_to_string(&nb_path).unwrap();
        let updated: Value = serde_json::from_str(&content).unwrap();
        let cell = &updated["cells"][0];
        assert_eq!(cell["cell_type"], json!("code"));
        assert!(cell.get("outputs").is_some(), "Code cell should have outputs field");
        assert!(cell["outputs"].as_array().unwrap().is_empty());
        assert!(cell.get("execution_count").is_some(), "Code cell should have execution_count");
    }
}
