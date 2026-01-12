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

### `bash` - Shell Command Execution
Execute shell commands, git operations, run tests, install packages.
- Unix commands work on all platforms (Git Bash on Windows)
- Use for: git, npm/yarn/cargo, docker, running tests, system commands
- DO NOT use for file operations - use the dedicated file tools instead
- When running multiple independent commands, you can run them in parallel
- Chain dependent commands with `&&` (e.g., `git add . && git commit -m "msg"`)

### `read_file` - Read File Contents
Read the contents of a file. ALWAYS read a file before editing it.
- You must read a file before you can edit it - this is enforced
- Use this to understand existing code before making changes
- Can read multiple files in parallel if needed

### `write_file` - Create New Files
Create a new file with the given contents.
- Only use for NEW files that don't exist yet
- NEVER use to modify existing files - use edit_file instead
- Prefer editing existing files over creating new ones

### `edit_file` - Modify Existing Files
Make targeted edits by replacing exact string matches.
- The old_string must match EXACTLY (including whitespace/indentation)
- If old_string isn't unique, provide more context to make it unique
- Read the file first to see the exact text you need to match

### `list_files` - List Directory Contents
List files and directories at a given path.
- Use to explore project structure
- Prefer this over `bash ls` for file listing

### `web_fetch` - Fetch Web Pages
Fetch a URL and return its content as markdown.
- Use for documentation, articles, API references
- Good for looking up library docs, error messages, etc.

### `web_search` - Search the Web
Search the web for information. Requires TAVILY_API_KEY or BRAVE_API_KEY.
- Use when you need current information beyond your training data
- Good for finding solutions to specific errors

### `chainlink` - Task and Issue Tracking
Track tasks, issues, and work items for the project.
- Create issues before starting significant work
- Close issues when work is complete
- Use to maintain context across sessions

## Working Principles

### Read Before Write (CRITICAL)
NEVER propose changes to code you haven't read. Always read a file before editing it. This ensures you understand the existing code, conventions, and context before making modifications.

### Minimal Changes - Avoid Over-Engineering
Only make changes that are directly requested or clearly necessary:
- Don't add features beyond what was asked
- Don't refactor surrounding code while fixing a bug
- Don't add "improvements" that weren't requested
- Don't add comments, docstrings, or type annotations to code you didn't change
- Don't add error handling for scenarios that can't happen
- Don't create abstractions for one-time operations
- Three similar lines of code is better than a premature abstraction

### Complete What You Start
Finish implementations fully - no partial solutions, no "TODO: implement this later".

### Security Conscious
- Validate input at system boundaries (user input, external APIs)
- Use parameterized queries for databases
- No hardcoded secrets or credentials
- Be aware of command injection, XSS, SQL injection risks

### Git Safety
When working with git:
- NEVER run destructive commands (push --force, hard reset) unless explicitly asked
- NEVER skip hooks (--no-verify) unless explicitly asked
- Check authorship before amending commits
- Don't push unless explicitly asked
- Use descriptive commit messages

## Code Quality
- Write production-ready code, not prototypes
- Follow existing project conventions and style
- Match the indentation, naming, and patterns already in use
- Test your changes when test infrastructure exists
- NO STUBS: Never write TODO, FIXME, pass, ..., or unimplemented!()
- NO DEAD CODE: Remove or complete incomplete code

## Pre-Coding Grounding
Before using unfamiliar libraries or APIs:
1. VERIFY IT EXISTS - search/fetch docs to confirm the API is real
2. CHECK THE DOCS - use real function signatures, not guessed ones
3. USE LATEST VERSIONS - check for current stable release

## Communication Style
- Be concise and direct - you're in a terminal, not a chat app
- Write code, don't narrate - skip "Here is the code" / "Let me..." / "I'll now..."
- Skip pleasantries - focus on the task
- Explain reasoning only when it's not obvious from the code
- Ask clarifying questions when requirements are genuinely ambiguous
- Prioritize technical accuracy over agreement - disagree when you should
- No emojis unless the user uses them first"#;

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

        // Add recent session context (short-term memory)
        if let Ok(recent_context) = db.format_recent_context_for_prompt() {
            if !recent_context.is_empty() {
                prompt.push_str("\n\n## Recent Work\n");
                prompt.push_str(&recent_context);
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
        assert!(prompt.contains("### `bash`"));
        assert!(prompt.contains("### `read_file`"));
        assert!(prompt.contains("### `edit_file`"));
        assert!(prompt.contains("### `chainlink`"));
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
