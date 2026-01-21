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
            "read" | "read_file" => "read_file",
            "write" | "write_file" => "write_file",
            "edit" | "edit_file" => "edit_file",
            "glob" | "list_files" => "list_files", // Our internal name is list_files
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
                ("write", "contents") => "content", // Claude sometimes uses plural
                ("write_file", "file_path") => "path",
                ("write_file", "path") => "path",
                ("write_file", "content") => "content",
                ("write_file", "contents") => "content", // Claude sometimes uses plural
                ("edit", "file_path") => "path",
                ("edit", "old_string") => "old_string",
                ("edit", "new_string") => "new_string",
                ("edit_file", "file_path") => "path",
                ("edit_file", "path") => "path",
                ("edit_file", "old_string") => "old_string",
                ("edit_file", "new_string") => "new_string",
                ("read_file", "file_path") => "path",
                ("read_file", "path") => "path",
                ("glob", "pattern") => "pattern",
                ("glob", "path") => "path",
                ("grep", "pattern") => "pattern",
                ("grep", "path") => "path",
                (_, k) => k,
            };
            args.insert(
                internal_key.to_string(),
                serde_json::Value::String(value.clone()),
            );
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

    /// Shorthand tool tags that Claude might use (e.g., <bash>cmd</bash>)
    const SHORTHAND_TOOLS: &'static [&'static str] = &[
        "bash",
        "read",
        "write",
        "edit",
        "glob",
        "grep",
        "read_file",
        "write_file",
        "edit_file",
    ];

    /// Check if buffer contains tool invocations
    /// Claude Code uses multiple formats:
    /// 1. <invoke name="Bash"><parameter name="command">...</parameter></invoke>
    /// 2. <bash>...</bash> (shorthand)
    /// 3. <function_calls><invoke>...</invoke></function_calls>
    pub fn has_pending_tool_calls(&self) -> bool {
        // Check for full invoke format
        if self.buffer.contains("<invoke name=\"") {
            return true;
        }
        // Check for shorthand format like <bash>, <read>, etc.
        for tool in Self::SHORTHAND_TOOLS {
            let open_tag = format!("<{}>", tool);
            let open_tag_with_attr = format!("<{} ", tool); // <write path="...">
            if self.buffer.contains(&open_tag) || self.buffer.contains(&open_tag_with_attr) {
                return true;
            }
        }
        self.in_function_calls
    }

    /// Check if we have a complete tool block
    pub fn has_complete_block(&self) -> bool {
        // Check for full invoke format
        if let Some(start) = self.buffer.find("<invoke name=\"") {
            if let Some(end) = self.buffer[start..].find("</invoke>") {
                let invoke_end = start + end + "</invoke>".len();
                let after = &self.buffer[invoke_end..];
                if after.trim_start().starts_with("<result>") {
                    return after.contains("</result>");
                }
                return true;
            }
        }
        // Check for shorthand format
        for tool in Self::SHORTHAND_TOOLS {
            let open_tag = format!("<{}>", tool);
            let open_tag_with_attr = format!("<{} ", tool);
            let close_tag = format!("</{}>", tool);

            let has_open =
                self.buffer.contains(&open_tag) || self.buffer.contains(&open_tag_with_attr);
            let has_close = self.buffer.contains(&close_tag);

            if has_open && has_close {
                return true;
            }
        }
        false
    }

    /// Parse tool invocations from the buffer
    /// Returns extracted tool calls and the text content before/after the block
    /// NOTE: This also strips out <result> blocks (sandbox output we're replacing)
    pub fn extract_tool_calls(&mut self) -> (Vec<InterceptedToolCall>, String, String) {
        // Try full invoke format first
        if let Some(result) = self.try_extract_invoke_format() {
            return result;
        }

        // Try shorthand format (e.g., <bash>cmd</bash>)
        if let Some(result) = self.try_extract_shorthand_format() {
            return result;
        }

        // No tool calls found
        (vec![], self.buffer.clone(), String::new())
    }

    /// Try to extract tool calls in <invoke name="..."> format
    fn try_extract_invoke_format(&mut self) -> Option<(Vec<InterceptedToolCall>, String, String)> {
        const INVOKE_OPEN: &str = "<invoke name=\"";
        const INVOKE_CLOSE: &str = "</invoke>";
        const RESULT_OPEN: &str = "<result>";
        const RESULT_CLOSE: &str = "</result>";

        let start_idx = self.buffer.find(INVOKE_OPEN)?;
        let invoke_end_rel = self.buffer[start_idx..].find(INVOKE_CLOSE)?;
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
        self.buffer = after.clone();

        Some((tools, before, after))
    }

    /// Try to extract tool calls in shorthand format (e.g., <bash>cmd</bash>)
    fn try_extract_shorthand_format(
        &mut self,
    ) -> Option<(Vec<InterceptedToolCall>, String, String)> {
        // Find the first shorthand tool tag
        let mut earliest_match: Option<(usize, &str)> = None;

        for tool in Self::SHORTHAND_TOOLS {
            let open_tag = format!("<{}>", tool);
            let open_tag_attr = format!("<{} ", tool);

            // Check for <tool> or <tool attr="...">
            if let Some(idx) = self.buffer.find(&open_tag) {
                if earliest_match.is_none() || idx < earliest_match.unwrap().0 {
                    earliest_match = Some((idx, *tool));
                }
            }
            if let Some(idx) = self.buffer.find(&open_tag_attr) {
                if earliest_match.is_none() || idx < earliest_match.unwrap().0 {
                    earliest_match = Some((idx, *tool));
                }
            }
        }

        let (start_idx, tool_name) = earliest_match?;
        let close_tag = format!("</{}>", tool_name);
        let close_idx = self.buffer[start_idx..].find(&close_tag)?;
        let block_end = start_idx + close_idx + close_tag.len();

        // Extract the content between tags
        let tag_content = &self.buffer[start_idx..block_end];

        // Parse the shorthand tag
        let tool = self.parse_shorthand_tag(tool_name, tag_content)?;

        let before = self.buffer[..start_idx].to_string();
        let after = self.buffer[block_end..].to_string();
        self.buffer = after.clone();

        Some((vec![tool], before, after))
    }

    /// Parse a shorthand tool tag like <bash>command</bash> or <write path="file">content</write>
    /// Also handles nested element format: <write_file><path>file</path><content>...</content></write_file>
    fn parse_shorthand_tag(
        &self,
        tool_name: &str,
        tag_content: &str,
    ) -> Option<InterceptedToolCall> {
        let open_simple = format!("<{}>", tool_name);
        let open_attr = format!("<{} ", tool_name);
        let close_tag = format!("</{}>", tool_name);

        let mut parameters = HashMap::new();

        // Check if it's a simple tag <tool>content</tool> or has attributes <tool attr="val">content</tool>
        let content_start = if tag_content.starts_with(&open_simple) {
            open_simple.len()
        } else if tag_content.starts_with(&open_attr) {
            // Parse attributes from <tool attr="val" attr2="val2">
            let close_bracket = tag_content.find('>')?;
            let attr_str = &tag_content[open_attr.len()..close_bracket];

            // Simple attribute parsing: attr="value"
            let mut attr_search = 0;
            while let Some(eq_pos) = attr_str[attr_search..].find('=') {
                let abs_eq = attr_search + eq_pos;
                let attr_name = attr_str[attr_search..abs_eq].trim();

                // Find quoted value
                let quote_start = attr_str[abs_eq..].find('"')? + abs_eq + 1;
                let quote_end = attr_str[quote_start..].find('"')? + quote_start;
                let attr_value = &attr_str[quote_start..quote_end];

                parameters.insert(attr_name.to_string(), attr_value.to_string());
                attr_search = quote_end + 1;
            }

            close_bracket + 1
        } else {
            return None;
        };

        // Extract content between open and close tags
        let content_end = tag_content.len() - close_tag.len();
        let content = tag_content[content_start..content_end].to_string();

        // Check for nested element format: <tool><param>value</param><param2>value2</param2></tool>
        // This is used when Claude outputs things like:
        // <write_file><path>hello.c</path><content>...</content></write_file>
        let trimmed_content = content.trim();
        if trimmed_content.starts_with('<') && !trimmed_content.starts_with("</") {
            // Parse nested elements
            self.parse_nested_elements(trimmed_content, &mut parameters);
        }

        // If no nested elements found, use the old logic for simple content
        if parameters.is_empty() {
            // Map shorthand content to appropriate parameter
            match tool_name {
                "bash" => {
                    parameters.insert("command".to_string(), content);
                }
                "read" | "read_file" => {
                    if !parameters.contains_key("path") && !parameters.contains_key("file_path") {
                        parameters.insert("path".to_string(), content);
                    }
                }
                "write" | "write_file" => {
                    // Content is the file content, path should be in attributes
                    if !content.is_empty() {
                        parameters.insert("content".to_string(), content);
                    }
                }
                "edit" | "edit_file" => {
                    // Content might be used for something, but usually params are in attributes
                }
                "glob" => {
                    if !parameters.contains_key("pattern") {
                        parameters.insert("pattern".to_string(), content);
                    }
                }
                "grep" => {
                    if !parameters.contains_key("pattern") {
                        parameters.insert("pattern".to_string(), content);
                    }
                }
                _ => {}
            }
        }

        Some(InterceptedToolCall {
            name: tool_name.to_string(),
            parameters,
            id: format!(
                "toolu_{}",
                Uuid::new_v4().to_string().replace("-", "")[..24].to_string()
            ),
        })
    }

    /// Parse nested XML elements like <path>value</path><content>...</content>
    fn parse_nested_elements(&self, content: &str, parameters: &mut HashMap<String, String>) {
        let mut search_pos = 0;

        while search_pos < content.len() {
            // Find opening tag
            let Some(tag_start) = content[search_pos..].find('<') else {
                break;
            };
            let abs_tag_start = search_pos + tag_start;

            // Skip if it's a closing tag
            if content[abs_tag_start..].starts_with("</") {
                search_pos = abs_tag_start + 1;
                continue;
            }

            // Find end of opening tag
            let Some(tag_end) = content[abs_tag_start..].find('>') else {
                break;
            };
            let abs_tag_end = abs_tag_start + tag_end;

            // Extract element name (handle self-closing tags)
            let tag_content = &content[abs_tag_start + 1..abs_tag_end];
            let elem_name = tag_content
                .split_whitespace()
                .next()
                .unwrap_or("")
                .trim_end_matches('/');

            if elem_name.is_empty() {
                search_pos = abs_tag_end + 1;
                continue;
            }

            // Find closing tag
            let close_tag = format!("</{}>", elem_name);
            let Some(close_pos) = content[abs_tag_end..].find(&close_tag) else {
                search_pos = abs_tag_end + 1;
                continue;
            };
            let abs_close_pos = abs_tag_end + close_pos;

            // Extract value between tags
            let value = content[abs_tag_end + 1..abs_close_pos].to_string();

            // Map element names to parameter names
            let param_name = match elem_name {
                "file_path" => "path",
                "old_string" => "old_string",
                "new_string" => "new_string",
                "contents" => "content", // Claude sometimes uses plural
                _ => elem_name,
            };

            parameters.insert(param_name.to_string(), value);

            // Move past this element
            search_pos = abs_close_pos + close_tag.len();
        }
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
                id: format!(
                    "toolu_{}",
                    Uuid::new_v4().to_string().replace("-", "")[..24].to_string()
                ),
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
        let preview: String = result
            .content
            .lines()
            .take(5)
            .collect::<Vec<_>>()
            .join("\n");
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
        assert_eq!(
            tools[0].parameters.get("command"),
            Some(&"ls -la".to_string())
        );
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

    #[test]
    fn test_parse_shorthand_bash() {
        let mut interceptor = ToolInterceptor::new();

        // Shorthand format Claude sometimes uses: <bash>command</bash>
        let content = r#"I'll check the directory.

<bash>pwd</bash>

That's the current directory."#;

        interceptor.push(content);
        assert!(interceptor.has_pending_tool_calls());
        assert!(interceptor.has_complete_block());

        let (tools, before, after) = interceptor.extract_tool_calls();

        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "bash");
        assert_eq!(tools[0].parameters.get("command"), Some(&"pwd".to_string()));
        assert!(before.contains("I'll check the directory"));
        assert!(after.contains("That's the current directory"));
    }

    #[test]
    fn test_parse_shorthand_read() {
        let mut interceptor = ToolInterceptor::new();

        let content = r#"<read>/path/to/file.txt</read>"#;

        interceptor.push(content);
        assert!(interceptor.has_complete_block());

        let (tools, _, _) = interceptor.extract_tool_calls();

        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "read");
        assert_eq!(
            tools[0].parameters.get("path"),
            Some(&"/path/to/file.txt".to_string())
        );
    }

    #[test]
    fn test_parse_shorthand_glob() {
        let mut interceptor = ToolInterceptor::new();

        let content = r#"<glob>**/*.rs</glob>"#;

        interceptor.push(content);
        assert!(interceptor.has_complete_block());

        let (tools, _, _) = interceptor.extract_tool_calls();

        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "glob");
        assert_eq!(
            tools[0].parameters.get("pattern"),
            Some(&"**/*.rs".to_string())
        );
    }

    #[test]
    fn test_parse_nested_element_write_file() {
        let mut interceptor = ToolInterceptor::new();

        // Claude Code format: <write_file><path>file</path><content>...</content></write_file>
        let content = r#"<write_file>
<path>hello.c</path>
<content>#include <stdio.h>
int main() { return 0; }
</content>
</write_file>"#;

        interceptor.push(content);
        assert!(interceptor.has_complete_block());

        let (tools, _, _) = interceptor.extract_tool_calls();

        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "write_file");
        assert_eq!(
            tools[0].parameters.get("path"),
            Some(&"hello.c".to_string())
        );
        assert!(tools[0]
            .parameters
            .get("content")
            .unwrap()
            .contains("stdio.h"));
    }

    #[test]
    fn test_parse_nested_element_edit_file() {
        let mut interceptor = ToolInterceptor::new();

        // Claude Code format for edit
        let content = r#"<edit_file>
<path>src/main.rs</path>
<old_string>fn old() {}</old_string>
<new_string>fn new() {}</new_string>
</edit_file>"#;

        interceptor.push(content);
        assert!(interceptor.has_complete_block());

        let (tools, _, _) = interceptor.extract_tool_calls();

        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "edit_file");
        assert_eq!(
            tools[0].parameters.get("path"),
            Some(&"src/main.rs".to_string())
        );
        assert_eq!(
            tools[0].parameters.get("old_string"),
            Some(&"fn old() {}".to_string())
        );
        assert_eq!(
            tools[0].parameters.get("new_string"),
            Some(&"fn new() {}".to_string())
        );
    }
}
