use super::{FunctionCall, ToolCall};
use serde_json::Value;

/// Parse tool calls from a streaming response delta
/// Returns accumulated tool calls when complete
#[derive(Default, Debug)]
pub struct ToolCallAccumulator {
    pub tool_calls: Vec<PartialToolCall>,
}

#[derive(Default, Debug, Clone)]
pub struct PartialToolCall {
    pub index: usize,
    pub id: String,
    pub call_type: String,
    pub function_name: String,
    pub function_arguments: String,
}

impl ToolCallAccumulator {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            tool_calls: Vec::new(),
        }
    }

    /// Process a delta from streaming response
    pub fn process_delta(&mut self, delta: &Value) {
        if let Some(tool_calls) = delta.get("tool_calls").and_then(|v| v.as_array()) {
            for tc in tool_calls {
                let index = tc
                    .get("index")
                    .and_then(serde_json::Value::as_u64)
                    .map_or(0, |v| usize::try_from(v).unwrap_or(usize::MAX));

                // Ensure we have enough slots
                while self.tool_calls.len() <= index {
                    self.tool_calls.push(PartialToolCall::default());
                }

                let partial = &mut self.tool_calls[index];
                partial.index = index;

                if let Some(id) = tc.get("id").and_then(|v| v.as_str()) {
                    partial.id = id.to_string();
                }
                if let Some(t) = tc.get("type").and_then(|v| v.as_str()) {
                    partial.call_type = t.to_string();
                }
                if let Some(func) = tc.get("function") {
                    if let Some(name) = func.get("name").and_then(|v| v.as_str()) {
                        partial.function_name = name.to_string();
                    }
                    if let Some(args) = func.get("arguments").and_then(|v| v.as_str()) {
                        partial.function_arguments.push_str(args);
                    }
                }
            }
        }
    }

    /// Convert accumulated partials to complete tool calls
    #[must_use]
    pub fn finalize(&self) -> Vec<ToolCall> {
        self.tool_calls
            .iter()
            .filter(|tc| !tc.id.is_empty() && !tc.function_name.is_empty())
            .map(|tc| ToolCall {
                id: tc.id.clone(),
                call_type: if tc.call_type.is_empty() {
                    "function".to_string()
                } else {
                    tc.call_type.clone()
                },
                function: FunctionCall {
                    name: tc.function_name.clone(),
                    arguments: tc.function_arguments.clone(),
                },
            })
            .collect()
    }

    /// Check if we have any tool calls
    #[must_use]
    pub fn has_tool_calls(&self) -> bool {
        self.tool_calls.iter().any(|tc| !tc.id.is_empty())
    }

    /// Clear the accumulator
    pub fn clear(&mut self) {
        self.tool_calls.clear();
    }
}

// ==========================================================================
// Anthropic Streaming Tool Accumulator
// ==========================================================================

/// Content block types from Anthropic streaming responses
#[derive(Debug, Clone)]
pub enum AnthropicContentBlock {
    /// Text content block
    Text(String),
    /// Tool use content block
    ToolUse {
        id: String,
        name: String,
        input_json: String,
    },
}

/// Accumulates `tool_use` content blocks from Anthropic streaming responses.
///
/// When the Anthropic API receives tool definitions, it returns structured
/// `tool_use` content blocks instead of XML in text. This accumulator
/// processes the streaming events to collect those blocks.
///
/// Anthropic streaming event sequence for `tool_use`:
/// 1. `content_block_start` with `type: "tool_use"`, `id`, `name`
/// 2. `content_block_delta` with `type: "input_json_delta"`, `partial_json`
/// 3. `content_block_stop`
/// 4. `message_delta` with `stop_reason: "tool_use"`
#[derive(Debug)]
pub struct AnthropicToolAccumulator {
    /// Accumulated content blocks (text + `tool_use`)
    pub blocks: Vec<AnthropicContentBlock>,
    /// The stop reason from `message_delta`
    pub stop_reason: Option<String>,
}

impl Default for AnthropicToolAccumulator {
    fn default() -> Self {
        Self::new()
    }
}

impl AnthropicToolAccumulator {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            blocks: Vec::new(),
            stop_reason: None,
        }
    }

    /// Process a streaming SSE event from the Anthropic API.
    /// Returns any text that should be printed to the terminal.
    pub fn process_event(&mut self, event: &Value) -> Option<String> {
        let event_type = event.get("type").and_then(|t| t.as_str())?;

        match event_type {
            "content_block_start" => {
                let block = event.get("content_block")?;
                let block_type = block.get("type").and_then(|t| t.as_str())?;

                match block_type {
                    "text" => {
                        self.blocks.push(AnthropicContentBlock::Text(String::new()));
                    }
                    "tool_use" => {
                        let id = block
                            .get("id")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        let name = block
                            .get("name")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        self.blocks.push(AnthropicContentBlock::ToolUse {
                            id,
                            name,
                            input_json: String::new(),
                        });
                    }
                    _ => {}
                }
                None
            }
            "content_block_delta" => {
                let delta = event.get("delta")?;
                let delta_type = delta.get("type").and_then(|t| t.as_str())?;

                match delta_type {
                    "text_delta" => {
                        let text = delta.get("text").and_then(|t| t.as_str()).unwrap_or("");
                        // Append to last text block
                        if let Some(AnthropicContentBlock::Text(ref mut s)) = self.blocks.last_mut()
                        {
                            s.push_str(text);
                        }
                        Some(text.to_string())
                    }
                    "input_json_delta" => {
                        let json_chunk = delta
                            .get("partial_json")
                            .and_then(|t| t.as_str())
                            .unwrap_or("");
                        // Append to last tool_use block's input
                        if let Some(AnthropicContentBlock::ToolUse {
                            ref mut input_json, ..
                        }) = self.blocks.last_mut()
                        {
                            input_json.push_str(json_chunk);
                        }
                        None
                    }
                    _ => None,
                }
            }
            "message_delta" => {
                if let Some(delta) = event.get("delta") {
                    if let Some(reason) = delta.get("stop_reason").and_then(|r| r.as_str()) {
                        self.stop_reason = Some(reason.to_string());
                    }
                }
                None
            }
            _ => None,
        }
    }

    /// Check if the model requested tool use
    #[must_use]
    pub fn has_tool_use(&self) -> bool {
        self.stop_reason.as_deref() == Some("tool_use")
            && self
                .blocks
                .iter()
                .any(|b| matches!(b, AnthropicContentBlock::ToolUse { .. }))
    }

    /// Get concatenated text from all text blocks
    #[must_use]
    pub fn get_text(&self) -> String {
        self.blocks
            .iter()
            .filter_map(|b| match b {
                AnthropicContentBlock::Text(s) => Some(s.as_str()),
                AnthropicContentBlock::ToolUse { .. } => None,
            })
            .collect::<Vec<_>>()
            .join("")
    }

    /// Convert accumulated `tool_use` blocks to `ToolCall` format for execution
    #[must_use]
    pub fn finalize_tool_calls(&self) -> Vec<ToolCall> {
        self.blocks
            .iter()
            .filter_map(|b| match b {
                AnthropicContentBlock::ToolUse {
                    id,
                    name,
                    input_json,
                } => Some(ToolCall {
                    id: id.clone(),
                    call_type: "function".to_string(),
                    function: FunctionCall {
                        name: name.clone(),
                        arguments: input_json.clone(),
                    },
                }),
                AnthropicContentBlock::Text(_) => None,
            })
            .collect()
    }

    /// Convert to OpenAI-format `tool_calls` JSON for storage in `chat_session`.
    /// This allows `convert_messages_to_anthropic` to handle the back-conversion.
    #[must_use]
    pub fn to_openai_tool_calls_json(&self) -> Vec<serde_json::Value> {
        self.blocks
            .iter()
            .filter_map(|b| match b {
                AnthropicContentBlock::ToolUse {
                    id,
                    name,
                    input_json,
                } => Some(serde_json::json!({
                    "id": id,
                    "type": "function",
                    "function": {
                        "name": name,
                        "arguments": input_json
                    }
                })),
                AnthropicContentBlock::Text(_) => None,
            })
            .collect()
    }

    /// Clear the accumulator for reuse
    pub fn clear(&mut self) {
        self.blocks.clear();
        self.stop_reason = None;
    }
}
