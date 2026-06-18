//! Tool Interception for Claude Code Proxy Mode
//!
//! Parses Claude's XML-style tool invocations from the response stream
//! and executes them locally instead of letting Anthropic's sandbox handle them.
//!
//! Claude Code uses an XML format with `antml:function_calls` and antml:invoke tags.
//! This module parses those invocations and maps them to local tool execution.

use crate::tools::{safe_truncate, FunctionCall, ToolCall};
use std::collections::HashMap;
use std::fmt::Write;
use std::sync::LazyLock;
use uuid::Uuid;

/// Per-tool alias metadata used by the interceptor.
///
/// Each entry carries both the canonical (internal) tool name and the set of
/// parameter-name aliases the tool accepts, keyed by the *aliased* parameter
/// name with the canonical parameter name as the value.
///
/// This is the single source of truth for tool-name and parameter-name
/// translation in proxy-mode interception (see crosslink #477).
pub struct ToolAliasInfo {
    /// Canonical internal tool name (e.g. `read_file`, `list_files`).
    pub canonical: &'static str,
    /// Parameter-name aliases: `aliased_name -> canonical_name`.
    ///
    /// Entries where the alias equals the canonical name are included so
    /// callers do not need a separate "passthrough" code path.
    pub parameter_aliases: HashMap<&'static str, &'static str>,
}

/// Single source of truth mapping Claude-Code-style tool names (lowercased)
/// to their canonical internal name and per-tool parameter-name aliases.
///
/// Keys are the *aliased* (Claude-Code) tool names in lowercase. The canonical
/// internal tool name is also included as a key so the table is self-consistent
/// when a model emits the canonical name directly.
pub static TOOL_ALIASES: LazyLock<HashMap<&'static str, ToolAliasInfo>> = LazyLock::new(|| {
    let mut table: HashMap<&'static str, ToolAliasInfo> = HashMap::new();

    // --- bash ---
    let bash_params: HashMap<&'static str, &'static str> = [("command", "command")].into();
    table.insert(
        "bash",
        ToolAliasInfo {
            canonical: "bash",
            parameter_aliases: bash_params,
        },
    );

    // --- read / read_file ---
    let read_params: HashMap<&'static str, &'static str> =
        [("file_path", "path"), ("path", "path")].into();
    for name in ["read", "read_file"] {
        table.insert(
            name,
            ToolAliasInfo {
                canonical: "read_file",
                parameter_aliases: read_params.clone(),
            },
        );
    }

    // --- write / write_file ---
    let write_params: HashMap<&'static str, &'static str> = [
        ("file_path", "path"),
        ("path", "path"),
        ("content", "content"),
        ("contents", "content"),
    ]
    .into();
    for name in ["write", "write_file"] {
        table.insert(
            name,
            ToolAliasInfo {
                canonical: "write_file",
                parameter_aliases: write_params.clone(),
            },
        );
    }

    // --- edit / edit_file ---
    let edit_params: HashMap<&'static str, &'static str> = [
        ("file_path", "path"),
        ("path", "path"),
        ("old_string", "old_string"),
        ("new_string", "new_string"),
    ]
    .into();
    for name in ["edit", "edit_file"] {
        table.insert(
            name,
            ToolAliasInfo {
                canonical: "edit_file",
                parameter_aliases: edit_params.clone(),
            },
        );
    }

    // --- glob / list_files ---
    let glob_params: HashMap<&'static str, &'static str> =
        [("path", "path"), ("pattern", "pattern")].into();
    for name in ["glob", "list_files"] {
        table.insert(
            name,
            ToolAliasInfo {
                canonical: "list_files",
                parameter_aliases: glob_params.clone(),
            },
        );
    }

    // --- grep ---
    let grep_params: HashMap<&'static str, &'static str> =
        [("path", "path"), ("pattern", "pattern")].into();
    table.insert(
        "grep",
        ToolAliasInfo {
            canonical: "grep",
            parameter_aliases: grep_params,
        },
    );

    // --- web_fetch / webfetch ---
    let web_fetch_params: HashMap<&'static str, &'static str> = HashMap::new();
    for name in ["webfetch", "web_fetch"] {
        table.insert(
            name,
            ToolAliasInfo {
                canonical: "web_fetch",
                parameter_aliases: web_fetch_params.clone(),
            },
        );
    }

    // --- web_search / websearch ---
    let web_search_params: HashMap<&'static str, &'static str> = HashMap::new();
    for name in ["websearch", "web_search"] {
        table.insert(
            name,
            ToolAliasInfo {
                canonical: "web_search",
                parameter_aliases: web_search_params.clone(),
            },
        );
    }

    table
});

/// A parsed tool invocation from Claude's response
#[derive(Debug, Clone)]
pub struct InterceptedToolCall {
    /// The tool name (e.g., "Bash", "Read", "Write")
    pub name: String,
    /// Parameters for the tool
    pub parameters: HashMap<String, String>,
    /// Generated ID for tracking
    pub id: String,
}

impl InterceptedToolCall {
    /// Convert to a `ToolCall` that can be executed by our tool system.
    ///
    /// Both the tool name and parameter names are resolved through a single
    /// source of truth: [`TOOL_ALIASES`]. Unknown tool names pass through
    /// lowercased; unknown parameter names pass through unchanged. This means
    /// any tool that does not appear in [`TOOL_ALIASES`] (for example
    /// `ask_user_question`, `task_*`, MCP tools) still routes by its bare name
    /// without a silent rename.
    #[must_use]
    pub fn to_tool_call(&self) -> ToolCall {
        let name_lower = self.name.to_lowercase();
        let alias_info = TOOL_ALIASES.get(name_lower.as_str());

        let internal_name = alias_info.map_or(name_lower.as_str(), |info| info.canonical);

        let mut args = serde_json::Map::new();
        for (key, value) in &self.parameters {
            let internal_key = alias_info
                .and_then(|info| info.parameter_aliases.get(key.as_str()).copied())
                .unwrap_or(key.as_str());
            args.insert(
                internal_key.to_string(),
                serde_json::Value::String(value.clone()),
            );
        }

        ToolCall {
            id: self.id.clone(),
            call_type: "function".to_string(),
            function: FunctionCall {
                name: internal_name.to_string(),
                arguments: serde_json::to_string(&args).unwrap_or_default(),
            },
        }
    }
}

/// Incrementally maintained results of the marker scan over [`ToolInterceptor::buffer`].
///
/// Crosslink #743: the naive implementation of `has_pending_tool_calls` and
/// `has_complete_block` re-scanned the entire buffer for every shorthand-tool
/// open/close marker on every poll (up to 19 substring scans per call, called
/// once per streaming push and once per loop iteration in `extract_all_tool_calls`).
/// Total cost on an N-byte buffer accumulated over K pushes was O(K * N * M).
///
/// We now keep a single position cursor (`scan_pos`) and walk the buffer once,
/// recognising every marker (`<invoke name="`, `</invoke>`, `<tool>`, `<tool `,
/// `</tool>`) at each `<` byte. Subsequent pushes only re-scan the new suffix
/// (with a small backtrack to catch markers that straddle a chunk boundary), so
/// total cost is amortised O(N) — one byte read per buffer byte, plus a small
/// constant per `<` we encounter.
#[derive(Debug, Clone, Default)]
struct ScanState {
    /// Buffer offset already consumed by the scanner.
    scan_pos: usize,
    /// Earliest byte offset where `<invoke name="` was observed.
    invoke_open_at: Option<usize>,
    /// Whether `</invoke>` has been observed at or after `invoke_open_at`.
    invoke_closed: bool,
    /// Per-shorthand-tool flag: did we observe an open marker (`<tool>` or `<tool `)?
    shorthand_open: [bool; SHORTHAND_TOOL_COUNT],
    /// Per-shorthand-tool flag: did we observe a close marker `</tool>`?
    shorthand_close: [bool; SHORTHAND_TOOL_COUNT],
}

/// Number of entries in [`ToolInterceptor::SHORTHAND_TOOLS`].
///
/// Hard-coded so [`ScanState`] can store per-tool flags in fixed-size arrays
/// (and a compile-time assertion below ensures the two stay in sync).
const SHORTHAND_TOOL_COUNT: usize = 9;

/// Parser for Claude's XML-style tool invocations
pub struct ToolInterceptor {
    /// Accumulated content that may contain tool calls
    buffer: String,
    /// Whether we're currently inside a `function_calls` block
    in_function_calls: bool,
    /// Position-cached marker scan over [`Self::buffer`] (crosslink #743).
    ///
    /// Reset to default any time the buffer shrinks or is rewritten in-place;
    /// extended in-place by [`Self::extend_scan`] when the buffer only grew.
    scan: ScanState,
}

impl Default for ToolInterceptor {
    fn default() -> Self {
        Self::new()
    }
}

impl ToolInterceptor {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            buffer: String::new(),
            in_function_calls: false,
            scan: ScanState {
                scan_pos: 0,
                invoke_open_at: None,
                invoke_closed: false,
                shorthand_open: [false; SHORTHAND_TOOL_COUNT],
                shorthand_close: [false; SHORTHAND_TOOL_COUNT],
            },
        }
    }

    /// Maximum size, in bytes, the intercept buffer is allowed to grow
    /// to before [`Self::push`] starts dropping content (crosslink #343).
    ///
    /// 4 MiB covers every legitimate model output by an order of magnitude
    /// — Anthropic's typical max-completion budget tops out at 200 KB of
    /// text — while bounding the worst-case memory footprint of a single
    /// stream that keeps emitting `<bash>…` shorthand without ever closing
    /// the tag. The push-side cap means a single stream cannot grow the
    /// buffer without bound; the parser-side caps
    /// ([`Self::MAX_NESTED_DEPTH`], [`Self::MAX_NESTED_PARAMS`],
    /// [`Self::MAX_ATTRIBUTES`]) cover the parse step itself.
    pub(crate) const MAX_BUFFER_BYTES: usize = 4 * 1024 * 1024;

    /// Maximum number of `attr="value"` pairs `parse_shorthand_tag` will
    /// process per tag (crosslink #343).
    ///
    /// 32 covers every legitimate shorthand schema by an order of
    /// magnitude (the widest is `edit_file` with `path`, `old_string`,
    /// `new_string` — 3 attributes). The cap stops a hostile stream from
    /// allocating an unbounded `HashMap` by emitting
    /// `<bash a1="" a2="" … aN="">…</bash>`.
    pub(crate) const MAX_ATTRIBUTES: usize = 32;

    /// Add content to the intercept buffer.
    ///
    /// crosslink #343: if the buffer would exceed [`Self::MAX_BUFFER_BYTES`]
    /// the trailing content is dropped. This is a hard cap on the
    /// interceptor's worst-case memory footprint when a stream keeps
    /// emitting unclosed shorthand tags. The dropped content is logged at
    /// `warn!` so an audit log captures the rejection — the alternative
    /// (panicking, or letting the buffer grow to OOM) is worse.
    pub fn push(&mut self, content: &str) {
        if self.buffer.len() >= Self::MAX_BUFFER_BYTES {
            // Already over the cap: drop the new chunk entirely.
            tracing::warn!(
                buffer_bytes = self.buffer.len(),
                cap = Self::MAX_BUFFER_BYTES,
                drop_bytes = content.len(),
                "ToolInterceptor: buffer cap reached, dropping incoming content (crosslink #343)"
            );
            return;
        }
        let room = Self::MAX_BUFFER_BYTES.saturating_sub(self.buffer.len());
        if content.len() <= room {
            self.buffer.push_str(content);
            return;
        }
        // Partial fit: take only what we have room for and emit a warning.
        // Truncate at a char boundary so we never split a UTF-8 codepoint.
        let truncated = crate::tools::safe_truncate(content, room);
        tracing::warn!(
            buffer_bytes = self.buffer.len(),
            cap = Self::MAX_BUFFER_BYTES,
            drop_bytes = content.len() - truncated.len(),
            "ToolInterceptor: buffer cap nearly reached, truncating incoming content (crosslink #343)"
        );
        self.buffer.push_str(truncated);
    }

    /// Get the current buffer contents
    #[must_use]
    pub fn get_buffer(&self) -> &str {
        &self.buffer
    }

    /// Clear the buffer
    pub fn clear(&mut self) {
        self.buffer.clear();
        self.in_function_calls = false;
        self.scan = ScanState::default();
    }

    /// Discard the cached marker scan so the next poll re-scans from byte zero.
    ///
    /// Called whenever the buffer is rewritten or shrunk (e.g. after an
    /// extraction or `strip_hallucinated_blocks`). The cache assumes the buffer
    /// only grows by append; any other mutation must invalidate it.
    fn invalidate_scan_cache(&mut self) {
        self.scan = ScanState::default();
    }

    /// Walk any unscanned suffix of `self.buffer`, recording every tool-related
    /// marker we encounter into `self.scan`.
    ///
    /// This is the single source of truth for "is there a tool call in the
    /// buffer?" / "is it complete?" — both [`Self::has_pending_tool_calls`] and
    /// [`Self::has_complete_block`] read flags written here. The walk is a
    /// single linear pass over the new suffix that checks for any of the known
    /// markers at each `<` byte; total amortised cost across N total bytes of
    /// buffer is O(N), not O(N * markers).
    ///
    /// Backtracks a small constant before `scan_pos` so a marker that straddles
    /// the boundary between two pushes is still detected.
    fn extend_scan(&mut self) {
        // Longest marker we recognise: `<invoke name="` (14 bytes). Backtracking
        // by `MAX_MARKER_LEN - 1` ensures that if the previous push left a
        // partial marker at the buffer tail, the next scan picks it up.
        const MAX_MARKER_LEN: usize = 14;

        let bytes = self.buffer.as_bytes();
        if self.scan.scan_pos >= bytes.len() {
            return;
        }
        let start = self.scan.scan_pos.saturating_sub(MAX_MARKER_LEN - 1);

        let mut i = start;
        while i < bytes.len() {
            if bytes[i] != b'<' {
                i += 1;
                continue;
            }
            let tail = &self.buffer[i..];
            // Single dispatch on the byte(s) following `<`. Each branch is a
            // constant-time `starts_with` against a known fixed string, so the
            // per-`<` cost is O(markers) with a tiny constant.
            if tail.starts_with("<invoke name=\"") {
                if self.scan.invoke_open_at.is_none() {
                    self.scan.invoke_open_at = Some(i);
                }
                i += "<invoke name=\"".len();
                continue;
            }
            if tail.starts_with("</invoke>") {
                if let Some(open_at) = self.scan.invoke_open_at {
                    if i >= open_at {
                        self.scan.invoke_closed = true;
                    }
                }
                i += "</invoke>".len();
                continue;
            }
            // Check shorthand-tool open / close markers. Visiting at most a few
            // bytes after `<` — `starts_with` short-circuits on the first byte
            // mismatch so most candidates are rejected in one comparison.
            let mut matched = false;
            for (idx, tool) in Self::SHORTHAND_TOOLS.iter().enumerate() {
                // `</tool>`
                if tail.len() >= tool.len() + 3
                    && tail.as_bytes()[1] == b'/'
                    && tail[2..].starts_with(tool)
                    && tail.as_bytes()[2 + tool.len()] == b'>'
                {
                    self.scan.shorthand_close[idx] = true;
                    i += 3 + tool.len();
                    matched = true;
                    break;
                }
                // `<tool>` or `<tool `
                if tail.len() > tool.len() + 1 && tail[1..].starts_with(tool) {
                    let after = tail.as_bytes()[1 + tool.len()];
                    if after == b'>' || after == b' ' {
                        self.scan.shorthand_open[idx] = true;
                        i += 2 + tool.len();
                        matched = true;
                        break;
                    }
                }
            }
            if !matched {
                i += 1;
            }
        }

        self.scan.scan_pos = bytes.len();
    }

    /// Shorthand tool tags that Claude might use (e.g., <bash>cmd</bash>)
    const SHORTHAND_TOOLS: &'static [&'static str] = &[
        "bash",
        "read",
        "write",
        "edit",
        "glob",
        "grep",
        "read_file",
        "write_file",
        "edit_file",
    ];

    /// Compile-time check that [`SHORTHAND_TOOL_COUNT`] stays in sync with
    /// [`Self::SHORTHAND_TOOLS`]. If a future entry is added or removed without
    /// updating the constant, this assertion fails at compile time so the
    /// fixed-size arrays in [`ScanState`] cannot silently drift.
    const _SHORTHAND_LEN_CHECK: () = assert!(
        Self::SHORTHAND_TOOLS.len() == SHORTHAND_TOOL_COUNT,
        "SHORTHAND_TOOL_COUNT must equal SHORTHAND_TOOLS.len()",
    );

    /// Check if buffer contains tool invocations
    /// Claude Code uses multiple formats:
    /// 1. <invoke name="Bash"><parameter name="command">...</parameter></invoke>
    /// 2. <bash>...</bash> (shorthand)
    /// 3. <`function_calls`><invoke>...</invoke></`function_calls`>
    ///
    /// Crosslink #743: single-pass over the buffer with a position cache rather
    /// than N substring scans per call. See [`Self::extend_scan`] for details.
    #[must_use]
    pub fn has_pending_tool_calls(&mut self) -> bool {
        self.extend_scan();
        if self.scan.invoke_open_at.is_some() {
            return true;
        }
        if self.scan.shorthand_open.iter().any(|&seen| seen) {
            return true;
        }
        self.in_function_calls
    }

    /// Check if we have a complete tool block
    ///
    /// Crosslink #743: shares the position-cached marker scan with
    /// [`Self::has_pending_tool_calls`]. The expensive `<result>...</result>`
    /// gating is only performed if the cache already knows the buffer contains
    /// a closed `<invoke>` block, so the common "no tools yet" fast path is a
    /// handful of integer reads.
    #[must_use]
    pub fn has_complete_block(&mut self) -> bool {
        self.extend_scan();

        if let Some(start) = self.scan.invoke_open_at {
            if self.scan.invoke_closed {
                // We only need to consult the buffer text when an invoke has
                // closed: the `<result>...</result>` gating below cannot be
                // expressed by per-marker flags alone (it depends on what
                // follows the closing tag). The slice below is bounded by the
                // closed-invoke region, not a full-buffer scan.
                if let Some(end_rel) = self.buffer[start..].find("</invoke>") {
                    let invoke_end = start + end_rel + "</invoke>".len();
                    let after = &self.buffer[invoke_end..];
                    if after.trim_start().starts_with("<result>") {
                        return after.contains("</result>");
                    }
                    return true;
                }
            }
        }

        // Shorthand: any tool with both an opener and a closer counts.
        for idx in 0..SHORTHAND_TOOL_COUNT {
            if self.scan.shorthand_open[idx] && self.scan.shorthand_close[idx] {
                return true;
            }
        }
        false
    }

    /// Parse tool invocations from the buffer
    /// Returns extracted tool calls and the text content before/after the block
    /// NOTE: This also strips out <result> blocks (sandbox output we're replacing)
    pub fn extract_tool_calls(&mut self) -> (Vec<InterceptedToolCall>, String, String) {
        // Try full invoke format first
        if let Some(result) = self.try_extract_invoke_format() {
            return result;
        }

        // Try shorthand format (e.g., <bash>cmd</bash>)
        if let Some(result) = self.try_extract_shorthand_format() {
            return result;
        }

        // No tool calls found
        (vec![], self.buffer.clone(), String::new())
    }

    /// Strip hallucinated result blocks and wrapper tags from the buffer.
    ///
    /// When a model generates tool calls in text mode (no structured `tool_use`),
    /// it often continues generating fabricated `<function_results>` blocks after
    /// each tool call. This method strips those hallucinated outputs so we can
    /// extract the real tool calls and execute them ourselves.
    ///
    /// Also strips `<function_calls>` / `</function_calls>` wrapper tags since
    /// the parser works directly with `<invoke>` blocks.
    pub fn strip_hallucinated_blocks(&mut self) {
        // Remove <function_results>...</function_results> or <function_results>...</function_calls>
        // (models sometimes hallucinate the wrong closing tag)
        while let Some(start) = self.buffer.find("<function_results>") {
            // Prefer proper closing tag; fall back to </function_calls> (common hallucination)
            let end = if let Some(rel) = self.buffer[start..].find("</function_results>") {
                start + rel + "</function_results>".len()
            } else if let Some(rel) = self.buffer[start..].find("</function_calls>") {
                start + rel + "</function_calls>".len()
            } else {
                // No closing tag found — discard from <function_results> to end of buffer
                self.buffer.truncate(start);
                break;
            };

            self.buffer = format!("{}{}", &self.buffer[..start], &self.buffer[end..]);
        }

        // Remove <function_calls> and </function_calls> wrapper tags (keep content inside)
        self.buffer = self.buffer.replace("<function_calls>", "");
        self.buffer = self.buffer.replace("</function_calls>", "");

        // Buffer rewritten in-place — the cached marker positions no longer
        // correspond to byte offsets in `self.buffer`, so drop them.
        self.invalidate_scan_cache();
    }

    /// Extract ALL tool calls from the buffer at once, stripping hallucinated results.
    ///
    /// This is the main entry point for proxy-mode tool extraction. It:
    /// 1. Strips hallucinated `<function_results>` blocks the model generated
    /// 2. Strips `<function_calls>` wrapper tags
    /// 3. Extracts every tool call (invoke and shorthand formats)
    /// 4. Returns all tools and the interleaved text content
    ///
    /// This prevents the model from "running ahead" with fabricated tool outputs
    /// by ensuring we execute all real tools before sending results back.
    pub fn extract_all_tool_calls(&mut self) -> (Vec<InterceptedToolCall>, Vec<String>) {
        // Strip hallucinated outputs first
        self.strip_hallucinated_blocks();

        let mut all_tools = Vec::new();
        let mut text_parts = Vec::new();

        // Extract tool calls one by one until none remain
        while self.has_complete_block() {
            let (tools, before, _after) = self.extract_tool_calls();
            if tools.is_empty() {
                break;
            }
            let trimmed = before.trim().to_string();
            if !trimmed.is_empty() {
                text_parts.push(trimmed);
            }
            all_tools.extend(tools);
        }

        // Any remaining buffer content is text after all tools
        let remaining = self.buffer.trim().to_string();
        if !remaining.is_empty() {
            text_parts.push(remaining);
        }

        self.buffer.clear();

        (all_tools, text_parts)
    }

    /// Try to extract tool calls in <invoke name="..."> format
    fn try_extract_invoke_format(&mut self) -> Option<(Vec<InterceptedToolCall>, String, String)> {
        const INVOKE_OPEN: &str = "<invoke name=\"";
        const INVOKE_CLOSE: &str = "</invoke>";
        const RESULT_OPEN: &str = "<result>";
        const RESULT_CLOSE: &str = "</result>";

        let start_idx = self.buffer.find(INVOKE_OPEN)?;
        let invoke_end_rel = self.buffer[start_idx..].find(INVOKE_CLOSE)?;
        let invoke_end = start_idx + invoke_end_rel + INVOKE_CLOSE.len();

        // Check if there's a <result> block to skip
        let after_invoke = &self.buffer[invoke_end..];
        let result_end = if after_invoke.trim_start().starts_with(RESULT_OPEN) {
            after_invoke
                .find(RESULT_CLOSE)
                .map_or(invoke_end, |idx| invoke_end + idx + RESULT_CLOSE.len())
        } else {
            invoke_end
        };

        let before = self.buffer[..start_idx].to_string();
        let invoke_block = &self.buffer[start_idx..invoke_end];

        let tools = self.parse_invocations(invoke_block);

        // Crosslink #789: replaced `let after = self.buffer[result_end..].to_string();
        // self.buffer.clone_from(&after);` (two clones of the trailing N-K bytes
        // per iteration → O(N*K) over the driving loop) with a single in-place
        // `drain` and one copy of the suffix to satisfy the public return
        // signature. Total cost across K extractions is now O(N).
        self.buffer.drain(..result_end);
        let after = self.buffer.clone();
        // Buffer shrunk to the post-extraction suffix; cached marker offsets
        // would now point at the wrong bytes, so drop them (crosslink #743).
        self.invalidate_scan_cache();

        Some((tools, before, after))
    }

    /// Try to extract tool calls in shorthand format (e.g., <bash>cmd</bash>)
    fn try_extract_shorthand_format(
        &mut self,
    ) -> Option<(Vec<InterceptedToolCall>, String, String)> {
        // Find the first shorthand tool tag
        let mut earliest_match: Option<(usize, &str)> = None;

        for tool in Self::SHORTHAND_TOOLS {
            let open_tag = format!("<{tool}>");
            let open_tag_attr = format!("<{tool} ");

            // Check for <tool> or <tool attr="...">
            //
            // Crosslink #768: the prior form
            // `earliest_match.is_none() || idx < earliest_match.unwrap().0`
            // relied on short-circuit evaluation to dodge a panic on the
            // `.unwrap()`. A future reorder of the operands would introduce
            // an unconditional panic the compiler cannot warn about.
            // `map_or(true, ...)` removes the `.unwrap()` entirely.
            if let Some(idx) = self.buffer.find(&open_tag) {
                let should_replace = earliest_match.is_none_or(|(prev, _)| idx < prev);
                if should_replace {
                    earliest_match = Some((idx, *tool));
                }
            }
            if let Some(idx) = self.buffer.find(&open_tag_attr) {
                let should_replace = earliest_match.is_none_or(|(prev, _)| idx < prev);
                if should_replace {
                    earliest_match = Some((idx, *tool));
                }
            }
        }

        let (start_idx, tool_name) = earliest_match?;
        let close_tag = format!("</{tool_name}>");
        let close_idx = self.buffer[start_idx..].find(&close_tag)?;
        let block_end = start_idx + close_idx + close_tag.len();

        // Extract the content between tags
        let tag_content = &self.buffer[start_idx..block_end];

        // Parse the shorthand tag
        let tool = self.parse_shorthand_tag(tool_name, tag_content)?;

        let before = self.buffer[..start_idx].to_string();
        // Crosslink #789: drain in-place rather than the previous
        // to_string → clone_from pair. Eliminates one full-buffer clone per
        // iteration; the remaining `after.clone()` only exists to satisfy the
        // public return signature.
        self.buffer.drain(..block_end);
        let after = self.buffer.clone();
        // Buffer shrunk to the post-extraction suffix; cached marker offsets
        // would now point at the wrong bytes, so drop them (crosslink #743).
        self.invalidate_scan_cache();

        Some((vec![tool], before, after))
    }

    /// Parse a shorthand tool tag like <bash>command</bash> or <write path="file">content</write>
    /// Also handles nested element format: <`write_file`><path>file</path><content>...</content></`write_file`>
    fn parse_shorthand_tag(
        &self,
        tool_name: &str,
        tag_content: &str,
    ) -> Option<InterceptedToolCall> {
        let open_simple = format!("<{tool_name}>");
        let open_attr = format!("<{tool_name} ");
        let close_tag = format!("</{tool_name}>");

        let mut parameters = HashMap::new();

        // Check if it's a simple tag <tool>content</tool> or has attributes <tool attr="val">content</tool>
        let content_start = if tag_content.starts_with(&open_simple) {
            open_simple.len()
        } else if tag_content.starts_with(&open_attr) {
            // Parse attributes from <tool attr="val" attr2="val2">
            let close_bracket = tag_content.find('>')?;
            let attr_str = &tag_content[open_attr.len()..close_bracket];

            // Simple attribute parsing: attr="value".
            // crosslink #343: bounded by Self::MAX_ATTRIBUTES so a tag
            // like `<bash a1="" … aN="">…</bash>` cannot allocate an
            // unbounded HashMap or run quadratic-time in attr count.
            let mut attr_search = 0;
            while let Some(eq_pos) = attr_str[attr_search..].find('=') {
                if parameters.len() >= Self::MAX_ATTRIBUTES {
                    tracing::warn!(
                        cap = Self::MAX_ATTRIBUTES,
                        "parse_shorthand_tag: attribute cap reached, dropping remaining attrs (crosslink #343)"
                    );
                    break;
                }
                let abs_eq = attr_search + eq_pos;
                let attr_name = attr_str[attr_search..abs_eq].trim();

                // Find quoted value
                let quote_start = attr_str[abs_eq..].find('"')? + abs_eq + 1;
                let quote_end = attr_str[quote_start..].find('"')? + quote_start;
                let attr_value = &attr_str[quote_start..quote_end];

                parameters.insert(attr_name.to_string(), attr_value.to_string());
                attr_search = quote_end + 1;
            }

            close_bracket + 1
        } else {
            return None;
        };

        // Extract content between open and close tags
        let content_end = tag_content.len() - close_tag.len();
        let content = tag_content[content_start..content_end].to_string();

        // Check for nested element format: <tool><param>value</param><param2>value2</param2></tool>
        // This is used when Claude outputs things like:
        // <write_file><path>hello.c</path><content>...</content></write_file>
        let trimmed_content = content.trim();
        if trimmed_content.starts_with('<') && !trimmed_content.starts_with("</") {
            // Parse nested elements
            self.parse_nested_elements(trimmed_content, &mut parameters);
        }

        // If no nested elements found, use the old logic for simple content
        if parameters.is_empty() {
            // Map shorthand content to appropriate parameter
            match tool_name {
                "bash" => {
                    parameters.insert("command".to_string(), content);
                }
                "read" | "read_file"
                    if !parameters.contains_key("path") && !parameters.contains_key("file_path") => {
                        parameters.insert("path".to_string(), content);
                    }
                "write" | "write_file"
                    // Content is the file content, path should be in attributes
                    if !content.is_empty() => {
                        parameters.insert("content".to_string(), content);
                    }
                "glob" | "grep"
                    if !parameters.contains_key("pattern") => {
                        parameters.insert("pattern".to_string(), content);
                    }
                // edit/edit_file: params are in attributes; other tools: no special handling
                _ => {}
            }
        }

        Some(InterceptedToolCall {
            name: tool_name.to_string(),
            parameters,
            id: format!(
                "toolu_{}",
                safe_truncate(&Uuid::new_v4().to_string().replace('-', ""), 24)
            ),
        })
    }

    /// Maximum nesting / tag-scan recursion depth for `parse_nested_elements`.
    ///
    /// crosslink #899: the parser today is single-level, but the surrounding
    /// `extract_all_tool_calls` driver can be made to recurse into nested
    /// pseudo-XML via repeated `<tool>...</tool>` tags. We cap the work
    /// performed by any single invocation at 8 tag-scan iterations beyond
    /// the natural `MAX_PARAMS` budget so a pathological input cannot
    /// linearly amplify into quadratic cost.
    pub(crate) const MAX_NESTED_DEPTH: usize = 8;

    /// Maximum distinct parameter entries `parse_nested_elements` will
    /// emit per call.
    ///
    /// crosslink #899: a malicious model can emit 100 000 `<param_n>value</param_n>`
    /// elements; without a cap the resulting `HashMap` allocates without
    /// bound. 32 covers every legitimate tool schema in the codebase by an
    /// order of magnitude.
    pub(crate) const MAX_NESTED_PARAMS: usize = 32;

    /// Parse nested XML elements like <path>value</path><content>...</content>
    ///
    /// crosslink #899: bounded by [`Self::MAX_NESTED_DEPTH`] tag-scan iterations
    /// and [`Self::MAX_NESTED_PARAMS`] distinct parameters. Once either cap is
    /// hit we stop parsing and emit a synthetic `__parse_error` parameter so
    /// the failure is visible to the caller and the test suite rather than
    /// silently truncated.
    #[allow(clippy::unused_self)]
    fn parse_nested_elements(&self, content: &str, parameters: &mut HashMap<String, String>) {
        let mut search_pos = 0;
        let mut depth_iterations: usize = 0;

        while search_pos < content.len() {
            // crosslink #899: hard cap on scan iterations. Each iteration
            // either advances `search_pos` past a recognized element or
            // skips a malformed prefix; either way we bound the total work.
            if depth_iterations >= Self::MAX_NESTED_DEPTH.saturating_mul(Self::MAX_NESTED_PARAMS) {
                parameters.insert(
                    "__parse_error".to_string(),
                    format!(
                        "parse_nested_elements: depth cap exceeded \
                         (MAX_DEPTH={} * MAX_PARAMS={})",
                        Self::MAX_NESTED_DEPTH,
                        Self::MAX_NESTED_PARAMS,
                    ),
                );
                break;
            }
            depth_iterations += 1;

            // Find opening tag
            let Some(tag_start) = content[search_pos..].find('<') else {
                break;
            };
            let abs_tag_start = search_pos + tag_start;

            // Skip if it's a closing tag
            if content[abs_tag_start..].starts_with("</") {
                search_pos = abs_tag_start + 1;
                continue;
            }

            // Find end of opening tag
            let Some(tag_end) = content[abs_tag_start..].find('>') else {
                break;
            };
            let abs_tag_end = abs_tag_start + tag_end;

            // Extract element name (handle self-closing tags)
            let tag_content = &content[abs_tag_start + 1..abs_tag_end];
            let elem_name = tag_content
                .split_whitespace()
                .next()
                .unwrap_or("")
                .trim_end_matches('/');

            if elem_name.is_empty() {
                search_pos = abs_tag_end + 1;
                continue;
            }

            // Find closing tag
            let close_tag = format!("</{elem_name}>");
            let Some(close_pos) = content[abs_tag_end..].find(&close_tag) else {
                search_pos = abs_tag_end + 1;
                continue;
            };
            let abs_close_pos = abs_tag_end + close_pos;

            // Extract value between tags
            let value = content[abs_tag_end + 1..abs_close_pos].to_string();

            // Map element names to parameter names
            let param_name = match elem_name {
                "file_path" => "path",
                "old_string" => "old_string",
                "new_string" => "new_string",
                "contents" => "content", // Claude sometimes uses plural
                _ => elem_name,
            };

            // crosslink #899: hard cap on distinct parameter entries. We
            // count distinct entries — an already-present key being
            // overwritten does not consume budget. Note we still advance
            // `search_pos` past the matched element so the loop terminates.
            if !parameters.contains_key(param_name) && parameters.len() >= Self::MAX_NESTED_PARAMS {
                parameters.insert(
                    "__parse_error".to_string(),
                    format!(
                        "parse_nested_elements: param cap exceeded \
                         (MAX_PARAMS={})",
                        Self::MAX_NESTED_PARAMS,
                    ),
                );
                break;
            }

            parameters.insert(param_name.to_string(), value);

            // Move past this element
            search_pos = abs_close_pos + close_tag.len();
        }
    }

    /// Parse invoke tags within a `function_calls` block
    #[allow(clippy::unused_self)]
    fn parse_invocations(&self, block: &str) -> Vec<InterceptedToolCall> {
        const INVOKE_OPEN: &str = "<invoke name=\"";
        const INVOKE_CLOSE: &str = "</invoke>";
        const PARAM_OPEN: &str = "<parameter name=\"";
        const PARAM_CLOSE: &str = "</parameter>";

        let mut tools = Vec::new();
        let mut search_start = 0;

        while let Some(invoke_start) = block[search_start..].find(INVOKE_OPEN) {
            let abs_start = search_start + invoke_start;

            // Find tool name
            let name_start = abs_start + INVOKE_OPEN.len();
            let Some(name_end_rel) = block[name_start..].find('"') else {
                search_start = abs_start + 1;
                continue;
            };
            let name_end = name_start + name_end_rel;
            let tool_name = block[name_start..name_end].to_string();

            // Find end of this invoke block
            let Some(invoke_end_rel) = block[abs_start..].find(INVOKE_CLOSE) else {
                search_start = abs_start + 1;
                continue;
            };
            let invoke_end = abs_start + invoke_end_rel;
            let invoke_block = &block[abs_start..invoke_end];

            // Parse parameters within this invoke block
            let mut parameters = HashMap::new();
            let mut param_search = 0;

            while let Some(param_start) = invoke_block[param_search..].find(PARAM_OPEN) {
                let abs_param_start = param_search + param_start;

                // Get parameter name
                let pname_start = abs_param_start + PARAM_OPEN.len();
                let Some(pname_end_rel) = invoke_block[pname_start..].find('"') else {
                    param_search = abs_param_start + 1;
                    continue;
                };
                let pname_end = pname_start + pname_end_rel;
                let param_name = invoke_block[pname_start..pname_end].to_string();

                // Find the closing > after the parameter name
                let Some(value_start_rel) = invoke_block[pname_end..].find('>') else {
                    param_search = pname_end;
                    continue;
                };
                let value_start = pname_end + value_start_rel + 1;

                // Find closing tag
                let Some(value_end_rel) = invoke_block[value_start..].find(PARAM_CLOSE) else {
                    param_search = value_start;
                    continue;
                };
                let value_end = value_start + value_end_rel;
                let param_value = invoke_block[value_start..value_end].to_string();

                parameters.insert(param_name, param_value);
                param_search = value_end + PARAM_CLOSE.len();
            }

            tools.push(InterceptedToolCall {
                name: tool_name,
                parameters,
                id: format!(
                    "toolu_{}",
                    &Uuid::new_v4().to_string().replace('-', "")[..24]
                ),
            });

            search_start = invoke_end + INVOKE_CLOSE.len();
        }

        tools
    }
}

/// Result of executing an intercepted tool call
pub struct ToolExecutionResult {
    pub id: String,
    pub name: String,
    pub content: String,
    pub is_error: bool,
}

/// Execute intercepted tool calls locally and format results for Claude.
///
/// `permission_mgr` is threaded through to the library-level gate inside
/// `execute_tool_with_memory`. Passing `None` preserves legacy behavior
/// (warn-once) and is expected from callers that intentionally allow
/// upstream-controlled tool calls; passing `Some(&mgr)` enforces the
/// config's `default_allow` patterns even on the proxy-intercept path.
/// See crosslink #505.
#[must_use]
pub fn execute_intercepted_tools(
    tools: &[InterceptedToolCall],
    memory_db: Option<&crate::memory::MemoryDb>,
    permission_mgr: Option<&crate::permissions::PermissionManager>,
) -> Vec<ToolExecutionResult> {
    execute_intercepted_tools_for_session(tools, memory_db, permission_mgr, None)
}

/// Execute intercepted tool calls locally with an optional session ledger.
///
/// When `session_id` is supplied, filesystem/command observers see the same
/// active reality ledger as ordinary function-call tools, and every tool
/// result is appended as a bounded generic `ToolResult` observation.
#[must_use]
pub fn execute_intercepted_tools_for_session(
    tools: &[InterceptedToolCall],
    memory_db: Option<&crate::memory::MemoryDb>,
    permission_mgr: Option<&crate::permissions::PermissionManager>,
    session_id: Option<&str>,
) -> Vec<ToolExecutionResult> {
    let mut results = Vec::new();

    for tool in tools {
        let tool_call = tool.to_tool_call();

        println!("\n\x1b[36m⚡ Running {} locally...\x1b[0m", tool.name);

        let _session_guard = session_id.map(crate::tools::SessionIdGuard::set);
        let _ledger_guard =
            session_id.and_then(crate::grounded_loop::install_active_project_ledger_for_session);
        let result = crate::tools::execute_tool_with_memory(&tool_call, memory_db, permission_mgr);
        if let Some(session_id) = session_id {
            crate::grounded_loop::observe_tool_result_for_session(
                session_id,
                &tool_call.function.name,
                &result,
            );
        }

        // Show preview
        let preview: String = result
            .content
            .lines()
            .take(5)
            .collect::<Vec<_>>()
            .join("\n");
        if result.is_error {
            println!("\x1b[31m✗ Error:\x1b[0m {preview}");
        } else {
            println!(
                "\x1b[32m✓\x1b[0m {}",
                if preview.len() > 200 {
                    format!("{}...", safe_truncate(&preview, 200))
                } else {
                    preview
                }
            );
        }

        results.push(ToolExecutionResult {
            id: tool.id.clone(),
            name: tool.name.clone(),
            content: result.content,
            is_error: result.is_error,
        });
    }

    results
}

/// Format tool execution results as XML with tool names for better completion signaling
#[must_use]
pub fn format_execution_results_xml(results: &[ToolExecutionResult]) -> String {
    let refs: Vec<(&str, Option<&str>, &str, bool)> = results
        .iter()
        .map(|r| {
            (
                r.id.as_str(),
                Some(r.name.as_str()),
                r.content.as_str(),
                r.is_error,
            )
        })
        .collect();
    format_tool_results_xml_with_names(&refs)
}

/// Format tool results as XML for injection back to Claude
///
/// Results include explicit status and completion signals to prevent the model
/// from retrying operations that already succeeded.
#[must_use]
pub fn format_tool_results_xml(results: &[(String, String, bool)]) -> String {
    format_tool_results_xml_with_names(
        &results
            .iter()
            .map(|(id, content, is_error)| (id.as_str(), None, content.as_str(), *is_error))
            .collect::<Vec<_>>(),
    )
}

/// Escape special XML characters in content to prevent malformed XML output.
///
/// This must be applied to any user/tool content before interpolation into XML tags.
fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

/// Format tool results with tool names for better completion signaling
#[must_use]
pub fn format_tool_results_xml_with_names(results: &[(&str, Option<&str>, &str, bool)]) -> String {
    let mut xml = String::new();
    xml.push_str("<function_results>\n");

    for (id, tool_name, content, is_error) in results {
        xml.push_str("<result>\n");
        let _ = writeln!(xml, "<tool_use_id>{}</tool_use_id>", xml_escape(id));

        if *is_error {
            xml.push_str("<status>error</status>\n");
            xml.push_str("<error>");
            xml.push_str(&xml_escape(content));
            xml.push_str("</error>\n");
        } else {
            xml.push_str("<status>success</status>\n");
            xml.push_str("<output>");
            xml.push_str(&xml_escape(content));
            xml.push_str("</output>\n");

            // Add explicit completion hint for file operations.
            //
            // Crosslink #486: bash success used to be detected with a
            // substring search for "error"/"Error"/"failed" in the captured
            // output, which misclassified perfectly successful commands like
            // `echo "no errors found"` as failures and silently dropped the
            // completion hint. We are already inside the `!is_error` branch
            // here (the outer `if *is_error` handled the failure path), so
            // the exit-code-based truth is in scope: the only reason to skip
            // a hint is the tool's own structured success/failure signal,
            // not a textual heuristic on stdout.
            if let Some(name) = tool_name {
                let completion_hint = match *name {
                    "write_file" | "Write" | "write" => {
                        Some("File created successfully. The operation is COMPLETE - do NOT call write_file again for this file.")
                    }
                    "edit_file" | "Edit" | "edit" => {
                        Some("Edit applied successfully. The operation is COMPLETE - do NOT call edit_file again with the same change.")
                    }
                    "bash" | "Bash" => {
                        // Reached this arm ⇒ `is_error == false` ⇒ the tool
                        // result reported success (exit code 0). Emit the
                        // hint unconditionally; do NOT re-derive failure
                        // from stdout content.
                        Some("Command executed successfully.")
                    }
                    _ => None,
                };
                if let Some(hint) = completion_hint {
                    let _ = writeln!(xml, "<completion_note>{hint}</completion_note>");
                }
            }
        }
        xml.push_str("</result>\n");
    }

    xml.push_str("</function_results>\n");
    xml.push_str("<system_note>All tool operations above completed. Respond to the user with a summary of what was done. Do NOT re-execute these tools unless the user asks for additional changes.</system_note>");
    xml
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn execute_intercepted_tools_for_session_records_tool_result_observation() {
        let session_id = "xmltoolledger";
        let ledger =
            std::sync::Arc::new(std::sync::Mutex::new(crate::ledger::RealityLedger::new()));
        let _ledger_guard = crate::ledger::install_active_ledger_for_session(
            session_id,
            std::sync::Arc::clone(&ledger),
        );
        let mut parameters = HashMap::new();
        parameters.insert("path".to_string(), ".".to_string());
        let tool = InterceptedToolCall {
            name: "list_files".to_string(),
            parameters,
            id: "xml_call_list".to_string(),
        };

        let results = execute_intercepted_tools_for_session(&[tool], None, None, Some(session_id));

        assert_eq!(results.len(), 1);
        assert!(
            !results[0].is_error,
            "unexpected tool error: {}",
            results[0].content
        );
        let observation = {
            let ledger = ledger.lock().expect("ledger lock");
            ledger
                .observations_chronological()
                .into_iter()
                .find(|obs| matches!(obs.kind, crate::ledger::ObservationKind::ToolResult { .. }))
                .cloned()
        }
        .expect("tool result observation");
        assert_eq!(observation.authority, crate::ledger::Authority::Tool);
        let crate::ledger::ObservationKind::ToolResult { tool, result } = &observation.kind else {
            panic!("expected tool result observation");
        };
        assert_eq!(tool, "list_files");
        assert_eq!(result["tool_call_id"], "xml_call_list");
        assert_eq!(result["is_error"], false);
    }

    #[test]
    fn test_parse_bash_invocation() {
        let mut interceptor = ToolInterceptor::new();

        // Simulate Claude Code's actual format (direct invoke, no function_calls wrapper)
        let content = r#"Let me check the directory.

<invoke name="Bash">
<parameter name="command">ls -la</parameter>
</invoke>
<result>
sandbox output here
</result>

And here's some text after."#;

        interceptor.push(content);
        assert!(interceptor.has_complete_block());

        let (tools, before, _after) = interceptor.extract_tool_calls();

        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "Bash");
        assert_eq!(
            tools[0].parameters.get("command"),
            Some(&"ls -la".to_string())
        );
        assert!(before.contains("Let me check the directory"));
    }

    #[test]
    fn test_parse_with_sandbox_result() {
        let mut interceptor = ToolInterceptor::new();

        // Claude Code returns sandbox results inline - we need to skip them
        let content = r#"<invoke name="list_files">
<parameter name="path">.</parameter>
</invoke>
<result>
LICENSE
README.md
claude_code
</result>

Some text after."#;

        interceptor.push(content);
        assert!(interceptor.has_complete_block());

        let (tools, _before, after) = interceptor.extract_tool_calls();

        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "list_files");
        // The result block should be stripped, not in 'after'
        assert!(!after.contains("LICENSE"));
        assert!(after.contains("Some text after"));
    }

    #[test]
    fn test_parse_multiple_invocations() {
        let mut interceptor = ToolInterceptor::new();

        // First invocation with result
        let content = r#"<invoke name="Read">
<parameter name="file_path">/tmp/test.txt</parameter>
</invoke>
<result>file contents</result>

<invoke name="Bash">
<parameter name="command">pwd</parameter>
</invoke>
<result>/tmp</result>"#;

        interceptor.push(content);

        // First extraction gets Read
        let (tools1, _, _) = interceptor.extract_tool_calls();
        assert_eq!(tools1.len(), 1);
        assert_eq!(tools1[0].name, "Read");

        // Second extraction gets Bash
        let (tools2, _, _) = interceptor.extract_tool_calls();
        assert_eq!(tools2.len(), 1);
        assert_eq!(tools2[0].name, "Bash");
    }

    #[test]
    fn test_tool_call_conversion() {
        let tool = InterceptedToolCall {
            name: "Bash".to_string(),
            parameters: [("command".to_string(), "echo hello".to_string())].into(),
            id: "test123".to_string(),
        };

        let tc = tool.to_tool_call();
        assert_eq!(tc.function.name, "bash");
        assert!(tc.function.arguments.contains("echo hello"));
    }

    #[test]
    fn test_parse_shorthand_bash() {
        let mut interceptor = ToolInterceptor::new();

        // Shorthand format Claude sometimes uses: <bash>command</bash>
        let content = r"I'll check the directory.

<bash>pwd</bash>

That's the current directory.";

        interceptor.push(content);
        assert!(interceptor.has_pending_tool_calls());
        assert!(interceptor.has_complete_block());

        let (tools, before, after) = interceptor.extract_tool_calls();

        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "bash");
        assert_eq!(tools[0].parameters.get("command"), Some(&"pwd".to_string()));
        assert!(before.contains("I'll check the directory"));
        assert!(after.contains("That's the current directory"));
    }

    #[test]
    fn test_parse_shorthand_read() {
        let mut interceptor = ToolInterceptor::new();

        let content = r"<read>/path/to/file.txt</read>";

        interceptor.push(content);
        assert!(interceptor.has_complete_block());

        let (tools, _, _) = interceptor.extract_tool_calls();

        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "read");
        assert_eq!(
            tools[0].parameters.get("path"),
            Some(&"/path/to/file.txt".to_string())
        );
    }

    #[test]
    fn test_parse_shorthand_glob() {
        let mut interceptor = ToolInterceptor::new();

        let content = r"<glob>**/*.rs</glob>";

        interceptor.push(content);
        assert!(interceptor.has_complete_block());

        let (tools, _, _) = interceptor.extract_tool_calls();

        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "glob");
        assert_eq!(
            tools[0].parameters.get("pattern"),
            Some(&"**/*.rs".to_string())
        );
    }

    #[test]
    fn test_parse_nested_element_write_file() {
        let mut interceptor = ToolInterceptor::new();

        // Claude Code format: <write_file><path>file</path><content>...</content></write_file>
        let content = r"<write_file>
<path>hello.c</path>
<content>#include <stdio.h>
int main() { return 0; }
</content>
</write_file>";

        interceptor.push(content);
        assert!(interceptor.has_complete_block());

        let (tools, _, _) = interceptor.extract_tool_calls();

        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "write_file");
        assert_eq!(
            tools[0].parameters.get("path"),
            Some(&"hello.c".to_string())
        );
        assert!(tools[0]
            .parameters
            .get("content")
            .unwrap()
            .contains("stdio.h"));
    }

    #[test]
    fn test_parse_nested_element_edit_file() {
        let mut interceptor = ToolInterceptor::new();

        // Claude Code format for edit
        let content = r"<edit_file>
<path>src/main.rs</path>
<old_string>fn old() {}</old_string>
<new_string>fn new() {}</new_string>
</edit_file>";

        interceptor.push(content);
        assert!(interceptor.has_complete_block());

        let (tools, _, _) = interceptor.extract_tool_calls();

        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "edit_file");
        assert_eq!(
            tools[0].parameters.get("path"),
            Some(&"src/main.rs".to_string())
        );
        assert_eq!(
            tools[0].parameters.get("old_string"),
            Some(&"fn old() {}".to_string())
        );
        assert_eq!(
            tools[0].parameters.get("new_string"),
            Some(&"fn new() {}".to_string())
        );
    }

    #[test]
    fn test_strip_hallucinated_blocks_proper_closing() {
        let mut interceptor = ToolInterceptor::new();

        let content = r#"<invoke name="Bash">
<parameter name="command">ls</parameter>
</invoke>
<function_results>
<result>
file1.txt
file2.txt
</result>
</function_results>
<invoke name="Read">
<parameter name="file_path">file1.txt</parameter>
</invoke>"#;

        interceptor.push(content);
        interceptor.strip_hallucinated_blocks();

        let buf = interceptor.get_buffer();
        assert!(buf.contains("<invoke name=\"Bash\">"));
        assert!(buf.contains("<invoke name=\"Read\">"));
        assert!(!buf.contains("<function_results>"));
        assert!(!buf.contains("file1.txt\nfile2.txt"));
    }

    #[test]
    fn test_strip_hallucinated_blocks_malformed_closing() {
        let mut interceptor = ToolInterceptor::new();

        // Model uses </function_calls> instead of </function_results>
        let content = r#"<function_calls>
<invoke name="read_file">
<parameter name="path">Cargo.toml</parameter>
</invoke>
</function_calls>
<function_results>
<result>
[package]
name = "test"
</result>
</function_calls>
<function_calls>
<invoke name="edit_file">
<parameter name="path">Cargo.toml</parameter>
<parameter name="old_string">[dependencies]</parameter>
<parameter name="new_string">[dependencies]
serde = "1"</parameter>
</invoke>
</function_calls>"#;

        interceptor.push(content);
        interceptor.strip_hallucinated_blocks();

        let buf = interceptor.get_buffer();
        assert!(buf.contains("<invoke name=\"read_file\">"));
        assert!(buf.contains("<invoke name=\"edit_file\">"));
        assert!(!buf.contains("<function_results>"));
        assert!(!buf.contains("<function_calls>"));
        assert!(!buf.contains("</function_calls>"));
        assert!(!buf.contains("[package]"));
    }

    #[test]
    fn test_extract_all_tool_calls_multiple_invokes() {
        let mut interceptor = ToolInterceptor::new();

        let content = r#"Let me check things.

<function_calls>
<invoke name="read_file">
<parameter name="path">file.txt</parameter>
</invoke>
</function_calls>
<function_results>
<result>
fake content
</result>
</function_calls>

Now I'll edit it.

<function_calls>
<invoke name="edit_file">
<parameter name="path">file.txt</parameter>
<parameter name="old_string">old</parameter>
<parameter name="new_string">new</parameter>
</invoke>
</function_calls>
<function_results>
<result>
Successfully edited
</result>
</function_calls>

And run tests.

<function_calls>
<invoke name="Bash">
<parameter name="command">cargo test</parameter>
</invoke>
</function_calls>"#;

        interceptor.push(content);
        let (tools, text_parts) = interceptor.extract_all_tool_calls();

        assert_eq!(tools.len(), 3);
        assert_eq!(tools[0].name, "read_file");
        assert_eq!(tools[1].name, "edit_file");
        assert_eq!(tools[2].name, "Bash");

        // Text parts should contain the interleaved text
        let combined = text_parts.join(" ");
        assert!(combined.contains("Let me check things"));
        assert!(combined.contains("Now I'll edit it"));
        assert!(combined.contains("And run tests"));

        // Buffer should be cleared
        assert!(interceptor.get_buffer().is_empty());
    }

    #[test]
    fn test_extract_all_tool_calls_mixed_formats() {
        let mut interceptor = ToolInterceptor::new();

        // Mix of invoke and shorthand formats
        let content = r#"<invoke name="Read">
<parameter name="file_path">src/main.rs</parameter>
</invoke>
<result>fn main() {}</result>

<bash>cargo build</bash>"#;

        interceptor.push(content);
        let (tools, _text_parts) = interceptor.extract_all_tool_calls();

        assert_eq!(tools.len(), 2);
        assert_eq!(tools[0].name, "Read");
        assert_eq!(tools[1].name, "bash");
    }

    #[test]
    fn test_extract_all_tool_calls_no_hallucination() {
        let mut interceptor = ToolInterceptor::new();

        // Clean tool calls without hallucinated results
        let content = r#"I'll read the file.

<invoke name="Read">
<parameter name="file_path">test.txt</parameter>
</invoke>"#;

        interceptor.push(content);
        let (tools, text_parts) = interceptor.extract_all_tool_calls();

        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "Read");
        assert!(text_parts[0].contains("I'll read the file"));
    }

    #[test]
    fn test_extract_all_tool_calls_empty_buffer() {
        let mut interceptor = ToolInterceptor::new();
        interceptor.push("Just some text, no tool calls.");

        let (tools, text_parts) = interceptor.extract_all_tool_calls();

        assert!(tools.is_empty());
        assert_eq!(text_parts.len(), 1);
        assert!(text_parts[0].contains("Just some text"));
    }

    #[test]
    fn test_tool_aliases_roundtrip_name_resolves_to_canonical() {
        // Issue #477: aliased Claude-Code names must resolve to canonical
        // internal names via the single TOOL_ALIASES table.
        let cases = [
            ("read", "read_file"),
            ("write", "write_file"),
            ("edit", "edit_file"),
            ("glob", "list_files"),
            ("webfetch", "web_fetch"),
            ("websearch", "web_search"),
            ("bash", "bash"),
            ("grep", "grep"),
        ];
        for (alias, canonical) in cases {
            let info = TOOL_ALIASES
                .get(alias)
                .unwrap_or_else(|| panic!("alias {alias} missing from TOOL_ALIASES"));
            assert_eq!(
                info.canonical, canonical,
                "alias {alias} should resolve to {canonical}"
            );
        }

        // End-to-end: round-trip via to_tool_call.
        let tool = InterceptedToolCall {
            name: "Read".to_string(),
            parameters: [("file_path".to_string(), "/tmp/x".to_string())].into(),
            id: "id-1".to_string(),
        };
        let tc = tool.to_tool_call();
        assert_eq!(tc.function.name, "read_file");
    }

    #[test]
    fn test_tool_aliases_parameter_resolves_via_same_table() {
        // Issue #477: parameter-name aliases live in the same table as
        // tool-name aliases. Verify file_path -> path and contents -> content
        // are translated by the per-tool parameter_aliases map.
        let read_info = TOOL_ALIASES.get("read").expect("read alias");
        assert_eq!(read_info.parameter_aliases.get("file_path"), Some(&"path"));
        assert_eq!(read_info.parameter_aliases.get("path"), Some(&"path"));

        let write_info = TOOL_ALIASES.get("write_file").expect("write_file alias");
        assert_eq!(
            write_info.parameter_aliases.get("contents"),
            Some(&"content")
        );
        assert_eq!(
            write_info.parameter_aliases.get("content"),
            Some(&"content")
        );

        // End-to-end: contents -> content via to_tool_call.
        let tool = InterceptedToolCall {
            name: "Write".to_string(),
            parameters: [
                ("file_path".to_string(), "out.txt".to_string()),
                ("contents".to_string(), "hello".to_string()),
            ]
            .into(),
            id: "id-2".to_string(),
        };
        let tc = tool.to_tool_call();
        assert_eq!(tc.function.name, "write_file");
        let parsed: serde_json::Value = serde_json::from_str(&tc.function.arguments).unwrap();
        assert_eq!(parsed.get("path").and_then(|v| v.as_str()), Some("out.txt"));
        assert_eq!(
            parsed.get("content").and_then(|v| v.as_str()),
            Some("hello")
        );
        // Original aliased keys must NOT survive translation.
        assert!(parsed.get("file_path").is_none());
        assert!(parsed.get("contents").is_none());
    }

    #[test]
    fn test_tool_aliases_unknown_tool_is_none_no_panic() {
        // Issue #477: TOOL_ALIASES.get for an unknown tool returns None and
        // does NOT panic; to_tool_call passes the name through (lowercased)
        // and leaves parameter keys untouched.
        assert!(TOOL_ALIASES.get("ask_user_question").is_none());
        assert!(TOOL_ALIASES.get("definitely_not_a_real_tool").is_none());

        let tool = InterceptedToolCall {
            name: "Ask_User_Question".to_string(),
            parameters: [("question".to_string(), "why?".to_string())].into(),
            id: "id-3".to_string(),
        };
        let tc = tool.to_tool_call();
        // Lowercased passthrough — no silent rename for unknown tools.
        assert_eq!(tc.function.name, "ask_user_question");
        let parsed: serde_json::Value = serde_json::from_str(&tc.function.arguments).unwrap();
        // Parameter key untouched for unknown tools.
        assert_eq!(
            parsed.get("question").and_then(|v| v.as_str()),
            Some("why?")
        );
    }

    #[test]
    fn test_tool_aliases_property_every_old_alias_in_new_table() {
        // Issue #477: property check — every (tool_alias, canonical) pair
        // from the previous hand-written match in to_tool_call and every
        // (tool_alias, param_alias, canonical_param) triple from the
        // previous parameter-name match MUST be expressible via the new
        // TOOL_ALIASES table. If any drift is introduced, this test fails.

        // Tool-name aliases that existed in the old match block.
        let old_tool_aliases: &[(&str, &str)] = &[
            ("bash", "bash"),
            ("read", "read_file"),
            ("read_file", "read_file"),
            ("write", "write_file"),
            ("write_file", "write_file"),
            ("edit", "edit_file"),
            ("edit_file", "edit_file"),
            ("glob", "list_files"),
            ("list_files", "list_files"),
            ("grep", "grep"),
            ("webfetch", "web_fetch"),
            ("web_fetch", "web_fetch"),
            ("websearch", "web_search"),
            ("web_search", "web_search"),
        ];
        for (alias, canonical) in old_tool_aliases {
            let info = TOOL_ALIASES.get(*alias).unwrap_or_else(|| {
                panic!("old tool alias {alias} missing from new TOOL_ALIASES table")
            });
            assert_eq!(
                info.canonical, *canonical,
                "tool alias {alias} drifted: expected {canonical}, got {}",
                info.canonical
            );
        }

        // Parameter-name aliases that existed in the old match block,
        // expressed as (tool_alias, aliased_param, canonical_param).
        let old_param_aliases: &[(&str, &str, &str)] = &[
            ("bash", "command", "command"),
            // (read|write|write_file|edit|edit_file|read_file, file_path|path) -> path
            ("read", "file_path", "path"),
            ("read", "path", "path"),
            ("read_file", "file_path", "path"),
            ("read_file", "path", "path"),
            ("write", "file_path", "path"),
            ("write", "path", "path"),
            ("write_file", "file_path", "path"),
            ("write_file", "path", "path"),
            ("edit", "file_path", "path"),
            ("edit", "path", "path"),
            ("edit_file", "file_path", "path"),
            ("edit_file", "path", "path"),
            // (glob|grep, path) -> path
            ("glob", "path", "path"),
            ("grep", "path", "path"),
            // (write|write_file, content|contents) -> content
            ("write", "content", "content"),
            ("write", "contents", "content"),
            ("write_file", "content", "content"),
            ("write_file", "contents", "content"),
            // (edit|edit_file, old_string/new_string) -> identity
            ("edit", "old_string", "old_string"),
            ("edit", "new_string", "new_string"),
            ("edit_file", "old_string", "old_string"),
            ("edit_file", "new_string", "new_string"),
            // (glob|grep, pattern) -> pattern
            ("glob", "pattern", "pattern"),
            ("grep", "pattern", "pattern"),
        ];
        for (tool_alias, aliased_param, canonical_param) in old_param_aliases {
            let info = TOOL_ALIASES
                .get(*tool_alias)
                .unwrap_or_else(|| panic!("tool alias {tool_alias} missing"));
            let resolved = info.parameter_aliases.get(*aliased_param).copied();
            assert_eq!(
                resolved,
                Some(*canonical_param),
                "param alias drifted for ({tool_alias}, {aliased_param}): \
                 expected Some({canonical_param}), got {resolved:?}"
            );
        }
    }

    #[test]
    fn test_strip_hallucinated_blocks_no_closing_tag() {
        let mut interceptor = ToolInterceptor::new();

        // <function_results> with no closing tag — truncate from there
        let content = r#"<invoke name="Bash">
<parameter name="command">ls</parameter>
</invoke>
<function_results>
<result>
this never closes"#;

        interceptor.push(content);
        interceptor.strip_hallucinated_blocks();

        let buf = interceptor.get_buffer();
        assert!(buf.contains("<invoke name=\"Bash\">"));
        assert!(!buf.contains("<function_results>"));
        assert!(!buf.contains("this never closes"));
    }

    // ── crosslink #486: bash completion_note uses exit-code, not content ────
    //
    // Regression tests for the substring heuristic that previously inspected
    // bash stdout for the literal strings "error" / "Error" / "failed" and
    // suppressed the completion hint on false positives.

    #[test]
    fn completion_note_bash_success_emitted_even_when_content_says_error() {
        // Exit code 0 (is_error = false) but stdout contains the word
        // "error" — historically suppressed the hint; now the structured
        // success signal wins.
        let xml = format_tool_results_xml_with_names(&[(
            "id-1",
            Some("Bash"),
            "no errors found in build output\nfailed: 0",
            false,
        )]);
        assert!(
            xml.contains("<status>success</status>"),
            "exit-code success must surface as success status"
        );
        assert!(
            xml.contains("<completion_note>Command executed successfully.</completion_note>"),
            "bash success ⇒ completion_note must be emitted regardless of stdout content; got:\n{xml}"
        );
    }

    #[test]
    fn completion_note_bash_failure_has_no_completion_note() {
        // Exit code != 0 (is_error = true) takes the error branch and
        // intentionally omits the completion_note. The content here is the
        // mirror of the success test — plain ASCII that does NOT mention
        // "error" anywhere — to prove the routing is exit-code-driven, not
        // content-driven.
        let xml = format_tool_results_xml_with_names(&[("id-2", Some("Bash"), "foo", true)]);
        assert!(
            xml.contains("<status>error</status>"),
            "exit-code failure must surface as error status"
        );
        assert!(
            !xml.contains("<completion_note>"),
            "bash failure path must not emit a completion_note; got:\n{xml}"
        );
    }

    #[test]
    fn completion_note_bash_success_with_benign_content_still_emitted() {
        // Plain success with neutral stdout — sanity check that the happy
        // path keeps emitting the hint after the heuristic was removed.
        let xml =
            format_tool_results_xml_with_names(&[("id-3", Some("Bash"), "hello world", false)]);
        assert!(xml.contains("<status>success</status>"));
        assert!(xml.contains("<completion_note>Command executed successfully.</completion_note>"));
    }

    #[test]
    fn completion_note_non_bash_tools_unaffected() {
        // The write_file / edit_file branches were not part of #486 — verify
        // they still emit their own hints on success and stay quiet on
        // error, so the refactor didn't collapse them by accident.
        let write_ok = format_tool_results_xml_with_names(&[("id-w", Some("Write"), "ok", false)]);
        assert!(write_ok.contains("File created successfully"));

        let edit_err = format_tool_results_xml_with_names(&[("id-e", Some("Edit"), "boom", true)]);
        assert!(!edit_err.contains("<completion_note>"));
    }

    // ── crosslink #743: position-cached single-pass marker scanner ────────

    /// Forensic test for crosslink #743.
    ///
    /// Streams a long chunk of plain text followed by a single tool call in
    /// many small pushes (simulating the SSE delta loop). Before the fix each
    /// push triggered 19 full-buffer substring scans from
    /// `has_pending_tool_calls` / `has_complete_block`; now the scanner
    /// position-caches its progress and re-scans only the new suffix. We
    /// verify both flags end up correct and that the scanner *did* advance
    /// past every byte exactly once across the streaming sequence.
    #[test]
    fn scan_cache_extends_across_streaming_pushes_743() {
        let mut interceptor = ToolInterceptor::new();

        // Push 2KB of plain text in 128-byte chunks (no markers).
        let chunk = "x".repeat(128);
        for _ in 0..16 {
            interceptor.push(&chunk);
            assert!(
                !interceptor.has_pending_tool_calls(),
                "no markers yet — must report no pending tool calls"
            );
            assert!(
                !interceptor.has_complete_block(),
                "no markers yet — must report no complete block"
            );
        }

        // After 16 polls the cache cursor must equal the buffer length: the
        // scanner walked each byte once total (amortised O(N)), not 16 * N.
        assert_eq!(
            interceptor.scan.scan_pos,
            interceptor.buffer.len(),
            "scan cursor must track buffer length after each poll (regression of #743)"
        );

        // Now stream a tool call in halves and confirm the cache picks up the
        // marker that straddles the chunk boundary (the backtrack window).
        interceptor.push("<invoke nam");
        assert!(!interceptor.has_pending_tool_calls());
        interceptor.push("e=\"Bash\"><parameter name=\"command\">ls</parameter></invoke>");
        assert!(interceptor.has_pending_tool_calls());
        assert!(interceptor.has_complete_block());
    }

    /// Forensic test for crosslink #743.
    ///
    /// Ensures the cache is invalidated after extraction (`extract_tool_calls`
    /// shrinks the buffer to the post-block suffix). If the cache survived,
    /// stale `invoke_open_at` offsets would point past the new buffer end and
    /// the next `has_complete_block` would either panic on a slice or return
    /// the wrong answer. Drives a multi-block stream and asserts each round
    /// of poll → extract → poll behaves correctly.
    #[test]
    fn scan_cache_invalidated_on_extract_and_strip_743() {
        let mut interceptor = ToolInterceptor::new();

        // Two complete blocks back-to-back.
        interceptor.push(r#"<invoke name="Bash"><parameter name="command">a</parameter></invoke>"#);
        interceptor.push(r#"<invoke name="Bash"><parameter name="command">b</parameter></invoke>"#);
        assert!(interceptor.has_complete_block());

        // Extract first block — buffer shrinks to the second block.
        let (tools_a, _, _) = interceptor.extract_tool_calls();
        assert_eq!(tools_a.len(), 1);
        assert_eq!(tools_a[0].parameters.get("command"), Some(&"a".to_string()));

        // Cache MUST be invalidated; otherwise has_complete_block could read
        // a stale offset past the new buffer end.
        assert_eq!(interceptor.scan.scan_pos, 0, "cache must reset on extract");

        // Second block is still complete and extractable.
        assert!(interceptor.has_complete_block());
        let (tools_b, _, _) = interceptor.extract_tool_calls();
        assert_eq!(tools_b.len(), 1);
        assert_eq!(tools_b[0].parameters.get("command"), Some(&"b".to_string()));

        // Buffer is now empty; cache must agree.
        assert!(!interceptor.has_pending_tool_calls());
        assert!(!interceptor.has_complete_block());

        // strip_hallucinated_blocks also rewrites the buffer in place and
        // must therefore invalidate the cache.
        interceptor.push(
            r#"<invoke name="Bash"><parameter name="command">c</parameter></invoke><function_results>stale</function_results>"#,
        );
        assert!(interceptor.has_complete_block());
        interceptor.strip_hallucinated_blocks();
        assert_eq!(
            interceptor.scan.scan_pos, 0,
            "cache must reset after strip_hallucinated_blocks"
        );
        assert!(interceptor.has_complete_block());
        assert!(!interceptor.get_buffer().contains("stale"));
    }

    /// Forensic test for crosslink #743.
    ///
    /// Verifies the single-pass scanner correctly recognises shorthand-tool
    /// markers (open + close pairs across all 9 entries in `SHORTHAND_TOOLS`)
    /// without re-checking each marker against the full buffer. Iterates every
    /// shorthand tool and asserts both `has_pending_tool_calls` and
    /// `has_complete_block` flip on as expected. Catches regressions where
    /// a future scanner refactor accidentally drops a tool from dispatch.
    #[test]
    fn scan_recognises_every_shorthand_tool_743() {
        for tool in ToolInterceptor::SHORTHAND_TOOLS {
            let mut interceptor = ToolInterceptor::new();
            interceptor.push(&format!("<{tool}>body</{tool}>"));
            assert!(
                interceptor.has_pending_tool_calls(),
                "shorthand tool <{tool}> should be detected as pending"
            );
            assert!(
                interceptor.has_complete_block(),
                "shorthand tool <{tool}>...</{tool}> should be detected as complete"
            );
        }
    }

    // ── crosslink #899: depth/size caps in parse_nested_elements ─────────────

    /// #899 — A flood of `<param_N>v</param_N>` entries is capped at
    /// `MAX_NESTED_PARAMS` and the parser emits a visible `__parse_error`
    /// marker. Without the cap, this input would allocate an unbounded
    /// `HashMap` keyed on attacker-controlled names.
    #[test]
    fn fix899_param_cap_truncates_with_visible_error() {
        let interceptor = ToolInterceptor::new();
        // Build a payload with 200 distinct nested params — well beyond
        // `MAX_NESTED_PARAMS = 32`.
        let mut content = String::new();
        for i in 0..200 {
            use std::fmt::Write as _;
            write!(content, "<param_{i}>v{i}</param_{i}>").unwrap();
        }

        let mut params: HashMap<String, String> = HashMap::new();
        interceptor.parse_nested_elements(&content, &mut params);

        // We allow the cap-trigger marker as one extra entry, so the
        // observed size must be at most MAX_NESTED_PARAMS + 1.
        assert!(
            params.len() <= ToolInterceptor::MAX_NESTED_PARAMS + 1,
            "#899: expected <= MAX_NESTED_PARAMS+1 entries, got {}",
            params.len(),
        );
        let err = params
            .get("__parse_error")
            .expect("#899: cap breach must surface a __parse_error marker");
        assert!(
            err.contains("MAX_PARAMS=32") || err.contains("MAX_DEPTH=8"),
            "#899: marker must name the constants: got {err}"
        );
    }

    /// #899 — A degenerate input that never matches a closing tag must
    /// still terminate via the depth cap (the loop would otherwise scan
    /// every byte looking for closing tags that do not exist).
    #[test]
    fn fix899_depth_cap_terminates_pathological_input() {
        let interceptor = ToolInterceptor::new();
        // 10 000 unmatched opening tags. Each iteration skips one byte;
        // the depth cap (MAX_DEPTH * MAX_PARAMS = 256) is the only
        // guarantee that this returns in bounded time.
        let content = "<a>".repeat(10_000);
        let mut params: HashMap<String, String> = HashMap::new();
        interceptor.parse_nested_elements(&content, &mut params);
        // Either we exited via the depth cap (visible marker) or via
        // the natural "no closing tag" early-out — both are acceptable;
        // what matters is bounded work + bounded map size.
        assert!(
            params.len() <= ToolInterceptor::MAX_NESTED_PARAMS + 1,
            "#899: map size {} exceeded MAX_NESTED_PARAMS+1",
            params.len(),
        );
    }

    /// #899 — A normal payload with a handful of params is unaffected.
    #[test]
    fn fix899_normal_payload_parses_fully() {
        let interceptor = ToolInterceptor::new();
        let content = "<path>src/main.rs</path><content>hello</content>";
        let mut params: HashMap<String, String> = HashMap::new();
        interceptor.parse_nested_elements(content, &mut params);
        assert_eq!(params.get("path"), Some(&"src/main.rs".to_string()));
        assert_eq!(params.get("content"), Some(&"hello".to_string()));
        assert!(
            !params.contains_key("__parse_error"),
            "#899: under-cap payload must not raise a parse-error marker"
        );
    }

    // ── crosslink #343: buffer / attribute caps ─────────────────────────

    /// #343 — push beyond `MAX_BUFFER_BYTES` drops the trailing content
    /// instead of growing without bound. The cap is enforced on each
    /// push call so a single multi-megabyte chunk also gets clamped.
    #[test]
    fn fix343_buffer_cap_drops_excess_push() {
        let mut interceptor = ToolInterceptor::new();
        // Fill the buffer right up to the cap.
        let fill = "a".repeat(ToolInterceptor::MAX_BUFFER_BYTES);
        interceptor.push(&fill);
        assert_eq!(
            interceptor.get_buffer().len(),
            ToolInterceptor::MAX_BUFFER_BYTES,
            "buffer must be exactly at the cap after initial fill"
        );
        // Subsequent push must be dropped entirely.
        interceptor.push("overflow");
        assert_eq!(
            interceptor.get_buffer().len(),
            ToolInterceptor::MAX_BUFFER_BYTES,
            "#343: pushes beyond MAX_BUFFER_BYTES must be dropped"
        );
    }

    /// #343 — push with partial room takes only what fits and drops the
    /// remainder. Critical for streaming: a chunk that straddles the cap
    /// boundary must not grow the buffer past the cap.
    #[test]
    fn fix343_partial_fit_truncates_at_boundary() {
        let mut interceptor = ToolInterceptor::new();
        let fill = "x".repeat(ToolInterceptor::MAX_BUFFER_BYTES - 10);
        interceptor.push(&fill);
        // Try to push 100 more bytes — only 10 should land.
        interceptor.push(&"y".repeat(100));
        assert_eq!(
            interceptor.get_buffer().len(),
            ToolInterceptor::MAX_BUFFER_BYTES,
            "#343: partial-fit push must clamp to the cap"
        );
    }

    /// #343 — A tag with too many attributes only emits up to
    /// `MAX_ATTRIBUTES` parameters; the remainder are silently dropped
    /// and logged. Prevents a hostile `<bash a1="" … aN="">` from
    /// allocating an unbounded `HashMap`.
    #[test]
    fn fix343_shorthand_attribute_cap_bounds_map_size() {
        use std::fmt::Write as _;
        let interceptor = ToolInterceptor::new();
        // Construct a tag with twice the cap's worth of attributes.
        let cap = ToolInterceptor::MAX_ATTRIBUTES;
        let mut attrs = String::with_capacity(cap * 16);
        for i in 0..cap * 2 {
            let _ = write!(attrs, " a{i}=\"v\"");
        }
        let tag = format!("<bash{attrs}></bash>");
        let parsed = interceptor.parse_shorthand_tag("bash", &tag);
        let parsed = parsed.expect("shorthand parse should succeed even when capped");
        // The map must be bounded at the cap (no overshoot).
        assert!(
            parsed.parameters.len() <= cap,
            "#343: attribute count {} exceeded MAX_ATTRIBUTES={}",
            parsed.parameters.len(),
            cap
        );
    }

    /// #343 — a normal small tag with a few attributes still parses all
    /// of them. Pins that the cap does not regress legitimate inputs.
    #[test]
    fn fix343_under_cap_attributes_parse_all() {
        let interceptor = ToolInterceptor::new();
        let parsed = interceptor
            .parse_shorthand_tag(
                "edit_file",
                r#"<edit_file path="a.rs" old_string="foo" new_string="bar"></edit_file>"#,
            )
            .expect("under-cap tag must parse");
        assert_eq!(parsed.parameters.get("path"), Some(&"a.rs".to_string()));
        assert_eq!(
            parsed.parameters.get("old_string"),
            Some(&"foo".to_string())
        );
        assert_eq!(
            parsed.parameters.get("new_string"),
            Some(&"bar".to_string())
        );
    }
}
