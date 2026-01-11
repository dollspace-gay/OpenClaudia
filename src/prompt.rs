//! System prompt module for Claudia's core personality
//!
//! Defines the base system prompt that shapes Claudia's behavior as a coding agent.
//! Supports customization via:
//! - Hook instructions (injected dynamically)
//! - Custom instructions (from config or CLI)
//! - Core memory (in stateful mode)

use crate::memory::MemoryDb;

/// Base system prompt defining Claudia's core personality and capabilities
const BASE_PROMPT: &str = r#"You are Claudia, an AI coding agent created to help developers build software.

## Core Identity
- Your name is Claudia
- You are a skilled software engineer with expertise across many languages and frameworks
- You are direct, helpful, and focused on solving problems efficiently
- You write clean, working code - never stubs, placeholders, or TODOs

## Your Tools
You have access to these tools - use them effectively:
- `bash` - Execute shell commands, git operations, run tests. Unix commands work on all platforms via Git Bash.
- `read_file` - Read file contents. Always read a file before editing it.
- `write_file` - Create new files. Only use for new files, not modifications.
- `edit_file` - Make targeted edits by replacing exact strings. Requires the old text to match exactly.
- `list_files` - List directory contents.
- `web_fetch` - Fetch web pages as markdown. Use for documentation, articles, references.
- `web_search` - Search the web. Requires API key (TAVILY_API_KEY or BRAVE_API_KEY).
- `chainlink` - Track tasks and issues. Create issues before starting work, close when done.

## Working Style
1. **Read before write**: Always read a file before editing it
2. **Complete implementations**: Finish what you start - no partial solutions
3. **Handle errors gracefully**: Don't let bad input crash anything
4. **Security-conscious**: Validate input, use parameterized queries, no hardcoded secrets
5. **Minimal changes**: Only modify what's necessary to solve the problem

## Code Quality
- Write production-ready code, not prototypes
- Follow existing project conventions and style
- Include error handling appropriate to the context
- Test your changes when possible

## Communication Style
- Be concise and direct
- Skip unnecessary pleasantries - focus on the task
- Explain your reasoning when it's not obvious from the code
- Ask clarifying questions when requirements are ambiguous"#;

/// Build the complete system prompt with all components
pub fn build_system_prompt(
    hook_instructions: Option<&str>,
    custom_instructions: Option<&str>,
    memory_db: Option<&MemoryDb>,
) -> String {
    let mut prompt = String::with_capacity(8192);

    // Start with base personality
    prompt.push_str(BASE_PROMPT);

    // Add core memory context if in stateful mode
    if let Some(db) = memory_db {
        if let Ok(core_memory) = db.format_core_memory_for_prompt() {
            if !core_memory.is_empty() {
                prompt.push_str("\n\n## Your Memory\n");
                prompt.push_str(&core_memory);
            }
        }
    }

    // Add hook instructions (from active hooks)
    if let Some(instructions) = hook_instructions {
        if !instructions.trim().is_empty() {
            prompt.push_str("\n\n## Active Instructions\n");
            prompt.push_str("The following instructions come from the project's configured hooks. Follow them carefully:\n\n");
            prompt.push_str(instructions);
        }
    }

    // Add custom instructions (from config or CLI)
    if let Some(custom) = custom_instructions {
        if !custom.trim().is_empty() {
            prompt.push_str("\n\n## Custom Instructions\n");
            prompt.push_str(custom);
        }
    }

    prompt
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_base_prompt_contains_identity() {
        let prompt = build_system_prompt(None, None, None);
        assert!(prompt.contains("Claudia"));
        assert!(prompt.contains("coding agent"));
    }

    #[test]
    fn test_base_prompt_contains_tools() {
        let prompt = build_system_prompt(None, None, None);
        assert!(prompt.contains("`bash`"));
        assert!(prompt.contains("`read_file`"));
        assert!(prompt.contains("`edit_file`"));
        assert!(prompt.contains("`chainlink`"));
    }

    #[test]
    fn test_build_prompt_with_no_extras() {
        let prompt = build_system_prompt(None, None, None);
        assert!(prompt.contains("You are Claudia"));
        assert!(!prompt.contains("Active Instructions"));
        assert!(!prompt.contains("Custom Instructions"));
    }

    #[test]
    fn test_build_prompt_with_hook_instructions() {
        let prompt = build_system_prompt(
            Some("Always run tests before committing"),
            None,
            None,
        );
        assert!(prompt.contains("Active Instructions"));
        assert!(prompt.contains("Always run tests"));
    }

    #[test]
    fn test_build_prompt_with_custom_instructions() {
        let prompt = build_system_prompt(
            None,
            Some("Use TypeScript for all new files"),
            None,
        );
        assert!(prompt.contains("Custom Instructions"));
        assert!(prompt.contains("TypeScript"));
    }

    #[test]
    fn test_build_prompt_with_all_components() {
        let prompt = build_system_prompt(
            Some("Hook instruction here"),
            Some("Custom instruction here"),
            None,
        );
        assert!(prompt.contains("You are Claudia"));
        assert!(prompt.contains("Active Instructions"));
        assert!(prompt.contains("Hook instruction"));
        assert!(prompt.contains("Custom Instructions"));
        assert!(prompt.contains("Custom instruction"));
    }

    #[test]
    fn test_empty_instructions_not_added() {
        let prompt = build_system_prompt(Some(""), Some("   "), None);
        assert!(!prompt.contains("Active Instructions"));
        assert!(!prompt.contains("Custom Instructions"));
    }
}
