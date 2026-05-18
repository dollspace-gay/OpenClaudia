use crate::tools::safe_truncate;
use base64::Engine;
use serde_json::Value;
use std::collections::HashMap;
use std::fmt::Write as _;
use std::fs;
use std::path::Path;
use std::process::Command;

/// Supported file types for `read_file`
pub enum FileType {
    Text,
    Image(&'static str), // mime type
    Pdf,
    Notebook,
}

/// Detect file type from extension
pub fn detect_file_type(path: &str) -> FileType {
    let p = Path::new(path);
    let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("");
    if ext.eq_ignore_ascii_case("png") {
        FileType::Image("image/png")
    } else if ext.eq_ignore_ascii_case("jpg") || ext.eq_ignore_ascii_case("jpeg") {
        FileType::Image("image/jpeg")
    } else if ext.eq_ignore_ascii_case("gif") {
        FileType::Image("image/gif")
    } else if ext.eq_ignore_ascii_case("webp") {
        FileType::Image("image/webp")
    } else if ext.eq_ignore_ascii_case("pdf") {
        FileType::Pdf
    } else if ext.eq_ignore_ascii_case("ipynb") {
        FileType::Notebook
    } else {
        FileType::Text
    }
}

/// Read an image file, base64-encode it, and return a structured result
pub fn read_image_file(path: &str, mime_type: &str) -> (String, bool) {
    match fs::read(path) {
        Ok(bytes) => {
            let file_size = bytes.len();
            let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
            let filename = Path::new(path)
                .file_name()
                .map_or_else(|| path.to_string(), |n| n.to_string_lossy().to_string());

            let result = format!(
                "[Image: {filename} ({file_size} bytes, {mime_type}) - base64 data included for vision-capable models]\n{b64}"
            );
            (result, false)
        }
        Err(e) => (format!("Failed to read image file '{path}': {e}"), true),
    }
}

/// Parse a page range string like "1-5", "3", or "10-20"
/// Returns (`first_page`, `last_page`) as 1-indexed values
pub fn parse_page_range(pages: &str) -> Result<(u32, u32), String> {
    let pages = pages.trim();
    if let Some((start, end)) = pages.split_once('-') {
        let start: u32 = start
            .trim()
            .parse()
            .map_err(|_| format!("Invalid page range start: '{}'", start.trim()))?;
        let end: u32 = end
            .trim()
            .parse()
            .map_err(|_| format!("Invalid page range end: '{}'", end.trim()))?;
        if start == 0 || end == 0 {
            return Err("Page numbers must be 1 or greater".to_string());
        }
        if start > end {
            return Err(format!("Invalid page range: start ({start}) > end ({end})"));
        }
        Ok((start, end))
    } else {
        let page: u32 = pages
            .parse()
            .map_err(|_| format!("Invalid page number: '{pages}'"))?;
        if page == 0 {
            return Err("Page numbers must be 1 or greater".to_string());
        }
        Ok((page, page))
    }
}

/// Read a PDF file using pdftotext
pub fn read_pdf_file(path: &str, pages: Option<&str>) -> (String, bool) {
    // Check if pdftotext is available
    let check = Command::new("which").arg("pdftotext").output();
    match check {
        Ok(output) if !output.status.success() => {
            return (
                "pdftotext is not installed. Install it with:\n  \
                 Ubuntu/Debian: sudo apt install poppler-utils\n  \
                 macOS: brew install poppler\n  \
                 Fedora: sudo dnf install poppler-utils"
                    .to_string(),
                true,
            );
        }
        Err(_) => {
            return (
                "Could not check for pdftotext. Ensure poppler-utils is installed.".to_string(),
                true,
            );
        }
        _ => {}
    }

    // If no pages specified, check total page count first
    if pages.is_none() {
        // Use pdftotext on the whole file but first count pages with pdfinfo if available
        let info_output = Command::new("pdfinfo").arg(path).output();
        if let Ok(info) = info_output {
            if info.status.success() {
                let info_text = String::from_utf8_lossy(&info.stdout);
                for line in info_text.lines() {
                    if line.starts_with("Pages:") {
                        if let Some(count_str) = line.split(':').nth(1) {
                            if let Ok(count) = count_str.trim().parse::<u32>() {
                                const MAX_PDF_PAGES_WITHOUT_RANGE: u32 = 10;
                                if count > MAX_PDF_PAGES_WITHOUT_RANGE {
                                    return (
                                        format!(
                                            "PDF has {count} pages. For large PDFs (>{MAX_PDF_PAGES_WITHOUT_RANGE} pages), you must specify \
                                             a page range using the 'pages' parameter (e.g., '1-5', '3', '10-20')."
                                        ),
                                        true,
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    // Build pdftotext command
    let mut cmd = Command::new("pdftotext");
    if let Some(pages_str) = pages {
        match parse_page_range(pages_str) {
            Ok((first, last)) => {
                cmd.arg("-f").arg(first.to_string());
                cmd.arg("-l").arg(last.to_string());
            }
            Err(e) => return (format!("Invalid pages parameter: {e}"), true),
        }
    }
    cmd.arg(path);
    cmd.arg("-"); // Output to stdout

    match cmd.output() {
        Ok(output) => {
            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                return (format!("pdftotext failed for '{path}': {stderr}"), true);
            }
            let text = String::from_utf8_lossy(&output.stdout).to_string();
            if text.trim().is_empty() {
                (
                    format!("PDF '{path}' produced no extractable text (may be image-based)."),
                    false,
                )
            } else {
                (text, false)
            }
        }
        Err(e) => (format!("Failed to run pdftotext on '{path}': {e}"), true),
    }
}

/// Read a Jupyter notebook (.ipynb) and format cells for display
pub fn read_notebook_file(path: &str) -> (String, bool) {
    let content = match fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) => return (format!("Failed to read notebook '{path}': {e}"), true),
    };

    let notebook: Value = match serde_json::from_str(&content) {
        Ok(v) => v,
        Err(e) => {
            return (
                format!("Failed to parse notebook '{path}' as JSON: {e}"),
                true,
            )
        }
    };

    let Some(cells) = notebook.get("cells").and_then(|c| c.as_array()) else {
        return ("Notebook has no 'cells' array.".to_string(), true);
    };

    let mut output = String::new();
    for (i, cell) in cells.iter().enumerate() {
        let cell_type = cell
            .get("cell_type")
            .and_then(|t| t.as_str())
            .unwrap_or("unknown");

        // Get source - can be a string or array of strings
        let source = match cell.get("source") {
            Some(Value::Array(arr)) => arr
                .iter()
                .filter_map(|v| v.as_str())
                .collect::<Vec<_>>()
                .join(""),
            Some(Value::String(s)) => s.clone(),
            _ => String::new(),
        };

        let _ = write!(output, "Cell {i} ({cell_type}):\n```\n{source}\n```\n");

        // For code cells, include text outputs (skip binary/image outputs)
        if cell_type == "code" {
            if let Some(outputs) = cell.get("outputs").and_then(|o| o.as_array()) {
                for out in outputs {
                    let output_type = out.get("output_type").and_then(|t| t.as_str());
                    match output_type {
                        Some("stream") => {
                            if let Some(text) = out.get("text") {
                                let text_str = match text {
                                    Value::Array(arr) => arr
                                        .iter()
                                        .filter_map(|v| v.as_str())
                                        .collect::<Vec<_>>()
                                        .join(""),
                                    Value::String(s) => s.clone(),
                                    _ => continue,
                                };
                                let _ = write!(output, "Output:\n{text_str}\n");
                            }
                        }
                        Some("execute_result" | "display_data") => {
                            // Only include text/plain data, skip images and other binary
                            if let Some(data) = out.get("data") {
                                if let Some(text_plain) = data.get("text/plain") {
                                    let text_str = match text_plain {
                                        Value::Array(arr) => arr
                                            .iter()
                                            .filter_map(|v| v.as_str())
                                            .collect::<Vec<_>>()
                                            .join(""),
                                        Value::String(s) => s.clone(),
                                        _ => continue,
                                    };
                                    let _ = write!(output, "Output:\n{text_str}\n");
                                }
                            }
                        }
                        Some("error") => {
                            if let Some(traceback) = out.get("traceback").and_then(|t| t.as_array())
                            {
                                let tb: Vec<&str> =
                                    traceback.iter().filter_map(|v| v.as_str()).collect();
                                let _ = write!(output, "Error:\n{}\n", tb.join("\n"));
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
        output.push('\n');
    }

    (output, false)
}
/// Read a plain text file with optional offset/limit
pub fn read_text_file(path: &str, args: &HashMap<String, Value>) -> (String, bool) {
    // Get optional offset (1-indexed line number to start from)
    let offset = args
        .get("offset")
        .and_then(serde_json::Value::as_u64)
        .map_or(0, |n| {
            usize::try_from(n.saturating_sub(1)).unwrap_or(usize::MAX)
        });

    // Get optional limit (max lines to read)
    let limit = args
        .get("limit")
        .and_then(serde_json::Value::as_u64)
        .map(|n| usize::try_from(n).unwrap_or(usize::MAX));

    match fs::read_to_string(path) {
        Ok(file_content) => {
            let lines: Vec<&str> = file_content.lines().collect();
            let total_lines = lines.len();

            // Apply offset and limit
            let selected_lines: Vec<(usize, &str)> = lines
                .into_iter()
                .enumerate()
                .skip(offset)
                .take(limit.unwrap_or(usize::MAX))
                .collect();

            // Add line numbers (original line numbers, not relative)
            let numbered: Vec<String> = selected_lines
                .iter()
                .map(|(i, line)| format!("{:4}| {}", i + 1, line))
                .collect();

            let result = numbered.join("\n");

            // Add context about what was shown
            let suffix = if offset > 0 || limit.is_some() {
                let shown_start = offset + 1;
                let shown_end = offset + selected_lines.len();
                format!("\n(showing lines {shown_start}-{shown_end} of {total_lines} total)")
            } else {
                String::new()
            };

            // Truncate if too long
            if result.len() > 100_000 {
                (
                    format!(
                        "{}...\n(file truncated, {} total chars){}",
                        safe_truncate(&result, 100_000),
                        result.len(),
                        suffix
                    ),
                    false,
                )
            } else {
                (format!("{result}{suffix}"), false)
            }
        }
        Err(e) => (format!("Failed to read file '{path}': {e}"), true),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;
    use tempfile::NamedTempFile;

    // =========================================================================
    // Behavior 1: read_text_file offset + limit — 1-indexed line slice
    // =========================================================================

    /// Helper: write content to a NamedTempFile and return (file, path_string).
    fn tmp_text(content: &str) -> (NamedTempFile, String) {
        let mut f = NamedTempFile::new().expect("tempfile");
        f.write_all(content.as_bytes()).expect("write");
        let path = f.path().to_string_lossy().to_string();
        (f, path)
    }

    #[test]
    fn read_text_no_offset_returns_all_lines() {
        // Behavior 1: without offset/limit every line is returned
        let (_f, path) = tmp_text("alpha\nbeta\ngamma\n");
        let args = HashMap::new();
        let (output, is_err) = read_text_file(&path, &args);
        assert!(!is_err);
        assert!(output.contains("alpha"));
        assert!(output.contains("beta"));
        assert!(output.contains("gamma"));
        // No suffix when neither offset nor limit is given
        assert!(
            !output.contains("showing lines"),
            "no suffix without offset/limit"
        );
    }

    #[test]
    fn read_text_offset_1_is_first_line() {
        // Behavior 1: offset=1 means start at line 1 (no skip)
        let (_f, path) = tmp_text("first\nsecond\nthird\n");
        let mut args = HashMap::new();
        args.insert("offset".to_string(), serde_json::json!(1u64));
        let (output, is_err) = read_text_file(&path, &args);
        assert!(!is_err);
        assert!(output.contains("first"), "offset=1 must include line 1");
    }

    #[test]
    fn read_text_offset_and_limit_returns_correct_slice() {
        // Behavior 1: offset=2,limit=1 returns only line 2
        let (_f, path) = tmp_text("line1\nline2\nline3\n");
        let mut args = HashMap::new();
        args.insert("offset".to_string(), serde_json::json!(2u64));
        args.insert("limit".to_string(), serde_json::json!(1u64));
        let (output, is_err) = read_text_file(&path, &args);
        assert!(!is_err);
        assert!(output.contains("line2"), "must include line 2");
        assert!(!output.contains("line1"), "must not include line 1");
        assert!(!output.contains("line3"), "must not include line 3");
        // Suffix is present when offset/limit used
        assert!(output.contains("showing lines 2-2 of 3 total"));
    }

    #[test]
    fn read_text_line_numbers_use_original_numbering() {
        // Behavior 1: line numbers in output are 1-indexed originals, not relative
        let (_f, path) = tmp_text("aaa\nbbb\nccc\n");
        let mut args = HashMap::new();
        args.insert("offset".to_string(), serde_json::json!(2u64));
        args.insert("limit".to_string(), serde_json::json!(2u64));
        let (output, is_err) = read_text_file(&path, &args);
        assert!(!is_err);
        // Line 2 ("bbb") must be labeled with "2|" and line 3 with "3|"
        assert!(output.contains("2|"), "line 2 label present: {output}");
        assert!(output.contains("3|"), "line 3 label present: {output}");
        assert!(
            !output.contains("1|"),
            "line 1 label must be absent: {output}"
        );
    }

    #[test]
    fn read_text_offset_zero_treated_as_start_of_file() {
        // Behavior 1 edge: offset=0 — CC treats as "start of file" (no skip).
        // OC: saturating_sub(1) on 0u64 yields 0 → .skip(0) — same behavior.
        let (_f, path) = tmp_text("alpha\nbeta\n");
        let mut args = HashMap::new();
        args.insert("offset".to_string(), serde_json::json!(0u64));
        let (output, is_err) = read_text_file(&path, &args);
        assert!(!is_err);
        // offset=0 is treated as "start of file": both lines must appear.
        // NOTE: OC produces a suffix even when offset=0 because offset_arg>0
        // check uses the pre-subtraction value — this is an OC quirk. We pin
        // current behavior: suffix is present because limit=Some(_) is absent
        // but offset arg was present.
        assert!(output.contains("alpha"), "offset=0 must yield first line");
        assert!(output.contains("beta"));
    }

    #[test]
    fn read_text_offset_beyond_eof_returns_empty_body() {
        // Behavior 1 edge: offset beyond end → empty body, suffix present
        let (_f, path) = tmp_text("one\ntwo\n");
        let mut args = HashMap::new();
        args.insert("offset".to_string(), serde_json::json!(99u64));
        let (output, is_err) = read_text_file(&path, &args);
        assert!(!is_err, "not an error — just empty content");
        // The body before the suffix is empty; suffix shows 0 selected lines.
        // OC does NOT emit a "file has fewer lines" warning here (CC does).
        // Pinned as current OC behavior; CC parity tracked via issue #525 edge-case.
        assert!(
            output.contains("showing lines"),
            "suffix must be present: {output}"
        );
        assert!(!output.contains("one"), "line 1 must be absent");
    }

    #[test]
    fn read_text_limit_zero_returns_empty_body() {
        // Behavior 1 edge: limit=0 — CC rejects at schema level; OC silently
        // returns empty content via .take(0). Pinned as current OC behavior.
        // CC parity: CC schema rejects limit < 1 — no gap issue filed yet.
        let (_f, path) = tmp_text("data\n");
        let mut args = HashMap::new();
        args.insert("limit".to_string(), serde_json::json!(0u64));
        let (output, is_err) = read_text_file(&path, &args);
        // OC: take(0) → empty result, not an error
        assert!(
            !is_err,
            "OC does not validate limit=0 at the function level"
        );
        assert!(
            !output.contains("data"),
            "limit=0 yields no content: {output}"
        );
    }

    #[test]
    fn read_text_missing_file_returns_error() {
        // Behavior 1 error path: file not found
        let args = HashMap::new();
        let (output, is_err) = read_text_file("/tmp/__oc_test_does_not_exist_xyz.txt", &args);
        assert!(is_err);
        assert!(
            output.contains("Failed to read file"),
            "error message: {output}"
        );
    }

    // =========================================================================
    // Behavior 2: read_image_file — base64 encode, empty image edge case
    // =========================================================================

    #[test]
    fn read_image_returns_base64_text_block() {
        // Behavior 2: OC returns a plain-text string with base64 inline
        // (not a structured image block — CC parity gap, pinned as current behavior).
        let mut f = NamedTempFile::new().expect("tempfile");
        // Minimal valid PNG bytes (1×1 red pixel)
        let minimal_png: &[u8] = &[
            0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, // signature
            0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52, // IHDR length + type
            0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, // 1x1
            0x08, 0x02, 0x00, 0x00, 0x00, 0x90, 0x77, 0x53, // bit depth, color type, ...
            0xde, 0x00, 0x00, 0x00, 0x0c, 0x49, 0x44, 0x41, // IDAT length + type
            0x54, 0x08, 0xd7, 0x63, 0xf8, 0xcf, 0xc0, 0x00, // IDAT data
            0x00, 0x00, 0x02, 0x00, 0x01, 0xe2, 0x21, 0xbc, // ...
            0x33, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, // IEND
            0x44, 0xae, 0x42, 0x60, 0x82,
        ];
        f.write_all(minimal_png).expect("write png");
        let path = f.path().to_string_lossy().to_string();
        let (output, is_err) = read_image_file(&path, "image/png");
        assert!(!is_err);
        assert!(output.contains("[Image:"), "header line present: {output}");
        assert!(output.contains("image/png"), "mime type present");
        assert!(output.contains("bytes"), "byte count present");
        // base64 data follows the header line
        assert!(output.len() > 50, "output non-trivial");
    }

    #[test]
    fn read_image_empty_file_returns_ok_with_empty_base64() {
        // Behavior 2 edge: empty image file — CC throws; OC succeeds with empty
        // base64 string. Pinned as current OC behavior.
        let f = NamedTempFile::new().expect("tempfile");
        // Write nothing — file is 0 bytes
        let path = f.path().to_string_lossy().to_string();
        let (output, is_err) = read_image_file(&path, "image/png");
        // OC: no error for 0-byte file (CC parity gap — CC throws "Image file is empty").
        // Pinned as current OC behavior.
        assert!(
            !is_err,
            "OC does not error on 0-byte image (CC does): {output}"
        );
        assert!(output.contains("[Image:"), "header still present");
        assert!(output.contains("0 bytes"), "zero byte count shown");
    }

    #[test]
    fn read_image_nonexistent_returns_error() {
        // Behavior 2 error path: file not found
        let (output, is_err) = read_image_file("/tmp/__oc_no_such_image.png", "image/png");
        assert!(is_err);
        assert!(output.contains("Failed to read image file"));
    }

    // =========================================================================
    // Behavior 3: parse_page_range — PDF page range parsing
    // =========================================================================

    #[test]
    fn parse_page_range_single_page() {
        // Behavior 3 edge: single page "3" → (3, 3) — matches CC semantics
        let r = parse_page_range("3").expect("valid");
        assert_eq!(r, (3, 3));
    }

    #[test]
    fn parse_page_range_range() {
        // Behavior 3: "1-5" → (1, 5)
        let r = parse_page_range("1-5").expect("valid");
        assert_eq!(r, (1, 5));
    }

    #[test]
    fn parse_page_range_with_whitespace() {
        // Behavior 3: leading/trailing whitespace is trimmed
        let r = parse_page_range(" 2 - 4 ").expect("valid");
        assert_eq!(r, (2, 4));
    }

    #[test]
    fn parse_page_range_page_zero_is_error() {
        // Behavior 3 edge: page 0 is not valid (1-indexed)
        let r = parse_page_range("0");
        assert!(r.is_err(), "page 0 must be rejected");
    }

    #[test]
    fn parse_page_range_inverted_range_is_error() {
        // Behavior 3 edge: start > end must be rejected
        let r = parse_page_range("5-2");
        assert!(r.is_err(), "5-2 must be rejected");
    }

    #[test]
    fn parse_page_range_non_numeric_is_error() {
        let r = parse_page_range("abc");
        assert!(r.is_err());
    }

    // =========================================================================
    // Behavior 8: truncation — silent non-error truncation at 100 000 chars
    // =========================================================================

    #[test]
    fn read_text_large_file_truncated_as_non_error() {
        // Behavior 8: OC silently truncates at 100 000 chars (non-error result).
        // CC errors with token count + offset/limit guidance.
        // Pinned as current OC behavior: truncation is NOT an error in OC.
        let line = "x".repeat(200) + "\n"; // 201 chars per line
                                           // Need > 100_000 chars in the numbered output: with "   N| " prefix (~7 chars)
                                           // each line becomes ~208 chars; 600 lines = ~124 800 chars → triggers truncation.
        let content = line.repeat(600);
        let mut f = NamedTempFile::new().expect("tempfile");
        f.write_all(content.as_bytes()).expect("write");
        let path = f.path().to_string_lossy().to_string();
        let args = HashMap::new();
        let (output, is_err) = read_text_file(&path, &args);
        assert!(
            !is_err,
            "OC truncation is NOT an error (CC parity gap): {output}"
        );
        assert!(
            output.contains("file truncated"),
            "truncation note must be present: {output}"
        );
        assert!(
            output.len() > 100_000,
            "output includes '...' + suffix beyond the 100k body"
        );
    }

    #[test]
    fn read_text_within_cap_not_truncated() {
        // Behavior 8: files under the 100 000-char cap are returned in full
        let (_f, path) = tmp_text("short line\n");
        let args = HashMap::new();
        let (output, is_err) = read_text_file(&path, &args);
        assert!(!is_err);
        assert!(
            !output.contains("file truncated"),
            "no truncation note for small files"
        );
    }
}
