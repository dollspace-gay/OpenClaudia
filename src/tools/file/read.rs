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
