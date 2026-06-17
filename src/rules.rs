//! Rules Engine - Loads markdown rules for context injection.
//!
//! Loads .md files from .openclaudia/rules/ directory and injects them
//! as context based on file types being edited.

use regex::Regex;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use tracing::{debug, info, warn};

/// Compiled-once regex used by [`extract_extensions_from_tool_input`] to pull
/// the trailing extension out of a glob pattern.  Matches a literal `.`
/// followed by 1-8 alphanumeric characters at the end of the pattern, with an
/// optional tail of glob metacharacters (`*?]}\)`).  See crosslink #796.
fn compile_glob_extension_regex(pattern: &str) -> Option<Regex> {
    match Regex::new(pattern) {
        Ok(regex) => Some(regex),
        Err(error) => {
            warn!(
                pattern,
                error = %error,
                "Invalid glob-extension regex; glob rule inference disabled",
            );
            None
        }
    }
}

fn glob_extension_regex() -> Option<&'static Regex> {
    static RE: OnceLock<Option<Regex>> = OnceLock::new();
    RE.get_or_init(|| {
        // Pattern: `.<ext>` optionally followed by glob metacharacters, anchored
        // at end-of-string. Capture group 1 is the bare extension.
        compile_glob_extension_regex(r"\.([A-Za-z0-9]{1,8})[\*\?\]\}\)]*$")
    })
    .as_ref()
}

/// Single source-of-truth for `language -> extensions` mapping.
///
/// Both [`extension_to_language`] and [`known_languages`] are derived from
/// this table so adding a new language requires editing exactly one place
/// (crosslink #354).
pub(crate) const LANGUAGES: &[(&str, &[&str])] = &[
    ("rust", &["rs"]),
    ("python", &["py", "pyw"]),
    ("javascript", &["js", "mjs", "cjs"]),
    ("typescript", &["ts", "mts", "cts"]),
    ("tsx", &["tsx"]),
    ("jsx", &["jsx"]),
    ("go", &["go"]),
    ("java", &["java"]),
    ("kotlin", &["kt", "kts"]),
    ("swift", &["swift"]),
    ("c", &["c", "h"]),
    ("cpp", &["cpp", "cc", "cxx", "hpp", "hxx"]),
    ("csharp", &["cs"]),
    ("ruby", &["rb"]),
    ("php", &["php"]),
    ("scala", &["scala"]),
    ("elixir", &["ex", "exs"]),
    ("erlang", &["erl", "hrl"]),
    ("haskell", &["hs"]),
    ("clojure", &["clj", "cljs", "cljc"]),
    ("lua", &["lua"]),
    ("r", &["r"]),
    ("julia", &["jl"]),
    ("dart", &["dart"]),
    ("zig", &["zig"]),
    ("nim", &["nim"]),
    ("vlang", &["v"]),
    ("sql", &["sql"]),
    ("shell", &["sh", "bash", "zsh"]),
    ("powershell", &["ps1", "psm1"]),
    ("yaml", &["yml", "yaml"]),
    ("json", &["json"]),
    ("toml", &["toml"]),
    ("xml", &["xml"]),
    ("html", &["html", "htm"]),
    ("css", &["css"]),
    ("scss", &["scss", "sass"]),
    ("less", &["less"]),
    ("markdown", &["md", "markdown"]),
    ("vue", &["vue"]),
    ("svelte", &["svelte"]),
];

/// File extension to language name mapping (derived from [`LANGUAGES`]).
///
/// Uses `eq_ignore_ascii_case` per-entry rather than `ext.to_lowercase()`
/// (allocates) — see crosslink #790.
fn extension_to_language(ext: &str) -> Option<&'static str> {
    for (lang, exts) in LANGUAGES {
        if exts.iter().any(|e| e.eq_ignore_ascii_case(ext)) {
            return Some(lang);
        }
    }
    None
}

/// True when `ext` is registered for *any* language in [`LANGUAGES`].
///
/// Crosslink #790: replaces the hand-rolled `has_source_extension` /
/// `has_file_extension` substring lists in `auto_learn.rs`, so adding a
/// new language requires editing exactly one table.
#[must_use]
pub(crate) fn is_known_extension(ext: &str) -> bool {
    extension_to_language(ext).is_some()
}

/// A loaded rule with its metadata
#[derive(Debug, Clone)]
pub struct Rule {
    /// Name of the rule (filename without extension)
    pub name: String,
    /// The markdown content
    pub content: String,
    /// Languages this rule applies to (empty = global)
    pub languages: Vec<String>,
}

/// Rules engine that loads and matches markdown rules
#[derive(Debug, Clone)]
pub struct RulesEngine {
    /// Directory containing rule files
    rules_dir: PathBuf,
    /// Cached rules
    rules: Vec<Rule>,
}

impl RulesEngine {
    /// Create a new rules engine and load rules from the directory
    pub fn new(rules_dir: impl Into<PathBuf>) -> Self {
        let rules_dir = rules_dir.into();
        let rules = Self::load_rules(&rules_dir);
        Self { rules_dir, rules }
    }

    /// Load all markdown rules from the rules directory
    fn load_rules(rules_dir: &Path) -> Vec<Rule> {
        let mut rules = Vec::new();

        if !rules_dir.exists() {
            debug!(path = ?rules_dir, "Rules directory does not exist");
            return rules;
        }

        let entries = match fs::read_dir(rules_dir) {
            Ok(entries) => entries,
            Err(e) => {
                warn!(error = %e, path = ?rules_dir, "Failed to read rules directory");
                return rules;
            }
        };

        for entry in entries.flatten() {
            let path = entry.path();

            // Only process .md files
            if path.extension().is_some_and(|e| e == "md") {
                if let Some(rule) = Self::load_rule(&path) {
                    info!(name = %rule.name, languages = ?rule.languages, "Loaded rule");
                    rules.push(rule);
                }
            }
        }

        rules
    }

    /// Load a single rule file
    fn load_rule(path: &Path) -> Option<Rule> {
        let filename = path.file_stem()?.to_string_lossy().to_string();
        let content = match fs::read_to_string(path) {
            Ok(c) => c,
            Err(e) => {
                warn!(error = %e, path = ?path, "Failed to read rule file");
                return None;
            }
        };

        // Determine if this is a language-specific or global rule
        let (name, languages) = Self::parse_rule_name(&filename);

        Some(Rule {
            name,
            content,
            languages,
        })
    }

    /// Parse rule name to extract language associations
    ///
    /// Naming conventions:
    /// - "always.md" or "global.md" - applies to all
    /// - "rust.md" - applies to rust files
    /// - "python.md" - applies to python files
    /// - "security.md" - applies to all (no language prefix)
    /// - "rust-memory.md" - applies to rust files
    fn parse_rule_name(filename: &str) -> (String, Vec<String>) {
        let lower = filename.to_lowercase();

        // Global rules
        if lower == "always" || lower == "global" || lower == "all" {
            return (filename.to_string(), vec![]);
        }

        // Check if filename starts with a known language. Derived from the
        // single LANGUAGES table so both directions stay in sync.
        for (lang, _) in LANGUAGES {
            if lower == *lang || lower.starts_with(&format!("{lang}-")) {
                return (filename.to_string(), vec![(*lang).to_string()]);
            }
        }

        // No language prefix - this is a global rule
        (filename.to_string(), vec![])
    }

    /// Get all rules that apply to the given file extensions
    #[must_use]
    pub fn get_rules_for_extensions(&self, extensions: &[&str]) -> Vec<&Rule> {
        // Convert extensions to languages
        let languages: Vec<&str> = extensions
            .iter()
            .filter_map(|ext| extension_to_language(ext))
            .collect();

        self.rules
            .iter()
            .filter(|rule| {
                // Global rules always apply
                if rule.languages.is_empty() {
                    return true;
                }
                // Language-specific rules apply if any language matches
                rule.languages
                    .iter()
                    .any(|lang| languages.contains(&lang.as_str()))
            })
            .collect()
    }

    /// Get all rules that apply to files with the given paths
    #[must_use]
    pub fn get_rules_for_files(&self, file_paths: &[&str]) -> Vec<&Rule> {
        let extensions: Vec<&str> = file_paths
            .iter()
            .filter_map(|path| Path::new(path).extension().and_then(|e| e.to_str()))
            .collect();

        self.get_rules_for_extensions(&extensions)
    }

    /// Get combined rule content for the given extensions
    #[must_use]
    pub fn get_combined_rules(&self, extensions: &[&str]) -> String {
        let rules = self.get_rules_for_extensions(extensions);

        if rules.is_empty() {
            return String::new();
        }

        rules
            .iter()
            .map(|r| format!("## {} Rules\n\n{}", r.name, r.content))
            .collect::<Vec<_>>()
            .join("\n\n---\n\n")
    }

    /// Reload rules from disk
    pub fn reload(&mut self) {
        self.rules = Self::load_rules(&self.rules_dir);
    }

    /// Get the rules directory path
    #[must_use]
    pub fn rules_dir(&self) -> &Path {
        &self.rules_dir
    }

    /// Get all loaded rules
    #[must_use]
    pub fn all_rules(&self) -> &[Rule] {
        &self.rules
    }
}

/// Extract file extensions from tool input (for `PreToolUse` hooks)
#[must_use]
pub fn extract_extensions_from_tool_input(
    tool_name: &str,
    input: &serde_json::Value,
) -> Vec<String> {
    let mut extensions = Vec::new();

    // Handle common file-related tools
    match tool_name {
        "Write" | "Edit" | "Read" => {
            if let Some(path) = input.get("file_path").and_then(|v| v.as_str()) {
                if let Some(ext) = Path::new(path).extension().and_then(|e| e.to_str()) {
                    extensions.push(ext.to_string());
                }
            }
        }
        "Glob" => {
            // #796: only extract an extension when the pattern actually has
            // a trailing `.<ext>` form.  Patterns without a `.` (e.g.
            // `src/util`) must yield no extensions — the previous code
            // returned the bare segment as a fake extension.
            if let Some(pattern) = input.get("pattern").and_then(|v| v.as_str()) {
                if let Some(regex) = glob_extension_regex() {
                    if let Some(caps) = regex.captures(pattern) {
                        if let Some(ext) = caps.get(1) {
                            extensions.push(ext.as_str().to_string());
                        }
                    }
                }
            }
        }
        _ => {}
    }

    extensions
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn create_test_rules_dir() -> TempDir {
        let dir = TempDir::new().unwrap();
        let rules_dir = dir.path().join("rules");
        fs::create_dir(&rules_dir).unwrap();

        // Create test rule files
        fs::write(
            rules_dir.join("always.md"),
            "# Global Rules\n\nAlways follow these rules.",
        )
        .unwrap();

        fs::write(
            rules_dir.join("rust.md"),
            "# Rust Rules\n\nUse proper error handling.",
        )
        .unwrap();

        fs::write(
            rules_dir.join("python.md"),
            "# Python Rules\n\nUse type hints.",
        )
        .unwrap();

        dir
    }

    #[test]
    fn test_extension_to_language() {
        assert_eq!(extension_to_language("rs"), Some("rust"));
        assert_eq!(extension_to_language("py"), Some("python"));
        assert_eq!(extension_to_language("ts"), Some("typescript"));
        assert_eq!(extension_to_language("unknown"), None);
    }

    /// Crosslink #354: every (language, extensions) entry in the canonical
    /// table must round-trip through `extension_to_language`, names are
    /// kebab-case-ish (lowercase), and every entry has at least one ext.
    #[test]
    fn test_languages_table_consistency() {
        for (lang, exts) in LANGUAGES {
            assert!(!exts.is_empty(), "language {lang} has zero extensions");
            assert!(
                lang.chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'),
                "language name {lang} must be lowercase / kebab-case"
            );
            for ext in *exts {
                assert_eq!(
                    extension_to_language(ext),
                    Some(*lang),
                    "extension {ext} did not resolve to {lang}"
                );
            }
        }
    }

    #[test]
    fn test_parse_rule_name() {
        let (name, langs) = RulesEngine::parse_rule_name("always");
        assert_eq!(name, "always");
        assert!(langs.is_empty());

        let (name, langs) = RulesEngine::parse_rule_name("rust");
        assert_eq!(name, "rust");
        assert_eq!(langs, vec!["rust"]);

        let (name, langs) = RulesEngine::parse_rule_name("rust-memory");
        assert_eq!(name, "rust-memory");
        assert_eq!(langs, vec!["rust"]);

        let (name, langs) = RulesEngine::parse_rule_name("security");
        assert_eq!(name, "security");
        assert!(langs.is_empty()); // Not a known language, so global
    }

    #[test]
    fn test_load_rules() {
        let dir = create_test_rules_dir();
        let engine = RulesEngine::new(dir.path().join("rules"));

        assert_eq!(engine.all_rules().len(), 3);
    }

    #[test]
    fn test_get_rules_for_extensions() {
        let dir = create_test_rules_dir();
        let engine = RulesEngine::new(dir.path().join("rules"));

        // Rust files should get always + rust rules
        let rules = engine.get_rules_for_extensions(&["rs"]);
        assert_eq!(rules.len(), 2);

        // Python files should get always + python rules
        let rules = engine.get_rules_for_extensions(&["py"]);
        assert_eq!(rules.len(), 2);

        // Unknown extensions should only get global rules
        let rules = engine.get_rules_for_extensions(&["xyz"]);
        assert_eq!(rules.len(), 1);
    }

    #[test]
    fn test_extract_extensions_from_tool_input() {
        let input = serde_json::json!({"file_path": "/src/main.rs"});
        let exts = extract_extensions_from_tool_input("Write", &input);
        assert_eq!(exts, vec!["rs"]);

        let input = serde_json::json!({"pattern": "**/*.ts"});
        let exts = extract_extensions_from_tool_input("Glob", &input);
        assert_eq!(exts, vec!["ts"]);
    }

    #[test]
    fn invalid_glob_extension_regex_is_skipped() {
        assert!(compile_glob_extension_regex("[").is_none());
    }

    /// #796: glob extension extraction must only return the trailing `.ext`,
    /// and must yield nothing for patterns with no embedded `.`.
    #[test]
    fn issue_796_glob_extension_extraction_strict() {
        // Bare extension after a wildcard.
        let exts =
            extract_extensions_from_tool_input("Glob", &serde_json::json!({"pattern": "*.rs"}));
        assert_eq!(exts, vec!["rs"]);

        // Compound suffix: only the last segment counts.
        let exts =
            extract_extensions_from_tool_input("Glob", &serde_json::json!({"pattern": "*.tar.gz"}));
        assert_eq!(exts, vec!["gz"]);

        // Path with no extension must yield no extensions (was previously "util").
        let exts =
            extract_extensions_from_tool_input("Glob", &serde_json::json!({"pattern": "src/util"}));
        assert!(
            exts.is_empty(),
            "path with no `.` must not produce a fake extension, got {exts:?}"
        );

        // Recursive globs.
        let exts =
            extract_extensions_from_tool_input("Glob", &serde_json::json!({"pattern": "**/*.rs"}));
        assert_eq!(exts, vec!["rs"]);

        // Single `*` with no `.` must produce nothing.
        let exts = extract_extensions_from_tool_input("Glob", &serde_json::json!({"pattern": "*"}));
        assert!(exts.is_empty(), "bare `*` must not produce an extension");

        // Brace alternation: ambiguous (multiple extensions), so the
        // anchored regex declines to pick one rather than contaminating
        // the result with `"toml}"` like the old `trim_end_matches`
        // approach.  Callers that need brace-alternation handling must
        // expand the pattern first.
        let exts = extract_extensions_from_tool_input(
            "Glob",
            &serde_json::json!({"pattern": "*.{rs,toml}"}),
        );
        assert!(
            exts.is_empty(),
            "brace alternation is ambiguous and must yield no extension, got {exts:?}"
        );
    }
}
