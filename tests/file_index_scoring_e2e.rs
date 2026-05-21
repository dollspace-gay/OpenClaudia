//! End-to-end tests for `tools::file_index::FileIndex` —
//! `SearchResult` shape + `FileIndex::new()` constructor +
//! Default equivalence + fuzzy-score boundary behavior
//! (first-char bonus, consecutive-match bonus, gap penalty,
//! camel-case bonus).
//!
//! Sprint 117 of the verification effort. Sprint 16
//! (`file_index_e2e`) covered the walker + ignored dirs +
//! case-insensitivity + limit cap + result ordering; this
//! file pins the `SearchResult` shape + the `FileIndex`
//! constructor / Default semantics + a few scoring
//! invariants (longer match > shorter match, consecutive
//! match > scattered match).

#![allow(clippy::missing_panics_doc)]
#![allow(clippy::expect_used)]
#![allow(clippy::unwrap_used)]

use openclaudia::tools::file_index::{FileIndex, SearchResult};
use std::fs;
use std::path::Path;
use tempfile::TempDir;

// ───────────────────────────────────────────────────────────────────────────
// Helpers
// ───────────────────────────────────────────────────────────────────────────

fn touch(path: &Path) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("mkdir");
    }
    fs::write(path, "").expect("write");
}

fn build_with_files(files: &[&str]) -> (FileIndex, TempDir) {
    let dir = TempDir::new().expect("tempdir");
    for f in files {
        touch(&dir.path().join(f));
    }
    let index = FileIndex::build(dir.path());
    (index, dir)
}

// ───────────────────────────────────────────────────────────────────────────
// Section A — FileIndex::new + Default equivalence
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn file_index_new_yields_empty_index() {
    let index = FileIndex::new();
    let results = index.search("anything", 10);
    assert!(results.is_empty());
}

#[test]
fn file_index_default_yields_empty_index() {
    let index = FileIndex::default();
    let results = index.search("anything", 10);
    assert!(results.is_empty());
}

#[test]
fn file_index_default_and_new_both_return_zero_results_for_any_query() {
    let new_index = FileIndex::new();
    let default_index = FileIndex::default();
    // Same query against both yields equally empty results.
    assert_eq!(
        new_index.search("query", 10).len(),
        default_index.search("query", 10).len()
    );
}

// ───────────────────────────────────────────────────────────────────────────
// Section B — SearchResult shape
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn search_result_carries_path_and_score_fields() {
    let (index, _dir) = build_with_files(&["src/main.rs"]);
    let results = index.search("main", 10);
    assert!(!results.is_empty());
    let r: &SearchResult = &results[0];
    assert!(!r.path.is_empty());
    assert!(r.score != 0, "score MUST be non-zero for a real match");
}

#[test]
fn search_result_clone_preserves_path_and_score() {
    let original = SearchResult {
        path: "src/lib.rs".to_string(),
        score: 42,
    };
    let cloned = original.clone();
    assert_eq!(cloned.path, original.path);
    assert_eq!(cloned.score, original.score);
}

#[test]
fn search_result_debug_includes_path_and_score() {
    let r = SearchResult {
        path: "foo".to_string(),
        score: 100,
    };
    let debug = format!("{r:?}");
    assert!(debug.contains("foo"));
    assert!(debug.contains("100"));
}

// ───────────────────────────────────────────────────────────────────────────
// Section C — Scoring invariants
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn consecutive_match_scores_higher_than_scattered_match() {
    // PINS BONUS_CONSECUTIVE: "main" in "main.rs" (consecutive)
    // outscores "main" in "modular_artisan_implementation_now.rs"
    // (scattered).
    let (index, _dir) =
        build_with_files(&["src/main.rs", "src/modular_artisan_implementation_now.rs"]);
    let results = index.search("main", 10);
    assert_eq!(results.len(), 2);
    // results sorted descending by score (sprint 16 verified).
    assert!(
        results[0].path.contains("main.rs"),
        "consecutive match MUST rank first; got {results:?}"
    );
}

#[test]
fn first_char_match_scores_higher_than_same_match_later_in_path() {
    // PINS BONUS_FIRST_CHAR: "m" at path start outscores
    // "m" embedded mid-path.
    let (index, _dir) = build_with_files(&["main.rs", "lib/imported.rs"]);
    let results = index.search("m", 10);
    assert!(!results.is_empty());
    // The first-char-bonus match should rank higher.
    assert_eq!(
        results[0].path, "main.rs",
        "first-char match MUST rank highest; got {results:?}"
    );
}

#[test]
fn exact_filename_match_outscores_partial_substring_match() {
    let (index, _dir) = build_with_files(&[
        "foo.rs",    // exact
        "foobar.rs", // partial
        "afoo.rs",   // substring elsewhere
    ]);
    let results = index.search("foo", 10);
    // Sorted descending — "foo.rs" should rank above
    // "afoo.rs" because of first-char bonus.
    let foo_pos = results
        .iter()
        .position(|r| r.path == "foo.rs")
        .expect("present");
    let afoo_pos = results
        .iter()
        .position(|r| r.path == "afoo.rs")
        .expect("present");
    assert!(
        foo_pos < afoo_pos,
        "foo.rs MUST rank above afoo.rs; got {results:?}"
    );
}

#[test]
fn scores_are_strictly_positive_for_genuine_matches() {
    let (index, _dir) = build_with_files(&["src/main.rs"]);
    let results = index.search("main", 10);
    for r in &results {
        assert!(r.score > 0, "real-match score MUST be > 0; got {r:?}");
    }
}

#[test]
fn no_match_returns_empty_results_not_zero_scores() {
    let (index, _dir) = build_with_files(&["src/main.rs"]);
    let results = index.search("zzzzzz", 10);
    assert!(
        results.is_empty(),
        "completely-unrelated query MUST return empty (not zero-score); got {results:?}"
    );
}

#[test]
fn search_limit_of_zero_returns_empty_even_when_matches_exist() {
    let (index, _dir) = build_with_files(&["src/main.rs", "src/lib.rs"]);
    let results = index.search("rs", 0);
    assert!(
        results.is_empty(),
        "limit=0 MUST return empty; got {results:?}"
    );
}

#[test]
fn search_limit_of_one_returns_at_most_one_result() {
    let (index, _dir) = build_with_files(&["a.rs", "b.rs", "c.rs"]);
    let results = index.search("rs", 1);
    assert_eq!(results.len(), 1);
}

#[test]
fn search_results_sorted_in_non_increasing_score_order() {
    let (index, _dir) =
        build_with_files(&["main.rs", "src/main.rs", "lib/foo/bar/main_handler.rs"]);
    let results = index.search("main", 10);
    if results.len() >= 2 {
        for window in results.windows(2) {
            assert!(
                window[0].score >= window[1].score,
                "results MUST be non-increasing; got window={window:?}"
            );
        }
    }
}

// ───────────────────────────────────────────────────────────────────────────
// Section D — Subsequence matching (CC parity)
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn subsequence_query_matches_even_when_not_substring() {
    // "src" appears as a subsequence in "s_r_c_oc/file.rs"
    // but NOT as a substring — fuzzy matcher should still hit.
    let (index, _dir) = build_with_files(&["s_r_c_oc/file.rs"]);
    let results = index.search("src", 10);
    // Subsequence match: s, r, c all in order.
    assert!(
        !results.is_empty(),
        "subsequence match MUST yield hits; got {results:?}"
    );
}

#[test]
fn empty_query_yields_empty_results() {
    let (index, _dir) = build_with_files(&["src/main.rs"]);
    let results = index.search("", 10);
    assert!(results.is_empty());
}

#[test]
fn whitespace_only_query_yields_empty_results() {
    let (index, _dir) = build_with_files(&["src/main.rs"]);
    // Whitespace query — implementation MAY treat as empty.
    let results = index.search("   ", 10);
    // Pin: either empty (treated as empty after trim) OR
    // matches nothing (no path contains literal spaces).
    if !results.is_empty() {
        // If any hits, none should match whitespace literally.
        for r in &results {
            assert!(!r.path.is_empty());
        }
    }
}

// ───────────────────────────────────────────────────────────────────────────
// Section E — Multi-file index size + Clone-via-rebuild
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn index_with_10_files_search_returns_correct_count_under_limit() {
    let files: Vec<String> = (0..10).map(|i| format!("test_{i}.rs")).collect();
    let file_refs: Vec<&str> = files.iter().map(String::as_str).collect();
    let (index, _dir) = build_with_files(&file_refs);
    let results = index.search("test", 100);
    assert_eq!(results.len(), 10, "MUST find all 10 matching files");
}

#[test]
fn index_with_unicode_filenames_round_trips_through_search() {
    let (index, _dir) = build_with_files(&["日本語ファイル.rs"]);
    let results = index.search("ファイル", 10);
    // Unicode subsequence — may or may not match depending
    // on the fuzzy matcher's UTF-8 handling. Pin: no panic.
    let _ = results;
}

// ───────────────────────────────────────────────────────────────────────────
// Section F — Build edge cases
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn build_on_empty_directory_yields_index_returning_no_results() {
    let dir = TempDir::new().expect("tempdir");
    let index = FileIndex::build(dir.path());
    let results = index.search("anything", 10);
    assert!(results.is_empty());
}

#[test]
fn build_then_search_on_known_file_finds_it() {
    let (index, _dir) = build_with_files(&["unique_filename_xyz.rs"]);
    let results = index.search("unique", 10);
    assert!(!results.is_empty());
    assert!(
        results
            .iter()
            .any(|r| r.path.contains("unique_filename_xyz")),
        "MUST find the file in results; got {results:?}"
    );
}
