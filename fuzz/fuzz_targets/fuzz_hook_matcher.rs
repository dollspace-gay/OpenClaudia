#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(s) = std::str::from_utf8(data) {
        // Split input: first half is pattern, second half is context
        // Use floor_char_boundary to avoid splitting multi-byte chars
        let mid = s.len() / 2;
        let mid = s.floor_char_boundary(mid);
        let pattern = &s[..mid];
        let context = &s[mid..];

        // Regex compilation and matching should never panic
        // (should return Err on invalid patterns, not panic)
        let re_result = regex::RegexBuilder::new(pattern)
            .size_limit(10 * 1024)
            .build();
        if let Ok(re) = re_result {
            let _ = re.is_match(context);
        }
    }
});
