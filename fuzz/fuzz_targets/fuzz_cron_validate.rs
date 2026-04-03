#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(s) = std::str::from_utf8(data) {
        // Cron expression validation should never panic
        let args = std::collections::HashMap::from([
            ("expression".to_string(), serde_json::json!(s)),
            ("command".to_string(), serde_json::json!("echo test")),
            ("name".to_string(), serde_json::json!("fuzz-test")),
        ]);
        let _ = openclaudia::tools::execute_tool(&openclaudia::tools::ToolCall {
            id: "fuzz".to_string(),
            call_type: "function".to_string(),
            function: openclaudia::tools::FunctionCall {
                name: "cron_create".to_string(),
                arguments: serde_json::to_string(&args).unwrap_or_default(),
            },
        });
    }
});
