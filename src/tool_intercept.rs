//! Tool Interception for Claude Code Proxy Mode
//!
//! Parses Claude's XML-style tool invocations from the response stream
//! and executes them locally instead of letting Anthropic's sandbox handle them.
//!
//! Claude Code uses an XML format with antml:function_calls and antml:invoke tags.
//! This module parses those invocations and maps them to local tool execution.

use crate::tools::{FunctionCall, ToolCall};
use std::collections::HashMap;
use uuid::Uuid;

/// A parsed tool invocation from Claude's response
#[derive(Debug, Clone)]
pub struct InterceptedToolCall {
    /// The tool name (e.g., "Bash", "Read", "Write")
    pub name: String,
    /// Parameters for the tool
    pub parameters: HashMap<String, String>,
    /// Generated ID for tracking
    pub id: String,
}

impl InterceptedToolCall {
    /// Convert to a ToolCall that can be executed by our tool system
    pub fn to_tool_call(&self) -> ToolCall {
        // Map Claude Code tool names to our internal names
        let name_lower = self.name.to_lowercase();
        let internal_name = match name_lower.as_str() {
            "bash" => "bash",
            "read" => "read_file",
            "write" => "write_file",
            "edit" => "edit_file",
            "glob" => "glob",
            "grep" => "grep",
            "webfetch" | "web_fetch" => "web_fetch",
            "websearch" | "web_search" => "web_search",
            _ => &name_lower,
        };

        // Map Claude Code parameter names to our internal names
        let mut args = serde_json::Map::new();
        for (key, value) in &self.parameters {
            let internal_key = match (name_lower.as_str(), key.as_str()) {
                ("bash", "command") => "command",
                ("read", "file_path") => "path",
                ("read", "path") => "path",
                ("write", "file_path") => "path",
                ("write", "content") => "content",
                ("edit", "file_path") => "path",
                ("edit", "old_string") => "old_string",
                ("edit", "new_string") => "new_string",
                ("glob", "pattern") => "pattern",
                ("glob", "path") => "path",
                ("grep", "pattern") => "pattern",
                ("grep", "path") => "path",
                (_, k) => k,
            };
            args.insert(internal_key.to_string(), serde_json::Value::String(value.clone()));
        }

        ToolCall {
            id: self.id.clone(),
            call_type: "function".to_string(),
            function: FunctionCall {
                name: internal_name.to_string(),
                arguments: serde_json::to_string(&args).unwrap_or_default(),
            },
        }
    }
}

/// Parser for Claude's XML-style tool invocations
pub struct ToolInterceptor {
    /// Accumulated content that may contain tool calls
    buffer: String,
    /// Whether we're currently inside a function_calls block
    in_function_calls: bool,
}

impl Default for ToolInterceptor {
    fn default() -> Self {
        Self::new()
    }
}

impl ToolInterceptor {
    pub fn new() -> Self {
        Self {
            buffer: String::new(),
            in_function_calls: false,
        }
    }

    /// Add content to the buffer
    pub fn push(&mut self, content: &str) {
        self.buffer.push_str(content);
    }

    /// Get the current buffer contents
    pub fn get_buffer(&self) -> &str {
        &self.buffer
    }

    /// Clear the buffer
    pub fn clear(&mut self) {
        self.buffer.clear();
        self.in_function_calls = false;
    }

    /// Check if buffer contains tool invocations
    /// Claude Code uses direct <invoke> tags (not wrapped in <function_calls>)
    pub fn has_pending_tool_calls(&self) -> bool {
        const INVOKE_OPEN: &str = "<invoke name=\"";
        self.buffer.contains(INVOKE_OPEN) || self.in_function_calls
    }

    /// Check if we have a complete invoke block (invoke + result or just invoke closed)
    pub fn has_complete_block(&self) -> bool {
        const INVOKE_OPEN: &str = "<invoke name=\"";
        const INVOKE_CLOSE: &str = "</invoke>";

        // Look for complete <invoke>...</invoke> blocks
        if let Some(start) = self.buffer.find(INVOKE_OPEN) {
            if let Some(end) = self.buffer[start..].find(INVOKE_CLOSE) {
                // Check if there's a <result> after this invoke
                let invoke_end = start + end + INVOKE_CLOSE.len();
                let after = &self.buffer[invoke_end..];
                // If there's a result, wait for it to complete
                if after.trim_start().starts_with("<result>") {
                    return after.contains("</result>");
                }
                return true;
            }
        }
        false
    }

    /// Parse tool invocations from the buffer
    /// Returns extracted tool calls and the text content before/after the block
    /// NOTE: This also strips out <result> blocks (sandbox output we're replacing)
    pub fn extract_tool_calls(&mut self) -> (Vec<InterceptedToolCall>, String, String) {
        const INVOKE_OPEN: &str = "<invoke name=\"";
        const INVOKE_CLOSE: &str = "</invoke>";
        const RESULT_OPEN: &str = "<result>";
        const RESULT_CLOSE: &str = "</result>";

        // Find first invoke block
        let Some(start_idx) = self.buffer.find(INVOKE_OPEN) else {
            return (vec![], self.buffer.clone(), String::new());
        };

        // Find end of invoke block
        let Some(invoke_end_rel) = self.buffer[start_idx..].find(INVOKE_CLOSE) else {
            return (vec![], self.buffer.clone(), String::new());
        };
        let invoke_end = start_idx + invoke_end_rel + INVOKE_CLOSE.len();

        // Check if there's a <result> block to skip
        let after_invoke = &self.buffer[invoke_end..];
        let result_end = if after_invoke.trim_start().starts_with(RESULT_OPEN) {
            if let Some(result_close_idx) = after_invoke.find(RESULT_CLOSE) {
                invoke_end + result_close_idx + RESULT_CLOSE.len()
            } else {
                invoke_end
            }
        } else {
            invoke_end
        };

        let before = self.buffer[..start_idx].to_string();
        let after = self.buffer[result_end..].to_string();
        let invoke_block = &self.buffer[start_idx..invoke_end];

        let tools = self.parse_invocations(invoke_block);

        // Clear buffer and store remainder
        self.buffer = after.clone();

        (tools, before, after)
    }

    /// Parse invoke tags within a function_calls block
    fn parse_invocations(&self, block: &str) -> Vec<InterceptedToolCall> {
        const INVOKE_OPEN: &str = "<invoke name=\"";
        const INVOKE_CLOSE: &str = "</invoke>";
        const PARAM_OPEN: &str = "<parameter name=\"";
        const PARAM_CLOSE: &str = "</parameter>";

        let mut tools = Vec::new();
        let mut search_start = 0;

        while let Some(invoke_start) = block[search_start..].find(INVOKE_OPEN) {
            let abs_start = search_start + invoke_start;

            // Find tool name
            let name_start = abs_start + INVOKE_OPEN.len();
            let Some(name_end_rel) = block[name_start..].find('"') else {
                search_start = abs_start + 1;
                continue;
            };
            let name_end = name_start + name_end_rel;
            let tool_name = block[name_start..name_end].to_string();

            // Find end of this invoke block
            let Some(invoke_end_rel) = block[abs_start..].find(INVOKE_CLOSE) else {
                search_start = abs_start + 1;
                continue;
            };
            let invoke_end = abs_start + invoke_end_rel;
            let invoke_block = &block[abs_start..invoke_end];

            // Parse parameters within this invoke block
            let mut parameters = HashMap::new();
            let mut param_search = 0;

            while let Some(param_start) = invoke_block[param_search..].find(PARAM_OPEN) {
                let abs_param_start = param_search + param_start;

                // Get parameter name
                let pname_start = abs_param_start + PARAM_OPEN.len();
                let Some(pname_end_rel) = invoke_block[pname_start..].find('"') else {
                    param_search = abs_param_start + 1;
                    continue;
                };
                let pname_end = pname_start + pname_end_rel;
                let param_name = invoke_block[pname_start..pname_end].to_string();

                // Find the closing > after the parameter name
                let Some(value_start_rel) = invoke_block[pname_end..].find('>') else {
                    param_search = pname_end;
                    continue;
                };
                let value_start = pname_end + value_start_rel + 1;

                // Find closing tag
                let Some(value_end_rel) = invoke_block[value_start..].find(PARAM_CLOSE) else {
                    param_search = value_start;
                    continue;
                };
                let value_end = value_start + value_end_rel;
                let param_value = invoke_block[value_start..value_end].to_string();

                parameters.insert(param_name, param_value);
                param_search = value_end + PARAM_CLOSE.len();
            }

            tools.push(InterceptedToolCall {
                name: tool_name,
                parameters,
                id: format!("toolu_{}", Uuid::new_v4().to_string().replace("-", "")[..24].to_string()),
            });

            search_start = invoke_end + INVOKE_CLOSE.len();
        }

        tools
    }
}

/// Execute intercepted tool calls locally and format results for Claude
pub fn execute_intercepted_tools(
    tools: &[InterceptedToolCall],
    memory_db: Option<&crate::memory::MemoryDb>,
) -> Vec<(String, String, bool)> {
    let mut results = Vec::new();

    for tool in tools {
        let tool_call = tool.to_tool_call();

        println!("\n\x1b[36m⚡ Running {} locally...\x1b[0m", tool.name);

        let result = if let Some(db) = memory_db {
            crate::tools::execute_tool_with_memory(&tool_call, Some(db))
        } else {
            crate::tools::execute_tool(&tool_call)
        };

        // Show preview
        let preview: String = result.content.lines().take(5).collect::<Vec<_>>().join("\n");
        if result.is_error {
            println!("\x1b[31m✗ Error:\x1b[0m {}", preview);
        } else {
            println!(
                "\x1b[32m✓\x1b[0m {}",
                if preview.len() > 200 {
                    format!("{}...", &preview[..200])
                } else {
                    preview
                }
            );
        }

        results.push((tool.id.clone(), result.content, result.is_error));
    }

    results
}

/// Format tool results as XML for injection back to Claude
pub fn format_tool_results_xml(results: &[(String, String, bool)]) -> String {
    const OPEN_TAG: &str = "<function_results>";
    const CLOSE_TAG: &str = "</function_results>";
    const RESULT_OPEN: &str = "<result>";
    const RESULT_CLOSE: &str = "</result>";
    const OUTPUT_OPEN: &str = "<output>";
    const OUTPUT_CLOSE: &str = "</output>";
    const ERROR_OPEN: &str = "<error>";
    const ERROR_CLOSE: &str = "</error>";

    let mut xml = String::new();
    xml.push_str(OPEN_TAG);
    xml.push('\n');

    for (id, content, is_error) in results {
        xml.push_str(RESULT_OPEN);
        xml.push('\n');
        xml.push_str(&format!("<tool_use_id>{}</tool_use_id>\n", id));

        if *is_error {
            xml.push_str(ERROR_OPEN);
            xml.push_str(content);
            xml.push_str(ERROR_CLOSE);
        } else {
            xml.push_str(OUTPUT_OPEN);
            xml.push_str(content);
            xml.push_str(OUTPUT_CLOSE);
        }
        xml.push('\n');
        xml.push_str(RESULT_CLOSE);
        xml.push('\n');
    }

    xml.push_str(CLOSE_TAG);
    xml
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_bash_invocation() {
        let mut interceptor = ToolInterceptor::new();

        // Simulate Claude Code's actual format (direct invoke, no function_calls wrapper)
        let content = r#"Let me check the directory.

<invoke name="Bash">
<parameter name="command">ls -la</parameter>
</invoke>
<result>
sandbox output here
</result>

And here's some text after."#;

        interceptor.push(content);
        assert!(interceptor.has_complete_block());

        let (tools, before, _after) = interceptor.extract_tool_calls();

        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "Bash");
        assert_eq!(tools[0].parameters.get("command"), Some(&"ls -la".to_string()));
        assert!(before.contains("Let me check the directory"));
    }

    #[test]
    fn test_parse_with_sandbox_result() {
        let mut interceptor = ToolInterceptor::new();

        // Claude Code returns sandbox results inline - we need to skip them
        let content = r#"<invoke name="list_files">
<parameter name="path">.</parameter>
</invoke>
<result>
LICENSE
README.md
claude_code
</result>

Some text after."#;

        interceptor.push(content);
        assert!(interceptor.has_complete_block());

        let (tools, _before, after) = interceptor.extract_tool_calls();

        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "list_files");
        // The result block should be stripped, not in 'after'
        assert!(!after.contains("LICENSE"));
        assert!(after.contains("Some text after"));
    }

    #[test]
    fn test_parse_multiple_invocations() {
        let mut interceptor = ToolInterceptor::new();

        // First invocation with result
        let content = r#"<invoke name="Read">
<parameter name="file_path">/tmp/test.txt</parameter>
</invoke>
<result>file contents</result>

<invoke name="Bash">
<parameter name="command">pwd</parameter>
</invoke>
<result>/tmp</result>"#;

        interceptor.push(content);

        // First extraction gets Read
        let (tools1, _, _) = interceptor.extract_tool_calls();
        assert_eq!(tools1.len(), 1);
        assert_eq!(tools1[0].name, "Read");

        // Second extraction gets Bash
        let (tools2, _, _) = interceptor.extract_tool_calls();
        assert_eq!(tools2.len(), 1);
        assert_eq!(tools2[0].name, "Bash");
    }

    #[test]
    fn test_tool_call_conversion() {
        let tool = InterceptedToolCall {
            name: "Bash".to_string(),
            parameters: [("command".to_string(), "echo hello".to_string())].into(),
            id: "test123".to_string(),
        };

        let tc = tool.to_tool_call();
        assert_eq!(tc.function.name, "bash");
        assert!(tc.function.arguments.contains("echo hello"));
    }
}
