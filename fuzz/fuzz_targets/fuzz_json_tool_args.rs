#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(args_str) = std::str::from_utf8(data) {
        // Tool execution with arbitrary JSON arguments should never panic
        let tools = [
            "read_file", "write_file", "edit_file", "list_files",
            "bash", "web_fetch", "web_search", "chainlink",
            "todo_write", "todo_read", "bash_output", "kill_shell",
        ];
        for tool_name in tools {
            let _ = openclaudia::tools::execute_tool(&openclaudia::tools::ToolCall {
                id: "fuzz".to_string(),
                call_type: "function".to_string(),
                function: openclaudia::tools::FunctionCall {
                    name: tool_name.to_string(),
                    arguments: args_str.to_string(),
                },
            });
        }
    }
});
