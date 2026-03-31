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

mod accumulator;
mod ask_user;
mod bash;
mod chainlink;
mod cron;
mod file;
pub mod file_index;
pub mod lsp;
mod plan_mode;
mod task;
mod todo;
mod web;
pub mod worktree;

// Re-exports
pub use accumulator::{
    AnthropicContentBlock, AnthropicToolAccumulator, PartialToolCall, ToolCallAccumulator,
};
pub use todo::{clear_todo_list, get_todo_list, TodoItem};

use crate::config::AppConfig;
use crate::memory::MemoryDb;
use crate::permissions::{CheckResult, PermissionManager};
use crate::session::TaskManager;
use crate::subagent;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;

/// Safely truncate a string at a byte boundary without splitting multi-byte UTF-8 characters.
/// Returns the longest prefix of `s` that is at most `max_bytes` bytes and ends on a char boundary.
pub fn safe_truncate(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// Reset the read tracker - used for testing
/// In production, this is called at the start of each new session
#[doc(hidden)]
pub fn reset_read_tracker() {
    file::READ_TRACKER.clear();
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

/// Marker type for ask_user_question results.
/// The tool returns a JSON object with type "user_question" that the main loop
/// intercepts to display questions and collect answers from the user.
pub const USER_QUESTION_MARKER: &str = "user_question";

/// Marker type for enter_plan_mode results.
pub const ENTER_PLAN_MODE_MARKER: &str = "enter_plan_mode";

/// Marker type for exit_plan_mode results.
pub const EXIT_PLAN_MODE_MARKER: &str = "exit_plan_mode";

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
        },
        // ====================================================================
        // Structured Task Management Tools
        // ====================================================================
        {
            "type": "function",
            "function": {
                "name": "task_create",
                "description": "Create a new structured task with dependency tracking. Tasks are stored in the session and support blocking/blocked_by relationships. Only one task can be in_progress at a time.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "subject": {
                            "type": "string",
                            "description": "Brief title in imperative form (e.g., 'Add permission system')"
                        },
                        "description": {
                            "type": "string",
                            "description": "Detailed description of the task"
                        },
                        "active_form": {
                            "type": "string",
                            "description": "Present continuous form for spinner display (e.g., 'Adding permission system')"
                        }
                    },
                    "required": ["subject", "description"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "ask_user_question",
                "description": "Ask the user one or more structured questions with predefined options. Use this when you need clarification or want the user to make a choice before proceeding. Each question can have 2-4 options plus an automatic 'Other' option. Supports single or multi-select.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "questions": {
                            "type": "array",
                            "description": "1-4 questions to ask the user",
                            "minItems": 1,
                            "maxItems": 4,
                            "items": {
                                "type": "object",
                                "properties": {
                                    "question": {
                                        "type": "string",
                                        "description": "The question text to display"
                                    },
                                    "header": {
                                        "type": "string",
                                        "description": "Short label (max 12 chars) shown as a tag",
                                        "maxLength": 12
                                    },
                                    "options": {
                                        "type": "array",
                                        "description": "2-4 answer options",
                                        "minItems": 2,
                                        "maxItems": 4,
                                        "items": {
                                            "type": "object",
                                            "properties": {
                                                "label": {
                                                    "type": "string",
                                                    "description": "Option name (e.g., 'PostgreSQL')"
                                                },
                                                "description": {
                                                    "type": "string",
                                                    "description": "Brief description of this option"
                                                }
                                            },
                                            "required": ["label", "description"]
                                        }
                                    },
                                    "multi_select": {
                                        "type": "boolean",
                                        "description": "If true, user can select multiple options (comma-separated)"
                                    }
                                },
                                "required": ["question", "header", "options"]
                            }
                        }
                    },
                    "required": ["questions"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "task_update",
                "description": "Update an existing task's status, subject, description, or dependencies. Setting status to 'in_progress' will demote any currently in-progress task to 'pending'. Setting status to 'deleted' removes the task entirely.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "task_id": {
                            "type": "string",
                            "description": "The task ID (e.g., 'task-1')"
                        },
                        "status": {
                            "type": "string",
                            "enum": ["pending", "in_progress", "completed", "deleted"],
                            "description": "New task status"
                        },
                        "subject": {
                            "type": "string",
                            "description": "Updated task title"
                        },
                        "description": {
                            "type": "string",
                            "description": "Updated task description"
                        },
                        "active_form": {
                            "type": "string",
                            "description": "Updated spinner text (present continuous form)"
                        },
                        "add_blocks": {
                            "type": "array",
                            "items": { "type": "string" },
                            "description": "Task IDs that this task blocks (downstream dependencies)"
                        },
                        "add_blocked_by": {
                            "type": "array",
                            "items": { "type": "string" },
                            "description": "Task IDs that block this task (upstream dependencies)"
                        }
                    },
                    "required": ["task_id"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "task_get",
                "description": "Get full details of a specific task including its dependencies, status, and timestamps.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "task_id": {
                            "type": "string",
                            "description": "The task ID (e.g., 'task-1')"
                        }
                    },
                    "required": ["task_id"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "task_list",
                "description": "List all tasks with their status and dependency summary. Shows pending, in-progress, and completed counts.",
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
                "name": "enter_plan_mode",
                "description": "Switch to plan mode. In plan mode, only read-only tools (read_file, list_files, grep, web_fetch, web_search), ask_user_question, and the task/agent tool are available. Write/Edit/Bash are blocked. Use write_file ONLY to write to the plan file. This is useful when you want to analyze the codebase and create a structured implementation plan before making changes.",
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
                "name": "exit_plan_mode",
                "description": "Exit plan mode and return to build mode. The plan file content will be shown to the user for approval. If approved, full tool access is restored and the plan is injected as context. If rejected, you stay in plan mode.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "allowed_prompts": {
                            "type": "array",
                            "description": "Optional list of allowed tool+prompt pairs that constrain what operations are permitted after plan approval",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "tool": {
                                        "type": "string",
                                        "description": "Tool name (e.g., 'write_file', 'bash')"
                                    },
                                    "prompt": {
                                        "type": "string",
                                        "description": "Description of the allowed operation"
                                    }
                                },
                                "required": ["tool", "prompt"]
                            }
                        }
                    },
                    "required": []
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "list_mcp_resources",
                "description": "List resources available from connected MCP servers. Resources are data sources (files, database tables, API endpoints) that MCP servers expose for reading.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "server": {
                            "type": "string",
                            "description": "Optional: filter resources to a specific MCP server by name. If omitted, lists resources from all connected servers."
                        }
                    },
                    "required": []
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "read_mcp_resource",
                "description": "Read the content of a specific resource from an MCP server. Use list_mcp_resources first to discover available resources and their URIs.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "server": {
                            "type": "string",
                            "description": "The name of the MCP server that provides the resource"
                        },
                        "uri": {
                            "type": "string",
                            "description": "The URI of the resource to read (as returned by list_mcp_resources)"
                        }
                    },
                    "required": ["server", "uri"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "lsp",
                "description": "Perform code intelligence operations via Language Server Protocol. Communicates with external language servers (rust-analyzer, typescript-language-server, pylsp, gopls, clangd, etc.) to provide goToDefinition, findReferences, hover, and documentSymbols. Automatically detects the appropriate language server based on file extension. Line numbers are 1-indexed.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "action": {
                            "type": "string",
                            "enum": ["goToDefinition", "findReferences", "hover", "documentSymbols"],
                            "description": "The LSP operation to perform"
                        },
                        "file_path": {
                            "type": "string",
                            "description": "Absolute path to the source file"
                        },
                        "line": {
                            "type": "integer",
                            "description": "1-indexed line number of the symbol (required for goToDefinition, findReferences, hover)"
                        },
                        "character": {
                            "type": "integer",
                            "description": "0-indexed character offset within the line (required for goToDefinition, findReferences, hover)"
                        }
                    },
                    "required": ["action", "file_path"]
                }
            }
        },
        // ====================================================================
        // Git Worktree Isolation Tools
        // ====================================================================
        {
            "type": "function",
            "function": {
                "name": "enter_worktree",
                "description": "Create an isolated git worktree and switch into it. This creates a new branch based on the current HEAD and a separate working directory under .worktrees/ so the agent can make changes without affecting the main working tree.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "branch": {
                            "type": "string",
                            "description": "The branch name to create for the worktree (e.g., 'agent/fix-bug-123')"
                        }
                    },
                    "required": ["branch"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "exit_worktree",
                "description": "Exit the current git worktree and return to the main working tree. Optionally commit and merge changes back, or discard them.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "apply_changes": {
                            "type": "boolean",
                            "description": "If true, commit any uncommitted changes and merge the worktree branch into the main branch. If false (default), discard the worktree."
                        }
                    },
                    "required": []
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "list_worktrees",
                "description": "List all active git worktrees in the current repository, showing their paths and branches.",
                "parameters": {
                    "type": "object",
                    "properties": {},
                    "required": []
                }
            }
        },
        // ====================================================================
        // Cron Scheduling Tools
        // ====================================================================
        {
            "type": "function",
            "function": {
                "name": "cron_create",
                "description": "Create a recurring scheduled task with a cron expression. Schedules are stored in .openclaudia/schedules.json and executed by loop mode or an external scheduler.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "name": {
                            "type": "string",
                            "description": "Unique name for the schedule (e.g., 'daily-cleanup')"
                        },
                        "schedule": {
                            "type": "string",
                            "description": "Standard 5-field cron expression: minute hour day month weekday (e.g., '0 9 * * 1-5' for weekdays at 9am)"
                        },
                        "prompt": {
                            "type": "string",
                            "description": "The prompt or command to execute on each trigger"
                        }
                    },
                    "required": ["name", "schedule", "prompt"]
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "cron_delete",
                "description": "Delete a scheduled task by its ID or name.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "id": {
                            "type": "string",
                            "description": "The schedule ID (8-character hex string)"
                        },
                        "name": {
                            "type": "string",
                            "description": "The schedule name (alternative to ID)"
                        }
                    },
                    "required": []
                }
            }
        },
        {
            "type": "function",
            "function": {
                "name": "cron_list",
                "description": "List all scheduled tasks with their status, cron expressions, and run history.",
                "parameters": {
                    "type": "object",
                    "properties": {},
                    "required": []
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

/// Execute a tool call (memory_db kept for API compatibility with execute_tool_full)
pub fn execute_tool_with_memory(tool_call: &ToolCall, _memory_db: Option<&MemoryDb>) -> ToolResult {
    let args: HashMap<String, Value> =
        serde_json::from_str(&tool_call.function.arguments).unwrap_or_default();

    let (content, is_error) = match tool_call.function.name.as_str() {
        // Standard tools
        "bash" => bash::execute_bash(&args),
        "bash_output" => bash::execute_bash_output(&args),
        "kill_shell" => bash::execute_kill_shell(&args),
        "read_file" => file::execute_read_file(&args),
        "write_file" => file::execute_write_file(&args),
        "edit_file" => file::execute_edit_file(&args),
        "notebook_edit" => file::execute_notebook_edit(&args),
        "list_files" => file::execute_list_files(&args),
        "chainlink" => chainlink::execute_chainlink(&args),

        // Web tools
        "web_fetch" => web::execute_web_fetch(&args),
        "web_search" => web::execute_web_search(&args),
        "web_browser" => web::execute_web_browser(&args),

        // LSP tools
        "lsp" => lsp::execute_lsp(&args),

        // Todo tools (fallback for chainlink)
        "todo_write" => todo::execute_todo_write(&args),
        "todo_read" => todo::execute_todo_read(),

        // User interaction tools
        "ask_user_question" => ask_user::execute_ask_user_question(&args),

        // Worktree tools
        "enter_worktree" => worktree::execute_enter_worktree(&args),
        "exit_worktree" => worktree::execute_exit_worktree(&args),
        "list_worktrees" => worktree::execute_list_worktrees(),

        // Cron scheduling tools
        "cron_create" => cron::execute_cron_create(&args),
        "cron_delete" => cron::execute_cron_delete(&args),
        "cron_list" => cron::execute_cron_list(&args),

        // Plan mode tools
        "enter_plan_mode" => plan_mode::execute_enter_plan_mode(),
        "exit_plan_mode" => plan_mode::execute_exit_plan_mode(&args),

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

/// Get all tool definitions, optionally including subagent tools
pub fn get_all_tool_definitions(subagents: bool) -> Value {
    let mut tools = get_tool_definitions();

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

/// Check if a tool result contains a special marker that needs main loop handling.
/// Returns the marker type if found, None otherwise.
pub fn check_tool_result_marker(content: &str) -> Option<String> {
    if let Ok(parsed) = serde_json::from_str::<Value>(content) {
        if let Some(marker_type) = parsed.get("type").and_then(|v| v.as_str()) {
            match marker_type {
                USER_QUESTION_MARKER | ENTER_PLAN_MODE_MARKER | EXIT_PLAN_MODE_MARKER => {
                    return Some(marker_type.to_string());
                }
                _ => {}
            }
        }
    }
    None
}

/// Parse user questions from a tool result with the user_question marker.
pub fn parse_user_questions(content: &str) -> Option<Vec<Value>> {
    let parsed: Value = serde_json::from_str(content).ok()?;
    parsed.get("questions").and_then(|v| v.as_array()).cloned()
}

/// Parse allowed prompts from an exit_plan_mode tool result.
pub fn parse_exit_plan_mode_prompts(content: &str) -> Vec<crate::session::AllowedPrompt> {
    let parsed: Value = match serde_json::from_str(content) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };

    parsed
        .get("allowed_prompts")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|item| {
                    let tool = item.get("tool")?.as_str()?.to_string();
                    let prompt = item.get("prompt")?.as_str()?.to_string();
                    Some(crate::session::AllowedPrompt { tool, prompt })
                })
                .collect()
        })
        .unwrap_or_default()
}

// =========================================================================
// Permission-Checked Tool Execution
// =========================================================================

/// Check permissions before executing a tool. Returns a ToolResult with an
/// error if permission is denied, or None if the tool should proceed.
pub fn check_tool_permission(
    tool_call: &ToolCall,
    permission_mgr: Option<&PermissionManager>,
) -> Option<ToolResult> {
    let mgr = match permission_mgr {
        Some(m) if m.is_enabled() => m,
        _ => return None, // No permission manager or disabled -- allow everything
    };

    let args: Value = serde_json::from_str(&tool_call.function.arguments).unwrap_or_default();

    match mgr.check(&tool_call.function.name, &args) {
        CheckResult::Allowed => None, // Proceed with execution
        CheckResult::Denied(reason) => Some(ToolResult {
            tool_call_id: tool_call.id.clone(),
            content: format!("Permission denied: {}", reason),
            is_error: true,
        }),
        CheckResult::NeedsPrompt { tool, target } => {
            // In the library layer, we can't prompt the user directly.
            // We return a structured message that the caller (main.rs / TUI)
            // can intercept to show a prompt.
            Some(ToolResult {
                tool_call_id: tool_call.id.clone(),
                content: format!(
                    "PERMISSION_PROMPT: Allow {} on '{}'? [y/n/a(lways)]",
                    tool, target
                ),
                is_error: true,
            })
        }
    }
}

/// Execute a tool call with task manager support.
///
/// This is the highest-level execution function that handles:
/// - Permission checking (via the caller using `check_tool_permission`)
/// - Task management tools (task_create, task_update, task_get, task_list)
/// - Subagent tools (via config)
/// - Memory tools (via memory_db)
/// - All standard tools
pub fn execute_tool_with_tasks(
    tool_call: &ToolCall,
    memory_db: Option<&MemoryDb>,
    app_config: Option<&AppConfig>,
    task_mgr: Option<&mut TaskManager>,
) -> ToolResult {
    let args: HashMap<String, Value> =
        serde_json::from_str(&tool_call.function.arguments).unwrap_or_default();

    // Handle task management tools
    match tool_call.function.name.as_str() {
        "task_create" => {
            if let Some(tm) = task_mgr {
                let (content, is_error) = task::execute_task_create(&args, tm);
                return ToolResult {
                    tool_call_id: tool_call.id.clone(),
                    content,
                    is_error,
                };
            }
            return ToolResult {
                tool_call_id: tool_call.id.clone(),
                content: "Task management not available (no session)".to_string(),
                is_error: true,
            };
        }
        "task_update" => {
            if let Some(tm) = task_mgr {
                let (content, is_error) = task::execute_task_update(&args, tm);
                return ToolResult {
                    tool_call_id: tool_call.id.clone(),
                    content,
                    is_error,
                };
            }
            return ToolResult {
                tool_call_id: tool_call.id.clone(),
                content: "Task management not available (no session)".to_string(),
                is_error: true,
            };
        }
        "task_get" => {
            if let Some(tm) = task_mgr {
                let (content, is_error) = task::execute_task_get(&args, tm);
                return ToolResult {
                    tool_call_id: tool_call.id.clone(),
                    content,
                    is_error,
                };
            }
            return ToolResult {
                tool_call_id: tool_call.id.clone(),
                content: "Task management not available (no session)".to_string(),
                is_error: true,
            };
        }
        "task_list" => {
            if let Some(tm) = task_mgr {
                let (content, is_error) = task::execute_task_list(tm);
                return ToolResult {
                    tool_call_id: tool_call.id.clone(),
                    content,
                    is_error,
                };
            }
            return ToolResult {
                tool_call_id: tool_call.id.clone(),
                content: "Task management not available (no session)".to_string(),
                is_error: true,
            };
        }
        _ => {}
    }

    // Fall through to existing execution path
    execute_tool_full(tool_call, memory_db, app_config)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::TaskManager;
    use base64::Engine;
    use file::{
        detect_file_type, parse_page_range, read_image_file, read_notebook_file,
        source_to_line_array, FileType, READ_TRACKER,
    };
    use std::fs;

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
            "ask_user_question",
            "enter_plan_mode",
            "exit_plan_mode",
            "task_create",
            "task_update",
            "task_get",
            "task_list",
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
        let (output, is_error) = bash::execute_bash(&args);
        assert!(!is_error);
        assert!(output.contains("hello"));
    }

    #[test]
    fn test_list_files() {
        let args = HashMap::new();
        let (output, is_error) = file::execute_list_files(&args);
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
        assert!(matches!(
            detect_file_type("photo.png"),
            FileType::Image("image/png")
        ));
        assert!(matches!(
            detect_file_type("photo.PNG"),
            FileType::Image("image/png")
        ));
        assert!(matches!(
            detect_file_type("photo.jpg"),
            FileType::Image("image/jpeg")
        ));
        assert!(matches!(
            detect_file_type("photo.jpeg"),
            FileType::Image("image/jpeg")
        ));
        assert!(matches!(
            detect_file_type("photo.JPEG"),
            FileType::Image("image/jpeg")
        ));
        assert!(matches!(
            detect_file_type("anim.gif"),
            FileType::Image("image/gif")
        ));
        assert!(matches!(
            detect_file_type("modern.webp"),
            FileType::Image("image/webp")
        ));
    }

    #[test]
    fn test_detect_file_type_pdf() {
        assert!(matches!(detect_file_type("document.pdf"), FileType::Pdf));
        assert!(matches!(detect_file_type("DOCUMENT.PDF"), FileType::Pdf));
    }

    #[test]
    fn test_detect_file_type_notebook() {
        assert!(matches!(
            detect_file_type("analysis.ipynb"),
            FileType::Notebook
        ));
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
        args.insert(
            "notebook_path".to_string(),
            json!(nb_path.to_str().unwrap()),
        );
        args.insert("cell_number".to_string(), json!(0));
        args.insert("new_source".to_string(), json!("new code\nline 2"));

        let (output, is_error) = file::execute_notebook_edit(&args);
        assert!(
            !is_error,
            "notebook_edit replace should succeed: {}",
            output
        );
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
        args.insert(
            "notebook_path".to_string(),
            json!(nb_path.to_str().unwrap()),
        );
        args.insert("cell_number".to_string(), json!(0));
        args.insert("new_source".to_string(), json!("# New markdown cell"));
        args.insert("cell_type".to_string(), json!("markdown"));
        args.insert("edit_mode".to_string(), json!("insert"));

        let (output, is_error) = file::execute_notebook_edit(&args);
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
        args.insert(
            "notebook_path".to_string(),
            json!(nb_path.to_str().unwrap()),
        );
        args.insert("cell_number".to_string(), json!(0));
        args.insert("new_source".to_string(), json!(""));
        args.insert("edit_mode".to_string(), json!("delete"));

        let (output, is_error) = file::execute_notebook_edit(&args);
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

        let (output, is_error) = file::execute_notebook_edit(&args);
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
        args.insert(
            "notebook_path".to_string(),
            json!(nb_path.to_str().unwrap()),
        );
        args.insert("cell_number".to_string(), json!(5));
        args.insert("new_source".to_string(), json!("test"));

        let (output, is_error) = file::execute_notebook_edit(&args);
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
        args.insert(
            "notebook_path".to_string(),
            json!(nb_path.to_str().unwrap()),
        );
        args.insert("cell_number".to_string(), json!(0));
        args.insert("new_source".to_string(), json!("test"));
        args.insert("edit_mode".to_string(), json!("insert"));
        // No cell_type provided

        let (output, is_error) = file::execute_notebook_edit(&args);
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
        args.insert(
            "notebook_path".to_string(),
            json!(nb_path.to_str().unwrap()),
        );
        args.insert("cell_number".to_string(), json!(0));
        args.insert("new_source".to_string(), json!("x = 1"));
        args.insert("cell_type".to_string(), json!("code"));
        args.insert("edit_mode".to_string(), json!("insert"));

        let (output, is_error) = file::execute_notebook_edit(&args);
        assert!(!is_error, "insert code cell should succeed: {}", output);

        let content = fs::read_to_string(&nb_path).unwrap();
        let updated: Value = serde_json::from_str(&content).unwrap();
        let cell = &updated["cells"][0];
        assert_eq!(cell["cell_type"], json!("code"));
        assert!(
            cell.get("outputs").is_some(),
            "Code cell should have outputs field"
        );
        assert!(cell["outputs"].as_array().unwrap().is_empty());
        assert!(
            cell.get("execution_count").is_some(),
            "Code cell should have execution_count"
        );
    }

    // ====================================================================
    // Task Management Tool Tests
    // ====================================================================

    #[test]
    fn test_task_create() {
        let mut task_mgr = TaskManager::new();
        let mut args = HashMap::new();
        args.insert("subject".to_string(), json!("Fix the bug"));
        args.insert(
            "description".to_string(),
            json!("There is a null pointer dereference in main"),
        );
        args.insert("active_form".to_string(), json!("Fixing the bug"));

        let (output, is_error) = task::execute_task_create(&args, &mut task_mgr);
        assert!(!is_error, "task_create should succeed: {}", output);
        assert!(output.contains("task-1"));
        assert!(output.contains("Fix the bug"));

        // Verify the task was stored
        let tasks = task_mgr.list_tasks();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].subject, "Fix the bug");
    }

    #[test]
    fn test_task_update_status() {
        let mut task_mgr = TaskManager::new();
        task_mgr.create_task("Task A".to_string(), "Desc A".to_string(), None);

        let mut args = HashMap::new();
        args.insert("task_id".to_string(), json!("task-1"));
        args.insert("status".to_string(), json!("in_progress"));

        let (output, is_error) = task::execute_task_update(&args, &mut task_mgr);
        assert!(!is_error, "task_update should succeed: {}", output);
        assert!(output.contains("in_progress"));
    }

    #[test]
    fn test_task_only_one_in_progress() {
        let mut task_mgr = TaskManager::new();
        task_mgr.create_task("Task A".to_string(), "Desc A".to_string(), None);
        task_mgr.create_task("Task B".to_string(), "Desc B".to_string(), None);

        // Set task-1 to in_progress
        let mut args = HashMap::new();
        args.insert("task_id".to_string(), json!("task-1"));
        args.insert("status".to_string(), json!("in_progress"));
        task::execute_task_update(&args, &mut task_mgr);

        // Set task-2 to in_progress -- task-1 should be demoted to pending
        args.insert("task_id".to_string(), json!("task-2"));
        task::execute_task_update(&args, &mut task_mgr);

        let task1 = task_mgr.get_task("task-1").unwrap();
        let task2 = task_mgr.get_task("task-2").unwrap();
        assert_eq!(task1.status, crate::session::TaskStatus::Pending);
        assert_eq!(task2.status, crate::session::TaskStatus::InProgress);
    }

    #[test]
    fn test_task_list_empty() {
        let task_mgr = TaskManager::new();
        let (output, is_error) = task::execute_task_list(&task_mgr);
        assert!(!is_error);
        assert_eq!(output, "No tasks.");
    }

    #[test]
    fn test_task_get_not_found() {
        let task_mgr = TaskManager::new();
        let mut args = HashMap::new();
        args.insert("task_id".to_string(), json!("task-999"));
        let (output, is_error) = task::execute_task_get(&args, &task_mgr);
        assert!(is_error);
        assert!(output.contains("not found"));
    }

    #[test]
    fn test_task_delete() {
        let mut task_mgr = TaskManager::new();
        task_mgr.create_task("Task to delete".to_string(), "Desc".to_string(), None);

        let mut args = HashMap::new();
        args.insert("task_id".to_string(), json!("task-1"));
        args.insert("status".to_string(), json!("deleted"));
        let (output, is_error) = task::execute_task_update(&args, &mut task_mgr);
        assert!(!is_error, "delete should not be an error: {}", output);
        assert!(output.contains("deleted"));
        assert!(task_mgr.list_tasks().is_empty());
    }

    #[test]
    fn test_task_dependencies() {
        let mut task_mgr = TaskManager::new();
        task_mgr.create_task("Setup DB".to_string(), "Create schema".to_string(), None);
        task_mgr.create_task("Add API".to_string(), "REST endpoints".to_string(), None);

        // task-2 is blocked by task-1
        let mut args = HashMap::new();
        args.insert("task_id".to_string(), json!("task-2"));
        args.insert("add_blocked_by".to_string(), json!(["task-1"]));
        let (_, is_error) = task::execute_task_update(&args, &mut task_mgr);
        assert!(!is_error);

        let task1 = task_mgr.get_task("task-1").unwrap();
        let task2 = task_mgr.get_task("task-2").unwrap();
        // task-2 should have task-1 in blocked_by
        assert!(task2.blocked_by.contains(&"task-1".to_string()));
        // task-1 should have task-2 in blocks (reverse relationship)
        assert!(task1.blocks.contains(&"task-2".to_string()));
    }

    // ====================================================================
    // Permission Checking Tests
    // ====================================================================

    #[test]
    fn test_check_tool_permission_none_manager() {
        let tool_call = ToolCall {
            id: "call_1".to_string(),
            call_type: "function".to_string(),
            function: FunctionCall {
                name: "bash".to_string(),
                arguments: r#"{"command": "ls"}"#.to_string(),
            },
        };
        // No permission manager -- should return None (allow)
        assert!(check_tool_permission(&tool_call, None).is_none());
    }
}
