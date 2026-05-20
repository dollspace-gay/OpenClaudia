//! `tool_search` — deferred tool-schema loading (crosslink #614).
//!
//! Large tool catalogues (MCP-imported tools, plugin tools, marketplace
//! installs) bloat the system prompt past the point where the model can fit
//! useful context. The remedy CC uses is "deferred tools": the model sees a
//! catalogue of *names* in `<system-reminder>`, and explicitly asks for the
//! schema of the subset it actually needs via this tool. The schema body is
//! returned in a `<functions>` envelope identical to the bootstrap tool list,
//! so the model can immediately call any returned tool.
//!
//! Two query forms are supported, mirroring the CC contract documented in the
//! `ToolSearch` system prompt block:
//!
//! * **Direct selection** — `select:Read,Edit,Grep` returns the schemas of
//!   those exact tools. Names not present in the registry are silently
//!   ignored; partial matches still succeed for the names that did match
//!   (callers can detect a miss by counting returned `<function>` blocks).
//! * **Keyword search** — `notebook jupyter` returns up to `max_results`
//!   ranked matches. A leading `+term` token (e.g. `+slack send`) forces
//!   `slack` to appear in the tool name, then ranks remaining terms.
//!
//! ## Design notes
//!
//! * **Pure registry view.** Nothing here loads MCP servers or files — the
//!   search index is built from the static [`ToolRegistry`] every call. The
//!   registry is small enough that a fresh O(n) scan is cheaper than the
//!   cache-coherence cost of keeping a precomputed inverted index in sync
//!   with registry mutations (which are rare-to-nonexistent at runtime).
//! * **Schema fidelity.** The returned `<function>` blocks contain the
//!   exact JSON `ToolHandler::definition` emits, so a tool that loaded via
//!   `tool_search` is byte-for-byte indistinguishable from a tool loaded
//!   from the bootstrap list.
//! * **Output shape mirrors CC.** Each match becomes one
//!   `<function>{...}</function>` line inside a single `<functions>` block
//!   wrapped in a single string — matching the encoding the model already
//!   knows from the prompt-level tool list.

use serde_json::Value;
use std::collections::HashMap;
use std::hash::BuildHasher;

use super::registry::{registry, ToolHandler};

/// Maximum number of keyword-search results returned when the caller does not
/// supply `max_results`. Chosen to match the default cited in the deferred-tool
/// system prompt block.
pub(crate) const DEFAULT_MAX_RESULTS: usize = 5;

/// Hard cap on `max_results` regardless of caller request — guards against a
/// poisoned argument flooding the model context with the entire registry.
pub(crate) const MAX_RESULTS_CEILING: usize = 50;

/// Render a `Vec<&dyn ToolHandler>` to the `<functions>...</functions>`
/// envelope shape CC uses.
fn render_envelope(matches: &[&'static dyn ToolHandler]) -> String {
    let mut out = String::new();
    out.push_str("<functions>\n");
    for handler in matches {
        out.push_str("<function>");
        // The definition is already JSON; `to_string` keeps it on one line so
        // each `<function>...</function>` is a single tag, matching the form
        // documented in the prompt-side schema block.
        let def = handler.definition();
        out.push_str(&def.to_string());
        out.push_str("</function>\n");
    }
    out.push_str("</functions>");
    out
}

/// Resolve a `select:` query into the matching handlers, in the order the
/// names appeared in the query.
fn resolve_select(spec: &str) -> Vec<&'static dyn ToolHandler> {
    let reg = registry();
    spec.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .filter_map(|name| reg.get(name))
        .collect()
}

/// Tokenise a free-form keyword query into `(required, ranked)` term slices.
///
/// A `+term` token (no inner whitespace) is treated as a *required* substring
/// match against the tool name. The remaining terms are scored for ranking
/// but are not gates.
fn split_terms(query: &str) -> (Vec<String>, Vec<String>) {
    let mut required = Vec::new();
    let mut ranked = Vec::new();
    for raw in query.split_whitespace() {
        if let Some(stripped) = raw.strip_prefix('+') {
            if !stripped.is_empty() {
                required.push(stripped.to_ascii_lowercase());
            }
        } else {
            ranked.push(raw.to_ascii_lowercase());
        }
    }
    (required, ranked)
}

/// Score a handler against a tokenised keyword query.
///
/// Returns `None` when a required term is missing from the handler name,
/// otherwise returns the sum of substring-hit weights across name + schema
/// description. A name hit is worth more than a description hit so tools
/// whose name literally matches the query rank above tools that merely
/// mention the term in a long description.
fn score_handler(
    handler: &'static dyn ToolHandler,
    required: &[String],
    ranked: &[String],
) -> Option<u32> {
    let name_lc = handler.name().to_ascii_lowercase();
    for req in required {
        if !name_lc.contains(req) {
            return None;
        }
    }

    let description = handler
        .definition()
        .get("function")
        .and_then(|f| f.get("description"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_ascii_lowercase();

    let mut score: u32 = 0;
    for term in ranked.iter().chain(required.iter()) {
        if name_lc.contains(term) {
            score = score.saturating_add(10);
        }
        if description.contains(term) {
            score = score.saturating_add(1);
        }
    }
    // A handler that matched every required term but scored nothing in the
    // ranked pool is still a legitimate hit (the required gate is the user's
    // strongest signal). Floor at 1 so it survives the `> 0` filter below.
    if score == 0 && !required.is_empty() {
        score = 1;
    }
    Some(score)
}

/// Resolve a keyword-search query, returning up to `max_results` handlers.
fn resolve_keyword(query: &str, max_results: usize) -> Vec<&'static dyn ToolHandler> {
    let (required, ranked) = split_terms(query);
    if required.is_empty() && ranked.is_empty() {
        return Vec::new();
    }

    let mut scored: Vec<(u32, &'static dyn ToolHandler)> = super::registry::iter_handlers()
        .filter_map(|h| score_handler(h, &required, &ranked).map(|s| (s, h)))
        .filter(|(s, _)| *s > 0)
        .collect();

    // Sort by descending score; break ties by handler name for determinism.
    scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.name().cmp(b.1.name())));
    scored
        .into_iter()
        .take(max_results)
        .map(|(_, h)| h)
        .collect()
}

/// Execute the `tool_search` tool.
///
/// Required argument: `query` (string). Optional: `max_results` (integer,
/// capped at [`MAX_RESULTS_CEILING`]). Returns `(text, is_error)`.
#[must_use]
pub fn execute_tool_search<S: BuildHasher>(args: &HashMap<String, Value, S>) -> (String, bool) {
    let Some(query) = args.get("query").and_then(Value::as_str) else {
        return (
            "tool_search: missing required argument `query`".to_string(),
            true,
        );
    };

    let max_results = args
        .get("max_results")
        .and_then(Value::as_u64)
        .map_or(DEFAULT_MAX_RESULTS, |v| {
            usize::try_from(v).unwrap_or(DEFAULT_MAX_RESULTS)
        })
        .clamp(1, MAX_RESULTS_CEILING);

    let matches = query
        .strip_prefix("select:")
        .map_or_else(|| resolve_keyword(query, max_results), resolve_select);

    if matches.is_empty() {
        return (
            format!("tool_search: no matches for query `{query}`"),
            false,
        );
    }

    (render_envelope(&matches), false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn missing_query_errors() {
        let (text, is_err) = execute_tool_search(&HashMap::new());
        assert!(is_err);
        assert!(text.contains("missing required argument"));
    }

    #[test]
    fn select_returns_named_tools() {
        let mut args = HashMap::new();
        args.insert("query".to_string(), json!("select:bash,read_file"));
        let (text, is_err) = execute_tool_search(&args);
        assert!(!is_err);
        assert!(text.contains("<functions>"));
        assert!(text.contains("\"name\":\"bash\""));
        assert!(text.contains("\"name\":\"read_file\""));
    }

    #[test]
    fn select_unknown_names_are_skipped() {
        let mut args = HashMap::new();
        args.insert(
            "query".to_string(),
            json!("select:bash,not_a_real_tool_xyz"),
        );
        let (text, is_err) = execute_tool_search(&args);
        assert!(!is_err);
        assert!(text.contains("\"name\":\"bash\""));
        // Function blocks are one per line; only one survived.
        assert_eq!(text.matches("<function>").count(), 1);
    }

    #[test]
    fn select_all_unknown_yields_no_match() {
        let mut args = HashMap::new();
        args.insert("query".to_string(), json!("select:totally_made_up"));
        let (text, is_err) = execute_tool_search(&args);
        assert!(!is_err);
        assert!(text.starts_with("tool_search: no matches"));
    }

    #[test]
    fn keyword_query_finds_relevant_tools() {
        let mut args = HashMap::new();
        args.insert("query".to_string(), json!("bash"));
        args.insert("max_results".to_string(), json!(3));
        let (text, is_err) = execute_tool_search(&args);
        assert!(!is_err);
        // bash-related tools should surface
        assert!(text.contains("\"name\":\"bash\""));
    }

    #[test]
    fn required_term_filter() {
        // `+kill` forces the name to contain "kill"; bash should NOT appear.
        let (required, ranked) = split_terms("+kill shell");
        assert_eq!(required, vec!["kill"]);
        assert_eq!(ranked, vec!["shell"]);

        let mut args = HashMap::new();
        args.insert("query".to_string(), json!("+kill shell"));
        let (text, _is_err) = execute_tool_search(&args);
        // kill_shell is the only kill-named handler in the registry.
        assert!(text.contains("\"name\":\"kill_shell\""));
        assert!(!text.contains("\"name\":\"bash\""));
    }

    #[test]
    fn max_results_capped() {
        let mut args = HashMap::new();
        args.insert("query".to_string(), json!("file"));
        args.insert("max_results".to_string(), json!(10_000_u64));
        let (text, is_err) = execute_tool_search(&args);
        assert!(!is_err);
        let block_count = text.matches("<function>").count();
        assert!(
            block_count <= MAX_RESULTS_CEILING,
            "got {block_count} blocks, ceiling is {MAX_RESULTS_CEILING}"
        );
    }

    #[test]
    fn empty_query_returns_no_matches() {
        let mut args = HashMap::new();
        args.insert("query".to_string(), json!("   "));
        let (text, is_err) = execute_tool_search(&args);
        assert!(!is_err);
        assert!(text.starts_with("tool_search: no matches"));
    }
}
