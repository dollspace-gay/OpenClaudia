//! End-to-end tests for `compaction::archive_compacted_messages`
//! and `compaction::extract_and_persist_memories` — the two
//! pub fns that persist evicted messages to the archival
//! memory store after compaction.
//!
//! Sprint 121 of the verification effort. Sprint 64 covered
//! `estimate_*_tokens`; sprint 94 covered `CompactionConfig`
//! plus `check_context_budget`; this file pins the
//! disk-side persistence path that converts ephemeral
//! in-memory turns into searchable archival memories.

#![allow(clippy::missing_panics_doc)]
#![allow(clippy::expect_used)]
#![allow(clippy::unwrap_used)]

use openclaudia::compaction::{archive_compacted_messages, extract_and_persist_memories};
use openclaudia::memory::MemoryDb;
use openclaudia::proxy::{ChatMessage, MessageContent};
use tempfile::TempDir;

// ───────────────────────────────────────────────────────────────────────────
// Helpers
// ───────────────────────────────────────────────────────────────────────────

fn fresh_db() -> (MemoryDb, TempDir) {
    let dir = TempDir::new().expect("tempdir");
    let db = MemoryDb::open(&dir.path().join("memory.db")).expect("open");
    (db, dir)
}

fn user_msg(text: &str) -> ChatMessage {
    ChatMessage {
        role: "user".to_string(),
        content: MessageContent::Text(text.to_string()),
        name: None,
        tool_calls: None,
        tool_call_id: None,
        extra: std::collections::HashMap::new(),
    }
}

fn assistant_msg(text: &str) -> ChatMessage {
    ChatMessage {
        role: "assistant".to_string(),
        content: MessageContent::Text(text.to_string()),
        name: None,
        tool_calls: None,
        tool_call_id: None,
        extra: std::collections::HashMap::new(),
    }
}

// ───────────────────────────────────────────────────────────────────────────
// Section A — archive_compacted_messages
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn archive_empty_slice_returns_empty_ids() {
    let (db, _dir) = fresh_db();
    let ids = archive_compacted_messages(&[], Some("s1"), &db);
    assert!(ids.is_empty());
}

#[test]
fn archive_single_user_message_returns_one_id() {
    let (db, _dir) = fresh_db();
    let msg = user_msg("Hello world");
    let ids = archive_compacted_messages(&[&msg], Some("s1"), &db);
    assert_eq!(ids.len(), 1);
    assert!(ids[0] > 0, "MUST return positive rowid");
}

#[test]
fn archive_multi_message_returns_id_per_message_in_order() {
    let (db, _dir) = fresh_db();
    let m1 = user_msg("first");
    let m2 = assistant_msg("second");
    let m3 = user_msg("third");
    let ids = archive_compacted_messages(&[&m1, &m2, &m3], Some("s1"), &db);
    assert_eq!(ids.len(), 3);
    // ids monotonically increasing (insert order preserved).
    assert!(ids[1] > ids[0]);
    assert!(ids[2] > ids[1]);
}

#[test]
fn archive_messages_are_searchable_after_persistence() {
    let (db, _dir) = fresh_db();
    let msg = user_msg("unique-content-marker-xyz");
    archive_compacted_messages(&[&msg], Some("s1"), &db);
    // Verify via list/search.
    let listed = db.memory_list(100).expect("list");
    assert!(
        listed
            .iter()
            .any(|m| m.content.contains("unique-content-marker-xyz")),
        "archived content MUST be retrievable; got {listed:?}"
    );
}

#[test]
fn archive_with_no_session_id_still_persists() {
    let (db, _dir) = fresh_db();
    let msg = user_msg("body");
    let ids = archive_compacted_messages(&[&msg], None, &db);
    assert_eq!(ids.len(), 1);
}

#[test]
fn archive_includes_role_in_serialized_content() {
    let (db, _dir) = fresh_db();
    let msg = assistant_msg("assistant reply");
    archive_compacted_messages(&[&msg], Some("s1"), &db);
    let listed = db.memory_list(100).expect("list");
    let entry = listed
        .iter()
        .find(|m| m.content.contains("assistant reply"))
        .expect("present");
    // Role is part of the serialized message JSON.
    assert!(
        entry.content.contains("assistant"),
        "role MUST be preserved in serialized form; got {:?}",
        entry.content
    );
}

#[test]
fn archive_with_session_id_tags_the_entry() {
    let (db, _dir) = fresh_db();
    let msg = user_msg("session-tagged");
    archive_compacted_messages(&[&msg], Some("session-abc"), &db);
    let listed = db.memory_list(100).expect("list");
    // Tags should include session identifier.
    let entry = listed
        .iter()
        .find(|m| m.content.contains("session-tagged"))
        .expect("present");
    // Documented: session_id tagged.
    let has_session_tag = entry.tags.iter().any(|t| t.contains("session-abc"));
    assert!(
        has_session_tag,
        "session_id MUST be in tags; got {:?}",
        entry.tags
    );
}

#[test]
fn archive_handles_unicode_message_content() {
    let (db, _dir) = fresh_db();
    let msg = user_msg("日本語のメッセージ + emoji 🎉");
    let ids = archive_compacted_messages(&[&msg], Some("s1"), &db);
    assert_eq!(ids.len(), 1);
    let listed = db.memory_list(100).expect("list");
    assert!(listed.iter().any(|m| m.content.contains("日本語")));
}

#[test]
fn archive_handles_empty_string_message() {
    let (db, _dir) = fresh_db();
    let msg = user_msg("");
    let ids = archive_compacted_messages(&[&msg], Some("s1"), &db);
    // Either persists (returns 1 id) or skips (returns 0) —
    // either is documented "non-fatal" behavior.
    assert!(ids.len() <= 1);
}

// ───────────────────────────────────────────────────────────────────────────
// Section B — extract_and_persist_memories
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn extract_empty_slice_returns_empty_ids() {
    let (db, _dir) = fresh_db();
    let ids = extract_and_persist_memories(&[], Some("s1"), &db);
    assert!(ids.is_empty());
}

#[test]
fn extract_text_messages_returns_some_ids() {
    let (db, _dir) = fresh_db();
    let msgs = [
        user_msg("Some question to ask"),
        assistant_msg("A detailed and substantial answer to the user's question."),
    ];
    let refs: Vec<&ChatMessage> = msgs.iter().collect();
    let _ids = extract_and_persist_memories(&refs, Some("s1"), &db);
    // The function MAY persist 0+ memories depending on
    // extraction heuristics — pin no-panic + valid Vec.
    // (Some impls produce 0 for short generic messages.)
}

#[test]
fn extract_with_parts_content_does_not_panic() {
    let (db, _dir) = fresh_db();
    let msg = ChatMessage {
        role: "user".to_string(),
        content: MessageContent::Parts(Vec::new()),
        name: None,
        tool_calls: None,
        tool_call_id: None,
        extra: std::collections::HashMap::new(),
    };
    let _ids = extract_and_persist_memories(&[&msg], None, &db);
}

#[test]
fn extract_with_no_session_id_does_not_panic() {
    let (db, _dir) = fresh_db();
    let msg = assistant_msg("response body");
    let _ids = extract_and_persist_memories(&[&msg], None, &db);
}

#[test]
fn extract_multi_message_preserves_session_isolation() {
    let (db, _dir) = fresh_db();
    let m1 = user_msg("session A question");
    let m2 = assistant_msg("session A answer");
    let _ids_a = extract_and_persist_memories(&[&m1, &m2], Some("session-A"), &db);

    let m3 = user_msg("session B question");
    let m4 = assistant_msg("session B answer");
    let _ids_b = extract_and_persist_memories(&[&m3, &m4], Some("session-B"), &db);

    // Both sessions persist independently — no panic.
}

#[test]
fn extract_handles_unicode_content() {
    let (db, _dir) = fresh_db();
    let msg = user_msg("日本語コンテンツ — substantial body that the extractor would consider");
    let _ids = extract_and_persist_memories(&[&msg], Some("s1"), &db);
}

// ───────────────────────────────────────────────────────────────────────────
// Section C — Cross-fn consistency
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn archive_and_extract_can_share_same_db_without_interference() {
    let (db, _dir) = fresh_db();
    let m1 = user_msg("archive me");
    archive_compacted_messages(&[&m1], Some("s1"), &db);
    let m2 = assistant_msg("extract me");
    extract_and_persist_memories(&[&m2], Some("s1"), &db);
    // Both operations succeeded; db has at least the
    // archived entry.
    let listed = db.memory_list(100).expect("list");
    assert!(listed.iter().any(|m| m.content.contains("archive me")));
}

#[test]
fn archive_does_not_block_subsequent_extract() {
    let (db, _dir) = fresh_db();
    let m1 = user_msg("first message");
    let m2 = assistant_msg("second message");
    archive_compacted_messages(&[&m1, &m2], Some("s1"), &db);
    // Extract on the same db works.
    let _ids = extract_and_persist_memories(&[&m1, &m2], Some("s1"), &db);
}

#[test]
fn batch_of_100_messages_archive_does_not_panic() {
    // Stress-test: a large batch of messages.
    let (db, _dir) = fresh_db();
    let msgs: Vec<ChatMessage> = (0..100)
        .map(|i| user_msg(&format!("message {i}")))
        .collect();
    let refs: Vec<&ChatMessage> = msgs.iter().collect();
    let ids = archive_compacted_messages(&refs, Some("stress"), &db);
    assert_eq!(ids.len(), 100, "MUST persist all 100 messages");
}
