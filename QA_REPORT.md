# OpenClaudia Full QA Report

Generated: 2026-04-01 | Scope: Complete codebase (100+ source files)

## Summary

| Severity | Count | Categories |
|----------|-------|-----------|
| CRITICAL | 24 | Panics, data loss, path traversal, race conditions |
| WARNING | 48 | Logic errors, missing validation, silent failures, incomplete features |
| NITPICK | 32 | Style, naming, dead code, documentation |

---

## CRITICAL Issues

### C-1: Path traversal in file/write.rs and file/edit.rs
- **Files**: `src/tools/file/write.rs:30`, `src/tools/file/edit.rs:25-28`
- **Issue**: When `canonicalize()` fails (file doesn't exist), falls back to unsanitized original path. `../../etc/passwd` bypasses checks.
- **Fix**: Canonicalize parent directory; reject writes if parent can't be resolved.

### C-2: Null content with tool_calls creates invalid Anthropic request
- **File**: `src/main.rs:~2138,2869`
- **Issue**: When model returns only tool_calls (no text), `content` is set to `serde_json::Value::Null`. Anthropic API rejects this format.
- **Fix**: Use `"content": ""` or omit the field entirely.

### C-3: Data loss on failed API responses
- **File**: `src/main.rs:~950,1801,3255,3386`
- **Issue**: When API calls fail (hooks reject, parsing fails), user message is popped without saving session. Power loss = message lost.
- **Fix**: Save session before popping messages, or mark turns as "failed" instead of deleting.

### C-4: OAuth token refresh race condition (TOCTOU)
- **Files**: `src/oauth.rs:324-335`, `src/claude_credentials.rs:103-108`
- **Issue**: No locking on token refresh. Multiple threads could both trigger refresh simultaneously.
- **Fix**: Add file-based lock (like Claude Code uses `~/.claude/.lock`).

### C-5: Unwrap panics on editor creation
- **File**: `src/main.rs:843,854`
- **Issue**: `DefaultEditor::new().unwrap()` panics if terminal unavailable (detached session, restricted env).
- **Fix**: Return error gracefully instead of panicking mid-session.

### C-6: VDD config validate() never called
- **File**: `src/config/vdd.rs:207-249`
- **Issue**: `validate()` function exists but is never invoked. Validation could fail silently.
- **Fix**: Call `config.vdd.validate()?` in `load_config()`.

### C-7: Plugin path traversal via marketplace source
- **File**: `src/plugins/manager.rs:465-475`
- **Issue**: Plugin source path joined without sanitization. `source: "../../../../etc/passwd"` reads arbitrary files.
- **Fix**: Canonicalize paths, verify they remain within marketplace directory.

### C-8: LSP response parser infinite loop
- **File**: `src/tools/lsp.rs:286-325`
- **Issue**: If LSP server sends only notifications, `read_lsp_response` loops forever waiting for expected response ID.
- **Fix**: Add max iteration count (100) or timeout.

### C-9: Audit logger silently fails
- **File**: `src/session/audit.rs:14-22,26-38`
- **Issue**: `create_dir_all()` and `writeln!()` errors swallowed with `.ok()`. Audit log silently incomplete.
- **Fix**: Return `Result` and propagate, or log to stderr on failure.

### C-10: Session directory creation failure silently ignored
- **File**: `src/session/mod.rs:348`
- **Issue**: `create_dir_all().ok()` swallows error. Session persistence fails silently.
- **Fix**: Propagate error to caller.

### C-11: Unsafe unwrap in session undo
- **File**: `src/cli/repl/mod.rs:116-125`
- **Issue**: `self.messages.pop().unwrap()` panics if messages.len() < 2.
- **Fix**: Use `pop()` with proper Option handling.

### C-12: Empty API key strings pass validation
- **File**: `src/config/provider.rs:34`
- **Issue**: `api_key: Option<String>` allows `Some("")` which silently passes but fails at API level.
- **Fix**: Validate non-empty on load: `api_key.filter(|k| !k.is_empty())`.

### C-13: VDD JSON schema not validated
- **File**: `src/vdd/mod.rs:331-345`
- **Issue**: Adversary response parsed as JSON but never validated against schema. `{"findings": "not an array"}` crashes later.
- **Fix**: Validate response matches AdversaryResponse schema before use.

### C-14: Task deletion returns Err instead of Ok
- **File**: `src/session/task.rs:430-440`
- **Issue**: When task is deleted, function returns `Err(...)` but caller expects `Ok(&Task)`.
- **Fix**: Return `Ok(&task)` or change return type to `Result<Option<&Task>>`.

### C-15: Pricing pattern order bug
- **File**: `src/session/pricing.rs:70`
- **Issue**: `"gpt-4o"` pattern matches before `"gpt-4o-mini"` check due to `.contains()` order.
- **Fix**: Check `"gpt-4o-mini"` before `"gpt-4o"`.

### C-16: Keybinding case sensitivity by accident
- **File**: `src/config/keybindings.rs:74`
- **Issue**: `get_action()` lowercases key at lookup, but HashMap stores lowercase from defaults. Works by coincidence.
- **Fix**: Normalize keys at insertion time, document behavior.

### C-17: Hook check_blocked returns first error only
- **File**: `src/hooks/mod.rs:496-507`
- **Issue**: Returns after first blocked output, silently ignoring subsequent hook errors.
- **Fix**: Collect all errors into a combined error.

### C-18: Notebook edit has no symlink validation
- **File**: `src/tools/file/notebook.rs:80-87`
- **Issue**: Unlike edit.rs and write.rs, notebook reads/writes follow symlinks without canonicalization.
- **Fix**: Add canonicalization matching other file tools.

### C-19: Plan mode path comparison inconsistent
- **File**: `src/session/state.rs:129-159`
- **Issue**: `is_tool_allowed_in_plan_mode()` uses `canonicalize()` which fails for non-existent files, then compares different path formats.
- **Fix**: Use consistent normalization (always relative or always absolute).

### C-20: VDD provider URL not validated
- **File**: `src/vdd/mod.rs:444-455`
- **Issue**: `provider.base_url` used in HTTP request without URL validation. Malformed URLs cause cryptic errors.
- **Fix**: Validate URLs at config load time with `url::Url::parse()`.

### C-21: Unsafe YAML front matter parsing in plugins
- **File**: `src/plugins/mod.rs:380-381`
- **Issue**: Hand-rolled YAML parsing instead of using serde_yaml. Malformed arrays crash.
- **Fix**: Use proper YAML parser for front matter.

### C-22: Stream timeout discards partial content
- **File**: `src/main.rs:~1845-1850`
- **Issue**: If stream times out, partial `full_content` already received is NOT saved. User saw it but it's lost.
- **Fix**: Save whatever was received as "[partial response - stream timed out]".

### C-23: Anthropic tool argument serialization lossy
- **File**: `src/providers/anthropic.rs:213`
- **Issue**: `serde_json::to_string(block.get("input")?).ok()?` double-serializes JSON input values.
- **Fix**: Use the Value directly or validate structure before serialization.

### C-24: Session persist writes multiple files non-atomically
- **File**: `src/session/mod.rs:454-465`
- **Issue**: Writes to `{id}.json`, `latest.json`, and `handoff.md` sequentially. Partial failure leaves inconsistent state.
- **Fix**: Write to temp files first, then rename atomically.

---

## WARNING Issues

### W-1: Malformed tool args bypass permission checks
- **File**: `src/main.rs:~1464,2164,2895`
- **Issue**: `unwrap_or_default()` on malformed JSON produces empty HashMap. Empty args always pass permission checks.

### W-2: No retry on transient network errors
- **File**: `src/main.rs:~3382-3387`
- **Issue**: DNS, timeout, connection reset all treated the same as auth errors. No retry logic.

### W-3: Gemini error responses produce blank output
- **File**: `src/main.rs:~1276-1378`
- **Issue**: Rate limits (429) or quota errors from Gemini API silently produce empty response.

### W-4: Background shell limit checked after spawn
- **File**: `src/tools/bash/mod.rs:153-159`
- **Issue**: MAX_BACKGROUND_SHELLS checked after process spawned; brief window for 51st process.

### W-5: Cron step value `*/0` accepted
- **File**: `src/tools/cron.rs:81-88`
- **Issue**: `*/0` passes validation but creates invalid cron expression.

### W-6: LSP recursive symbol parsing has no depth limit
- **File**: `src/tools/lsp.rs:482`
- **Issue**: Recursive `parse_symbols` with no depth limit could stack overflow on deep hierarchies.

### W-7: Git commands in worktree have no timeout
- **File**: `src/tools/worktree.rs:34-50`
- **Issue**: `git rev-parse` can hang indefinitely on corrupted/network repos.

### W-8: ThinkingConfig default inconsistency
- **File**: `src/config/provider.rs:49-50`
- **Issue**: `Default::default()` returns `enabled=false` but serde default returns `enabled=true`.

### W-9: Guardrails max_files uses 0 for "unlimited"
- **File**: `src/config/guardrails.rs:101-103`
- **Issue**: Magical zero pattern is error-prone. Should use `Option<u32>` or explicit enum.

### W-10: Hook stdin write silently ignored
- **File**: `src/hooks/mod.rs:593-595`
- **Issue**: `let _ = stdin.write_all(...)` swallows pipe errors. Hook gets no input.

### W-11: Settings file load order inverted
- **File**: `src/hooks/claude_compat.rs:68`
- **Issue**: Project-level loaded first but should be user-level first (project overrides user).

### W-12: Hook merge allows duplicate entries
- **File**: `src/hooks/merge.rs:16-35`
- **Issue**: `.extend()` preserves duplicates. Loading config twice duplicates all hooks.

### W-13: Plan mode blocks create infinite retry loop
- **File**: `src/main.rs:~1442-1461`
- **Issue**: Model retries blocked tools forever. No max-retries or escalation to user.

### W-14: Duplicate tool call detection bypassable
- **File**: `src/main.rs:~2108-2131`
- **Issue**: Only exact `(name, arguments)` match detected. Slight arg changes bypass detection.

### W-15: Vim/effort state not persisted on resume
- **File**: `src/main.rs:~517-522`
- **Issue**: `--resume` resets vim_enabled and effort_level.

### W-16: Event handler thread death is silent
- **File**: `src/tui/events.rs:40-57`
- **Issue**: If event thread panics, TUI hangs forever waiting for events.

### W-17: Memory DB stale after working directory change
- **File**: `src/main.rs:~527-560`
- **Issue**: Memory DB initialized once at startup based on cwd. No refresh if project changes.

### W-18: Auto-learner stores unbounded data
- **File**: `src/main.rs:~923-933`
- **Issue**: Raw tool results stored without size limits. Memory DB can bloat over time.

### W-19: OAuth credential refresh doesn't notify OAuthStore
- **File**: `src/claude_credentials.rs:125-208`
- **Issue**: File updated but OAuthStore has stale tokens if running in parallel.

### W-20: Qwen thinking config dead code
- **File**: `src/providers/qwen.rs:47-52`
- **Issue**: `enable_thinking` set to both true AND false in conditional branches. One branch is dead.

### W-21: OpenAI reasoning_effort sent to non-reasoning models
- **File**: `src/providers/openai.rs:38-56`
- **Issue**: `reasoning_effort` added even for models that don't support it.

### W-22: Google adapter uses UUID per tool call (no correlation)
- **File**: `src/providers/google.rs:194`
- **Issue**: Same tool called twice gets different UUIDs, breaking correlation across turns.

### W-23: Floating-point comparison in VDD confabulation
- **File**: `src/vdd/confabulation.rs:40-42`
- **Issue**: Strict `>=` comparison on f32 rate. 0.750000001 won't trigger at threshold 0.75.

### W-24: VDD builder revision infinite loop risk
- **File**: `src/vdd/mod.rs:375-395`
- **Issue**: If builder returns identical response, loop continues until max_iterations.

### W-25: Plugin install path not validated
- **File**: `src/plugins/mod.rs:115-129`
- **Issue**: `install_path` from JSON not validated as absolute or within allowed locations.

### W-26: Pricing data hardcoded with no staleness check
- **File**: `src/session/pricing.rs:18-106`
- **Issue**: Model pricing never updates. No "last updated" metadata.

### W-27: Session cleanup ignores deletion errors
- **File**: `src/session/mod.rs:538-552`
- **Issue**: `cleanup_old_sessions()` silently ignores `.ok()` on file deletions.

### W-28: Plan file path traversal via session ID
- **File**: `src/cli/repl/plan_mode.rs:13`
- **Issue**: Session ID not validated before use in path. Could contain `../`.

### W-29: File expansion leaves @reference on failure
- **File**: `src/cli/repl/input.rs:200-209`
- **Issue**: Failed file reads warn but leave original @reference in message, confusing the AI.

### W-30: Token estimation ignores cache tokens
- **File**: `src/main.rs:~1775-1796`
- **Issue**: Status bar token count mixes input/output/cache inconsistently.

### W-31: Incomplete error handling in git slash commands
- **File**: `src/cli/repl/slash.rs:853-884`
- **Issue**: Git commands don't distinguish "not found" from "command failed".

### W-32: Tool result diff block parse silently fails
- **File**: `src/cli/display/tool_result.rs:91`
- **Issue**: Malformed diff JSON silently produces no diff display, no logging.

### W-33: PDF page count determination silently fails
- **File**: `src/tools/file/read.rs:127-136`
- **Issue**: If pdfinfo output is malformed, page count check skipped silently.

### W-34: Todo content has no length validation
- **File**: `src/tools/todo.rs:35-62`
- **Issue**: No length limits on todo content. Extremely long strings could cause display issues.

### W-35: Filename encoding lossy
- **File**: `src/tools/file/list.rs:13`
- **Issue**: `.to_string_lossy()` silently replaces invalid UTF-8 filenames.

### W-36: Session compact doesn't check minimum messages
- **File**: `src/cli/repl/session_io.rs:23`
- **Issue**: Hard-coded `preserve_count = 4` may cause issues in very short sessions.

### W-37: Hook timeout fallback inconsistent
- **File**: `src/hooks/merge.rs:99`
- **Issue**: Fallback timeout 60s differs from `default_prompt_timeout()=30` in config.

### W-38: Permission glob patterns not validated at load
- **File**: `src/config/permissions.rs:16`
- **Issue**: Malformed glob patterns silently fail at runtime instead of config time.

### W-39: Background shell mutex poison silently drops lines
- **File**: `src/tools/bash/mod.rs:99-104`
- **Issue**: If stdout reader's mutex is poisoned, output lines are silently dropped.

### W-40: VDD adversary provider/key not validated at startup
- **File**: `src/vdd/mod.rs:289-310`
- **Issue**: Missing provider config causes cryptic error at runtime, not startup.

### W-41: Doctor command continues after config failure
- **File**: `src/cli/commands/doctor.rs:45-53`
- **Issue**: Config load error printed but subsequent checks assume config exists.

### W-42: Notebook cell metadata incomplete for Jupyter 4.5+
- **File**: `src/tools/file/notebook.rs:152-162`
- **Issue**: New cells created with empty metadata `{}` but nbformat 4.5+ requires `id` field.

### W-43: Credential path has no symlink validation
- **File**: `src/claude_credentials.rs:64-66`
- **Issue**: `~/.claude/.credentials.json` path not canonicalized. Symlink could redirect.

### W-44: Editor open failure swallowed
- **File**: `src/cli/repl/input.rs:156`
- **Issue**: Temp file read-only or editor can't access — error swallowed by `.ok()`.

### W-45: Clipboard failure silent
- **File**: `src/cli/repl/slash.rs:356-361`
- **Issue**: Clipboard set_text failure silently ignored. User thinks content copied.

### W-46: Loop mode shutdown channel error ignored
- **File**: `src/cli/commands/loop_cmd.rs:150`
- **Issue**: `shutdown_rx_loop.changed().await` Err on closed channel silently ignored.

### W-47: Config environment variable loading race
- **File**: `src/config/mod.rs:152-175`
- **Issue**: Sequential `set_override` calls have potential race if env vars change concurrently.

### W-48: Chainlink output format fragile parsing
- **File**: `src/vdd/static_analysis.rs:103-109`
- **Issue**: Issue ID extracted via `split('#').nth(1)`. Format change breaks silently.

---

## NITPICK Issues

### N-1: Magic number 4096 max_tokens hardcoded
- **File**: `src/main.rs:~1139,1187,1199` (7 occurrences)

### N-2: Inconsistent error message formats
- **File**: `src/main.rs` (various), `src/tools/bash/*.rs`

### N-3: Dead code — vim field references
- **File**: `src/main.rs:599-600`

### N-4: Output truncation limits inconsistent (50000 vs 100000)
- **File**: `src/tools/bash/mod.rs:365`, `src/tools/file/read.rs:334`

### N-5: Terminal size queried repeatedly, not cached
- **File**: `src/tui/mod.rs:310,765`

### N-6: Colors defined in both constants and Theme struct
- **File**: `src/tui/mod.rs:40-44`

### N-7: Session file dedup uses O(n) Vec instead of HashSet
- **File**: `src/session/mod.rs:234-240`

### N-8: Pricing multipliers undocumented
- **File**: `src/session/pricing.rs:110-120`

### N-9: Config default pattern redundancy
- **File**: `src/config/proxy.rs:5-32`

### N-10: Glob pattern semantics undocumented
- **File**: `src/config/guardrails.rs:95-100`

### N-11: Default keybindings hardcoded instead of data-driven
- **File**: `src/config/keybindings.rs:52-68`

### N-12: Hook decision logic scattered across three mechanisms
- **File**: `src/hooks/mod.rs:373-398`

### N-13: ModelInfo uses i64 for timestamp instead of proper type
- **File**: `src/providers/mod.rs:56`

### N-14: Ollama tool call ID uses loop index
- **File**: `src/providers/ollama.rs:123-124`

### N-15: ZAI thinking config redundant
- **File**: `src/providers/zai.rs:63-66`

### N-16: Plugin manifest naming inconsistency
- **File**: `src/plugins/manifest.rs:40-48`

### N-17: VDD finding iteration not used in dedup
- **File**: `src/vdd/finding.rs:63`

### N-18: VDD confabulation CWE case-sensitive
- **File**: `src/vdd/confabulation.rs:74-117`

### N-19: VDD skip threshold arbitrary (100 chars)
- **File**: `src/vdd/mod.rs:218`

### N-20: Unused output_tokens in Gemini
- **File**: `src/main.rs:~1351`

### N-21: Diff display max lines constant
- **File**: `src/cli/repl/review.rs:46`

### N-22: Overly broad "main"/"master" branch check
- **File**: `src/cli/repl/slash.rs:962`

### N-23: Verbose auth banner formatting
- **File**: `src/cli/commands/auth.rs:74-80`

### N-24: Inconsistent eprintln vs println for errors
- **File**: `src/cli/repl/slash.rs:1075`

### N-25: Vim process_find/process_replace incomplete (returns None)
- **File**: `src/cli/repl/vim.rs:410-425`

### N-26: Markdown parser missing edge cases
- **File**: `src/tui/mod.rs:272-375`

### N-27: Test uses hardcoded home path
- **File**: `src/claude_credentials.rs:270-272`

### N-28: OAuth RwLock poison recovery not logged
- **File**: `src/oauth.rs:290,299,307`

### N-29: LSP detect_language_server not pub
- **File**: `src/tools/lsp.rs:56`

### N-30: Background shell UUID truncated to 8 chars
- **File**: `src/tools/bash/mod.rs:49`

### N-31: Unused HashMap import
- **File**: `src/tools/plan_mode.rs:3`

### N-32: Notebook source_to_line_array missing docs
- **File**: `src/tools/file/notebook.rs:7-27`
