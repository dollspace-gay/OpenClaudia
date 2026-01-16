//! Context Injector - Modifies API messages before sending to provider.
//!
//! Injects hook output as system messages using <system-reminder> tags.
//! Supports message array manipulation for context injection.

use crate::hooks::HookResult;
use crate::proxy::{ChatCompletionRequest, ChatMessage, MessageContent};

/// Wraps content in a system-reminder tag
fn wrap_system_reminder(content: &str) -> String {
    format!("<system-reminder>\n{}\n</system-reminder>", content)
}

/// Context injector that modifies requests based on hook results
pub struct ContextInjector;

impl ContextInjector {
    /// Inject context from hook results into the request
    ///
    /// This modifies the request in-place, adding system messages from hooks
    /// and applying any prompt modifications.
    pub fn inject(request: &mut ChatCompletionRequest, hook_result: &HookResult) {
        // Collect all system messages from hook outputs
        let system_messages: Vec<&str> = hook_result.system_messages();

        if system_messages.is_empty() {
            return;
        }

        // Combine all system messages into one wrapped reminder
        let combined = system_messages.join("\n\n");
        let reminder = wrap_system_reminder(&combined);

        // Find the last user message and inject the reminder after it
        // This ensures the reminder is seen just before the model responds
        if let Some(last_user_idx) = request.messages.iter().rposition(|m| m.role == "user") {
            // Append reminder to the last user message content
            Self::append_to_message(&mut request.messages[last_user_idx], &reminder);
        } else {
            // No user message found, add as a separate system message
            request.messages.push(ChatMessage {
                role: "system".to_string(),
                content: MessageContent::Text(reminder),
                name: None,
                tool_calls: None,
                tool_call_id: None,
            });
        }
    }

    /// Apply prompt modification from hooks
    ///
    /// If a hook returned a modified prompt, this replaces the last user message.
    pub fn apply_prompt_modification(
        request: &mut ChatCompletionRequest,
        hook_result: &HookResult,
    ) {
        if let Some(modified_prompt) = hook_result.modified_prompt() {
            // Find and update the last user message
            if let Some(last_user) = request.messages.iter_mut().rev().find(|m| m.role == "user") {
                last_user.content = MessageContent::Text(modified_prompt.to_string());
            }
        }
    }

    /// Inject a system message at the beginning of the conversation
    pub fn inject_system_prefix(request: &mut ChatCompletionRequest, content: &str) {
        let reminder = wrap_system_reminder(content);

        // Check if first message is already a system message
        if let Some(first) = request.messages.first_mut() {
            if first.role == "system" {
                // Append to existing system message
                Self::append_to_message(first, &reminder);
                return;
            }
        }

        // Insert new system message at the beginning
        request.messages.insert(
            0,
            ChatMessage {
                role: "system".to_string(),
                content: MessageContent::Text(reminder),
                name: None,
                tool_calls: None,
                tool_call_id: None,
            },
        );
    }

    /// Inject a system message at the end of the conversation (before response)
    pub fn inject_system_suffix(request: &mut ChatCompletionRequest, content: &str) {
        let reminder = wrap_system_reminder(content);

        // Find last user message and append
        if let Some(last_user_idx) = request.messages.iter().rposition(|m| m.role == "user") {
            Self::append_to_message(&mut request.messages[last_user_idx], &reminder);
        } else {
            // Add as separate system message at the end
            request.messages.push(ChatMessage {
                role: "system".to_string(),
                content: MessageContent::Text(reminder),
                name: None,
                tool_calls: None,
                tool_call_id: None,
            });
        }
    }

    /// Append content to a message
    fn append_to_message(message: &mut ChatMessage, content: &str) {
        match &mut message.content {
            MessageContent::Text(text) => {
                text.push_str("\n\n");
                text.push_str(content);
            }
            MessageContent::Parts(parts) => {
                // Add as a new text part
                parts.push(crate::proxy::ContentPart {
                    content_type: "text".to_string(),
                    text: Some(content.to_string()),
                    image_url: None,
                });
            }
        }
    }

    /// Inject multiple context items from a rules engine or plugin
    pub fn inject_all(request: &mut ChatCompletionRequest, contexts: &[String]) {
        if contexts.is_empty() {
            return;
        }

        let combined = contexts.join("\n\n");
        Self::inject_system_suffix(request, &combined);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hooks::HookOutput;

    fn create_test_request() -> ChatCompletionRequest {
        ChatCompletionRequest {
            model: "gpt-4".to_string(),
            messages: vec![
                ChatMessage {
                    role: "system".to_string(),
                    content: MessageContent::Text("You are a helpful assistant.".to_string()),
                    name: None,
                    tool_calls: None,
                    tool_call_id: None,
                },
                ChatMessage {
                    role: "user".to_string(),
                    content: MessageContent::Text("Hello!".to_string()),
                    name: None,
                    tool_calls: None,
                    tool_call_id: None,
                },
            ],
            temperature: None,
            max_tokens: None,
            stream: None,
            tools: None,
            tool_choice: None,
            extra: std::collections::HashMap::new(),
        }
    }

    #[test]
    fn test_inject_system_messages() {
        let mut request = create_test_request();
        let hook_result = HookResult {
            allowed: true,
            outputs: vec![
                HookOutput {
                    system_message: Some("Remember to be concise.".to_string()),
                    ..Default::default()
                },
                HookOutput {
                    system_message: Some("Use markdown formatting.".to_string()),
                    ..Default::default()
                },
            ],
            errors: vec![],
        };

        ContextInjector::inject(&mut request, &hook_result);

        // Check that the user message was modified
        let user_msg = &request.messages[1];
        if let MessageContent::Text(text) = &user_msg.content {
            assert!(text.contains("<system-reminder>"));
            assert!(text.contains("Remember to be concise."));
            assert!(text.contains("Use markdown formatting."));
        } else {
            panic!("Expected text content");
        }
    }

    #[test]
    fn test_inject_system_prefix() {
        let mut request = create_test_request();
        ContextInjector::inject_system_prefix(&mut request, "Security context here");

        // Should append to existing system message
        let system_msg = &request.messages[0];
        if let MessageContent::Text(text) = &system_msg.content {
            assert!(text.contains("You are a helpful assistant."));
            assert!(text.contains("<system-reminder>"));
            assert!(text.contains("Security context here"));
        } else {
            panic!("Expected text content");
        }
    }

    #[test]
    fn test_apply_prompt_modification() {
        let mut request = create_test_request();
        let hook_result = HookResult {
            allowed: true,
            outputs: vec![HookOutput {
                prompt: Some("Modified prompt here".to_string()),
                ..Default::default()
            }],
            errors: vec![],
        };

        ContextInjector::apply_prompt_modification(&mut request, &hook_result);

        let user_msg = &request.messages[1];
        if let MessageContent::Text(text) = &user_msg.content {
            assert_eq!(text, "Modified prompt here");
        } else {
            panic!("Expected text content");
        }
    }

    #[test]
    fn test_empty_hook_result() {
        let mut request = create_test_request();
        let original_len = request.messages.len();
        let hook_result = HookResult::allowed();

        ContextInjector::inject(&mut request, &hook_result);

        // Should not modify anything
        assert_eq!(request.messages.len(), original_len);
    }

    // ========================================================================
    // Extended Context Injector Tests
    // ========================================================================

    #[test]
    fn test_wrap_system_reminder() {
        let content = "Test content";
        let wrapped = wrap_system_reminder(content);

        assert!(wrapped.starts_with("<system-reminder>"));
        assert!(wrapped.ends_with("</system-reminder>"));
        assert!(wrapped.contains("Test content"));
    }

    #[test]
    fn test_inject_system_suffix() {
        let mut request = create_test_request();
        ContextInjector::inject_system_suffix(&mut request, "Remember this rule");

        // Should append to user message
        let user_msg = &request.messages[1];
        if let MessageContent::Text(text) = &user_msg.content {
            assert!(text.contains("<system-reminder>"));
            assert!(text.contains("Remember this rule"));
        } else {
            panic!("Expected text content");
        }
    }

    #[test]
    fn test_inject_system_suffix_no_user_message() {
        let mut request = ChatCompletionRequest {
            model: "gpt-4".to_string(),
            messages: vec![ChatMessage {
                role: "system".to_string(),
                content: MessageContent::Text("System prompt".to_string()),
                name: None,
                tool_calls: None,
                tool_call_id: None,
            }],
            temperature: None,
            max_tokens: None,
            stream: None,
            tools: None,
            tool_choice: None,
            extra: std::collections::HashMap::new(),
        };

        ContextInjector::inject_system_suffix(&mut request, "Suffix content");

        // Should add a new system message at the end
        assert_eq!(request.messages.len(), 2);
        assert_eq!(request.messages[1].role, "system");
    }

    #[test]
    fn test_inject_system_prefix_new_system() {
        let mut request = ChatCompletionRequest {
            model: "gpt-4".to_string(),
            messages: vec![ChatMessage {
                role: "user".to_string(),
                content: MessageContent::Text("Hello".to_string()),
                name: None,
                tool_calls: None,
                tool_call_id: None,
            }],
            temperature: None,
            max_tokens: None,
            stream: None,
            tools: None,
            tool_choice: None,
            extra: std::collections::HashMap::new(),
        };

        ContextInjector::inject_system_prefix(&mut request, "Prefix content");

        // Should insert new system message at the beginning
        assert_eq!(request.messages.len(), 2);
        assert_eq!(request.messages[0].role, "system");
        if let MessageContent::Text(text) = &request.messages[0].content {
            assert!(text.contains("Prefix content"));
        }
    }

    #[test]
    fn test_inject_all_empty() {
        let mut request = create_test_request();
        let original = request.messages.clone();

        ContextInjector::inject_all(&mut request, &[]);

        // Should not modify anything when contexts are empty
        assert_eq!(request.messages.len(), original.len());
    }

    #[test]
    fn test_inject_all_multiple() {
        let mut request = create_test_request();

        let contexts = vec![
            "First context".to_string(),
            "Second context".to_string(),
            "Third context".to_string(),
        ];

        ContextInjector::inject_all(&mut request, &contexts);

        // Should inject all contexts
        let user_msg = &request.messages[1];
        if let MessageContent::Text(text) = &user_msg.content {
            assert!(text.contains("First context"));
            assert!(text.contains("Second context"));
            assert!(text.contains("Third context"));
        } else {
            panic!("Expected text content");
        }
    }

    #[test]
    fn test_append_to_message_text() {
        let mut message = ChatMessage {
            role: "user".to_string(),
            content: MessageContent::Text("Original content".to_string()),
            name: None,
            tool_calls: None,
            tool_call_id: None,
        };

        ContextInjector::append_to_message(&mut message, "Appended content");

        if let MessageContent::Text(text) = &message.content {
            assert!(text.contains("Original content"));
            assert!(text.contains("Appended content"));
        } else {
            panic!("Expected text content");
        }
    }

    #[test]
    fn test_append_to_message_parts() {
        let mut message = ChatMessage {
            role: "user".to_string(),
            content: MessageContent::Parts(vec![crate::proxy::ContentPart {
                content_type: "text".to_string(),
                text: Some("Original part".to_string()),
                image_url: None,
            }]),
            name: None,
            tool_calls: None,
            tool_call_id: None,
        };

        ContextInjector::append_to_message(&mut message, "Appended content");

        if let MessageContent::Parts(parts) = &message.content {
            assert_eq!(parts.len(), 2);
            assert_eq!(parts[1].text, Some("Appended content".to_string()));
        } else {
            panic!("Expected parts content");
        }
    }

    #[test]
    fn test_inject_with_multiple_system_messages() {
        let mut request = create_test_request();
        let hook_result = HookResult {
            allowed: true,
            outputs: vec![
                HookOutput {
                    system_message: Some("Message 1".to_string()),
                    ..Default::default()
                },
                HookOutput {
                    system_message: Some("Message 2".to_string()),
                    ..Default::default()
                },
                HookOutput {
                    system_message: Some("Message 3".to_string()),
                    ..Default::default()
                },
            ],
            errors: vec![],
        };

        ContextInjector::inject(&mut request, &hook_result);

        // All messages should be combined
        let user_msg = &request.messages[1];
        if let MessageContent::Text(text) = &user_msg.content {
            assert!(text.contains("Message 1"));
            assert!(text.contains("Message 2"));
            assert!(text.contains("Message 3"));
        } else {
            panic!("Expected text content");
        }
    }

    #[test]
    fn test_inject_finds_last_user_message() {
        let mut request = ChatCompletionRequest {
            model: "gpt-4".to_string(),
            messages: vec![
                ChatMessage {
                    role: "system".to_string(),
                    content: MessageContent::Text("System".to_string()),
                    name: None,
                    tool_calls: None,
                    tool_call_id: None,
                },
                ChatMessage {
                    role: "user".to_string(),
                    content: MessageContent::Text("First user".to_string()),
                    name: None,
                    tool_calls: None,
                    tool_call_id: None,
                },
                ChatMessage {
                    role: "assistant".to_string(),
                    content: MessageContent::Text("Assistant response".to_string()),
                    name: None,
                    tool_calls: None,
                    tool_call_id: None,
                },
                ChatMessage {
                    role: "user".to_string(),
                    content: MessageContent::Text("Second user".to_string()),
                    name: None,
                    tool_calls: None,
                    tool_call_id: None,
                },
            ],
            temperature: None,
            max_tokens: None,
            stream: None,
            tools: None,
            tool_choice: None,
            extra: std::collections::HashMap::new(),
        };

        let hook_result = HookResult {
            allowed: true,
            outputs: vec![HookOutput {
                system_message: Some("Injected".to_string()),
                ..Default::default()
            }],
            errors: vec![],
        };

        ContextInjector::inject(&mut request, &hook_result);

        // Should inject into the LAST user message (index 3)
        if let MessageContent::Text(text) = &request.messages[3].content {
            assert!(text.contains("Second user"));
            assert!(text.contains("Injected"));
        } else {
            panic!("Expected text content");
        }

        // First user message should be unchanged
        if let MessageContent::Text(text) = &request.messages[1].content {
            assert!(!text.contains("Injected"));
        }
    }

    #[test]
    fn test_apply_prompt_modification_replaces_content() {
        let mut request = create_test_request();
        let hook_result = HookResult {
            allowed: true,
            outputs: vec![HookOutput {
                prompt: Some("Completely new prompt".to_string()),
                ..Default::default()
            }],
            errors: vec![],
        };

        ContextInjector::apply_prompt_modification(&mut request, &hook_result);

        let user_msg = &request.messages[1];
        if let MessageContent::Text(text) = &user_msg.content {
            assert_eq!(text, "Completely new prompt");
            // Should NOT contain original content
            assert!(!text.contains("Hello!"));
        } else {
            panic!("Expected text content");
        }
    }

    #[test]
    fn test_apply_prompt_modification_no_change() {
        let mut request = create_test_request();
        let hook_result = HookResult {
            allowed: true,
            outputs: vec![HookOutput::default()], // No prompt modification
            errors: vec![],
        };

        let original_content = if let MessageContent::Text(text) = &request.messages[1].content {
            text.clone()
        } else {
            panic!("Expected text content");
        };

        ContextInjector::apply_prompt_modification(&mut request, &hook_result);

        // Content should be unchanged
        if let MessageContent::Text(text) = &request.messages[1].content {
            assert_eq!(text, &original_content);
        }
    }

    #[test]
    fn test_inject_with_mixed_outputs() {
        let mut request = create_test_request();
        let hook_result = HookResult {
            allowed: true,
            outputs: vec![
                HookOutput {
                    system_message: Some("Has message".to_string()),
                    ..Default::default()
                },
                HookOutput::default(), // No message
                HookOutput {
                    system_message: Some("Another message".to_string()),
                    ..Default::default()
                },
            ],
            errors: vec![],
        };

        ContextInjector::inject(&mut request, &hook_result);

        // Should only inject non-None messages
        let user_msg = &request.messages[1];
        if let MessageContent::Text(text) = &user_msg.content {
            assert!(text.contains("Has message"));
            assert!(text.contains("Another message"));
        } else {
            panic!("Expected text content");
        }
    }
}
