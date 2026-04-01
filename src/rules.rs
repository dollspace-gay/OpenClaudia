//! Rules Engine - Loads markdown rules for context injection.
//!
//! Loads .md files from .openclaudia/rules/ directory and injects them
//! as context based on file types being edited.

use std::fs;
use std::path::{Path, PathBuf};
use tracing::{debug, info, warn};

/// File extension to language name mapping
fn extension_to_language(ext: &str) -> Option<&'static str> {
    match ext.to_lowercase().as_str() {
        "rs" => Some("rust"),
        "py" | "pyw" => Some("python"),
        "js" | "mjs" | "cjs" => Some("javascript"),
        "ts" | "mts" | "cts" => Some("typescript"),
        "tsx" => Some("tsx"),
        "jsx" => Some("jsx"),
        "go" => Some("go"),
        "java" => Some("java"),
        "kt" | "kts" => Some("kotlin"),
        "swift" => Some("swift"),
        "c" | "h" => Some("c"),
        "cpp" | "cc" | "cxx" | "hpp" | "hxx" => Some("cpp"),
        "cs" => Some("csharp"),
        "rb" => Some("ruby"),
        "php" => Some("php"),
        "scala" => Some("scala"),
        "ex" | "exs" => Some("elixir"),
        "erl" | "hrl" => Some("erlang"),
        "hs" => Some("haskell"),
        "clj" | "cljs" | "cljc" => Some("clojure"),
        "lua" => Some("lua"),
        "r" | "R" => Some("r"),
        "jl" => Some("julia"),
        "dart" => Some("dart"),
        "zig" => Some("zig"),
        "nim" => Some("nim"),
        "v" => Some("vlang"),
        "sql" => Some("sql"),
        "sh" | "bash" | "zsh" => Some("shell"),
        "ps1" | "psm1" => Some("powershell"),
        "yml" | "yaml" => Some("yaml"),
        "json" => Some("json"),
        "toml" => Some("toml"),
        "xml" => Some("xml"),
        "html" | "htm" => Some("html"),
        "css" => Some("css"),
        "scss" | "sass" => Some("scss"),
        "less" => Some("less"),
        "md" | "markdown" => Some("markdown"),
        "vue" => Some("vue"),
        "svelte" => Some("svelte"),
        _ => None,
    }
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

        // Check if filename starts with a known language
        let known_languages = [
            "rust",
            "python",
            "javascript",
            "typescript",
            "tsx",
            "jsx",
            "go",
            "java",
            "kotlin",
            "swift",
            "c",
            "cpp",
            "csharp",
            "ruby",
            "php",
            "scala",
            "elixir",
            "erlang",
            "haskell",
            "clojure",
            "lua",
            "r",
            "julia",
            "dart",
            "zig",
            "nim",
            "vlang",
            "sql",
            "shell",
            "powershell",
            "yaml",
            "json",
            "toml",
            "xml",
            "html",
            "css",
            "scss",
            "less",
            "markdown",
            "vue",
            "svelte",
        ];

        for lang in known_languages {
            if lower == lang || lower.starts_with(&format!("{lang}-")) {
                return (filename.to_string(), vec![lang.to_string()]);
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
            // Try to extract extension from glob pattern
            if let Some(pattern) = input.get("pattern").and_then(|v| v.as_str()) {
                // Handle patterns like "*.rs" or "**/*.ts"
                if let Some(ext_part) = pattern.rsplit('.').next() {
                    // Remove any trailing glob characters
                    let ext = ext_part.trim_end_matches(&['*', '?', ']', ')'][..]);
                    if !ext.is_empty() && ext.len() < 10 {
                        extensions.push(ext.to_string());
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
}
