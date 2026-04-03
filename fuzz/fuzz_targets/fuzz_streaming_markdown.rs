#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(s) = std::str::from_utf8(data) {
        // StreamingMarkdownRenderer should never panic on any input
        let mut renderer = openclaudia::tui::StreamingMarkdownRenderer::new();

        // Feed chunks of various sizes
        let chunk_sizes = [1, 3, 7, 13, 50, 200];
        let mut pos = 0;
        for &size in chunk_sizes.iter().cycle() {
            if pos >= s.len() {
                break;
            }
            let end = (pos + size).min(s.len());
            // Find a valid char boundary
            let end = s.floor_char_boundary(end);
            if end <= pos {
                break;
            }
            renderer.push(&s[pos..end]);
            pos = end;
        }
        renderer.flush();
    }
});
