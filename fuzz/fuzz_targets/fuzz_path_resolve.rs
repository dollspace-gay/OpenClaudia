#![no_main]
use libfuzzer_sys::fuzz_target;
use std::collections::HashMap;

fuzz_target!(|data: &[u8]| {
    if let Ok(path) = std::str::from_utf8(data) {
        // Path resolution in file tools should never panic
        let args = HashMap::from([
            ("path".to_string(), serde_json::json!(path)),
        ]);

        // read_file with arbitrary paths
        let _ = openclaudia::tools::execute_tool(&openclaudia::tools::ToolCall {
            id: "fuzz".to_string(),
            call_type: "function".to_string(),
            function: openclaudia::tools::FunctionCall {
                name: "read_file".to_string(),
                arguments: serde_json::to_string(&args).unwrap_or_default(),
            },
        });

        // list_files with arbitrary paths
        let _ = openclaudia::tools::execute_tool(&openclaudia::tools::ToolCall {
            id: "fuzz".to_string(),
            call_type: "function".to_string(),
            function: openclaudia::tools::FunctionCall {
                name: "list_files".to_string(),
                arguments: serde_json::to_string(&args).unwrap_or_default(),
            },
        });
    }
});
