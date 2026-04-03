#![no_main]
use libfuzzer_sys::fuzz_target;
use openclaudia::tools::{AnthropicToolAccumulator, ToolCallAccumulator};

fuzz_target!(|data: &[u8]| {
    if let Ok(s) = std::str::from_utf8(data) {
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(s) {
            let mut anthropic_acc = AnthropicToolAccumulator::new();
            let mut tool_acc = ToolCallAccumulator::new();

            // Should never panic regardless of JSON shape
            let _ = openclaudia::pipeline::process_sse_event(
                &json,
                false,
                &mut anthropic_acc,
                &mut tool_acc,
            );

            // Also test with in_thinking_block=true
            let _ = openclaudia::pipeline::process_sse_event(
                &json,
                true,
                &mut anthropic_acc,
                &mut tool_acc,
            );
        }
    }
});
