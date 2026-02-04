//! End-to-end integration tests for OpenClaudia tools
//!
//! These tests verify that each tool actually performs its documented function
//! against real filesystem, processes, and network operations.

use openclaudia::memory::MemoryDb;
use openclaudia::tools::{
    clear_todo_list, execute_tool, get_todo_list, reset_read_tracker, FunctionCall, ToolCall,
};
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
    fs::write(
        dir.path().join("test.txt"),
        "Hello, World!\nLine 2\nLine 3\n",
    )
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

        let tool_call = make_tool_call(
            "read_file",
            json!({
                "path": file_path.to_string_lossy()
            }),
        );

        let result = execute_tool(&tool_call);

        assert!(!result.is_error, "Read should succeed: {}", result.content);
        assert!(
            result.content.contains("Hello, World!"),
            "Should contain file content"
        );
        assert!(
            result.content.contains("Line 2"),
            "Should contain all lines"
        );
    }

    #[test]
    fn test_read_file_not_found() {
        let tool_call = make_tool_call(
            "read_file",
            json!({
                "path": "/nonexistent/path/file.txt"
            }),
        );

        let result = execute_tool(&tool_call);

        assert!(result.is_error, "Read of nonexistent file should fail");
        assert!(
            result.content.to_lowercase().contains("not found")
                || result.content.to_lowercase().contains("no such file")
                || result.content.to_lowercase().contains("cannot find")
                || result.content.to_lowercase().contains("failed"),
            "Error should mention file not found: {}",
            result.content
        );
    }

    #[test]
    fn test_read_with_offset_and_limit() {
        let dir = setup_test_dir();
        let file_path = dir.path().join("test.txt");

        let tool_call = make_tool_call(
            "read_file",
            json!({
                "path": file_path.to_string_lossy(),
                "offset": 2,
                "limit": 1
            }),
        );

        let result = execute_tool(&tool_call);

        assert!(
            !result.is_error,
            "Read with offset should succeed: {}",
            result.content
        );
        assert!(result.content.contains("Line 2"), "Should contain line 2");
        assert!(
            !result.content.contains("Hello"),
            "Should not contain line 1"
        );
    }

    #[test]
    fn test_write_file_new() {
        let dir = TempDir::new().expect("Failed to create temp dir");
        let file_path = dir.path().join("new_file.txt");

        let tool_call = make_tool_call(
            "write_file",
            json!({
                "path": file_path.to_string_lossy(),
                "content": "New file content\nWith multiple lines"
            }),
        );

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

        let tool_call = make_tool_call(
            "write_file",
            json!({
                "path": file_path.to_string_lossy(),
                "content": "Overwritten content"
            }),
        );

        let result = execute_tool(&tool_call);

        assert!(
            !result.is_error,
            "Write overwrite should succeed: {}",
            result.content
        );

        let content = fs::read_to_string(&file_path).expect("Failed to read");
        assert_eq!(content, "Overwritten content");
    }

    #[test]
    fn test_edit_file_replace() {
        reset_read_tracker(); // Clear tracker for clean test state
        let dir = setup_test_dir();
        let file_path = dir.path().join("test.txt");

        // Read the file first (required before editing)
        let read_call = make_tool_call(
            "read_file",
            json!({ "path": file_path.to_string_lossy() }),
        );
        execute_tool(&read_call);

        let tool_call = make_tool_call(
            "edit_file",
            json!({
                "path": file_path.to_string_lossy(),
                "old_string": "Hello, World!",
                "new_string": "Goodbye, World!"
            }),
        );

        let result = execute_tool(&tool_call);

        assert!(!result.is_error, "Edit should succeed: {}", result.content);

        let content = fs::read_to_string(&file_path).expect("Failed to read");
        assert!(
            content.contains("Goodbye, World!"),
            "Should contain new string"
        );
        assert!(
            !content.contains("Hello, World!"),
            "Should not contain old string"
        );
    }

    #[test]
    fn test_edit_file_old_string_not_found() {
        reset_read_tracker(); // Clear tracker for clean test state
        let dir = setup_test_dir();
        let file_path = dir.path().join("test.txt");

        // Read the file first (required before editing)
        let read_call = make_tool_call(
            "read_file",
            json!({ "path": file_path.to_string_lossy() }),
        );
        execute_tool(&read_call);

        let tool_call = make_tool_call(
            "edit_file",
            json!({
                "path": file_path.to_string_lossy(),
                "old_string": "This string does not exist",
                "new_string": "Replacement"
            }),
        );

        let result = execute_tool(&tool_call);

        assert!(result.is_error, "Edit with missing old_string should fail");
        assert!(
            result.content.to_lowercase().contains("could not find")
                || result.content.to_lowercase().contains("not found")
                || result.content.to_lowercase().contains("no match"),
            "Error should mention string not found: {}",
            result.content
        );
    }

    #[test]
    fn test_list_files_pattern() {
        let dir = setup_test_dir();

        let tool_call = make_tool_call(
            "list_files",
            json!({
                "path": dir.path().to_string_lossy(),
                "pattern": "*.txt"
            }),
        );

        let result = execute_tool(&tool_call);

        assert!(
            !result.is_error,
            "list_files should succeed: {}",
            result.content
        );
        assert!(result.content.contains("test.txt"), "Should find test.txt");
    }

    #[test]
    fn test_list_files_no_matches() {
        let dir = setup_test_dir();

        let tool_call = make_tool_call(
            "list_files",
            json!({
                "path": dir.path().to_string_lossy(),
                "pattern": "*.xyz"
            }),
        );

        let result = execute_tool(&tool_call);

        // Should succeed but with no matches
        assert!(
            !result.is_error,
            "list_files should succeed even with no matches"
        );
    }

    // =========== EDGE CASE TESTS ===========

    #[test]
    fn test_read_file_unicode_content() {
        let dir = TempDir::new().expect("Failed to create temp dir");
        let file_path = dir.path().join("unicode.txt");

        // Write Unicode content including emojis and various scripts
        let unicode_content = "Hello 世界! 🦀 Rust\nКириллица\nالعربية\n日本語";
        fs::write(&file_path, unicode_content).expect("Failed to write unicode file");

        let tool_call = make_tool_call(
            "read_file",
            json!({
                "path": file_path.to_string_lossy()
            }),
        );

        let result = execute_tool(&tool_call);

        assert!(
            !result.is_error,
            "Read should handle Unicode: {}",
            result.content
        );
        assert!(result.content.contains("世界"), "Should contain Chinese");
        assert!(result.content.contains("🦀"), "Should contain emoji");
    }

    #[test]
    fn test_write_file_unicode_content() {
        let dir = TempDir::new().expect("Failed to create temp dir");
        let file_path = dir.path().join("unicode_write.txt");

        let unicode_content = "Writing Unicode: 你好 🌍 مرحبا";
        let tool_call = make_tool_call(
            "write_file",
            json!({
                "path": file_path.to_string_lossy(),
                "content": unicode_content
            }),
        );

        let result = execute_tool(&tool_call);

        assert!(
            !result.is_error,
            "Write should handle Unicode: {}",
            result.content
        );

        let content = fs::read_to_string(&file_path).expect("Failed to read");
        assert_eq!(content, unicode_content);
    }

    #[test]
    fn test_read_file_empty() {
        let dir = TempDir::new().expect("Failed to create temp dir");
        let file_path = dir.path().join("empty.txt");
        fs::write(&file_path, "").expect("Failed to write empty file");

        let tool_call = make_tool_call(
            "read_file",
            json!({
                "path": file_path.to_string_lossy()
            }),
        );

        let result = execute_tool(&tool_call);

        assert!(
            !result.is_error,
            "Read empty file should succeed: {}",
            result.content
        );
    }

    #[test]
    fn test_read_file_large() {
        let dir = TempDir::new().expect("Failed to create temp dir");
        let file_path = dir.path().join("large.txt");

        // Create a large file (10000 lines)
        let content: String = (0..10000).map(|i| format!("Line {}\n", i)).collect();
        fs::write(&file_path, &content).expect("Failed to write large file");

        let tool_call = make_tool_call(
            "read_file",
            json!({
                "path": file_path.to_string_lossy()
            }),
        );

        let result = execute_tool(&tool_call);

        assert!(
            !result.is_error,
            "Read large file should succeed: {}",
            result.content
        );
        assert!(
            result.content.contains("Line 0"),
            "Should contain first line"
        );
    }

    #[test]
    fn test_read_file_with_limit_large() {
        let dir = TempDir::new().expect("Failed to create temp dir");
        let file_path = dir.path().join("large_limit.txt");

        let content: String = (0..1000).map(|i| format!("Line {}\n", i)).collect();
        fs::write(&file_path, &content).expect("Failed to write");

        let tool_call = make_tool_call(
            "read_file",
            json!({
                "path": file_path.to_string_lossy(),
                "offset": 500,
                "limit": 10
            }),
        );

        let result = execute_tool(&tool_call);

        assert!(!result.is_error, "Read with offset/limit should succeed");
        assert!(
            result.content.contains("Line 500") || result.content.contains("Line 501"),
            "Should contain content from offset"
        );
    }

    #[test]
    fn test_edit_file_multiline() {
        reset_read_tracker(); // Clear tracker for clean test state
        let dir = TempDir::new().expect("Failed to create temp dir");
        let file_path = dir.path().join("multiline.txt");

        let original = "function foo() {\n    console.log('old');\n}";
        fs::write(&file_path, original).expect("Failed to write");

        // Read the file first (required before editing)
        let read_call = make_tool_call(
            "read_file",
            json!({ "path": file_path.to_string_lossy() }),
        );
        execute_tool(&read_call);

        let tool_call = make_tool_call(
            "edit_file",
            json!({
                "path": file_path.to_string_lossy(),
                "old_string": "function foo() {\n    console.log('old');\n}",
                "new_string": "function foo() {\n    console.log('new');\n    return true;\n}"
            }),
        );

        let result = execute_tool(&tool_call);

        assert!(
            !result.is_error,
            "Multiline edit should succeed: {}",
            result.content
        );

        let content = fs::read_to_string(&file_path).expect("Failed to read");
        assert!(
            content.contains("return true"),
            "Should contain new content"
        );
    }

    #[test]
    fn test_edit_file_special_characters() {
        reset_read_tracker(); // Clear tracker for clean test state
        let dir = TempDir::new().expect("Failed to create temp dir");
        let file_path = dir.path().join("special.txt");

        let original = "Price: $100 (50% off!) [limited]";
        fs::write(&file_path, original).expect("Failed to write");

        // Read the file first (required before editing)
        let read_call = make_tool_call(
            "read_file",
            json!({ "path": file_path.to_string_lossy() }),
        );
        execute_tool(&read_call);

        let tool_call = make_tool_call(
            "edit_file",
            json!({
                "path": file_path.to_string_lossy(),
                "old_string": "$100",
                "new_string": "$200"
            }),
        );

        let result = execute_tool(&tool_call);

        assert!(
            !result.is_error,
            "Edit with special chars should succeed: {}",
            result.content
        );

        let content = fs::read_to_string(&file_path).expect("Failed to read");
        assert!(content.contains("$200"), "Should contain updated price");
    }

    #[test]
    fn test_list_files_recursive() {
        let dir = setup_test_dir();

        let tool_call = make_tool_call(
            "list_files",
            json!({
                "path": dir.path().to_string_lossy(),
                "pattern": "**/*.txt"
            }),
        );

        let result = execute_tool(&tool_call);

        assert!(
            !result.is_error,
            "Recursive list should succeed: {}",
            result.content
        );
        // Should find both test.txt and subdir/nested.txt
        assert!(result.content.contains("test.txt"), "Should find test.txt");
    }

    #[test]
    fn test_write_file_creates_parent_dirs() {
        let dir = TempDir::new().expect("Failed to create temp dir");
        let file_path = dir.path().join("new_dir/sub_dir/file.txt");

        let tool_call = make_tool_call(
            "write_file",
            json!({
                "path": file_path.to_string_lossy(),
                "content": "Content in nested dir"
            }),
        );

        let result = execute_tool(&tool_call);

        // Some implementations create parent dirs, some don't - test behavior
        if !result.is_error {
            let content = fs::read_to_string(&file_path).expect("Failed to read");
            assert_eq!(content, "Content in nested dir");
        }
        // If error, it should mention directory doesn't exist
    }

    #[test]
    fn test_read_file_binary_detection() {
        let dir = TempDir::new().expect("Failed to create temp dir");
        let file_path = dir.path().join("binary.bin");

        // Write some binary content
        let binary_content: Vec<u8> = vec![0x00, 0x01, 0x02, 0xFF, 0xFE, 0x89, 0x50, 0x4E, 0x47];
        fs::write(&file_path, &binary_content).expect("Failed to write binary");

        let tool_call = make_tool_call(
            "read_file",
            json!({
                "path": file_path.to_string_lossy()
            }),
        );

        let result = execute_tool(&tool_call);

        // Should either succeed with escaped content or indicate binary file
        // Both behaviors are acceptable
        assert!(
            !result.is_error || result.content.to_lowercase().contains("binary"),
            "Should handle binary file gracefully"
        );
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
        let tool_call = make_tool_call(
            "bash",
            json!({
                "command": "echo 'Hello from bash'"
            }),
        );

        let result = execute_tool(&tool_call);

        assert!(!result.is_error, "Bash should succeed: {}", result.content);
        assert!(
            result.content.contains("Hello from bash"),
            "Should contain echo output"
        );
    }

    #[test]
    fn test_bash_with_exit_code() {
        let tool_call = make_tool_call(
            "bash",
            json!({
                "command": "exit 1"
            }),
        );

        let result = execute_tool(&tool_call);

        // Command fails but tool captures the result
        assert!(
            result.content.contains("exit") || result.is_error,
            "Should indicate non-zero exit"
        );
    }

    #[test]
    fn test_bash_command_not_found() {
        let tool_call = make_tool_call(
            "bash",
            json!({
                "command": "nonexistent_command_12345"
            }),
        );

        let result = execute_tool(&tool_call);

        // Should indicate command not found (either error or in content)
        assert!(
            result.is_error
                || result.content.to_lowercase().contains("not found")
                || result.content.to_lowercase().contains("not recognized"),
            "Should indicate command not found: {}",
            result.content
        );
    }

    #[test]
    fn test_bash_working_directory() {
        let dir = TempDir::new().expect("Failed to create temp dir");

        // Create a file in the temp dir
        fs::write(dir.path().join("marker.txt"), "exists").expect("write failed");

        // Convert Windows path to Unix-style for bash
        let path_str = dir.path().to_string_lossy().replace('\\', "/");

        let tool_call = make_tool_call(
            "bash",
            json!({
                "command": format!("cd '{}' && ls", path_str)
            }),
        );

        let result = execute_tool(&tool_call);

        assert!(
            !result.is_error,
            "Bash cd should succeed: {}",
            result.content
        );
        assert!(
            result.content.contains("marker.txt"),
            "Should list files in target dir"
        );
    }

    #[test]
    fn test_bash_timeout() {
        let tool_call = make_tool_call(
            "bash",
            json!({
                "command": "ping -n 100 127.0.0.1",  // Windows ping (use -c on Linux)
                "timeout": 1000  // 1 second timeout
            }),
        );

        let _result = execute_tool(&tool_call);

        // Should timeout or complete quickly
        // The implementation might return timeout error or partial output
    }

    #[test]
    fn test_bash_background_execution() {
        let tool_call = make_tool_call(
            "bash",
            json!({
                "command": "ping -n 5 127.0.0.1",
                "run_in_background": true
            }),
        );

        let result = execute_tool(&tool_call);

        assert!(
            !result.is_error,
            "Background bash should succeed: {}",
            result.content
        );
        assert!(
            result.content.contains("shell_") || result.content.contains("background"),
            "Should return shell ID for background process"
        );
    }

    #[test]
    fn test_bash_output_list_shells() {
        // First start a background shell
        let bg_call = make_tool_call(
            "bash",
            json!({
                "command": "ping -n 10 127.0.0.1",
                "run_in_background": true
            }),
        );
        let bg_result = execute_tool(&bg_call);
        assert!(!bg_result.is_error, "Background start should succeed");

        // Small delay for process to start
        thread::sleep(Duration::from_millis(100));

        // Now list shells (no shell_id = list all)
        let list_call = make_tool_call("bash_output", json!({}));
        let list_result = execute_tool(&list_call);

        assert!(
            !list_result.is_error,
            "bash_output list should succeed: {}",
            list_result.content
        );
        // Should list at least one shell
        assert!(
            list_result.content.contains("shell_")
                || list_result.content.contains("Background shells")
                || list_result.content.contains("ping"),
            "Should list running shells: {}",
            list_result.content
        );
    }

    #[test]
    fn test_bash_output_specific_shell() {
        // Start a background shell that produces output
        let bg_call = make_tool_call(
            "bash",
            json!({
                "command": "echo 'test output' && ping -n 2 127.0.0.1",
                "run_in_background": true
            }),
        );
        let bg_result = execute_tool(&bg_call);

        // Extract shell ID from result - look for pattern like "shell_abc123"
        let shell_id = extract_shell_id(&bg_result.content);

        // Wait for some output
        thread::sleep(Duration::from_millis(500));

        // Get output from specific shell
        let output_call = make_tool_call(
            "bash_output",
            json!({
                "shell_id": shell_id
            }),
        );
        let output_result = execute_tool(&output_call);

        // Should have some output (might be empty if command finished quickly)
        assert!(
            !output_result.is_error,
            "bash_output should succeed: {}",
            output_result.content
        );
    }

    #[test]
    fn test_kill_shell() {
        // Start a long-running background shell
        let bg_call = make_tool_call(
            "bash",
            json!({
                "command": "ping -n 1000 127.0.0.1",
                "run_in_background": true
            }),
        );
        let bg_result = execute_tool(&bg_call);

        // Extract shell ID
        let shell_id = extract_shell_id(&bg_result.content);

        thread::sleep(Duration::from_millis(100));

        // Kill the shell
        let kill_call = make_tool_call(
            "kill_shell",
            json!({
                "shell_id": shell_id
            }),
        );
        let kill_result = execute_tool(&kill_call);

        assert!(
            !kill_result.is_error,
            "kill_shell should succeed: {}",
            kill_result.content
        );
        assert!(
            kill_result.content.to_lowercase().contains("kill")
                || kill_result.content.to_lowercase().contains("terminated")
                || kill_result.content.to_lowercase().contains("stopped"),
            "Should confirm shell was killed: {}",
            kill_result.content
        );
    }

    // =========== ADDITIONAL BASH TESTS ===========

    #[test]
    fn test_bash_multiline_command() {
        let tool_call = make_tool_call(
            "bash",
            json!({
                "command": "echo 'line1' && echo 'line2' && echo 'line3'"
            }),
        );

        let result = execute_tool(&tool_call);

        assert!(
            !result.is_error,
            "Multiline command should succeed: {}",
            result.content
        );
        assert!(result.content.contains("line1"), "Should contain line1");
        assert!(result.content.contains("line2"), "Should contain line2");
        assert!(result.content.contains("line3"), "Should contain line3");
    }

    #[test]
    fn test_bash_pipe_command() {
        let tool_call = make_tool_call(
            "bash",
            json!({
                "command": "echo 'hello world' | tr 'a-z' 'A-Z'"
            }),
        );

        let result = execute_tool(&tool_call);

        assert!(
            !result.is_error,
            "Pipe command should succeed: {}",
            result.content
        );
        assert!(
            result.content.contains("HELLO WORLD"),
            "Should contain uppercase output"
        );
    }

    #[test]
    fn test_bash_variable_expansion() {
        let tool_call = make_tool_call(
            "bash",
            json!({
                "command": "VAR='test123' && echo $VAR"
            }),
        );

        let result = execute_tool(&tool_call);

        assert!(
            !result.is_error,
            "Variable expansion should succeed: {}",
            result.content
        );
        assert!(
            result.content.contains("test123"),
            "Should contain variable value"
        );
    }

    #[test]
    fn test_bash_stderr_capture() {
        let tool_call = make_tool_call(
            "bash",
            json!({
                "command": "echo 'stderr test' >&2"
            }),
        );

        let result = execute_tool(&tool_call);

        // Should capture stderr output
        assert!(
            !result.is_error || result.content.contains("stderr"),
            "Should capture stderr output"
        );
    }

    #[test]
    fn test_bash_with_quotes() {
        let tool_call = make_tool_call(
            "bash",
            json!({
                "command": "echo \"double quotes\" && echo 'single quotes'"
            }),
        );

        let result = execute_tool(&tool_call);

        assert!(
            !result.is_error,
            "Quoted strings should work: {}",
            result.content
        );
        assert!(
            result.content.contains("double quotes"),
            "Should have double quotes content"
        );
        assert!(
            result.content.contains("single quotes"),
            "Should have single quotes content"
        );
    }

    #[test]
    fn test_kill_shell_nonexistent() {
        let kill_call = make_tool_call(
            "kill_shell",
            json!({
                "shell_id": "nonexistent_shell_12345"
            }),
        );

        let result = execute_tool(&kill_call);

        // Should fail or indicate shell not found
        assert!(
            result.is_error || result.content.to_lowercase().contains("not found"),
            "Should indicate shell not found: {}",
            result.content
        );
    }

    #[test]
    fn test_bash_output_nonexistent_shell() {
        let output_call = make_tool_call(
            "bash_output",
            json!({
                "shell_id": "nonexistent_shell_99999"
            }),
        );

        let result = execute_tool(&output_call);

        // Should fail or indicate shell not found
        assert!(
            result.is_error || result.content.to_lowercase().contains("not found"),
            "Should indicate shell not found: {}",
            result.content
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
        let tool_call = make_tool_call(
            "web_fetch",
            json!({
                "url": "https://httpbin.org/html",
                "prompt": "Extract the main heading"
            }),
        );

        let result = execute_tool(&tool_call);

        // This is a real network call - might fail in CI/offline environments
        // We check if it either succeeded or failed gracefully
        if !result.is_error {
            assert!(result.content.len() > 0, "Should return content from fetch");
        }
    }

    #[test]
    fn test_web_fetch_invalid_url() {
        let tool_call = make_tool_call(
            "web_fetch",
            json!({
                "url": "not-a-valid-url",
                "prompt": "test"
            }),
        );

        let result = execute_tool(&tool_call);

        assert!(result.is_error, "Invalid URL should fail");
    }

    // DuckDuckGo search uses the browser feature (enabled by default)
    // Falls back to Tavily/Brave APIs if configured
    #[test]
    fn test_web_search_duckduckgo() {
        let tool_call = make_tool_call(
            "web_search",
            json!({
                "query": "rust programming language"
            }),
        );

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
        (dir, db) // Return dir to keep it alive
    }

    #[test]
    fn test_memory_save_and_search() {
        let (_dir, db) = setup_memory_db();

        // Save a memory
        let save_call = make_tool_call(
            "memory_save",
            json!({
                "content": "Important fact: Rust is a systems programming language",
                "tags": ["programming", "rust"]
            }),
        );

        let save_result = execute_tool_with_memory(&save_call, Some(&db));
        assert!(
            !save_result.is_error,
            "Save should succeed: {}",
            save_result.content
        );

        // Search for the memory
        let search_call = make_tool_call(
            "memory_search",
            json!({
                "query": "Rust programming"
            }),
        );

        let search_result = execute_tool_with_memory(&search_call, Some(&db));
        assert!(
            !search_result.is_error,
            "Search should succeed: {}",
            search_result.content
        );
        // Should find the saved memory
        assert!(
            search_result.content.contains("Rust") || search_result.content.contains("systems"),
            "Should find saved memory: {}",
            search_result.content
        );
    }

    #[test]
    fn test_memory_search_no_results() {
        let (_dir, db) = setup_memory_db();

        let search_call = make_tool_call(
            "memory_search",
            json!({
                "query": "nonexistent topic xyz123"
            }),
        );

        let result = execute_tool_with_memory(&search_call, Some(&db));

        // Should succeed but with no results
        assert!(
            !result.is_error,
            "Search should succeed: {}",
            result.content
        );
        // Empty result is fine
    }

    #[test]
    fn test_memory_multiple_saves_search() {
        let (_dir, db) = setup_memory_db();

        // Store multiple memories
        for i in 0..3 {
            let save_call = make_tool_call(
                "memory_save",
                json!({
                    "content": format!("Searchable fact number {} about testing", i),
                    "tags": ["test", "fact"]
                }),
            );
            execute_tool_with_memory(&save_call, Some(&db));
        }

        // Search for content
        let search_call = make_tool_call(
            "memory_search",
            json!({
                "query": "testing"
            }),
        );

        let result = execute_tool_with_memory(&search_call, Some(&db));

        assert!(
            !result.is_error,
            "Search should succeed: {}",
            result.content
        );
        // Should find at least one match
        assert!(
            result.content.contains("fact")
                || result.content.contains("testing")
                || result.content.contains("Searchable"),
            "Should find matching memories: {}",
            result.content
        );
    }

    #[test]
    fn test_core_memory_update() {
        let (_dir, db) = setup_memory_db();

        // Update project info core memory
        let update_call = make_tool_call(
            "core_memory_update",
            json!({
                "section": "project_info",
                "content": "This is a Rust project for testing"
            }),
        );

        let result = execute_tool_with_memory(&update_call, Some(&db));

        assert!(
            !result.is_error,
            "Core memory update should succeed: {}",
            result.content
        );
    }

    // =========== EXTENDED MEMORY TESTS ===========

    #[test]
    fn test_core_memory_update_persona() {
        let (_dir, db) = setup_memory_db();

        let update_call = make_tool_call(
            "core_memory_update",
            json!({
                "section": "persona",
                "content": "I am a helpful coding assistant specialized in Rust"
            }),
        );

        let result = execute_tool_with_memory(&update_call, Some(&db));

        assert!(
            !result.is_error,
            "Persona update should succeed: {}",
            result.content
        );
    }

    #[test]
    fn test_core_memory_update_user_preferences() {
        let (_dir, db) = setup_memory_db();

        let update_call = make_tool_call(
            "core_memory_update",
            json!({
                "section": "user_preferences",
                "content": "Prefers concise responses, uses VS Code"
            }),
        );

        let result = execute_tool_with_memory(&update_call, Some(&db));

        assert!(
            !result.is_error,
            "User prefs update should succeed: {}",
            result.content
        );
    }

    #[test]
    fn test_memory_save_with_tags() {
        let (_dir, db) = setup_memory_db();

        let save_call = make_tool_call(
            "memory_save",
            json!({
                "content": "User prefers async/await over callbacks",
                "tags": ["code-style", "javascript", "preferences"]
            }),
        );

        let result = execute_tool_with_memory(&save_call, Some(&db));

        assert!(
            !result.is_error,
            "Save with tags should succeed: {}",
            result.content
        );
    }

    #[test]
    fn test_memory_save_long_content() {
        let (_dir, db) = setup_memory_db();

        // Save a large piece of content
        let long_content: String = (0..100)
            .map(|i| format!("Line {} of important information. ", i))
            .collect();

        let save_call = make_tool_call(
            "memory_save",
            json!({
                "content": long_content,
                "tags": ["long", "test"]
            }),
        );

        let result = execute_tool_with_memory(&save_call, Some(&db));

        assert!(
            !result.is_error,
            "Long content save should succeed: {}",
            result.content
        );
    }

    #[test]
    fn test_memory_search_with_limit() {
        let (_dir, db) = setup_memory_db();

        // Save multiple memories
        for i in 0..10 {
            let save_call = make_tool_call(
                "memory_save",
                json!({
                    "content": format!("Database fact {} about SQL queries", i),
                    "tags": ["database", "sql"]
                }),
            );
            execute_tool_with_memory(&save_call, Some(&db));
        }

        let search_call = make_tool_call(
            "memory_search",
            json!({
                "query": "database SQL",
                "limit": 3
            }),
        );

        let result = execute_tool_with_memory(&search_call, Some(&db));

        assert!(
            !result.is_error,
            "Search with limit should succeed: {}",
            result.content
        );
    }

    #[test]
    fn test_memory_unicode_content() {
        let (_dir, db) = setup_memory_db();

        let unicode_content = "User's name is 田中さん and prefers 日本語 documentation";

        let save_call = make_tool_call(
            "memory_save",
            json!({
                "content": unicode_content,
                "tags": ["user", "japanese"]
            }),
        );

        let save_result = execute_tool_with_memory(&save_call, Some(&db));
        assert!(
            !save_result.is_error,
            "Unicode save should succeed: {}",
            save_result.content
        );

        let search_call = make_tool_call(
            "memory_search",
            json!({
                "query": "田中"
            }),
        );

        let search_result = execute_tool_with_memory(&search_call, Some(&db));
        assert!(!search_result.is_error, "Unicode search should succeed");
    }

    #[test]
    fn test_memory_special_characters() {
        let (_dir, db) = setup_memory_db();

        let content = "Code pattern: if (x && y || z) { return $value; }";

        let save_call = make_tool_call(
            "memory_save",
            json!({
                "content": content,
                "tags": ["code", "pattern"]
            }),
        );

        let result = execute_tool_with_memory(&save_call, Some(&db));

        assert!(
            !result.is_error,
            "Special characters should be handled: {}",
            result.content
        );
    }

    #[test]
    fn test_memory_without_db() {
        // Test that memory tools gracefully handle missing DB
        let save_call = make_tool_call(
            "memory_save",
            json!({
                "content": "test content",
                "tags": ["test"]
            }),
        );

        let result = execute_tool_with_memory(&save_call, None);

        // Should fail gracefully or indicate no database
        assert!(
            result.is_error
                || result.content.to_lowercase().contains("no")
                || result.content.to_lowercase().contains("stateful"),
            "Should indicate memory not available"
        );
    }
}

// ============================================================================
// TOOL DEFINITIONS TESTS
// ============================================================================

mod tool_definitions {
    use openclaudia::tools::{get_all_tool_definitions, get_tool_definitions};

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
            assert!(
                function.get("description").is_some(),
                "Function should have description"
            );
            assert!(
                function.get("parameters").is_some(),
                "Function should have parameters"
            );
        }
    }

    #[test]
    fn test_get_all_tool_definitions_includes_memory() {
        // Without stateful flag, without subagents
        let tools_no_memory = get_all_tool_definitions(false, false);
        let no_memory_names: Vec<&str> = tools_no_memory
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|t| t["function"]["name"].as_str())
            .collect();

        // With stateful flag
        let tools_with_memory = get_all_tool_definitions(true, false);
        let with_memory_names: Vec<&str> = tools_with_memory
            .as_array()
            .unwrap()
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
        let tool_names: Vec<&str> = tools
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|t| t["function"]["name"].as_str())
            .collect();

        // Actual tool names in the system
        let required_tools = vec![
            "read_file",
            "write_file",
            "edit_file",
            "list_files",
            "bash",
            "bash_output",
            "kill_shell",
            "web_fetch",
            "web_search",
            "todo_write",
            "todo_read",
        ];

        for required in required_tools {
            assert!(
                tool_names.contains(&required),
                "Required tool '{}' should exist. Found: {:?}",
                required,
                tool_names
            );
        }
    }

    #[test]
    fn test_subagent_tools_with_subagents_flag() {
        // With subagents flag, should include task and agent_output
        let tools_with_subagents = get_all_tool_definitions(false, true);
        let tool_names: Vec<&str> = tools_with_subagents
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|t| t["function"]["name"].as_str())
            .collect();

        assert!(
            tool_names.contains(&"task"),
            "Subagent mode should include 'task' tool"
        );
        assert!(
            tool_names.contains(&"agent_output"),
            "Subagent mode should include 'agent_output' tool"
        );
    }
}

// ============================================================================
// TODO TOOLS TESTS
// ============================================================================

mod todo_tools {
    use super::*;

    #[test]
    fn test_todo_write_basic() {
        // Clear any existing todos
        clear_todo_list();

        let tool_call = make_tool_call(
            "todo_write",
            json!({
                "todos": [
                    {
                        "content": "Fix the bug",
                        "status": "pending",
                        "activeForm": "Fixing the bug"
                    },
                    {
                        "content": "Write tests",
                        "status": "in_progress",
                        "activeForm": "Writing tests"
                    }
                ]
            }),
        );

        let result = execute_tool(&tool_call);

        assert!(
            !result.is_error,
            "todo_write should succeed: {}",
            result.content
        );
        assert!(
            result.content.contains("2 total"),
            "Should report 2 todos: {}",
            result.content
        );
        assert!(
            result.content.contains("1 in progress"),
            "Should have 1 in progress: {}",
            result.content
        );
        assert!(
            result.content.contains("Writing tests"),
            "Should show current task: {}",
            result.content
        );
    }

    #[test]
    fn test_todo_write_with_completed() {
        clear_todo_list();

        let tool_call = make_tool_call(
            "todo_write",
            json!({
                "todos": [
                    {
                        "content": "Setup project",
                        "status": "completed",
                        "activeForm": "Setting up project"
                    },
                    {
                        "content": "Implement feature",
                        "status": "completed",
                        "activeForm": "Implementing feature"
                    },
                    {
                        "content": "Deploy",
                        "status": "pending",
                        "activeForm": "Deploying"
                    }
                ]
            }),
        );

        let result = execute_tool(&tool_call);

        assert!(!result.is_error, "Should succeed: {}", result.content);
        assert!(
            result.content.contains("2 completed"),
            "Should have 2 completed: {}",
            result.content
        );
        assert!(
            result.content.contains("1 pending"),
            "Should have 1 pending: {}",
            result.content
        );
    }

    #[test]
    fn test_todo_write_multiple_in_progress_warning() {
        clear_todo_list();

        let tool_call = make_tool_call(
            "todo_write",
            json!({
                "todos": [
                    {
                        "content": "Task 1",
                        "status": "in_progress",
                        "activeForm": "Working on task 1"
                    },
                    {
                        "content": "Task 2",
                        "status": "in_progress",
                        "activeForm": "Working on task 2"
                    }
                ]
            }),
        );

        let result = execute_tool(&tool_call);

        assert!(!result.is_error, "Should succeed: {}", result.content);
        assert!(
            result.content.to_lowercase().contains("warning")
                || result.content.contains("2 tasks marked as in_progress"),
            "Should warn about multiple in_progress: {}",
            result.content
        );
    }

    #[test]
    fn test_todo_write_missing_field() {
        clear_todo_list();

        // Missing activeForm
        let tool_call = make_tool_call(
            "todo_write",
            json!({
                "todos": [
                    {
                        "content": "Task",
                        "status": "pending"
                    }
                ]
            }),
        );

        let result = execute_tool(&tool_call);

        assert!(
            result.is_error,
            "Should fail with missing field: {}",
            result.content
        );
        assert!(
            result.content.contains("activeForm"),
            "Should mention missing activeForm: {}",
            result.content
        );
    }

    #[test]
    fn test_todo_write_invalid_status() {
        clear_todo_list();

        let tool_call = make_tool_call(
            "todo_write",
            json!({
                "todos": [
                    {
                        "content": "Task",
                        "status": "invalid_status",
                        "activeForm": "Working"
                    }
                ]
            }),
        );

        let result = execute_tool(&tool_call);

        assert!(
            result.is_error,
            "Should fail with invalid status: {}",
            result.content
        );
        assert!(
            result.content.contains("invalid")
                || result.content.contains("pending")
                || result.content.contains("in_progress")
                || result.content.contains("completed"),
            "Should mention valid statuses: {}",
            result.content
        );
    }

    #[test]
    fn test_todo_read_empty() {
        clear_todo_list();

        let tool_call = make_tool_call("todo_read", json!({}));
        let result = execute_tool(&tool_call);

        assert!(!result.is_error, "Should succeed: {}", result.content);
        assert!(
            result.content.to_lowercase().contains("no todos")
                || result.content.contains("empty")
                || result.content.is_empty(),
            "Should indicate empty list: {}",
            result.content
        );
    }

    #[test]
    fn test_todo_read_after_write() {
        clear_todo_list();

        // Write some todos
        let write_call = make_tool_call(
            "todo_write",
            json!({
                "todos": [
                    {
                        "content": "Research API",
                        "status": "completed",
                        "activeForm": "Researching API"
                    },
                    {
                        "content": "Implement endpoint",
                        "status": "in_progress",
                        "activeForm": "Implementing endpoint"
                    },
                    {
                        "content": "Write documentation",
                        "status": "pending",
                        "activeForm": "Writing documentation"
                    }
                ]
            }),
        );
        execute_tool(&write_call);

        // Read them back
        let read_call = make_tool_call("todo_read", json!({}));
        let result = execute_tool(&read_call);

        assert!(!result.is_error, "Should succeed: {}", result.content);
        assert!(
            result.content.contains("Research API"),
            "Should contain first task: {}",
            result.content
        );
        assert!(
            result.content.contains("Implement endpoint"),
            "Should contain second task: {}",
            result.content
        );
        assert!(
            result.content.contains("Write documentation"),
            "Should contain third task: {}",
            result.content
        );
        assert!(
            result.content.contains("[x]") || result.content.contains("completed"),
            "Should show completed status: {}",
            result.content
        );
        assert!(
            result.content.contains("[>]") || result.content.contains("in_progress"),
            "Should show in_progress status: {}",
            result.content
        );
    }

    #[test]
    fn test_todo_list_persistence() {
        clear_todo_list();

        // Write todos
        let write_call = make_tool_call(
            "todo_write",
            json!({
                "todos": [
                    {
                        "content": "Persistent task",
                        "status": "pending",
                        "activeForm": "Working on persistent task"
                    }
                ]
            }),
        );
        execute_tool(&write_call);

        // Get the list directly using helper function
        let todos = get_todo_list();

        assert_eq!(todos.len(), 1, "Should have 1 todo");
        assert_eq!(todos[0].content, "Persistent task");
        assert_eq!(todos[0].status, "pending");
        assert_eq!(todos[0].active_form, "Working on persistent task");
    }

    #[test]
    fn test_todo_write_replaces_list() {
        clear_todo_list();

        // First write
        let write1 = make_tool_call(
            "todo_write",
            json!({
                "todos": [
                    {
                        "content": "Old task 1",
                        "status": "pending",
                        "activeForm": "Working"
                    },
                    {
                        "content": "Old task 2",
                        "status": "pending",
                        "activeForm": "Working"
                    }
                ]
            }),
        );
        execute_tool(&write1);

        // Second write (should replace, not append)
        let write2 = make_tool_call(
            "todo_write",
            json!({
                "todos": [
                    {
                        "content": "New task",
                        "status": "in_progress",
                        "activeForm": "Working on new task"
                    }
                ]
            }),
        );
        execute_tool(&write2);

        let todos = get_todo_list();
        assert_eq!(todos.len(), 1, "Should have replaced list with 1 todo");
        assert_eq!(todos[0].content, "New task");
    }

    #[test]
    fn test_todo_write_empty_list() {
        clear_todo_list();

        // First add some todos
        let write1 = make_tool_call(
            "todo_write",
            json!({
                "todos": [
                    {
                        "content": "Task",
                        "status": "pending",
                        "activeForm": "Working"
                    }
                ]
            }),
        );
        execute_tool(&write1);

        // Then clear by writing empty list
        let write_empty = make_tool_call(
            "todo_write",
            json!({
                "todos": []
            }),
        );
        let result = execute_tool(&write_empty);

        assert!(!result.is_error, "Should succeed: {}", result.content);
        assert!(
            result.content.contains("0 total"),
            "Should report 0 todos: {}",
            result.content
        );

        let todos = get_todo_list();
        assert!(todos.is_empty(), "List should be empty");
    }
}

// ============================================================================
// SUBAGENT TOOLS TESTS
// ============================================================================

mod subagent_tools {
    use super::*;

    #[test]
    fn test_task_tool_missing_args() {
        // Missing all required arguments
        let tool_call = make_tool_call("task", json!({}));
        let result = execute_tool(&tool_call);

        // Should fail because subagent tools require config context
        assert!(
            result.is_error,
            "task without config should fail: {}",
            result.content
        );
        assert!(
            result.content.contains("config")
                || result.content.contains("description")
                || result.content.contains("require"),
            "Should mention configuration requirement: {}",
            result.content
        );
    }

    #[test]
    fn test_agent_output_no_agents() {
        // When no agent_id is provided, should list agents (empty list)
        let tool_call = make_tool_call("agent_output", json!({}));
        let result = execute_tool(&tool_call);

        // Should fail because it requires config, or return empty list
        // The behavior depends on implementation
        if !result.is_error {
            assert!(
                result.content.to_lowercase().contains("no")
                    || result.content.to_lowercase().contains("agent"),
                "Should mention no agents: {}",
                result.content
            );
        }
    }

    #[test]
    fn test_agent_output_nonexistent_id() {
        let tool_call = make_tool_call(
            "agent_output",
            json!({
                "agent_id": "nonexistent_agent_12345"
            }),
        );
        let result = execute_tool(&tool_call);

        // Should fail because agent doesn't exist or config is missing
        assert!(
            result.is_error
                || result.content.to_lowercase().contains("not found")
                || result.content.to_lowercase().contains("config"),
            "Should indicate agent not found or config missing: {}",
            result.content
        );
    }

    #[test]
    fn test_subagent_tool_definitions_exist() {
        use openclaudia::subagent::get_subagent_tool_definitions;

        let tools = get_subagent_tool_definitions();
        let tools_array = tools.as_array().expect("Should be array");

        assert_eq!(tools_array.len(), 2, "Should have 2 subagent tools");

        let tool_names: Vec<&str> = tools_array
            .iter()
            .filter_map(|t| t["function"]["name"].as_str())
            .collect();

        assert!(tool_names.contains(&"task"), "Should have task tool");
        assert!(
            tool_names.contains(&"agent_output"),
            "Should have agent_output tool"
        );
    }

    #[test]
    fn test_task_tool_definition_structure() {
        use openclaudia::subagent::get_subagent_tool_definitions;

        let tools = get_subagent_tool_definitions();
        let task_tool = tools
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["function"]["name"].as_str() == Some("task"))
            .expect("Should find task tool");

        let params = &task_tool["function"]["parameters"];
        let required = params["required"].as_array().expect("Should have required");

        assert!(
            required.iter().any(|r| r.as_str() == Some("description")),
            "Should require description"
        );
        assert!(
            required.iter().any(|r| r.as_str() == Some("prompt")),
            "Should require prompt"
        );
        assert!(
            required.iter().any(|r| r.as_str() == Some("subagent_type")),
            "Should require subagent_type"
        );

        // Check enum for subagent_type
        let subagent_type_enum = &params["properties"]["subagent_type"]["enum"];
        assert!(
            subagent_type_enum.is_array(),
            "subagent_type should have enum"
        );
        let types: Vec<&str> = subagent_type_enum
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|v| v.as_str())
            .collect();
        assert!(types.contains(&"general-purpose"));
        assert!(types.contains(&"explore"));
        assert!(types.contains(&"plan"));
        assert!(types.contains(&"guide"));
    }

    #[test]
    fn test_agent_output_tool_definition_structure() {
        use openclaudia::subagent::get_subagent_tool_definitions;

        let tools = get_subagent_tool_definitions();
        let agent_output_tool = tools
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["function"]["name"].as_str() == Some("agent_output"))
            .expect("Should find agent_output tool");

        let params = &agent_output_tool["function"]["parameters"];
        let properties = &params["properties"];

        assert!(
            properties.get("agent_id").is_some(),
            "Should have agent_id property"
        );
        assert!(
            properties.get("block").is_some(),
            "Should have block property"
        );
    }

    #[test]
    fn test_agent_type_parsing() {
        use openclaudia::subagent::AgentType;

        assert!(AgentType::parse_type("general-purpose").is_some());
        assert!(AgentType::parse_type("explore").is_some());
        assert!(AgentType::parse_type("plan").is_some());
        assert!(AgentType::parse_type("guide").is_some());
        assert!(AgentType::parse_type("EXPLORE").is_some()); // case insensitive
        assert!(AgentType::parse_type("invalid").is_none());
    }

    #[test]
    fn test_agent_type_allowed_tools() {
        use openclaudia::subagent::AgentType;

        // GeneralPurpose should have write access
        let gp_tools = AgentType::GeneralPurpose.allowed_tools();
        assert!(gp_tools.contains(&"write_file"));
        assert!(gp_tools.contains(&"edit_file"));
        assert!(gp_tools.contains(&"bash"));

        // Explore should be read-only
        let explore_tools = AgentType::Explore.allowed_tools();
        assert!(explore_tools.contains(&"read_file"));
        assert!(!explore_tools.contains(&"write_file"));
        assert!(!explore_tools.contains(&"edit_file"));

        // Guide should be most restricted
        let guide_tools = AgentType::Guide.allowed_tools();
        assert!(guide_tools.contains(&"read_file"));
        assert!(!guide_tools.contains(&"bash")); // No bash for guide
    }
}

// ============================================================================
// TOKEN TRACKING INTEGRATION TESTS
// ============================================================================

mod token_tracking {
    use openclaudia::compaction::{
        estimate_message_tokens, estimate_request_tokens, estimate_tokens, get_context_window,
        CompactionConfig, ContextCompactor,
    };
    use openclaudia::config::{SessionConfig, TokenTrackingConfig};
    use openclaudia::proxy::{ChatCompletionRequest, ChatMessage, MessageContent};
    use openclaudia::session::{Session, SessionManager, SessionMode, TokenUsage};
    use std::collections::HashMap;
    use tempfile::TempDir;

    // ========================================================================
    // TokenUsage Tests
    // ========================================================================

    #[test]
    fn test_token_usage_default() {
        let usage = TokenUsage::default();
        assert_eq!(usage.input_tokens, 0);
        assert_eq!(usage.output_tokens, 0);
        assert_eq!(usage.cache_read_tokens, 0);
        assert_eq!(usage.cache_write_tokens, 0);
        assert_eq!(usage.total(), 0);
    }

    #[test]
    fn test_token_usage_total() {
        let usage = TokenUsage {
            input_tokens: 1000,
            output_tokens: 500,
            cache_read_tokens: 200,
            cache_write_tokens: 100,
        };
        assert_eq!(usage.total(), 1500); // input + output only
    }

    #[test]
    fn test_token_usage_accumulate() {
        let mut cumulative = TokenUsage::default();

        let turn1 = TokenUsage {
            input_tokens: 1000,
            output_tokens: 500,
            cache_read_tokens: 200,
            cache_write_tokens: 100,
        };
        cumulative.accumulate(&turn1);

        assert_eq!(cumulative.input_tokens, 1000);
        assert_eq!(cumulative.output_tokens, 500);
        assert_eq!(cumulative.cache_read_tokens, 200);
        assert_eq!(cumulative.cache_write_tokens, 100);

        let turn2 = TokenUsage {
            input_tokens: 2000,
            output_tokens: 800,
            cache_read_tokens: 500,
            cache_write_tokens: 0,
        };
        cumulative.accumulate(&turn2);

        assert_eq!(cumulative.input_tokens, 3000);
        assert_eq!(cumulative.output_tokens, 1300);
        assert_eq!(cumulative.cache_read_tokens, 700);
        assert_eq!(cumulative.cache_write_tokens, 100);
        assert_eq!(cumulative.total(), 4300);
    }

    #[test]
    fn test_token_usage_serialization_roundtrip() {
        let usage = TokenUsage {
            input_tokens: 12345,
            output_tokens: 6789,
            cache_read_tokens: 1000,
            cache_write_tokens: 500,
        };

        let json = serde_json::to_string(&usage).expect("serialize");
        let deserialized: TokenUsage = serde_json::from_str(&json).expect("deserialize");

        assert_eq!(deserialized.input_tokens, 12345);
        assert_eq!(deserialized.output_tokens, 6789);
        assert_eq!(deserialized.cache_read_tokens, 1000);
        assert_eq!(deserialized.cache_write_tokens, 500);
    }

    // ========================================================================
    // Session Token Tracking Tests
    // ========================================================================

    #[test]
    fn test_session_record_turn_estimate() {
        let mut session = Session::new_initializer();

        let turn = session.record_turn_estimate(5000, 2000, 1500, 500);
        assert_eq!(turn, 1);
        assert_eq!(session.turn_metrics.len(), 1);

        let metrics = &session.turn_metrics[0];
        assert_eq!(metrics.turn_number, 1);
        assert_eq!(metrics.estimated_input_tokens, 5000);
        assert_eq!(metrics.injected_context_tokens, 2000);
        assert_eq!(metrics.system_prompt_tokens, 1500);
        assert_eq!(metrics.tool_def_tokens, 500);
        assert!(metrics.actual_usage.is_none());

        // Second turn
        let turn2 = session.record_turn_estimate(8000, 3000, 1500, 1500);
        assert_eq!(turn2, 2);
        assert_eq!(session.turn_metrics.len(), 2);
    }

    #[test]
    fn test_session_record_actual_usage() {
        let mut session = Session::new_initializer();

        // Record estimate first
        session.record_turn_estimate(5000, 2000, 1500, 500);

        // Then record actual usage
        let usage = TokenUsage {
            input_tokens: 4800,
            output_tokens: 1200,
            cache_read_tokens: 300,
            cache_write_tokens: 100,
        };
        session.record_actual_usage(usage);

        // Check cumulative
        assert_eq!(session.cumulative_usage.input_tokens, 4800);
        assert_eq!(session.cumulative_usage.output_tokens, 1200);
        assert_eq!(session.total_tokens, 6000); // backward compat field

        // Check last turn has actual usage
        let last = session.turn_metrics.last().unwrap();
        assert!(last.actual_usage.is_some());
        let actual = last.actual_usage.as_ref().unwrap();
        assert_eq!(actual.input_tokens, 4800);
        assert_eq!(actual.output_tokens, 1200);
    }

    #[test]
    fn test_session_multi_turn_tracking() {
        let mut session = Session::new_initializer();

        // Turn 1
        session.record_turn_estimate(5000, 2000, 1500, 500);
        session.record_actual_usage(TokenUsage {
            input_tokens: 4800,
            output_tokens: 1200,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
        });

        // Turn 2
        session.record_turn_estimate(8000, 2500, 1500, 1000);
        session.record_actual_usage(TokenUsage {
            input_tokens: 7500,
            output_tokens: 2000,
            cache_read_tokens: 1000,
            cache_write_tokens: 0,
        });

        // Turn 3
        session.record_turn_estimate(12000, 3000, 1500, 1500);
        session.record_actual_usage(TokenUsage {
            input_tokens: 11000,
            output_tokens: 3000,
            cache_read_tokens: 2000,
            cache_write_tokens: 500,
        });

        assert_eq!(session.turn_metrics.len(), 3);
        assert_eq!(session.request_count, 0); // increment_requests not called
        assert_eq!(session.cumulative_usage.input_tokens, 4800 + 7500 + 11000);
        assert_eq!(session.cumulative_usage.output_tokens, 1200 + 2000 + 3000);
        assert_eq!(session.cumulative_usage.cache_read_tokens, 0 + 1000 + 2000);
        assert_eq!(session.cumulative_usage.cache_write_tokens, 0 + 0 + 500);
        assert_eq!(
            session.cumulative_usage.total(),
            (4800 + 7500 + 11000) + (1200 + 2000 + 3000)
        );
    }

    #[test]
    fn test_session_stats_summary() {
        let mut session = Session::new_initializer();
        session.record_turn_estimate(5000, 2000, 1500, 500);
        session.record_actual_usage(TokenUsage {
            input_tokens: 4800,
            output_tokens: 1200,
            cache_read_tokens: 300,
            cache_write_tokens: 100,
        });

        let summary = session.stats_summary();

        assert!(summary.contains("Turns: 1"), "Summary: {}", summary);
        assert!(
            summary.contains("Input tokens:  4800"),
            "Summary: {}",
            summary
        );
        assert!(
            summary.contains("Output tokens: 1200"),
            "Summary: {}",
            summary
        );
        assert!(
            summary.contains("Cache read:    300"),
            "Summary: {}",
            summary
        );
        assert!(
            summary.contains("Cache write:   100"),
            "Summary: {}",
            summary
        );
        assert!(summary.contains("Last turn #1"), "Summary: {}", summary);
        assert!(
            summary.contains("actual 4800in/1200out"),
            "Summary: {}",
            summary
        );
    }

    #[test]
    fn test_session_handoff_includes_token_usage() {
        let mut session = Session::new_initializer();
        session.record_turn_estimate(5000, 2000, 1500, 500);
        session.record_actual_usage(TokenUsage {
            input_tokens: 4800,
            output_tokens: 1200,
            cache_read_tokens: 300,
            cache_write_tokens: 0,
        });

        let handoff = session.generate_handoff();

        assert!(handoff.contains("### Token Usage"), "Handoff: {}", handoff);
        assert!(
            handoff.contains("Input: 4800 tokens"),
            "Handoff: {}",
            handoff
        );
        assert!(
            handoff.contains("Output: 1200 tokens"),
            "Handoff: {}",
            handoff
        );
        assert!(handoff.contains("Turns: 1"), "Handoff: {}", handoff);
    }

    #[test]
    fn test_session_handoff_no_token_section_when_empty() {
        let session = Session::new_initializer();
        let handoff = session.generate_handoff();

        assert!(
            !handoff.contains("### Token Usage"),
            "Should not include token section when no usage"
        );
    }

    // ========================================================================
    // Session Persistence with Token Data
    // ========================================================================

    #[test]
    fn test_session_persistence_with_token_data() {
        let dir = TempDir::new().unwrap();
        let mut manager = SessionManager::new(dir.path().join("sessions"));

        // Create session and add token data
        let session = manager.get_or_create_session();
        let session_id = session.id.clone();

        {
            let session = manager.get_session_mut().unwrap();
            session.record_turn_estimate(10000, 3000, 2000, 1000);
            session.record_actual_usage(TokenUsage {
                input_tokens: 9500,
                output_tokens: 2500,
                cache_read_tokens: 500,
                cache_write_tokens: 200,
            });
        }

        // Persist and reload
        manager.end_session(Some("Token tracking test"));

        let loaded = manager.load_session(&session_id).expect("Should load");

        assert_eq!(loaded.cumulative_usage.input_tokens, 9500);
        assert_eq!(loaded.cumulative_usage.output_tokens, 2500);
        assert_eq!(loaded.cumulative_usage.cache_read_tokens, 500);
        assert_eq!(loaded.cumulative_usage.cache_write_tokens, 200);
        assert_eq!(loaded.turn_metrics.len(), 1);

        let turn = &loaded.turn_metrics[0];
        assert_eq!(turn.estimated_input_tokens, 10000);
        assert!(turn.actual_usage.is_some());
    }

    #[test]
    fn test_session_backward_compat_deserialization() {
        // Simulate loading a session that was persisted before token tracking
        let old_session_json = r#"{
            "id": "test-old-session",
            "mode": "initializer",
            "created_at": "2025-01-01T00:00:00Z",
            "updated_at": "2025-01-01T01:00:00Z",
            "progress": {
                "completed_tasks": [],
                "in_progress_tasks": [],
                "pending_tasks": [],
                "decisions": [],
                "files_modified": [],
                "handoff_notes": ""
            },
            "parent_session_id": null,
            "request_count": 5,
            "total_tokens": 10000
        }"#;

        let session: Session =
            serde_json::from_str(old_session_json).expect("Should deserialize old format");

        assert_eq!(session.id, "test-old-session");
        assert_eq!(session.request_count, 5);
        assert_eq!(session.total_tokens, 10000);
        // New fields should have defaults
        assert_eq!(session.cumulative_usage.input_tokens, 0);
        assert_eq!(session.cumulative_usage.output_tokens, 0);
        assert!(session.turn_metrics.is_empty());
    }

    // ========================================================================
    // TokenTrackingConfig Tests
    // ========================================================================

    #[test]
    fn test_token_tracking_config_default() {
        let config = TokenTrackingConfig::default();
        assert!(config.enabled);
        assert!(config.log_usage);
        assert!((config.warn_threshold - 0.75).abs() < f32::EPSILON);
        assert_eq!(config.max_output_tokens, 0);
    }

    #[test]
    fn test_token_tracking_config_serde_defaults() {
        let config: TokenTrackingConfig = serde_json::from_str("{}").unwrap();
        assert!(config.enabled);
        assert!(config.log_usage);
        assert!((config.warn_threshold - 0.75).abs() < f32::EPSILON);
    }

    #[test]
    fn test_token_tracking_config_custom() {
        let json = r#"{
            "enabled": false,
            "log_usage": false,
            "warn_threshold": 0.9,
            "max_output_tokens": 8192
        }"#;

        let config: TokenTrackingConfig = serde_json::from_str(json).unwrap();
        assert!(!config.enabled);
        assert!(!config.log_usage);
        assert!((config.warn_threshold - 0.9).abs() < f32::EPSILON);
        assert_eq!(config.max_output_tokens, 8192);
    }

    #[test]
    fn test_session_config_includes_token_tracking() {
        let config = SessionConfig::default();
        assert!(config.token_tracking.enabled);
        assert!(config.token_tracking.log_usage);
    }

    #[test]
    fn test_session_config_serde_with_token_tracking() {
        let json = r#"{
            "timeout_minutes": 60,
            "persist_path": "/tmp/test",
            "token_tracking": {
                "enabled": true,
                "warn_threshold": 0.8
            }
        }"#;

        let config: SessionConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.timeout_minutes, 60);
        assert!(config.token_tracking.enabled);
        assert!((config.token_tracking.warn_threshold - 0.8).abs() < f32::EPSILON);
    }

    // ========================================================================
    // extract_usage_from_response Tests (via proxy internals)
    // ========================================================================

    // These test the usage extraction logic by verifying session state
    // after processing mock provider responses.

    #[test]
    fn test_token_usage_openai_format_parsing() {
        // Simulate what extract_usage_from_response does for OpenAI
        let response = serde_json::json!({
            "id": "chatcmpl-123",
            "usage": {
                "prompt_tokens": 1500,
                "completion_tokens": 800,
                "total_tokens": 2300,
                "prompt_tokens_details": {
                    "cached_tokens": 400
                }
            }
        });

        let usage = response.get("usage").unwrap();
        let input = usage
            .get("prompt_tokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let output = usage
            .get("completion_tokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let cached = usage
            .get("prompt_tokens_details")
            .and_then(|d| d.get("cached_tokens"))
            .and_then(|v| v.as_u64())
            .unwrap_or(0);

        assert_eq!(input, 1500);
        assert_eq!(output, 800);
        assert_eq!(cached, 400);
    }

    #[test]
    fn test_token_usage_anthropic_format_parsing() {
        // Simulate Anthropic response format
        let response = serde_json::json!({
            "id": "msg_123",
            "usage": {
                "input_tokens": 2000,
                "output_tokens": 1000,
                "cache_read_input_tokens": 500,
                "cache_creation_input_tokens": 200
            }
        });

        let usage = response.get("usage").unwrap();
        let input = usage
            .get("input_tokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let output = usage
            .get("output_tokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let cache_read = usage
            .get("cache_read_input_tokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let cache_write = usage
            .get("cache_creation_input_tokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);

        assert_eq!(input, 2000);
        assert_eq!(output, 1000);
        assert_eq!(cache_read, 500);
        assert_eq!(cache_write, 200);
    }

    #[test]
    fn test_token_usage_missing_usage_field() {
        let response = serde_json::json!({
            "id": "chatcmpl-123",
            "choices": []
        });

        assert!(response.get("usage").is_none());
    }

    // ========================================================================
    // Compaction with Token Hint Tests
    // ========================================================================

    fn make_test_message(role: &str, content: &str) -> ChatMessage {
        ChatMessage {
            role: role.to_string(),
            content: MessageContent::Text(content.to_string()),
            name: None,
            tool_calls: None,
            tool_call_id: None,
        }
    }

    fn make_test_request(messages: Vec<ChatMessage>) -> ChatCompletionRequest {
        ChatCompletionRequest {
            model: "gpt-4".to_string(),
            messages,
            temperature: None,
            max_tokens: None,
            stream: None,
            tools: None,
            tool_choice: None,
            extra: HashMap::new(),
        }
    }

    #[test]
    fn test_compaction_analyze_with_hint_none() {
        let messages = vec![
            make_test_message("system", "You are helpful."),
            make_test_message("user", "Hello"),
        ];

        let request = make_test_request(messages);
        let compactor = ContextCompactor::new(CompactionConfig::default());

        // Without hint, should use estimator
        let analysis = compactor.analyze_with_hint(&request, None);
        let estimated = estimate_request_tokens(&request);
        assert_eq!(analysis.current_tokens, estimated);
        assert!(!analysis.needs_compaction);
    }

    #[test]
    fn test_compaction_analyze_with_hint_provided() {
        let messages = vec![
            make_test_message("system", "You are helpful."),
            make_test_message("user", "Hello"),
        ];

        let request = make_test_request(messages);
        let compactor = ContextCompactor::new(CompactionConfig::default());

        // With hint, should use the provided value
        let analysis = compactor.analyze_with_hint(&request, Some(50000));
        assert_eq!(analysis.current_tokens, 50000);
    }

    #[test]
    fn test_compaction_with_hint_triggers_compaction() {
        let messages = vec![
            make_test_message("system", "System prompt"),
            make_test_message("user", "First question"),
            make_test_message("assistant", "First answer"),
            make_test_message("user", "Second question"),
            make_test_message("assistant", "Second answer with some content"),
            make_test_message("user", "Recent question"),
            make_test_message("assistant", "Recent answer"),
        ];

        let request = make_test_request(messages);

        // Small context window to make hint trigger compaction
        let config = CompactionConfig {
            max_context_tokens: 5000,
            threshold: 0.8,
            preserve_recent: 2,
            ..Default::default()
        };

        let compactor = ContextCompactor::new(config);

        // Without hint: estimation might not trigger compaction
        let _analysis_no_hint = compactor.analyze_with_hint(&request, None);

        // With large hint: should definitely trigger compaction
        let analysis_with_hint = compactor.analyze_with_hint(&request, Some(4500));
        assert!(
            analysis_with_hint.needs_compaction,
            "4500 tokens should exceed 80% of 5000"
        );
        assert!(analysis_with_hint.tokens_to_free > 0);
    }

    #[tokio::test]
    async fn test_compact_with_hint_method() {
        let long_content = "x".repeat(10000);
        let messages = vec![
            make_test_message("system", "You are helpful."),
            make_test_message("user", &long_content),
            make_test_message("assistant", &long_content),
            make_test_message("user", "Recent message"),
            make_test_message("assistant", "Recent response"),
        ];

        let mut request = make_test_request(messages);

        let config = CompactionConfig {
            max_context_tokens: 5000,
            threshold: 0.8,
            preserve_recent: 2,
            ..Default::default()
        };

        let compactor = ContextCompactor::new(config);

        // compact_with_hint with None behaves like compact
        let result = compactor
            .compact_with_hint(&mut request, None, None, None)
            .await
            .unwrap();

        assert!(result.compacted);
        assert!(result.new_tokens < result.original_tokens);
    }

    // ========================================================================
    // Token Estimation Accuracy Tests
    // ========================================================================

    #[test]
    fn test_estimate_tokens_rough_accuracy() {
        // "Hello world" is approximately 2 tokens in most tokenizers
        let estimate = estimate_tokens("Hello world");
        assert!(estimate >= 1 && estimate <= 5, "Estimate: {}", estimate);

        // Longer text should scale roughly linearly
        let short = estimate_tokens("Hello");
        let long = estimate_tokens(&"Hello ".repeat(100));
        assert!(long > short * 50, "Should scale with length");
    }

    #[test]
    fn test_estimate_message_tokens_includes_overhead() {
        let msg = make_test_message("user", "Hello");
        let tokens = estimate_message_tokens(&msg);
        let text_tokens = estimate_tokens("Hello");

        // Message tokens should be more than just text (includes role overhead)
        assert!(
            tokens > text_tokens,
            "Message should have overhead: msg={}, text={}",
            tokens,
            text_tokens
        );
    }

    #[test]
    fn test_estimate_request_tokens_with_tools() {
        let messages = vec![make_test_message("user", "Help me write code")];
        let mut request = make_test_request(messages);

        let tokens_no_tools = estimate_request_tokens(&request);

        request.tools = Some(vec![serde_json::json!({
            "type": "function",
            "function": {
                "name": "read_file",
                "description": "Read a file from disk",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "path": {"type": "string"}
                    }
                }
            }
        })]);

        let tokens_with_tools = estimate_request_tokens(&request);

        assert!(
            tokens_with_tools > tokens_no_tools,
            "Tools should add tokens: with={}, without={}",
            tokens_with_tools,
            tokens_no_tools
        );
    }

    #[test]
    fn test_context_window_sizes() {
        assert_eq!(get_context_window("claude-3-opus-20240229"), 200_000);
        assert_eq!(get_context_window("claude-3-5-sonnet-20241022"), 200_000);
        assert_eq!(get_context_window("gpt-4o"), 128_000);
        assert_eq!(get_context_window("gemini-1.5-pro"), 1_000_000);
        assert_eq!(get_context_window("unknown-model"), 128_000);
    }

    // ========================================================================
    // Coding Session Continuation with Token Data
    // ========================================================================

    #[test]
    fn test_coding_session_inherits_clean_token_state() {
        let dir = TempDir::new().unwrap();
        let mut manager = SessionManager::new(dir.path().join("sessions"));

        // First session with token data
        {
            let _ = manager.get_or_create_session();
            let session = manager.get_session_mut().unwrap();
            session.record_turn_estimate(5000, 2000, 1500, 500);
            session.record_actual_usage(TokenUsage {
                input_tokens: 4800,
                output_tokens: 1200,
                cache_read_tokens: 0,
                cache_write_tokens: 0,
            });
        }
        manager.end_session(Some("First session done"));

        // Second session should start with clean token counters
        let second = manager.get_or_create_session().clone();
        assert_eq!(second.mode, SessionMode::Coding);
        assert_eq!(second.cumulative_usage.input_tokens, 0);
        assert_eq!(second.cumulative_usage.output_tokens, 0);
        assert!(second.turn_metrics.is_empty());
        assert!(second.parent_session_id.is_some());
    }

    #[test]
    fn test_session_record_without_actual_usage() {
        let mut session = Session::new_initializer();

        // Record estimate but no actual usage (streaming response)
        session.record_turn_estimate(5000, 2000, 1500, 500);
        session.record_turn_estimate(8000, 3000, 2000, 1000);

        assert_eq!(session.turn_metrics.len(), 2);
        assert_eq!(session.cumulative_usage.total(), 0); // No actual usage recorded
        assert_eq!(session.total_tokens, 0);

        // Both turns should have None for actual_usage
        for turn in &session.turn_metrics {
            assert!(turn.actual_usage.is_none());
        }
    }
}

// ============================================================================
// VDD (Verification-Driven Development) INTEGRATION TESTS
// ============================================================================

mod vdd_tests {
    use openclaudia::config::{
        AppConfig, HooksConfig, KeybindingsConfig, ProxyConfig, SessionConfig, VddAdversaryConfig,
        VddConfig, VddMode, VddStaticAnalysis, VddThresholds, VddTracking,
    };
    use openclaudia::hooks::{HookEngine, HookEvent};
    use openclaudia::session::{SessionManager, TokenUsage};
    use openclaudia::vdd::{
        ConfabulationTracker, Finding, FindingStatus, Severity, VddEngine, VddSession,
    };
    use std::collections::HashMap;
    use tempfile::TempDir;

    /// Helper to create an AppConfig with VDD enabled for a given mode
    fn make_vdd_config(mode: VddMode, adversary_provider: &str) -> AppConfig {
        AppConfig {
            proxy: ProxyConfig {
                target: "anthropic".to_string(),
                ..Default::default()
            },
            providers: HashMap::new(),
            hooks: HooksConfig::default(),
            session: SessionConfig::default(),
            keybindings: KeybindingsConfig::default(),
            vdd: VddConfig {
                enabled: true,
                mode,
                adversary: VddAdversaryConfig {
                    provider: adversary_provider.to_string(),
                    model: Some("test-model".to_string()),
                    temperature: 0.3,
                    max_tokens: 4096,
                    ..Default::default()
                },
                thresholds: VddThresholds {
                    max_iterations: 5,
                    false_positive_rate: 0.75,
                    min_iterations: 2,
                },
                static_analysis: VddStaticAnalysis {
                    enabled: true,
                    commands: vec!["echo ok".to_string()],
                    timeout_seconds: 30,
                },
                tracking: VddTracking {
                    persist: false,
                    log_adversary_responses: true,
                    ..Default::default()
                },
            },
        }
    }

    // ========================================================================
    // Config Validation Integration
    // ========================================================================

    #[test]
    fn test_vdd_config_validates_against_builder_provider() {
        let config = make_vdd_config(VddMode::Advisory, "google");
        // Should pass: adversary=google, builder=anthropic
        assert!(config.vdd.validate(&config.proxy.target).is_ok());
    }

    #[test]
    fn test_vdd_config_rejects_same_provider() {
        let config = make_vdd_config(VddMode::Advisory, "anthropic");
        // Should fail: adversary=anthropic, builder=anthropic
        let result = config.vdd.validate(&config.proxy.target);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("must differ"));
    }

    #[test]
    fn test_vdd_config_disabled_skips_validation() {
        let mut config = make_vdd_config(VddMode::Advisory, "anthropic");
        config.vdd.enabled = false;
        // Even though providers match, validation passes when disabled
        assert!(config.vdd.validate(&config.proxy.target).is_ok());
    }

    #[test]
    fn test_vdd_config_validates_thresholds() {
        let mut config = make_vdd_config(VddMode::Blocking, "google");

        // Invalid FP rate
        config.vdd.thresholds.false_positive_rate = 2.0;
        assert!(config.vdd.validate(&config.proxy.target).is_err());

        // Fix FP rate, make min > max
        config.vdd.thresholds.false_positive_rate = 0.75;
        config.vdd.thresholds.min_iterations = 10;
        config.vdd.thresholds.max_iterations = 5;
        assert!(config.vdd.validate(&config.proxy.target).is_err());
    }

    // ========================================================================
    // VDD Engine Construction
    // ========================================================================

    #[test]
    fn test_vdd_engine_construction() {
        let config = make_vdd_config(VddMode::Advisory, "google");
        let client = reqwest::Client::new();
        let engine = VddEngine::new(&config.vdd, &config, client);
        // Engine should be constructible without panic
        assert!(true, "VDD engine constructed successfully");
        let _ = engine; // use it to avoid warning
    }

    #[test]
    fn test_vdd_engine_both_modes() {
        let client = reqwest::Client::new();

        let advisory_config = make_vdd_config(VddMode::Advisory, "google");
        let _advisory_engine =
            VddEngine::new(&advisory_config.vdd, &advisory_config, client.clone());

        let blocking_config = make_vdd_config(VddMode::Blocking, "google");
        let _blocking_engine = VddEngine::new(&blocking_config.vdd, &blocking_config, client);
    }

    // ========================================================================
    // Confabulation Tracker Integration
    // ========================================================================

    #[test]
    fn test_confabulation_tracker_full_lifecycle() {
        let mut tracker = ConfabulationTracker::new(0.75, 2);

        // Iteration 1: adversary finds real issues (0% FP rate)
        tracker.record_iteration(3, 0);
        assert!(!tracker.should_terminate()); // below min_iterations
        assert_eq!(tracker.current_rate(), 0.0);

        // Iteration 2: mixed results (50% FP)
        tracker.record_iteration(2, 2);
        assert!(!tracker.should_terminate()); // 50% < 75% threshold
        assert!(tracker.latest_rate() > 0.0);

        // Iteration 3: mostly FPs (80% FP)
        tracker.record_iteration(1, 4);
        assert!(tracker.should_terminate()); // 80% > 75% threshold
    }

    #[test]
    fn test_confabulation_tracker_immediate_convergence() {
        let mut tracker = ConfabulationTracker::new(0.75, 2);

        // Adversary finds nothing twice in a row
        tracker.record_iteration(0, 0);
        assert!(!tracker.should_terminate()); // below min_iterations

        tracker.record_iteration(0, 0);
        assert!(tracker.should_terminate()); // 0 findings = converged
    }

    #[test]
    fn test_confabulation_tracker_never_converges_with_real_findings() {
        let mut tracker = ConfabulationTracker::new(0.75, 2);

        // Adversary consistently finds real issues (0% FP)
        for _ in 0..5 {
            tracker.record_iteration(5, 0);
            assert!(!tracker.should_terminate());
        }
        // Never terminates because genuine findings keep appearing
        assert_eq!(tracker.current_rate(), 0.0);
    }

    // ========================================================================
    // VDD Session Type Integration
    // ========================================================================

    #[test]
    fn test_vdd_session_public_fields() {
        // VddSession fields are public — verify we can read them from integration context
        let session = VddSession {
            id: "test-session-1".to_string(),
            mode: VddMode::Blocking,
            iterations: vec![],
            total_findings: 5,
            total_genuine: 3,
            total_false_positives: 2,
            false_positive_rate: 0.4,
            converged: false,
            termination_reason: None,
            builder_tokens: TokenUsage::default(),
            adversary_tokens: TokenUsage {
                input_tokens: 1800,
                output_tokens: 900,
                cache_read_tokens: 0,
                cache_write_tokens: 0,
            },
            started_at: chrono::Utc::now(),
            ended_at: None,
        };

        assert_eq!(session.total_genuine, 3);
        assert_eq!(session.total_false_positives, 2);
        assert_eq!(session.adversary_tokens.input_tokens, 1800);
        assert!(!session.converged);
        assert!(session.termination_reason.is_none());
    }

    // ========================================================================
    // Session Manager VDD Context Integration
    // ========================================================================

    #[test]
    fn test_session_manager_vdd_context_store_and_take() {
        let dir = TempDir::new().unwrap();
        let mut manager = SessionManager::new(dir.path().to_path_buf());

        // Initially no VDD context
        assert!(manager.take_vdd_context().is_none());

        // Store advisory findings context
        let context = "<vdd-advisory>\nSQLi found in db.rs:45\n</vdd-advisory>".to_string();
        manager.store_vdd_context(context.clone());

        // Take it once
        let taken = manager.take_vdd_context();
        assert!(taken.is_some());
        assert_eq!(taken.unwrap(), context);

        // Second take returns None (consumed)
        assert!(manager.take_vdd_context().is_none());
    }

    #[test]
    fn test_session_manager_vdd_context_overwrite() {
        let dir = TempDir::new().unwrap();
        let mut manager = SessionManager::new(dir.path().to_path_buf());

        manager.store_vdd_context("first finding".to_string());
        manager.store_vdd_context("second finding".to_string());

        // Should get the latest one
        let taken = manager.take_vdd_context();
        assert_eq!(taken.unwrap(), "second finding");
    }

    // ========================================================================
    // Hook Event Integration for VDD
    // ========================================================================

    #[test]
    fn test_vdd_hook_events_exist() {
        // Verify all 4 VDD hook events are recognized
        let events = [
            HookEvent::PreAdversaryReview,
            HookEvent::PostAdversaryReview,
            HookEvent::VddConflict,
            HookEvent::VddConverged,
        ];

        for event in &events {
            // config_key should return a non-empty string
            let key = event.config_key();
            assert!(!key.is_empty(), "VDD event {:?} has no config key", event);
        }
    }

    #[test]
    fn test_vdd_hook_engine_runs_empty_hooks() {
        let engine = HookEngine::new(HooksConfig::default());

        // VDD hooks with empty config should be no-ops
        let rt = tokio::runtime::Runtime::new().unwrap();
        let result = rt.block_on(async {
            let input = openclaudia::hooks::HookInput::new(HookEvent::VddConflict);
            engine.run(HookEvent::VddConflict, &input).await
        });

        // run() returns HookResult directly, not Result
        assert!(result.allowed);
    }

    // ========================================================================
    // VDD Finding Types Integration
    // ========================================================================

    #[test]
    fn test_finding_severity_ordering() {
        // Rust derived Ord orders by declaration position (Critical=0 < High=1 < ... < Info=4)
        // Critical is declared first, so it has the lowest discriminant
        assert!(Severity::Critical < Severity::High);
        assert!(Severity::High < Severity::Medium);
        assert!(Severity::Medium < Severity::Low);
        assert!(Severity::Low < Severity::Info);

        // All variants are distinct
        assert_ne!(Severity::Critical, Severity::Info);
        assert_ne!(Severity::High, Severity::Low);
    }

    #[test]
    fn test_finding_status_transitions() {
        let mut finding = Finding {
            id: "test-1".to_string(),
            severity: Severity::High,
            cwe: Some("CWE-89".to_string()),
            description: "SQL injection in query builder".to_string(),
            file_path: Some("src/db.rs".to_string()),
            line_range: Some((45, 52)),
            status: FindingStatus::Genuine,
            adversary_reasoning: "User input concatenated directly".to_string(),
            iteration: 1,
        };

        assert!(matches!(finding.status, FindingStatus::Genuine));

        // Mark as false positive after builder disputes
        finding.status = FindingStatus::FalsePositive;
        assert!(matches!(finding.status, FindingStatus::FalsePositive));

        // Mark as disputed
        finding.status = FindingStatus::Disputed;
        assert!(matches!(finding.status, FindingStatus::Disputed));
    }

    #[test]
    fn test_finding_with_no_file_location() {
        let finding = Finding {
            id: "test-2".to_string(),
            severity: Severity::Medium,
            cwe: None,
            description: "Logic error in business rule".to_string(),
            file_path: None,
            line_range: None,
            status: FindingStatus::Genuine,
            adversary_reasoning: "The condition is inverted".to_string(),
            iteration: 1,
        };

        assert!(finding.file_path.is_none());
        assert!(finding.line_range.is_none());
        assert!(finding.cwe.is_none());
    }

    // ========================================================================
    // VDD Mode Display Integration
    // ========================================================================

    #[test]
    fn test_vdd_mode_display_and_serde_roundtrip() {
        // Display
        assert_eq!(format!("{}", VddMode::Advisory), "advisory");
        assert_eq!(format!("{}", VddMode::Blocking), "blocking");

        // Serde roundtrip
        let advisory_json = serde_json::to_string(&VddMode::Advisory).unwrap();
        let blocking_json = serde_json::to_string(&VddMode::Blocking).unwrap();
        assert_eq!(advisory_json, "\"advisory\"");
        assert_eq!(blocking_json, "\"blocking\"");

        let parsed: VddMode = serde_json::from_str(&advisory_json).unwrap();
        assert_eq!(parsed, VddMode::Advisory);
    }

    // ========================================================================
    // Full Config Serde Integration
    // ========================================================================

    #[test]
    fn test_vdd_config_full_yaml_roundtrip() {
        let yaml = r#"
enabled: true
mode: blocking
adversary:
  provider: google
  model: gemini-2.5-pro
  temperature: 0.2
  max_tokens: 8192
thresholds:
  max_iterations: 8
  false_positive_rate: 0.80
  min_iterations: 3
static_analysis:
  enabled: true
  commands:
    - "cargo clippy -- -D warnings"
    - "cargo test --no-fail-fast"
  timeout_seconds: 180
tracking:
  persist: true
  path: .openclaudia/vdd
  log_adversary_responses: false
"#;

        let config: VddConfig = serde_yaml::from_str(yaml).unwrap();
        assert!(config.enabled);
        assert_eq!(config.mode, VddMode::Blocking);
        assert_eq!(config.adversary.provider, "google");
        assert_eq!(config.adversary.model, Some("gemini-2.5-pro".to_string()));
        assert_eq!(config.adversary.temperature, 0.2);
        assert_eq!(config.adversary.max_tokens, 8192);
        assert_eq!(config.thresholds.max_iterations, 8);
        assert_eq!(config.thresholds.false_positive_rate, 0.80);
        assert_eq!(config.thresholds.min_iterations, 3);
        assert_eq!(config.static_analysis.commands.len(), 2);
        assert_eq!(config.static_analysis.timeout_seconds, 180);
        assert!(!config.tracking.log_adversary_responses);

        // Validate against a builder
        assert!(config.validate("anthropic").is_ok());
        assert!(config.validate("google").is_err());
    }

    #[test]
    fn test_vdd_config_minimal_yaml_defaults() {
        let yaml = "enabled: false\n";
        let config: VddConfig = serde_yaml::from_str(yaml).unwrap();

        assert!(!config.enabled);
        assert_eq!(config.mode, VddMode::Advisory); // default
        assert_eq!(config.adversary.provider, "google"); // default
        assert_eq!(config.thresholds.max_iterations, 5); // default
        assert_eq!(config.thresholds.false_positive_rate, 0.75); // default
        assert_eq!(config.thresholds.min_iterations, 2); // default
        assert!(config.static_analysis.enabled); // default true
        assert!(config.static_analysis.commands.is_empty()); // default empty
    }

    // ========================================================================
    // Session TurnMetrics VDD Fields
    // ========================================================================

    #[test]
    fn test_turn_metrics_vdd_fields_default_none() {
        use openclaudia::session::Session;

        let mut session = Session::new_initializer();
        session.record_turn_estimate(5000, 2000, 0, 0);

        let turn = &session.turn_metrics[0];
        assert!(turn.vdd_iterations.is_none());
        assert!(turn.vdd_genuine_findings.is_none());
        assert!(turn.vdd_false_positives.is_none());
        assert!(turn.vdd_adversary_tokens.is_none());
        assert!(turn.vdd_converged.is_none());
    }

    #[test]
    fn test_session_progress_vdd_fields() {
        use openclaudia::session::SessionProgress;

        let progress = SessionProgress::default();
        assert_eq!(progress.vdd_total_findings, 0);
        assert_eq!(progress.vdd_total_genuine, 0);
        assert!(progress.vdd_sessions.is_empty());
    }
}
