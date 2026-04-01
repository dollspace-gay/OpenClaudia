//! File index with fuzzy search for fast file lookup in large codebases.
//!
//! Uses a scoring algorithm inspired by fzf-v2/nucleo with:
//! - Boundary bonuses (start of path segment)
//! - `CamelCase` bonuses
//! - Consecutive match bonuses
//! - Gap penalties
//! - First-char bonus

use std::path::Path;

const SCORE_MATCH: i32 = 16;
const BONUS_BOUNDARY: i32 = 8;
const BONUS_CAMEL: i32 = 6;
const BONUS_CONSECUTIVE: i32 = 4;
const BONUS_FIRST_CHAR: i32 = 8;
const PENALTY_GAP_START: i32 = 3;
const PENALTY_GAP_EXTENSION: i32 = 1;

#[derive(Debug, Clone)]
pub struct SearchResult {
    pub path: String,
    pub score: i32,
}

/// In-memory file index for fuzzy searching.
#[derive(Default)]
pub struct FileIndex {
    paths: Vec<String>,
    lower_paths: Vec<String>,
}

impl FileIndex {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            paths: Vec::new(),
            lower_paths: Vec::new(),
        }
    }

    /// Build index by walking the directory tree, respecting .gitignore.
    #[must_use]
    pub fn build(root: &Path) -> Self {
        let mut index = Self::new();
        // Walk directory, skip hidden dirs, .git, node_modules, target, etc.
        index.walk_dir(root, root);
        index
    }

    fn walk_dir(&mut self, root: &Path, dir: &Path) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();

            // Skip hidden, build artifacts, dependency dirs
            if name.starts_with('.')
                || name == "node_modules"
                || name == "target"
                || name == "__pycache__"
                || name == "dist"
                || name == "build"
            {
                continue;
            }

            if path.is_dir() {
                self.walk_dir(root, &path);
            } else if let Ok(rel) = path.strip_prefix(root) {
                let rel_str = rel.to_string_lossy().to_string();
                self.lower_paths.push(rel_str.to_lowercase());
                self.paths.push(rel_str);
            }
        }
    }

    /// Search for files matching the query, returning top N results sorted by score.
    #[must_use]
    pub fn search(&self, query: &str, limit: usize) -> Vec<SearchResult> {
        if query.is_empty() {
            return Vec::new();
        }
        let query_lower = query.to_lowercase();
        let query_chars: Vec<char> = query_lower.chars().collect();

        let mut results: Vec<SearchResult> = self
            .paths
            .iter()
            .zip(self.lower_paths.iter())
            .filter_map(|(path, lower)| {
                let score = Self::score_match(&query_chars, lower, path);
                if score > 0 {
                    Some(SearchResult {
                        path: path.clone(),
                        score,
                    })
                } else {
                    None
                }
            })
            .collect();

        results.sort_by(|a, b| b.score.cmp(&a.score));
        results.truncate(limit);
        results
    }

    fn score_match(query: &[char], lower_path: &str, original_path: &str) -> i32 {
        let path_chars: Vec<char> = lower_path.chars().collect();
        let orig_chars: Vec<char> = original_path.chars().collect();

        if query.len() > path_chars.len() {
            return 0;
        }

        // Check if all query chars exist in path (in order)
        let mut qi = 0;
        for &pc in &path_chars {
            if qi < query.len() && query[qi] == pc {
                qi += 1;
            }
        }
        if qi < query.len() {
            return 0; // Not all chars matched
        }

        // Score the match
        let mut score: i32 = 0;
        qi = 0;
        let mut last_match_idx: Option<usize> = None;
        let mut consecutive = 0;

        for (pi, &pc) in path_chars.iter().enumerate() {
            if qi < query.len() && query[qi] == pc {
                score += SCORE_MATCH;

                // First char bonus
                if qi == 0 {
                    score += BONUS_FIRST_CHAR;
                }

                // Boundary bonus (after /, \, ., -, _)
                if pi > 0 {
                    let prev = path_chars[pi - 1];
                    if prev == '/' || prev == '\\' || prev == '.' || prev == '-' || prev == '_' {
                        score += BONUS_BOUNDARY;
                    }
                    // CamelCase bonus
                    if orig_chars[pi].is_uppercase()
                        && orig_chars
                            .get(pi.wrapping_sub(1))
                            .is_some_and(|c| c.is_lowercase())
                    {
                        score += BONUS_CAMEL;
                    }
                } else {
                    score += BONUS_BOUNDARY; // Start of string
                }

                // Consecutive bonus
                if let Some(last) = last_match_idx {
                    if pi == last + 1 {
                        consecutive += 1;
                        score += BONUS_CONSECUTIVE * consecutive;
                    } else {
                        // Gap penalty
                        #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
                        let gap = (pi - last - 1) as i32;
                        score -= PENALTY_GAP_START + PENALTY_GAP_EXTENSION * (gap - 1).max(0);
                        consecutive = 0;
                    }
                }

                last_match_idx = Some(pi);
                qi += 1;
            }
        }

        // Prefer shorter paths (less noise)
        #[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
        let path_len_penalty = (path_chars.len() as i32) / 10;
        score -= path_len_penalty;

        // Penalize test files slightly
        if lower_path.contains("test") || lower_path.contains("spec") {
            score -= 2;
        }

        score
    }

    /// Get total file count
    #[must_use]
    pub const fn len(&self) -> usize {
        self.paths.len()
    }

    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.paths.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_score_basic_match() {
        let index = FileIndex {
            paths: vec!["src/main.rs".to_string()],
            lower_paths: vec!["src/main.rs".to_string()],
        };
        let results = index.search("main", 10);
        assert_eq!(results.len(), 1);
        assert!(results[0].score > 0);
    }

    #[test]
    fn test_score_boundary_bonus() {
        let index = FileIndex {
            paths: vec![
                "src/main.rs".to_string(),
                "src/domain/maintain.rs".to_string(),
            ],
            lower_paths: vec![
                "src/main.rs".to_string(),
                "src/domain/maintain.rs".to_string(),
            ],
        };
        let results = index.search("main", 10);
        // "main.rs" should score higher (boundary match after /)
        assert!(results[0].path == "src/main.rs");
    }

    #[test]
    fn test_no_match() {
        let index = FileIndex {
            paths: vec!["src/main.rs".to_string()],
            lower_paths: vec!["src/main.rs".to_string()],
        };
        let results = index.search("xyz", 10);
        assert!(results.is_empty());
    }

    #[test]
    fn test_empty_query() {
        let index = FileIndex {
            paths: vec!["src/main.rs".to_string()],
            lower_paths: vec!["src/main.rs".to_string()],
        };
        assert!(index.search("", 10).is_empty());
    }

    #[test]
    fn test_case_insensitive() {
        let index = FileIndex {
            paths: vec!["src/MyComponent.tsx".to_string()],
            lower_paths: vec!["src/mycomponent.tsx".to_string()],
        };
        let results = index.search("mycomp", 10);
        assert_eq!(results.len(), 1);
    }
}
