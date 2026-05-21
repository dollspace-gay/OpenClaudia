//! End-to-end tests for `proxy::MessageContent` untagged
//! serde dispatch (Text vs Parts) and `proxy::ContentPart`
//! field-level serde shape including the `type` →
//! `content_type` rename and skip-None semantics.
//!
//! Sprint 157 of the verification effort. Sprint 18
//! covered the proxy translation matrix; this file pins
//! the wire-level `MessageContent` untagged dispatch +
//! `ContentPart` skip-None contract distinct from the
//! translation logic.

#![allow(clippy::missing_panics_doc)]
#![allow(clippy::expect_used)]
#![allow(clippy::unwrap_used)]

use openclaudia::proxy::{ContentPart, MessageContent};
use serde_json::{json, Value};

// ───────────────────────────────────────────────────────────────────────────
// Section A — MessageContent::Text serializes as bare string
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn message_content_text_serializes_as_bare_json_string() {
    // PINS UNTAGGED: Text variant serializes as `"..."`, NOT
    // wrapped in an object or tagged with a discriminator.
    let mc = MessageContent::Text("hello world".to_string());
    let json: Value = serde_json::to_value(&mc).expect("ser");
    assert_eq!(
        json,
        Value::String("hello world".to_string()),
        "Text MUST serialize as bare string; got {json}"
    );
}

#[test]
fn message_content_text_with_empty_string_serializes_correctly() {
    let mc = MessageContent::Text(String::new());
    let json: Value = serde_json::to_value(&mc).expect("ser");
    assert_eq!(json, Value::String(String::new()));
}

#[test]
fn message_content_text_with_unicode_serializes_byte_exact() {
    let mc = MessageContent::Text("日本語コンテンツ 🎉".to_string());
    let json: Value = serde_json::to_value(&mc).expect("ser");
    assert_eq!(json, Value::String("日本語コンテンツ 🎉".to_string()));
}

// ───────────────────────────────────────────────────────────────────────────
// Section B — MessageContent::Parts serializes as array
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn message_content_parts_serializes_as_json_array() {
    let mc = MessageContent::Parts(vec![ContentPart {
        content_type: "text".to_string(),
        text: Some("hi".to_string()),
        image_url: None,
    }]);
    let json: Value = serde_json::to_value(&mc).expect("ser");
    assert!(json.is_array(), "Parts MUST serialize as array; got {json}");
    let arr = json.as_array().unwrap();
    assert_eq!(arr.len(), 1);
}

#[test]
fn message_content_parts_empty_vec_serializes_as_empty_array() {
    let mc = MessageContent::Parts(Vec::new());
    let json: Value = serde_json::to_value(&mc).expect("ser");
    assert_eq!(json, json!([]));
}

// ───────────────────────────────────────────────────────────────────────────
// Section C — MessageContent untagged deserialization dispatch
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn untagged_deserialize_from_bare_string_yields_text_variant() {
    let json = Value::String("plain text".to_string());
    let mc: MessageContent = serde_json::from_value(json).expect("de");
    match mc {
        MessageContent::Text(s) => assert_eq!(s, "plain text"),
        MessageContent::Parts(_) => panic!("MUST dispatch to Text variant"),
    }
}

#[test]
fn untagged_deserialize_from_array_yields_parts_variant() {
    let json = json!([{"type": "text", "text": "hi"}]);
    let mc: MessageContent = serde_json::from_value(json).expect("de");
    match mc {
        MessageContent::Parts(parts) => {
            assert_eq!(parts.len(), 1);
            assert_eq!(parts[0].content_type, "text");
            assert_eq!(parts[0].text.as_deref(), Some("hi"));
        }
        MessageContent::Text(_) => panic!("MUST dispatch to Parts variant"),
    }
}

#[test]
fn untagged_deserialize_from_empty_array_yields_empty_parts() {
    let json = json!([]);
    let mc: MessageContent = serde_json::from_value(json).expect("de");
    match mc {
        MessageContent::Parts(parts) => assert!(parts.is_empty()),
        MessageContent::Text(_) => panic!("MUST be Parts"),
    }
}

#[test]
fn untagged_deserialize_from_object_errors() {
    // Untagged dispatch can't pick a variant for plain object →
    // neither Text (expects string) nor Parts (expects array).
    let json = json!({"not_a_known_shape": true});
    let outcome: Result<MessageContent, _> = serde_json::from_value(json);
    assert!(
        outcome.is_err(),
        "object MUST fail untagged dispatch (no matching variant)"
    );
}

#[test]
fn untagged_deserialize_from_number_errors() {
    let json = json!(42);
    let outcome: Result<MessageContent, _> = serde_json::from_value(json);
    assert!(outcome.is_err());
}

#[test]
fn untagged_deserialize_from_null_errors() {
    let json = Value::Null;
    let outcome: Result<MessageContent, _> = serde_json::from_value(json);
    assert!(outcome.is_err());
}

// ───────────────────────────────────────────────────────────────────────────
// Section D — Round-trip through serde
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn message_content_text_round_trips_through_json_string() {
    let original = MessageContent::Text("round trip".to_string());
    let json: Value = serde_json::to_value(&original).expect("ser");
    let back: MessageContent = serde_json::from_value(json).expect("de");
    match back {
        MessageContent::Text(s) => assert_eq!(s, "round trip"),
        MessageContent::Parts(_) => panic!("MUST be Text"),
    }
}

#[test]
fn message_content_parts_round_trips_through_json_array() {
    let original = MessageContent::Parts(vec![
        ContentPart {
            content_type: "text".to_string(),
            text: Some("hi".to_string()),
            image_url: None,
        },
        ContentPart {
            content_type: "image_url".to_string(),
            text: None,
            image_url: Some(json!({"url": "https://example.com/x.png"})),
        },
    ]);
    let json: Value = serde_json::to_value(&original).expect("ser");
    let back: MessageContent = serde_json::from_value(json).expect("de");
    let MessageContent::Parts(parts) = back else {
        panic!("MUST be Parts");
    };
    assert_eq!(parts.len(), 2);
    assert_eq!(parts[0].content_type, "text");
    assert_eq!(parts[1].content_type, "image_url");
}

// ───────────────────────────────────────────────────────────────────────────
// Section E — ContentPart serde
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn content_part_type_field_renames_from_content_type() {
    // PINS WIRE: content_type Rust field → "type" JSON key.
    let part = ContentPart {
        content_type: "text".to_string(),
        text: Some("body".to_string()),
        image_url: None,
    };
    let json: Value = serde_json::to_value(&part).expect("ser");
    assert_eq!(json["type"], "text");
    // No "content_type" wire key.
    assert!(json.get("content_type").is_none());
}

#[test]
fn content_part_none_text_field_skipped_on_serialize() {
    let part = ContentPart {
        content_type: "image_url".to_string(),
        text: None,
        image_url: Some(json!({"url": "x"})),
    };
    let json: Value = serde_json::to_value(&part).expect("ser");
    // PINS skip_serializing_if: None text MUST be absent from output.
    assert!(
        json.get("text").is_none(),
        "None text MUST be skipped; got {json}"
    );
    assert!(json["image_url"].is_object());
}

#[test]
fn content_part_none_image_url_field_skipped_on_serialize() {
    let part = ContentPart {
        content_type: "text".to_string(),
        text: Some("hello".to_string()),
        image_url: None,
    };
    let json: Value = serde_json::to_value(&part).expect("ser");
    assert!(
        json.get("image_url").is_none(),
        "None image_url MUST be skipped; got {json}"
    );
}

#[test]
fn content_part_deserialize_from_type_keyed_json() {
    let json = json!({"type": "text", "text": "hi"});
    let part: ContentPart = serde_json::from_value(json).expect("de");
    assert_eq!(part.content_type, "text");
    assert_eq!(part.text.as_deref(), Some("hi"));
}

#[test]
fn content_part_deserialize_with_image_url_only() {
    let json = json!({
        "type": "image_url",
        "image_url": {"url": "https://example.com/x.png", "detail": "high"}
    });
    let part: ContentPart = serde_json::from_value(json).expect("de");
    assert_eq!(part.content_type, "image_url");
    assert!(part.text.is_none());
    assert!(part.image_url.is_some());
}

#[test]
fn content_part_clone_preserves_all_3_fields() {
    let original = ContentPart {
        content_type: "text".to_string(),
        text: Some("body".to_string()),
        image_url: Some(json!({"url": "x"})),
    };
    let cloned = original.clone();
    assert_eq!(cloned.content_type, original.content_type);
    assert_eq!(cloned.text, original.text);
    assert_eq!(cloned.image_url, original.image_url);
}

// ───────────────────────────────────────────────────────────────────────────
// Section F — Cross-shape consistency
// ───────────────────────────────────────────────────────────────────────────

#[test]
fn text_and_parts_serialized_forms_are_distinguishable_for_untagged_dispatch() {
    // PINS UNTAGGED INVARIANT: Text → string-typed JSON;
    // Parts → array-typed JSON. The two shapes MUST be
    // mutually exclusive so untagged dispatch never has to
    // guess.
    let t = MessageContent::Text("x".to_string());
    let p = MessageContent::Parts(vec![]);
    let t_json: Value = serde_json::to_value(&t).expect("ser t");
    let p_json: Value = serde_json::to_value(&p).expect("ser p");
    assert!(t_json.is_string());
    assert!(p_json.is_array());
    // Round-trip MUST preserve variant.
    let t_back: MessageContent = serde_json::from_value(t_json).expect("de t");
    let p_back: MessageContent = serde_json::from_value(p_json).expect("de p");
    assert!(matches!(t_back, MessageContent::Text(_)));
    assert!(matches!(p_back, MessageContent::Parts(_)));
}
