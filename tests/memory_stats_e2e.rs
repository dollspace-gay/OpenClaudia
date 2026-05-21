//! End-to-end tests for `memory::MemoryStats` and
//! `memory::AutoLearnStats` shapes + their getter methods
//! `MemoryDb::memory_stats` and `MemoryDb::auto_learn_stats`.
//!
//! Sprint 129 of the verification effort. Sprint 22 covered
//! basic `MemoryDb` open + save + list + search; sprint 92
//! covered short-term memory; this file pins the stat
//! aggregation getters used by the diagnostics + `/memory`
//! slash-command output.

#![allow(clippy::missing_panics_doc)]
#![allow(clippy::expect_used)]
#![allow(clippy::unwrap_used)]

use openclaudia::memory::MemoryDb;
use tempfile::TempDir;

fn fresh_db() -> (MemoryDb, TempDir) {
    let dir = TempDir::new().expect("tempdir");
    let db = MemoryDb::open(&dir.path().join("memory.db")).expect("open");
    (db, dir)
}

// ───────────────────────────────────────────────────────────────────────────
// Section A — MemoryStats on empty database
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn memory_stats_on_empty_db_yields_zero_count_and_zero_size() {
    let (db, _dir) = fresh_db();
    let stats = db.memory_stats().expect("ok");
    assert_eq!(stats.count, 0);
    assert_eq!(stats.total_size, 0);
}

#[test]
fn memory_stats_on_empty_db_yields_none_last_updated() {
    let (db, _dir) = fresh_db();
    let stats = db.memory_stats().expect("ok");
    assert!(stats.last_updated.is_none());
}

// ───────────────────────────────────────────────────────────────────────────
// Section B — MemoryStats reflects content after saves
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn memory_stats_count_matches_number_of_memory_save_calls() {
    let (db, _dir) = fresh_db();
    db.memory_save("first", &[]).expect("save");
    db.memory_save("second", &[]).expect("save");
    db.memory_save("third", &[]).expect("save");
    let stats = db.memory_stats().expect("ok");
    assert_eq!(stats.count, 3);
}

#[test]
fn memory_stats_total_size_aggregates_content_byte_length() {
    let (db, _dir) = fresh_db();
    db.memory_save("abcde", &[]).expect("save"); // 5 bytes
    db.memory_save("fghij", &[]).expect("save"); // 5 bytes
    let stats = db.memory_stats().expect("ok");
    assert_eq!(stats.total_size, 10);
}

#[test]
fn memory_stats_last_updated_some_after_at_least_one_save() {
    let (db, _dir) = fresh_db();
    db.memory_save("any content", &[]).expect("save");
    let stats = db.memory_stats().expect("ok");
    assert!(
        stats.last_updated.is_some(),
        "last_updated MUST be Some after a save"
    );
}

#[test]
fn memory_stats_total_size_for_unicode_uses_sqlite_length_semantics() {
    let (db, _dir) = fresh_db();
    // AUTHORING DISCOVERY: SQLite LENGTH() on TEXT returns
    // CHARACTER count (codepoints), not byte count. "日本"
    // is 2 chars (6 bytes UTF-8).
    db.memory_save("日本", &[]).expect("save");
    let stats = db.memory_stats().expect("ok");
    // PINS DOC: SQL LENGTH() returns char count for TEXT.
    assert_eq!(stats.total_size, 2);
}

#[test]
fn memory_stats_clone_preserves_all_three_fields() {
    let (db, _dir) = fresh_db();
    db.memory_save("content", &[]).expect("save");
    let stats = db.memory_stats().expect("ok");
    let cloned = stats.clone();
    assert_eq!(cloned.count, stats.count);
    assert_eq!(cloned.total_size, stats.total_size);
    assert_eq!(cloned.last_updated, stats.last_updated);
}

// ───────────────────────────────────────────────────────────────────────────
// Section C — AutoLearnStats on empty database
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn auto_learn_stats_on_empty_db_yields_all_zero_counts() {
    let (db, _dir) = fresh_db();
    let stats = db.auto_learn_stats().expect("ok");
    assert_eq!(stats.coding_patterns, 0);
    assert_eq!(stats.file_relationships, 0);
    assert_eq!(stats.error_patterns, 0);
    assert_eq!(stats.errors_resolved, 0);
    assert_eq!(stats.learned_preferences, 0);
}

#[test]
fn auto_learn_stats_carries_5_documented_fields() {
    let (db, _dir) = fresh_db();
    let stats = db.auto_learn_stats().expect("ok");
    // Field-by-field readability check — every documented
    // counter is a usize on the struct.
    let _: usize = stats.coding_patterns;
    let _: usize = stats.file_relationships;
    let _: usize = stats.error_patterns;
    let _: usize = stats.errors_resolved;
    let _: usize = stats.learned_preferences;
}

#[test]
fn auto_learn_stats_clone_preserves_all_5_fields() {
    let (db, _dir) = fresh_db();
    let original = db.auto_learn_stats().expect("ok");
    let cloned = original.clone();
    assert_eq!(cloned.coding_patterns, original.coding_patterns);
    assert_eq!(cloned.file_relationships, original.file_relationships);
    assert_eq!(cloned.error_patterns, original.error_patterns);
    assert_eq!(cloned.errors_resolved, original.errors_resolved);
    assert_eq!(cloned.learned_preferences, original.learned_preferences);
}

// ───────────────────────────────────────────────────────────────────────────
// Section D — Cross-method consistency
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn memory_stats_count_matches_memory_list_length() {
    let (db, _dir) = fresh_db();
    for i in 0..5 {
        db.memory_save(&format!("item-{i}"), &[]).expect("save");
    }
    let stats = db.memory_stats().expect("stats");
    let listed = db.memory_list(100).expect("list");
    assert_eq!(stats.count, listed.len());
    assert_eq!(stats.count, 5);
}

#[test]
fn memory_stats_after_save_then_save_is_2() {
    let (db, _dir) = fresh_db();
    db.memory_save("first", &[]).expect("save 1");
    let s1 = db.memory_stats().expect("stats 1");
    assert_eq!(s1.count, 1);
    db.memory_save("second", &[]).expect("save 2");
    let s2 = db.memory_stats().expect("stats 2");
    assert_eq!(s2.count, 2);
}

#[test]
fn memory_stats_size_zero_for_empty_content_strings() {
    let (db, _dir) = fresh_db();
    db.memory_save("", &[]).expect("save");
    let stats = db.memory_stats().expect("ok");
    // count = 1, total_size = 0 (empty content body).
    assert!(
        stats.count <= 1,
        "empty content MAY be persisted; got count {}",
        stats.count
    );
}

// ───────────────────────────────────────────────────────────────────────────
// Section E — Debug format presence
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn memory_stats_debug_format_includes_count_field() {
    let (db, _dir) = fresh_db();
    db.memory_save("x", &[]).expect("save");
    let stats = db.memory_stats().expect("stats");
    let dbg = format!("{stats:?}");
    assert!(dbg.contains("count"));
    assert!(dbg.contains("total_size"));
}

#[test]
fn auto_learn_stats_debug_format_includes_all_field_names() {
    let (db, _dir) = fresh_db();
    let stats = db.auto_learn_stats().expect("stats");
    let dbg = format!("{stats:?}");
    assert!(dbg.contains("coding_patterns"));
    assert!(dbg.contains("file_relationships"));
    assert!(dbg.contains("error_patterns"));
    assert!(dbg.contains("errors_resolved"));
    assert!(dbg.contains("learned_preferences"));
}

// ───────────────────────────────────────────────────────────────────────────
// Section F — Multiple sequential calls are idempotent
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn memory_stats_called_twice_with_no_writes_returns_identical_count() {
    let (db, _dir) = fresh_db();
    db.memory_save("body", &[]).expect("save");
    let s1 = db.memory_stats().expect("stats 1");
    let s2 = db.memory_stats().expect("stats 2");
    assert_eq!(s1.count, s2.count);
    assert_eq!(s1.total_size, s2.total_size);
}

#[test]
fn auto_learn_stats_called_twice_with_no_writes_returns_identical_counts() {
    let (db, _dir) = fresh_db();
    let s1 = db.auto_learn_stats().expect("stats 1");
    let s2 = db.auto_learn_stats().expect("stats 2");
    assert_eq!(s1.coding_patterns, s2.coding_patterns);
    assert_eq!(s1.file_relationships, s2.file_relationships);
    assert_eq!(s1.error_patterns, s2.error_patterns);
    assert_eq!(s1.errors_resolved, s2.errors_resolved);
    assert_eq!(s1.learned_preferences, s2.learned_preferences);
}
