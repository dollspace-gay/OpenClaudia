#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(s) = std::str::from_utf8(data) {
        // Try parsing as a JSON message array
        if let Ok(messages) = serde_json::from_str::<Vec<serde_json::Value>>(s) {
            // Should never panic regardless of message content
            for provider in ["anthropic", "openai", "google", "unknown"] {
                for effort in ["low", "medium", "high", "invalid"] {
                    let _ = openclaudia::pipeline::build_request(
                        provider,
                        "test-model",
                        &messages,
                        effort,
                        None,
                    );
                }
            }
        }
    }
});
