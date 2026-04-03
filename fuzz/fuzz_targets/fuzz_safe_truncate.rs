#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(s) = std::str::from_utf8(data) {
        // Fuzz safe_truncate at various lengths
        for max in [0, 1, 3, 7, 47, 77, 100, 197, 200, 297, 300, 500] {
            let result = openclaudia::tools::safe_truncate(s, max);
            // Result must be valid UTF-8 and <= max bytes
            assert!(result.len() <= max);
            assert!(result.is_char_boundary(result.len()));
        }
    }
});
