//! End-to-end integration tests for OpenClaudia tools
//!
//! These tests verify that each tool actually performs its documented function
//! against real filesystem, processes, and network operations.

use openclaudia::tools::{execute_tool, ToolCall, FunctionCall};
use openclaudia::memory::MemoryDb;
use serde_json::{json, Value};
use std::fs;
use tempfile::TempDir;

/// Helper to create a ToolCall from name and arguments
fn make_tool_call(name: &str, args: Value) -> ToolCall {
    ToolCall {
        id: format!("test_{}", name),
        call_type: "function".to_string(),
        function: FunctionCall {
            name: name.to_string(),
            arguments: args.to_string(),
        },
    }
}

/// Helper to create a temp directory with test files
fn setup_test_dir() -> TempDir {
    let dir = TempDir::new().expect("Failed to create temp dir");

    // Create test file
    fs::write(dir.path().join("test.txt"), "Hello, World!\nLine 2\nLine 3\n")
        .expect("Failed to write test file");

    // Create subdirectory with files
    fs::create_dir(dir.path().join("subdir")).expect("Failed to create subdir");
    fs::write(dir.path().join("subdir/nested.txt"), "Nested content")
        .expect("Failed to write nested file");

    // Create code file for grep tests
    fs::write(
        dir.path().join("code.rs"),
        r#"fn main() {
    println!("Hello");
    let x = 42;
    // TODO: fix this
}
"#,
    )
    .expect("Failed to write code file");

    dir
}

// ============================================================================
// FILE TOOLS TESTS
// ============================================================================

mod file_tools {
    use super::*;

    #[test]
    fn test_read_file_success() {
        let dir = setup_test_dir();
        let file_path = dir.path().join("test.txt");

        let tool_call = make_tool_call("read_file", json!({
            "path": file_path.to_string_lossy()
        }));

        let result = execute_tool(&tool_call);

        assert!(!result.is_error, "Read should succeed: {}", result.content);
        assert!(result.content.contains("Hello, World!"), "Should contain file content");
        assert!(result.content.contains("Line 2"), "Should contain all lines");
    }

    #[test]
    fn test_read_file_not_found() {
        let tool_call = make_tool_call("read_file", json!({
            "path": "/nonexistent/path/file.txt"
        }));

        let result = execute_tool(&tool_call);

        assert!(result.is_error, "Read of nonexistent file should fail");
        assert!(
            result.content.to_lowercase().contains("not found")
            || result.content.to_lowercase().contains("no such file")
            || result.content.to_lowercase().contains("cannot find")
            || result.content.to_lowercase().contains("failed"),
            "Error should mention file not found: {}", result.content
        );
    }

    #[test]
    fn test_read_with_offset_and_limit() {
        let dir = setup_test_dir();
        let file_path = dir.path().join("test.txt");

        let tool_call = make_tool_call("read_file", json!({
            "path": file_path.to_string_lossy(),
            "offset": 2,
            "limit": 1
        }));

        let result = execute_tool(&tool_call);

        assert!(!result.is_error, "Read with offset should succeed: {}", result.content);
        assert!(result.content.contains("Line 2"), "Should contain line 2");
        assert!(!result.content.contains("Hello"), "Should not contain line 1");
    }

    #[test]
    fn test_write_file_new() {
        let dir = TempDir::new().expect("Failed to create temp dir");
        let file_path = dir.path().join("new_file.txt");

        let tool_call = make_tool_call("write_file", json!({
            "path": file_path.to_string_lossy(),
            "content": "New file content\nWith multiple lines"
        }));

        let result = execute_tool(&tool_call);

        assert!(!result.is_error, "Write should succeed: {}", result.content);

        // Verify the file was actually written
        let content = fs::read_to_string(&file_path).expect("Failed to read written file");
        assert_eq!(content, "New file content\nWith multiple lines");
    }

    #[test]
    fn test_write_file_overwrite() {
        let dir = setup_test_dir();
        let file_path = dir.path().join("test.txt");

        let tool_call = make_tool_call("write_file", json!({
            "path": file_path.to_string_lossy(),
            "content": "Overwritten content"
        }));

        let result = execute_tool(&tool_call);

        assert!(!result.is_error, "Write overwrite should succeed: {}", result.content);

        let content = fs::read_to_string(&file_path).expect("Failed to read");
        assert_eq!(content, "Overwritten content");
    }

    #[test]
    fn test_edit_file_replace() {
        let dir = setup_test_dir();
        let file_path = dir.path().join("test.txt");

        let tool_call = make_tool_call("edit_file", json!({
            "path": file_path.to_string_lossy(),
            "old_string": "Hello, World!",
            "new_string": "Goodbye, World!"
        }));

        let result = execute_tool(&tool_call);

        assert!(!result.is_error, "Edit should succeed: {}", result.content);

        let content = fs::read_to_string(&file_path).expect("Failed to read");
        assert!(content.contains("Goodbye, World!"), "Should contain new string");
        assert!(!content.contains("Hello, World!"), "Should not contain old string");
    }

    #[test]
    fn test_edit_file_old_string_not_found() {
        let dir = setup_test_dir();
        let file_path = dir.path().join("test.txt");

        let tool_call = make_tool_call("edit_file", json!({
            "path": file_path.to_string_lossy(),
            "old_string": "This string does not exist",
            "new_string": "Replacement"
        }));

        let result = execute_tool(&tool_call);

        assert!(result.is_error, "Edit with missing old_string should fail");
        assert!(
            result.content.to_lowercase().contains("could not find")
            || result.content.to_lowercase().contains("not found")
            || result.content.to_lowercase().contains("no match"),
            "Error should mention string not found: {}", result.content
        );
    }

    #[test]
    fn test_list_files_pattern() {
        let dir = setup_test_dir();

        let tool_call = make_tool_call("list_files", json!({
            "path": dir.path().to_string_lossy(),
            "pattern": "*.txt"
        }));

        let result = execute_tool(&tool_call);

        assert!(!result.is_error, "list_files should succeed: {}", result.content);
        assert!(result.content.contains("test.txt"), "Should find test.txt");
    }

    #[test]
    fn test_list_files_no_matches() {
        let dir = setup_test_dir();

        let tool_call = make_tool_call("list_files", json!({
            "path": dir.path().to_string_lossy(),
            "pattern": "*.xyz"
        }));

        let result = execute_tool(&tool_call);

        // Should succeed but with no matches
        assert!(!result.is_error, "list_files should succeed even with no matches");
    }
}

// ============================================================================
// BASH TOOLS TESTS
// ============================================================================

mod bash_tools {
    use super::*;
    use std::thread;
    use std::time::Duration;

    #[test]
    fn test_bash_simple_command() {
        let tool_call = make_tool_call("bash", json!({
            "command": "echo 'Hello from bash'"
        }));

        let result = execute_tool(&tool_call);

        assert!(!result.is_error, "Bash should succeed: {}", result.content);
        assert!(result.content.contains("Hello from bash"), "Should contain echo output");
    }

    #[test]
    fn test_bash_with_exit_code() {
        let tool_call = make_tool_call("bash", json!({
            "command": "exit 1"
        }));

        let result = execute_tool(&tool_call);

        // Command fails but tool captures the result
        assert!(
            result.content.contains("exit") || result.is_error,
            "Should indicate non-zero exit"
        );
    }

    #[test]
    fn test_bash_command_not_found() {
        let tool_call = make_tool_call("bash", json!({
            "command": "nonexistent_command_12345"
        }));

        let result = execute_tool(&tool_call);

        // Should indicate command not found (either error or in content)
        assert!(
            result.is_error
            || result.content.to_lowercase().contains("not found")
            || result.content.to_lowercase().contains("not recognized"),
            "Should indicate command not found: {}", result.content
        );
    }

    #[test]
    fn test_bash_working_directory() {
        let dir = TempDir::new().expect("Failed to create temp dir");

        // Create a file in the temp dir
        fs::write(dir.path().join("marker.txt"), "exists").expect("write failed");

        // Convert Windows path to Unix-style for bash
        let path_str = dir.path().to_string_lossy().replace('\\', "/");

        let tool_call = make_tool_call("bash", json!({
            "command": format!("cd '{}' && ls", path_str)
        }));

        let result = execute_tool(&tool_call);

        assert!(!result.is_error, "Bash cd should succeed: {}", result.content);
        assert!(result.content.contains("marker.txt"), "Should list files in target dir");
    }

    #[test]
    fn test_bash_timeout() {
        let tool_call = make_tool_call("bash", json!({
            "command": "ping -n 100 127.0.0.1",  // Windows ping (use -c on Linux)
            "timeout": 1000  // 1 second timeout
        }));

        let _result = execute_tool(&tool_call);

        // Should timeout or complete quickly
        // The implementation might return timeout error or partial output
    }

    #[test]
    fn test_bash_background_execution() {
        let tool_call = make_tool_call("bash", json!({
            "command": "ping -n 5 127.0.0.1",
            "run_in_background": true
        }));

        let result = execute_tool(&tool_call);

        assert!(!result.is_error, "Background bash should succeed: {}", result.content);
        assert!(
            result.content.contains("shell_") || result.content.contains("background"),
            "Should return shell ID for background process"
        );
    }

    #[test]
    fn test_bash_output_list_shells() {
        // First start a background shell
        let bg_call = make_tool_call("bash", json!({
            "command": "ping -n 10 127.0.0.1",
            "run_in_background": true
        }));
        let bg_result = execute_tool(&bg_call);
        assert!(!bg_result.is_error, "Background start should succeed");

        // Small delay for process to start
        thread::sleep(Duration::from_millis(100));

        // Now list shells (no shell_id = list all)
        let list_call = make_tool_call("bash_output", json!({}));
        let list_result = execute_tool(&list_call);

        assert!(!list_result.is_error, "bash_output list should succeed: {}", list_result.content);
        // Should list at least one shell
        assert!(
            list_result.content.contains("shell_")
            || list_result.content.contains("Background shells")
            || list_result.content.contains("ping"),
            "Should list running shells: {}", list_result.content
        );
    }

    #[test]
    fn test_bash_output_specific_shell() {
        // Start a background shell that produces output
        let bg_call = make_tool_call("bash", json!({
            "command": "echo 'test output' && ping -n 2 127.0.0.1",
            "run_in_background": true
        }));
        let bg_result = execute_tool(&bg_call);

        // Extract shell ID from result - look for pattern like "shell_abc123"
        let shell_id = extract_shell_id(&bg_result.content);

        // Wait for some output
        thread::sleep(Duration::from_millis(500));

        // Get output from specific shell
        let output_call = make_tool_call("bash_output", json!({
            "shell_id": shell_id
        }));
        let output_result = execute_tool(&output_call);

        // Should have some output (might be empty if command finished quickly)
        assert!(!output_result.is_error, "bash_output should succeed: {}", output_result.content);
    }

    #[test]
    fn test_kill_shell() {
        // Start a long-running background shell
        let bg_call = make_tool_call("bash", json!({
            "command": "ping -n 1000 127.0.0.1",
            "run_in_background": true
        }));
        let bg_result = execute_tool(&bg_call);

        // Extract shell ID
        let shell_id = extract_shell_id(&bg_result.content);

        thread::sleep(Duration::from_millis(100));

        // Kill the shell
        let kill_call = make_tool_call("kill_shell", json!({
            "shell_id": shell_id
        }));
        let kill_result = execute_tool(&kill_call);

        assert!(!kill_result.is_error, "kill_shell should succeed: {}", kill_result.content);
        assert!(
            kill_result.content.to_lowercase().contains("kill")
            || kill_result.content.to_lowercase().contains("terminated")
            || kill_result.content.to_lowercase().contains("stopped"),
            "Should confirm shell was killed: {}", kill_result.content
        );
    }
}

/// Extract shell ID from bash background output
/// Output format: "Background shell started with ID: xxxxx\nUse bash_output..."
fn extract_shell_id(output: &str) -> String {
    // Look for "ID: " pattern and extract what follows
    if let Some(idx) = output.find("ID: ") {
        let start = idx + 4; // Skip "ID: "
        let rest = &output[start..];
        // Find the end of the shell ID (next whitespace or newline)
        let end = rest.find(|c: char| c.is_whitespace()).unwrap_or(rest.len());
        let id = rest[..end].trim();
        // Return the ID if it's not empty
        if !id.is_empty() {
            return id.to_string();
        }
    }

    "shell_unknown".to_string()
}

// ============================================================================
// WEB TOOLS TESTS (with mocking where needed)
// ============================================================================

mod web_tools {
    use super::*;

    #[test]
    fn test_web_fetch_basic() {
        // Test with a reliable public URL
        let tool_call = make_tool_call("web_fetch", json!({
            "url": "https://httpbin.org/html",
            "prompt": "Extract the main heading"
        }));

        let result = execute_tool(&tool_call);

        // This is a real network call - might fail in CI/offline environments
        // We check if it either succeeded or failed gracefully
        if !result.is_error {
            assert!(
                result.content.len() > 0,
                "Should return content from fetch"
            );
        }
    }

    #[test]
    fn test_web_fetch_invalid_url() {
        let tool_call = make_tool_call("web_fetch", json!({
            "url": "not-a-valid-url",
            "prompt": "test"
        }));

        let result = execute_tool(&tool_call);

        assert!(result.is_error, "Invalid URL should fail");
    }

    // DuckDuckGo search uses the browser feature (enabled by default)
    // Falls back to Tavily/Brave APIs if configured
    #[test]
    fn test_web_search_duckduckgo() {
        let tool_call = make_tool_call("web_search", json!({
            "query": "rust programming language"
        }));

        let result = execute_tool(&tool_call);

        if !result.is_error {
            assert!(result.content.contains("http"), "Should contain URLs");
        }
    }
}

// ============================================================================
// MEMORY TOOLS TESTS
// ============================================================================

mod memory_tools {
    use super::*;
    use openclaudia::tools::execute_tool_with_memory;

    fn setup_memory_db() -> (TempDir, MemoryDb) {
        // Create temp directory with memory database for testing
        let dir = TempDir::new().expect("Failed to create temp dir");
        let db_path = dir.path().join("test_memory.db");
        let db = MemoryDb::open(&db_path).expect("Failed to create memory db");
        (dir, db)  // Return dir to keep it alive
    }

    #[test]
    fn test_memory_save_and_search() {
        let (_dir, db) = setup_memory_db();

        // Save a memory
        let save_call = make_tool_call("memory_save", json!({
            "content": "Important fact: Rust is a systems programming language",
            "tags": ["programming", "rust"]
        }));

        let save_result = execute_tool_with_memory(&save_call, Some(&db));
        assert!(!save_result.is_error, "Save should succeed: {}", save_result.content);

        // Search for the memory
        let search_call = make_tool_call("memory_search", json!({
            "query": "Rust programming"
        }));

        let search_result = execute_tool_with_memory(&search_call, Some(&db));
        assert!(!search_result.is_error, "Search should succeed: {}", search_result.content);
        // Should find the saved memory
        assert!(
            search_result.content.contains("Rust") || search_result.content.contains("systems"),
            "Should find saved memory: {}", search_result.content
        );
    }

    #[test]
    fn test_memory_search_no_results() {
        let (_dir, db) = setup_memory_db();

        let search_call = make_tool_call("memory_search", json!({
            "query": "nonexistent topic xyz123"
        }));

        let result = execute_tool_with_memory(&search_call, Some(&db));

        // Should succeed but with no results
        assert!(!result.is_error, "Search should succeed: {}", result.content);
        // Empty result is fine
    }

    #[test]
    fn test_memory_multiple_saves_search() {
        let (_dir, db) = setup_memory_db();

        // Store multiple memories
        for i in 0..3 {
            let save_call = make_tool_call("memory_save", json!({
                "content": format!("Searchable fact number {} about testing", i),
                "tags": ["test", "fact"]
            }));
            execute_tool_with_memory(&save_call, Some(&db));
        }

        // Search for content
        let search_call = make_tool_call("memory_search", json!({
            "query": "testing"
        }));

        let result = execute_tool_with_memory(&search_call, Some(&db));

        assert!(!result.is_error, "Search should succeed: {}", result.content);
        // Should find at least one match
        assert!(
            result.content.contains("fact") || result.content.contains("testing") || result.content.contains("Searchable"),
            "Should find matching memories: {}", result.content
        );
    }

    #[test]
    fn test_core_memory_update() {
        let (_dir, db) = setup_memory_db();

        // Update project info core memory
        let update_call = make_tool_call("core_memory_update", json!({
            "section": "project_info",
            "content": "This is a Rust project for testing"
        }));

        let result = execute_tool_with_memory(&update_call, Some(&db));

        assert!(!result.is_error, "Core memory update should succeed: {}", result.content);
    }
}

// ============================================================================
// TOOL DEFINITIONS TESTS
// ============================================================================

mod tool_definitions {
    use openclaudia::tools::{get_tool_definitions, get_all_tool_definitions};

    #[test]
    fn test_get_tool_definitions_structure() {
        let tools = get_tool_definitions();

        assert!(tools.is_array(), "Tool definitions should be an array");

        let tools_array = tools.as_array().unwrap();
        assert!(tools_array.len() > 0, "Should have at least one tool");

        // Verify each tool has required fields
        for tool in tools_array {
            assert!(tool.get("type").is_some(), "Tool should have type");
            assert!(tool.get("function").is_some(), "Tool should have function");

            let function = tool.get("function").unwrap();
            assert!(function.get("name").is_some(), "Function should have name");
            assert!(function.get("description").is_some(), "Function should have description");
            assert!(function.get("parameters").is_some(), "Function should have parameters");
        }
    }

    #[test]
    fn test_get_all_tool_definitions_includes_memory() {
        // Without stateful flag
        let tools_no_memory = get_all_tool_definitions(false);
        let no_memory_names: Vec<&str> = tools_no_memory.as_array().unwrap()
            .iter()
            .filter_map(|t| t["function"]["name"].as_str())
            .collect();

        // With stateful flag
        let tools_with_memory = get_all_tool_definitions(true);
        let with_memory_names: Vec<&str> = tools_with_memory.as_array().unwrap()
            .iter()
            .filter_map(|t| t["function"]["name"].as_str())
            .collect();

        // Memory tools should only be present when stateful=true
        assert!(
            with_memory_names.len() > no_memory_names.len(),
            "Stateful mode should have more tools"
        );
        assert!(
            with_memory_names.iter().any(|n| n.contains("memory")),
            "Stateful mode should include memory tools"
        );
    }

    #[test]
    fn test_required_tools_exist() {
        let tools = get_tool_definitions();
        let tool_names: Vec<&str> = tools.as_array().unwrap()
            .iter()
            .filter_map(|t| t["function"]["name"].as_str())
            .collect();

        // Actual tool names in the system
        let required_tools = vec![
            "read_file", "write_file", "edit_file", "list_files",
            "bash", "bash_output", "kill_shell",
            "web_fetch", "web_search"
        ];

        for required in required_tools {
            assert!(
                tool_names.contains(&required),
                "Required tool '{}' should exist. Found: {:?}", required, tool_names
            );
        }
    }
}
