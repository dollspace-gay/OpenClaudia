//! End-to-end tests for `compaction::estimate_tokens` —
//! exact constants for ASCII (4 chars/token) + CJK
//! (2 tokens/char) + emoji (3 tokens/char) + whitespace
//! exclusion + huge-input no-panic + `<image_data>` lockstep
//! with multiple placeholder occurrences.
//!
//! Sprint 169 of the verification effort. Sprint 92 covered
//! the basic shape; this file pins the exact constants
//! and saturating-arithmetic boundary corners.

#![allow(clippy::missing_panics_doc)]
#![allow(clippy::expect_used)]
#![allow(clippy::unwrap_used)]

use openclaudia::compaction::estimate_tokens;

// ───────────────────────────────────────────────────────────────────────────
// Section A — ASCII at exactly 4 chars/token boundary
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn ascii_4_chars_yields_1_token() {
    // PINS CONSTANT: ASCII_CHARS_PER_TOKEN = 4.
    assert_eq!(estimate_tokens("abcd"), 1);
}

#[test]
fn ascii_8_chars_yields_2_tokens() {
    assert_eq!(estimate_tokens("abcdefgh"), 2);
}

#[test]
fn ascii_3_chars_yields_0_tokens_under_integer_floor() {
    // PINS DOC: integer-floor division (3 / 4 = 0).
    assert_eq!(estimate_tokens("abc"), 0);
}

#[test]
fn ascii_400_chars_yields_100_tokens() {
    let s = "a".repeat(400);
    assert_eq!(estimate_tokens(&s), 100);
}

// ───────────────────────────────────────────────────────────────────────────
// Section B — Whitespace exclusion (all ASCII whitespace types)
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn space_does_not_count_toward_ascii_tokens() {
    // "    " (4 spaces) → 0 ASCII non-whitespace → 0 tokens.
    assert_eq!(estimate_tokens("    "), 0);
}

#[test]
fn tab_newline_carriage_return_do_not_count_toward_tokens() {
    // \t \n \r are all ASCII whitespace.
    assert_eq!(estimate_tokens("\t\n\r\t"), 0);
}

#[test]
fn mixed_whitespace_and_text_only_counts_non_whitespace_ascii() {
    // "ab cd" → 4 non-whitespace ASCII chars → 1 token.
    assert_eq!(estimate_tokens("ab cd"), 1);
}

#[test]
fn vertical_tab_form_feed_excluded_as_ascii_whitespace() {
    // 0x0b (vertical tab) and 0x0c (form feed) are ascii_whitespace.
    assert_eq!(estimate_tokens("\x0b\x0c"), 0);
}

// ───────────────────────────────────────────────────────────────────────────
// Section C — CJK at 2 tokens/char
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn single_cjk_char_yields_2_tokens() {
    // weight 4 / divisor 2 = 2 tokens per CJK char.
    // PINS CONSTANTS: NON_ASCII_ALPHANUMERIC_WEIGHT=4,
    // NON_ASCII_WEIGHT_DIVISOR=2.
    assert_eq!(estimate_tokens("日"), 2);
}

#[test]
fn five_cjk_chars_yield_10_tokens() {
    // 5 * 4 / 2 = 10.
    assert_eq!(estimate_tokens("日本語平和"), 10);
}

#[test]
fn ten_cjk_chars_yield_20_tokens() {
    let cjk = "日本語平和日本語平和".to_string();
    assert_eq!(estimate_tokens(&cjk), 20);
}

// ───────────────────────────────────────────────────────────────────────────
// Section D — Emoji / non-alphanumeric non-ASCII at 3 tokens/char
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn single_emoji_yields_3_tokens() {
    // Emoji 🎉 is non-ASCII non-alphanumeric → weight 6 / divisor 2 = 3.
    // PINS CONSTANT: NON_ASCII_SYMBOL_WEIGHT=6.
    assert_eq!(estimate_tokens("🎉"), 3);
}

#[test]
fn two_emojis_yield_6_tokens() {
    assert_eq!(estimate_tokens("🎉🎊"), 6);
}

#[test]
fn emoji_cost_strictly_greater_than_cjk_per_char() {
    // PINS RELATIVE COST: emoji > CJK per char.
    let emoji = estimate_tokens("🎉");
    let cjk = estimate_tokens("日");
    assert!(
        emoji > cjk,
        "emoji ({emoji}) MUST be more expensive per char than CJK ({cjk})"
    );
}

// ───────────────────────────────────────────────────────────────────────────
// Section E — Mixed ASCII + CJK additive
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn mixed_ascii_and_cjk_costs_add_independently() {
    // 4 ASCII = 1 ASCII-token, 1 CJK = 2 non-ASCII tokens.
    // Total = 1 + 2 = 3.
    assert_eq!(estimate_tokens("abcd日"), 3);
}

#[test]
fn mixed_ascii_with_whitespace_and_cjk() {
    // "ab cd 日本" → 4 ASCII non-ws = 1, 2 CJK = 4. Total 5.
    assert_eq!(estimate_tokens("ab cd 日本"), 5);
}

// ───────────────────────────────────────────────────────────────────────────
// Section F — image_data placeholder
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn single_image_data_placeholder_adds_at_least_1600_tokens() {
    // PINS CONSTANT: IMAGE_TOKEN_COST = 1600 per placeholder.
    let with_image = estimate_tokens("<image_data>blob</image_data>");
    assert!(
        with_image >= 1600,
        "single <image_data> MUST add at least IMAGE_TOKEN_COST (1600); got {with_image}"
    );
}

#[test]
fn two_image_data_placeholders_count_3200_or_more() {
    let two = estimate_tokens("<image_data>a</image_data><image_data>b</image_data>");
    assert!(
        two >= 3200,
        "two <image_data> MUST add 2*1600=3200+; got {two}"
    );
}

#[test]
fn three_image_data_placeholders_count_4800_or_more() {
    let three = estimate_tokens(
        "<image_data>a</image_data> <image_data>b</image_data> <image_data>c</image_data>",
    );
    assert!(three >= 4800);
}

#[test]
fn unmatched_image_data_open_tag_with_no_close_still_counts() {
    // The matcher counts "<image_data>" open occurrences.
    let s = estimate_tokens("<image_data>");
    assert!(s >= 1600);
}

#[test]
fn close_tag_alone_without_open_does_not_count_as_image() {
    // No <image_data> open tag means no image cost.
    let s = estimate_tokens("</image_data>");
    // Just an ASCII string with 14 chars → 14/4 = 3 tokens.
    assert!(
        s < 1600,
        "close-tag-only MUST NOT trigger image cost; got {s}"
    );
}

// ───────────────────────────────────────────────────────────────────────────
// Section G — Monotonicity + saturation
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn appending_text_strictly_increases_token_estimate() {
    let short = estimate_tokens("hello world");
    let long = estimate_tokens("hello world hello world hello world");
    assert!(long > short);
}

#[test]
fn huge_ascii_string_never_panics_via_saturating_arithmetic() {
    // 1 MB of "a" → 250k tokens. No panic.
    let s = "a".repeat(1_000_000);
    let t = estimate_tokens(&s);
    assert!(t > 100_000, "1MB ASCII MUST yield >100k tokens; got {t}");
}

#[test]
fn huge_cjk_string_never_panics() {
    let s = "日".repeat(100_000);
    let t = estimate_tokens(&s);
    assert!(t > 100_000, "100k CJK MUST yield >100k tokens; got {t}");
}

#[test]
fn huge_image_data_string_never_overflows() {
    // 100 image placeholders.
    let blob = "<image_data>x</image_data>".repeat(100);
    let t = estimate_tokens(&blob);
    assert!(
        t >= 160_000,
        "100 image placeholders MUST add ≥160k tokens; got {t}"
    );
}

// ───────────────────────────────────────────────────────────────────────────
// Section H — Special edge cases
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn empty_string_returns_zero_tokens() {
    assert_eq!(estimate_tokens(""), 0);
}

#[test]
fn single_char_ascii_returns_zero_under_integer_floor() {
    assert_eq!(estimate_tokens("a"), 0);
}

#[test]
fn null_byte_is_ascii_so_counts_as_char() {
    // \x00 is ASCII (codepoint 0), not whitespace → counts.
    let s = "\x00\x00\x00\x00";
    assert_eq!(estimate_tokens(s), 1);
}

#[test]
fn all_ascii_punctuation_counts_as_non_whitespace() {
    // !@#$%^&*() — 10 ASCII non-whitespace chars → 10/4 = 2 tokens.
    assert_eq!(estimate_tokens("!@#$%^&*()"), 2);
}
