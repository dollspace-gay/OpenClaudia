#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(s) = std::str::from_utf8(data) {
        // Try parsing as a JSON array of messages
        if let Ok(messages) = serde_json::from_str::<Vec<serde_json::Value>>(s) {
            // Message conversion should never panic
            let _ = openclaudia::providers::convert_messages_to_anthropic(&messages);
        }

        // Try parsing as a single message
        if let Ok(msg) = serde_json::from_str::<serde_json::Value>(s) {
            let messages = vec![msg];
            let _ = openclaudia::providers::convert_messages_to_anthropic(&messages);
        }
    }
});
