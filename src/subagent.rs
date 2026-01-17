//! Subagent System for OpenClaudia
//!
//! Provides Claude Code-style subagent capabilities:
//! - Task tool for spawning autonomous sub-agents
//! - AgentOutput tool for retrieving background agent results
//! - Agent type configurations with specialized system prompts
//! - Isolated conversation contexts per subagent
//! - Background execution with async tracking

use crate::config::AppConfig;
use crate::tools::{execute_tool, ToolCall};
use regex::Regex;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use tokio::runtime::Handle;
use uuid::Uuid;

/// Maximum turns a subagent can execute before forced termination
const MAX_SUBAGENT_TURNS: usize = 50;

/// Maximum tokens for subagent responses
const SUBAGENT_MAX_TOKENS: u32 = 8192;

/// Detect chainlink issue references in text (e.g., #123, issue 123, issue #123)
/// Returns the first issue ID found, if any
fn detect_chainlink_issue(text: &str) -> Option<u32> {
    // Match patterns like #123, issue 123, issue #123, chainlink #123
    let re = Regex::new(r"(?i)(?:#(\d+)|issue\s*#?(\d+)|chainlink\s*#?(\d+))").ok()?;

    for caps in re.captures_iter(text) {
        // Try each capture group
        for i in 1..=3 {
            if let Some(m) = caps.get(i) {
                if let Ok(id) = m.as_str().parse::<u32>() {
                    return Some(id);
                }
            }
        }
    }
    None
}

/// Spawn a TestBuilder agent to work alongside a coding agent
fn spawn_companion_test_builder(
    issue_id: u32,
    original_task: &str,
    app_config: &AppConfig,
) -> Option<String> {
    // Get the current working directory to pass to the subagent
    let working_dir = std::env::current_dir()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| ".".to_string());

    let test_builder_task = format!("Adversarial testing for issue #{}", issue_id);
    let test_builder_prompt = format!(
        r#"A coding agent is working on chainlink issue #{}. YOUR JOB IS TO BREAK THEIR CODE.

## Working Directory
You are working in: {}
All file paths and commands should be relative to or executed from this directory.

Original task they're working on: {}

## Your Mission
Don't trust them. They probably made mistakes. Find those mistakes.

## Instructions
1. Check the issue: `chainlink show {}`
2. Watch for their changes: `git diff` and `git diff --cached`
3. Read their code critically - what did they miss? What edge cases did they ignore?
4. Write ADVERSARIAL tests designed to break their implementation
5. Run the tests. Hunt for failures.

## When You Find Bugs (and you will)
- Reopen the issue: `chainlink reopen {}`
- Add a detailed comment explaining exactly how their code fails
- Include "⚠️ ALERT: Test failure detected - coding agent's work has bugs"

## Categories to Attack
- Empty/null inputs
- Boundary conditions (off-by-one, max values, zero)
- Invalid types and malformed data
- Error paths (what if things fail?)
- Concurrency (if applicable)

Start by checking what they've changed. Then break it."#,
        issue_id, working_dir, original_task, issue_id, issue_id
    );

    let config = SubagentConfig {
        agent_type: AgentType::TestBuilder,
        task: test_builder_task.clone(),
        prompt: test_builder_prompt,
        run_in_background: true,
        model_override: Some("opus".to_string()), // Use opus for adversarial reasoning
    };

    let client = Client::new();
    let agent_id = BACKGROUND_AGENTS.register(AgentType::TestBuilder, &test_builder_task);

    let config_clone = config.clone();
    let app_config_clone = app_config.clone();
    let agent_id_clone = agent_id.clone();

    if let Ok(handle) = Handle::try_current() {
        // Console output: TestBuilder spawning
        eprintln!(
            "\n\x1b[33m🧪 TestBuilder agent spawned (ID: {})\x1b[0m",
            agent_id
        );
        eprintln!(
            "\x1b[33m   └─ Adversarial testing for issue #{}\x1b[0m\n",
            issue_id
        );

        handle.spawn(async move {
            let result = run_subagent(&config_clone, &app_config_clone, &client).await;

            // Console output: TestBuilder completed
            if result.success {
                // Check for ALERT in output - this means bugs were found!
                if result.output.contains("ALERT") || result.output.contains("⚠️") {
                    eprintln!("\n\x1b[31m╔════════════════════════════════════════════════════════════╗\x1b[0m");
                    eprintln!("\x1b[31m║  ⚠️  TESTBUILDER FOUND BUGS IN CODING AGENT'S WORK!  ⚠️    ║\x1b[0m");
                    eprintln!("\x1b[31m╚════════════════════════════════════════════════════════════╝\x1b[0m");
                    eprintln!("\x1b[33mAgent ID: {}\x1b[0m", agent_id_clone);
                    eprintln!("\x1b[33mUse `agent_output` to see full details.\x1b[0m\n");
                } else {
                    eprintln!(
                        "\n\x1b[32m✓ TestBuilder completed (ID: {}) - {} turns\x1b[0m",
                        agent_id_clone, result.turns_used
                    );
                    eprintln!("\x1b[32m  Use `agent_output` to see results.\x1b[0m\n");
                }
            } else {
                eprintln!(
                    "\n\x1b[31m✗ TestBuilder failed (ID: {}): {}\x1b[0m\n",
                    agent_id_clone, result.output
                );
                BACKGROUND_AGENTS.fail(&agent_id_clone, result.output);
            }
        });
        Some(agent_id)
    } else {
        eprintln!("\x1b[31m✗ Failed to spawn TestBuilder - no async runtime\x1b[0m");
        None
    }
}

/// Agent types available for spawning
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AgentType {
    /// General-purpose agent for complex multi-step tasks
    GeneralPurpose,
    /// Fast agent for codebase exploration and searches
    Explore,
    /// Software architect agent for designing implementation plans
    Plan,
    /// Documentation lookup agent
    Guide,
    /// Test builder agent that writes tests alongside coding agents
    TestBuilder,
}

impl AgentType {
    /// Parse agent type from string
    pub fn parse_type(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "general-purpose" | "general_purpose" | "generalpurpose" => {
                Some(AgentType::GeneralPurpose)
            }
            "explore" | "explorer" => Some(AgentType::Explore),
            "plan" | "planner" => Some(AgentType::Plan),
            "guide" | "claude-code-guide" => Some(AgentType::Guide),
            "test-builder" | "test_builder" | "testbuilder" | "tester" => {
                Some(AgentType::TestBuilder)
            }
            _ => None,
        }
    }

    /// Get the system prompt for this agent type
    pub fn system_prompt(&self) -> &'static str {
        match self {
            AgentType::GeneralPurpose => GENERAL_PURPOSE_PROMPT,
            AgentType::Explore => EXPLORE_PROMPT,
            AgentType::Plan => PLAN_PROMPT,
            AgentType::Guide => GUIDE_PROMPT,
            AgentType::TestBuilder => TEST_BUILDER_PROMPT,
        }
    }

    /// Get the tools available to this agent type
    pub fn allowed_tools(&self) -> Vec<&'static str> {
        match self {
            AgentType::GeneralPurpose => vec![
                "bash",
                "bash_output",
                "kill_shell",
                "read_file",
                "write_file",
                "edit_file",
                "list_files",
                "web_fetch",
                "web_search",
            ],
            AgentType::Explore => {
                vec!["bash", "read_file", "list_files", "web_fetch", "web_search"]
            }
            AgentType::Plan => vec!["bash", "read_file", "list_files", "web_fetch", "web_search"],
            AgentType::Guide => vec!["read_file", "list_files", "web_fetch", "web_search"],
            AgentType::TestBuilder => vec![
                "bash",        // For running tests (cargo test, pytest)
                "bash_output", // For monitoring long-running tests
                "kill_shell",  // For stopping hung tests
                "read_file",   // For reading source code to understand what to test
                "write_file",  // For creating new test files
                "edit_file",   // For adding tests to existing test files
                "list_files",  // For finding test files and source files
                "chainlink",   // For reading issues, reopening on failure, adding comments
            ],
        }
    }

    /// Get model preference for this agent type
    pub fn preferred_model(&self) -> Option<&'static str> {
        match self {
            AgentType::Explore => Some("haiku"), // Fast, cheap searches
            AgentType::Guide => Some("haiku"),   // Documentation lookup
            AgentType::TestBuilder => Some("opus"), // Use opus for adversarial reasoning
            _ => None,                           // Use default model
        }
    }
}

// === System Prompts for Agent Types ===

const GENERAL_PURPOSE_PROMPT: &str = r#"You are a specialized subagent spawned to handle a complex task autonomously.

Your goal is to complete the assigned task thoroughly and return a comprehensive summary of what you accomplished.

Guidelines:
- Work autonomously to complete the task
- Use tools as needed to accomplish your goal
- Be thorough but efficient
- When you're done, provide a clear summary of:
  - What was accomplished
  - Any files created or modified
  - Any issues encountered
  - Recommendations for follow-up if needed

You have access to file and shell tools. Use them to explore the codebase, make changes, and verify your work."#;

const EXPLORE_PROMPT: &str = r#"You are a fast exploration agent specialized for searching codebases.

Your goal is to quickly find relevant files, code patterns, and answer questions about the codebase structure.

Guidelines:
- Use bash with grep, find, or similar tools to search efficiently
- Read files to understand their contents
- Be fast and focused - don't over-explore
- Return a concise summary of what you found including:
  - Relevant file paths
  - Key code snippets or patterns
  - Direct answers to the question asked

Focus on speed and relevance. Don't modify any files - this is read-only exploration."#;

const PLAN_PROMPT: &str = r#"You are a software architect agent for designing implementation plans.

Your goal is to analyze the codebase and design a clear implementation strategy for the requested feature or change.

Guidelines:
- Explore the existing codebase to understand patterns and architecture
- Identify the files that need to be modified
- Consider edge cases and potential issues
- Design a step-by-step implementation plan

Return a structured plan including:
- Overview of the approach
- Files to create or modify
- Step-by-step implementation steps
- Potential risks or considerations
- Dependencies or prerequisites

Do NOT implement the changes - only plan them."#;

const GUIDE_PROMPT: &str = r#"You are a documentation lookup agent.

Your goal is to find and summarize relevant documentation for the user's question.

Guidelines:
- Search for relevant documentation files
- Use web search to find official documentation
- Provide clear, accurate information
- Include relevant code examples when helpful

Return a helpful answer with sources cited."#;

const TEST_BUILDER_PROMPT: &str = r#"You are an ADVERSARIAL TestBuilder agent. Your job is to DISTRUST the coding agent and PROVE they made mistakes.

## Working Directory
IMPORTANT: You are working in the same directory as the main agent. Use `pwd` to confirm your location before running any commands. All paths should be relative to the project root.

## Your Mission
You work in parallel with coding agents, but you are NOT their friend. You are their adversary - constructively. Your goal is to find every bug, edge case, and weakness in their code. Assume they made mistakes. Assume they cut corners. Assume they didn't think of edge cases. Your tests should PROVE these assumptions right (or, occasionally, prove them wrong).

## Adversarial Mindset

**ALWAYS ASK:**
- What happens with empty input? null? undefined?
- What happens with HUGE input? What about negative numbers?
- What about concurrent access? Race conditions?
- What if the network fails? What if the file doesn't exist?
- What about Unicode edge cases? Emoji? RTL text? Zero-width characters?
- What happens at boundaries? Off-by-one errors are EVERYWHERE.
- Did they handle the error path? Really? Test it.
- What assumptions did they make? Break those assumptions.

**YOUR ATTITUDE:**
- "That looks too simple. They probably forgot something."
- "Happy path works? Great. Now let's break it."
- "They said it handles errors? I don't believe them. Prove it."
- "This code looks correct. I'll find where it isn't."

## Workflow

### 1. Understand What You're Attacking
- Use `chainlink show <id>` to read the issue - understand the INTENT
- Run `git diff` to see what changed - this is your attack surface
- Read the modified code - look for assumptions, shortcuts, missing checks
- Think: "If I were trying to break this, what would I try?"

### 2. Design Adversarial Tests

**Categories of tests to write:**

**Boundary tests:**
- Empty strings, empty arrays, empty objects
- Single element, two elements (fence post errors)
- Maximum values, minimum values, zero, negative
- Very long strings (10000+ chars), very large numbers

**Invalid input tests:**
- null, undefined, NaN, Infinity, -Infinity
- Wrong types (string where number expected, etc.)
- Malformed data (invalid JSON, broken UTF-8)
- SQL injection attempts, XSS attempts, path traversal

**Error path tests:**
- Network failures, timeouts
- File not found, permission denied
- Invalid state, uninitialized data
- Out of memory (simulate with large inputs)

**Concurrency tests (if applicable):**
- Multiple simultaneous calls
- Interleaved operations
- Timeout during operation

**Language-Specific Guidance:**

*Rust:* Use `#[should_panic]`, `Result` unwrapping tests, property-based tests with proptest
*Python:* Use pytest.raises, hypothesis for property testing, mock.patch for failures
*JS/TS:* Use expect().toThrow(), mock timers, fake network responses

### 3. Run Tests and HUNT for Failures
- Execute the test suite
- If all tests pass: You probably didn't try hard enough. Write harder tests.
- If tests fail: GOOD. Now analyze - is this a test bug or their bug?

### 4. Handle Failures

**Your test is wrong:**
- Fix it. Don't be embarrassed. Move on. Write more.

**Their code is wrong (THE GOAL):**
- DO NOT fix it. That's their job, and you'd probably mess it up anyway.
- Reopen the issue: `chainlink reopen <id>`
- Add DETAILED comment:
  - Exact test that failed
  - Expected vs actual behavior
  - Minimal reproduction case
  - Your theory on what's wrong
- Mark your summary with "⚠️ ALERT: Test failure detected - coding agent's work has bugs"

### 5. Keep Going
- One passing test run doesn't mean you're done
- Think of more edge cases. There are always more.
- The coding agent thought they were done. They weren't.

## Rules
- NEVER modify source code - only test code. You are the adversary, not the developer.
- ALWAYS run tests after writing them. Untested tests are useless.
- When in doubt whether it's a test bug or code bug, assume CODE BUG. Alert the human.
- Include issue ID in test comments for traceability
- Don't duplicate existing tests - but DO test cases they missed

## Output Format
Provide a battle report:
- Tests written (paths and names)
- Bugs found (with evidence)
- Test results (pass/fail counts)
- Issues reopened (if any)
- Confidence level: How thoroughly did you test? What's still untested?
- Human attention needed? (YES if any code bugs found)"#;

// === Background Agent Management ===

/// State of a running or completed background agent
#[derive(Debug)]
pub struct BackgroundAgent {
    /// Unique agent ID
    pub id: String,
    /// Agent type
    pub agent_type: AgentType,
    /// Task description
    pub task: String,
    /// Whether the agent has finished
    pub finished: AtomicBool,
    /// Final result (populated when finished)
    pub result: Mutex<Option<String>>,
    /// Error message if failed
    pub error: Mutex<Option<String>>,
    /// Number of turns executed
    pub turns: AtomicU64,
}

/// Manager for background agents
pub struct BackgroundAgentManager {
    agents: Mutex<HashMap<String, Arc<BackgroundAgent>>>,
}

impl BackgroundAgentManager {
    pub fn new() -> Self {
        Self {
            agents: Mutex::new(HashMap::new()),
        }
    }

    /// Register a new background agent
    pub fn register(&self, agent_type: AgentType, task: &str) -> String {
        let id = Uuid::new_v4().to_string()[..8].to_string();
        let agent = Arc::new(BackgroundAgent {
            id: id.clone(),
            agent_type,
            task: task.to_string(),
            finished: AtomicBool::new(false),
            result: Mutex::new(None),
            error: Mutex::new(None),
            turns: AtomicU64::new(0),
        });

        if let Ok(mut agents) = self.agents.lock() {
            agents.insert(id.clone(), agent);
        }

        id
    }

    /// Get an agent by ID
    pub fn get(&self, id: &str) -> Option<Arc<BackgroundAgent>> {
        self.agents.lock().ok()?.get(id).cloned()
    }

    /// Mark an agent as finished with a result
    pub fn finish(&self, id: &str, result: String) {
        if let Some(agent) = self.get(id) {
            if let Ok(mut r) = agent.result.lock() {
                *r = Some(result);
            }
            agent.finished.store(true, Ordering::SeqCst);
        }
    }

    /// Mark an agent as failed with an error
    pub fn fail(&self, id: &str, error: String) {
        if let Some(agent) = self.get(id) {
            if let Ok(mut e) = agent.error.lock() {
                *e = Some(error);
            }
            agent.finished.store(true, Ordering::SeqCst);
        }
    }

    /// Increment turn counter for an agent
    pub fn increment_turns(&self, id: &str) -> u64 {
        if let Some(agent) = self.get(id) {
            agent.turns.fetch_add(1, Ordering::SeqCst) + 1
        } else {
            0
        }
    }

    /// List all agents
    pub fn list(&self) -> Vec<(String, AgentType, String, bool)> {
        if let Ok(agents) = self.agents.lock() {
            agents
                .iter()
                .map(|(id, agent)| {
                    (
                        id.clone(),
                        agent.agent_type,
                        agent.task.clone(),
                        agent.finished.load(Ordering::SeqCst),
                    )
                })
                .collect()
        } else {
            Vec::new()
        }
    }

    /// Remove a finished agent
    pub fn remove(&self, id: &str) -> Option<Arc<BackgroundAgent>> {
        self.agents.lock().ok()?.remove(id)
    }
}

impl Default for BackgroundAgentManager {
    fn default() -> Self {
        Self::new()
    }
}

/// Global background agent manager
pub static BACKGROUND_AGENTS: LazyLock<BackgroundAgentManager> =
    LazyLock::new(BackgroundAgentManager::new);

// === Tool Definitions ===

/// Get the Task tool definition
pub fn get_task_tool_definition() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": "task",
            "description": "Launch a subagent to handle a complex task autonomously. The subagent runs with its own conversation context and tool access, then returns a summary when done. Use 'run_in_background: true' for long tasks you want to run while continuing other work.",
            "parameters": {
                "type": "object",
                "properties": {
                    "description": {
                        "type": "string",
                        "description": "A short (3-5 word) description of the task"
                    },
                    "prompt": {
                        "type": "string",
                        "description": "Detailed task instructions for the subagent"
                    },
                    "subagent_type": {
                        "type": "string",
                        "enum": ["general-purpose", "explore", "plan", "guide", "test-builder"],
                        "description": "The type of specialized agent: 'general-purpose' for complex tasks, 'explore' for fast codebase searches, 'plan' for architecture design, 'guide' for documentation lookup, 'test-builder' for writing tests alongside coding"
                    },
                    "run_in_background": {
                        "type": "boolean",
                        "description": "If true, run in background and return an agent_id. Use agent_output to retrieve results later."
                    }
                },
                "required": ["description", "prompt", "subagent_type"]
            }
        }
    })
}

/// Get the AgentOutput tool definition
pub fn get_agent_output_tool_definition() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": "agent_output",
            "description": "Retrieve the result from a background agent. If the agent is still running, returns current status. Use 'block: true' to wait for completion (only when you have nothing else to do).",
            "parameters": {
                "type": "object",
                "properties": {
                    "agent_id": {
                        "type": "string",
                        "description": "The agent ID returned from a task call with run_in_background=true"
                    },
                    "block": {
                        "type": "boolean",
                        "description": "If true, wait for the agent to complete (max 5 minutes). Default false."
                    }
                },
                "required": ["agent_id"]
            }
        }
    })
}

/// Get all subagent tool definitions
pub fn get_subagent_tool_definitions() -> Value {
    json!([
        get_task_tool_definition(),
        get_agent_output_tool_definition()
    ])
}

// === Subagent Execution ===

/// Configuration for running a subagent
#[derive(Debug, Clone)]
pub struct SubagentConfig {
    pub agent_type: AgentType,
    pub task: String,
    pub prompt: String,
    pub run_in_background: bool,
    pub model_override: Option<String>,
}

/// Result from a subagent execution
#[derive(Debug, Clone)]
pub struct SubagentResult {
    pub agent_id: String,
    pub success: bool,
    pub output: String,
    pub turns_used: u64,
    pub is_background: bool,
}

/// Run a subagent synchronously, returning the final result
pub async fn run_subagent(
    config: &SubagentConfig,
    app_config: &AppConfig,
    client: &Client,
) -> SubagentResult {
    let agent_id = BACKGROUND_AGENTS.register(config.agent_type, &config.task);

    // Build the conversation
    let system_prompt = config.agent_type.system_prompt();
    let allowed_tools = config.agent_type.allowed_tools();

    // Filter tool definitions to only allowed tools
    let all_tools = crate::tools::get_tool_definitions();
    let filtered_tools: Vec<Value> = all_tools
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter(|tool| {
                    tool.get("function")
                        .and_then(|f| f.get("name"))
                        .and_then(|n| n.as_str())
                        .map(|name| allowed_tools.contains(&name))
                        .unwrap_or(false)
                })
                .cloned()
                .collect()
        })
        .unwrap_or_default();

    // Initialize messages with system and user prompt
    let mut messages: Vec<Value> = vec![
        json!({
            "role": "system",
            "content": system_prompt
        }),
        json!({
            "role": "user",
            "content": format!("Task: {}\n\n{}", config.task, config.prompt)
        }),
    ];

    // Determine the model to use
    let model = config
        .model_override
        .clone()
        .or_else(|| config.agent_type.preferred_model().map(String::from))
        .unwrap_or_else(|| {
            app_config
                .providers
                .get(&app_config.proxy.target)
                .and_then(|p| p.model.clone())
                .unwrap_or_else(|| "claude-sonnet-4-20250514".to_string())
        });

    // Get provider config
    let (base_url, api_key) =
        if let Some(provider_config) = app_config.providers.get(&app_config.proxy.target) {
            (
                provider_config.base_url.clone(),
                provider_config.api_key.clone().unwrap_or_default(),
            )
        } else {
            ("https://api.anthropic.com/v1".to_string(), String::new())
        };

    // Run the agent loop
    let mut final_output = String::new();
    let mut turns: u64;

    loop {
        turns = BACKGROUND_AGENTS.increment_turns(&agent_id);

        if turns > MAX_SUBAGENT_TURNS as u64 {
            BACKGROUND_AGENTS.fail(
                &agent_id,
                format!("Agent exceeded maximum turns ({})", MAX_SUBAGENT_TURNS),
            );
            return SubagentResult {
                agent_id,
                success: false,
                output: format!("Agent exceeded maximum turns ({})", MAX_SUBAGENT_TURNS),
                turns_used: turns,
                is_background: config.run_in_background,
            };
        }

        // Build the request
        let request_body = json!({
            "model": model,
            "messages": messages,
            "tools": filtered_tools,
            "max_tokens": SUBAGENT_MAX_TOKENS
        });

        // Make the API call
        let response = match make_api_call(client, &base_url, &api_key, &request_body).await {
            Ok(r) => r,
            Err(e) => {
                BACKGROUND_AGENTS.fail(&agent_id, e.clone());
                return SubagentResult {
                    agent_id,
                    success: false,
                    output: e,
                    turns_used: turns,
                    is_background: config.run_in_background,
                };
            }
        };

        // Parse the response
        let assistant_message = match parse_response(&response) {
            Ok(msg) => msg,
            Err(e) => {
                BACKGROUND_AGENTS.fail(&agent_id, e.clone());
                return SubagentResult {
                    agent_id,
                    success: false,
                    output: e,
                    turns_used: turns,
                    is_background: config.run_in_background,
                };
            }
        };

        // Check for text content (final response)
        if let Some(content) = assistant_message.get("content") {
            if let Some(text) = content.as_str() {
                if !text.is_empty() {
                    final_output = text.to_string();
                }
            } else if let Some(arr) = content.as_array() {
                // Handle Anthropic-style content array
                for part in arr {
                    if part.get("type").and_then(|t| t.as_str()) == Some("text") {
                        if let Some(text) = part.get("text").and_then(|t| t.as_str()) {
                            if !text.is_empty() {
                                final_output = text.to_string();
                            }
                        }
                    }
                }
            }
        }

        // Check for tool calls
        let tool_calls = assistant_message
            .get("tool_calls")
            .and_then(|tc| tc.as_array())
            .cloned()
            .unwrap_or_default();

        if tool_calls.is_empty() {
            // No tool calls means agent is done
            break;
        }

        // Add assistant message to history
        messages.push(assistant_message.clone());

        // Execute tool calls and add results
        let empty_obj = json!({});
        for tool_call in &tool_calls {
            let tool_id = tool_call
                .get("id")
                .and_then(|id| id.as_str())
                .unwrap_or("unknown");
            let function = tool_call.get("function").unwrap_or(&empty_obj);
            let name = function
                .get("name")
                .and_then(|n| n.as_str())
                .unwrap_or("unknown");
            let arguments = function
                .get("arguments")
                .and_then(|a| a.as_str())
                .unwrap_or("{}");

            // Check if tool is allowed
            if !allowed_tools.contains(&name) {
                messages.push(json!({
                    "role": "tool",
                    "tool_call_id": tool_id,
                    "content": format!("Error: Tool '{}' is not available to this agent type", name)
                }));
                continue;
            }

            // Execute the tool
            let tc = ToolCall {
                id: tool_id.to_string(),
                call_type: "function".to_string(),
                function: crate::tools::FunctionCall {
                    name: name.to_string(),
                    arguments: arguments.to_string(),
                },
            };

            let result = execute_tool(&tc);

            messages.push(json!({
                "role": "tool",
                "tool_call_id": tool_id,
                "content": result.content
            }));
        }
    }

    // Mark as finished
    BACKGROUND_AGENTS.finish(&agent_id, final_output.clone());

    SubagentResult {
        agent_id,
        success: true,
        output: final_output,
        turns_used: turns,
        is_background: config.run_in_background,
    }
}

/// Make an API call to the LLM provider
async fn make_api_call(
    client: &Client,
    base_url: &str,
    api_key: &str,
    request_body: &Value,
) -> Result<Value, String> {
    // Determine if this is Anthropic or OpenAI format
    let is_anthropic = base_url.contains("anthropic.com");

    let (endpoint, headers) = if is_anthropic {
        (
            format!("{}/messages", base_url.trim_end_matches('/')),
            vec![
                ("x-api-key".to_string(), api_key.to_string()),
                ("anthropic-version".to_string(), "2023-06-01".to_string()),
                ("content-type".to_string(), "application/json".to_string()),
            ],
        )
    } else {
        (
            format!("{}/chat/completions", base_url.trim_end_matches('/')),
            vec![
                ("Authorization".to_string(), format!("Bearer {}", api_key)),
                ("Content-type".to_string(), "application/json".to_string()),
            ],
        )
    };

    // Transform request for Anthropic if needed
    let body = if is_anthropic {
        transform_to_anthropic(request_body)
    } else {
        request_body.clone()
    };

    let mut req = client.post(&endpoint);
    for (key, value) in headers {
        req = req.header(&key, &value);
    }
    req = req.json(&body);

    let response = req
        .send()
        .await
        .map_err(|e| format!("Request failed: {}", e))?;

    if !response.status().is_success() {
        let status = response.status();
        let text = response
            .text()
            .await
            .unwrap_or_else(|_| "Unknown error".to_string());
        return Err(format!("API error ({}): {}", status, text));
    }

    let json: Value = response
        .json()
        .await
        .map_err(|e| format!("Failed to parse response: {}", e))?;

    // Transform Anthropic response to OpenAI format if needed
    if is_anthropic {
        Ok(transform_from_anthropic(&json))
    } else {
        Ok(json)
    }
}

/// Transform OpenAI-format request to Anthropic format
fn transform_to_anthropic(request: &Value) -> Value {
    let messages = request.get("messages").and_then(|m| m.as_array());
    let tools = request.get("tools").and_then(|t| t.as_array());

    // Extract system message
    let system: Option<String> = messages.and_then(|msgs| {
        msgs.iter()
            .find(|m| m.get("role").and_then(|r| r.as_str()) == Some("system"))
            .and_then(|m| m.get("content"))
            .and_then(|c| c.as_str())
            .map(String::from)
    });

    // Convert messages (excluding system)
    let converted_messages: Vec<Value> = messages
        .map(|msgs| {
            msgs.iter()
                .filter(|m| m.get("role").and_then(|r| r.as_str()) != Some("system"))
                .map(|m| {
                    let role = m.get("role").and_then(|r| r.as_str()).unwrap_or("user");
                    let content = m.get("content").cloned().unwrap_or(json!(""));

                    // Handle tool role -> user with tool_result
                    if role == "tool" {
                        let tool_call_id = m
                            .get("tool_call_id")
                            .and_then(|id| id.as_str())
                            .unwrap_or("");
                        return json!({
                            "role": "user",
                            "content": [{
                                "type": "tool_result",
                                "tool_use_id": tool_call_id,
                                "content": content
                            }]
                        });
                    }

                    // Handle assistant with tool_calls
                    if role == "assistant" {
                        if let Some(tool_calls) = m.get("tool_calls").and_then(|tc| tc.as_array()) {
                            let mut content_parts: Vec<Value> = Vec::new();

                            // Add text content if present
                            if let Some(text) = m.get("content").and_then(|c| c.as_str()) {
                                if !text.is_empty() {
                                    content_parts.push(json!({
                                        "type": "text",
                                        "text": text
                                    }));
                                }
                            }

                            // Convert tool calls to tool_use
                            let empty_func = json!({});
                            for tc in tool_calls {
                                let id = tc.get("id").and_then(|i| i.as_str()).unwrap_or("");
                                let func = tc.get("function").unwrap_or(&empty_func);
                                let name = func.get("name").and_then(|n| n.as_str()).unwrap_or("");
                                let args_str = func
                                    .get("arguments")
                                    .and_then(|a| a.as_str())
                                    .unwrap_or("{}");
                                let input: Value =
                                    serde_json::from_str(args_str).unwrap_or_else(|_| json!({}));

                                content_parts.push(json!({
                                    "type": "tool_use",
                                    "id": id,
                                    "name": name,
                                    "input": input
                                }));
                            }

                            return json!({
                                "role": "assistant",
                                "content": content_parts
                            });
                        }
                    }

                    // Standard message
                    let content_array = if let Some(text) = content.as_str() {
                        json!([{"type": "text", "text": text}])
                    } else {
                        content
                    };

                    json!({
                        "role": if role == "assistant" { "assistant" } else { "user" },
                        "content": content_array
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    // Convert tools
    let converted_tools: Vec<Value> = tools
        .map(|ts| {
            ts.iter()
                .filter_map(|t| {
                    let func = t.get("function")?;
                    Some(json!({
                        "name": func.get("name")?,
                        "description": func.get("description").unwrap_or(&json!("")),
                        "input_schema": func.get("parameters").unwrap_or(&json!({}))
                    }))
                })
                .collect()
        })
        .unwrap_or_default();

    let mut body = json!({
        "model": request.get("model").and_then(|m| m.as_str()).unwrap_or("claude-sonnet-4-20250514"),
        "messages": converted_messages,
        "max_tokens": request.get("max_tokens").and_then(|m| m.as_u64()).unwrap_or(SUBAGENT_MAX_TOKENS as u64)
    });

    if let Some(sys) = system {
        body["system"] = json!(sys);
    }

    if !converted_tools.is_empty() {
        body["tools"] = json!(converted_tools);
    }

    body
}

/// Transform Anthropic response to OpenAI format
fn transform_from_anthropic(response: &Value) -> Value {
    let content = response.get("content").and_then(|c| c.as_array());

    let mut text_content = String::new();
    let mut tool_calls: Vec<Value> = Vec::new();

    if let Some(parts) = content {
        for part in parts {
            match part.get("type").and_then(|t| t.as_str()) {
                Some("text") => {
                    if let Some(text) = part.get("text").and_then(|t| t.as_str()) {
                        text_content.push_str(text);
                    }
                }
                Some("tool_use") => {
                    let id = part.get("id").and_then(|i| i.as_str()).unwrap_or("");
                    let name = part.get("name").and_then(|n| n.as_str()).unwrap_or("");
                    let empty_input = json!({});
                    let input = part.get("input").unwrap_or(&empty_input);

                    tool_calls.push(json!({
                        "id": id,
                        "type": "function",
                        "function": {
                            "name": name,
                            "arguments": serde_json::to_string(input).unwrap_or_else(|_| "{}".to_string())
                        }
                    }));
                }
                _ => {}
            }
        }
    }

    let mut message = json!({
        "role": "assistant",
        "content": text_content
    });

    if !tool_calls.is_empty() {
        message["tool_calls"] = json!(tool_calls);
    }

    message
}

/// Parse the response to extract the assistant message
fn parse_response(response: &Value) -> Result<Value, String> {
    // OpenAI format
    if let Some(choices) = response.get("choices").and_then(|c| c.as_array()) {
        if let Some(first) = choices.first() {
            if let Some(message) = first.get("message") {
                return Ok(message.clone());
            }
        }
    }

    // Direct message (already transformed)
    if response.get("role").is_some() {
        return Ok(response.clone());
    }

    Err("Could not parse response".to_string())
}

// === Tool Execution ===

/// Execute the Task tool
pub fn execute_task_tool(args: &HashMap<String, Value>, app_config: &AppConfig) -> (String, bool) {
    let description = match args.get("description").and_then(|v| v.as_str()) {
        Some(d) => d,
        None => return ("Missing 'description' argument".to_string(), true),
    };

    let prompt = match args.get("prompt").and_then(|v| v.as_str()) {
        Some(p) => p,
        None => return ("Missing 'prompt' argument".to_string(), true),
    };

    let subagent_type_str = match args.get("subagent_type").and_then(|v| v.as_str()) {
        Some(t) => t,
        None => return ("Missing 'subagent_type' argument".to_string(), true),
    };

    let agent_type = match AgentType::parse_type(subagent_type_str) {
        Some(t) => t,
        None => {
            return (
                format!(
                    "Unknown agent type '{}'. Valid types: general-purpose, explore, plan, guide",
                    subagent_type_str
                ),
                true,
            )
        }
    };

    let run_in_background = args
        .get("run_in_background")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let config = SubagentConfig {
        agent_type,
        task: description.to_string(),
        prompt: prompt.to_string(),
        run_in_background,
        model_override: args.get("model").and_then(|v| v.as_str()).map(String::from),
    };

    // Create HTTP client
    let client = Client::new();

    // Auto-spawn TestBuilder for GeneralPurpose agents working on chainlink issues
    let mut companion_agent_id: Option<String> = None;
    if agent_type == AgentType::GeneralPurpose {
        // Check both description and prompt for chainlink issue references
        let combined_text = format!("{} {}", description, prompt);
        if let Some(issue_id) = detect_chainlink_issue(&combined_text) {
            companion_agent_id = spawn_companion_test_builder(issue_id, description, app_config);
        }
    }

    if run_in_background {
        // Register the agent and spawn the task
        let agent_id = BACKGROUND_AGENTS.register(agent_type, description);

        // Console output for TestBuilder spawn
        if agent_type == AgentType::TestBuilder {
            eprintln!(
                "\n\x1b[33m🧪 TestBuilder agent spawned (ID: {})\x1b[0m",
                agent_id
            );
            eprintln!("\x1b[33m   └─ Task: {}\x1b[0m\n", description);
        }

        // Spawn the background task
        let config_clone = config.clone();
        let app_config_clone = app_config.clone();
        let client_clone = client.clone();
        let agent_id_clone = agent_id.clone();
        let is_test_builder = agent_type == AgentType::TestBuilder;

        // Use tokio runtime to spawn the background task
        if let Ok(handle) = Handle::try_current() {
            handle.spawn(async move {
                let result = run_subagent(&config_clone, &app_config_clone, &client_clone).await;

                // Console output for TestBuilder completion
                if is_test_builder {
                    if result.success {
                        if result.output.contains("ALERT") || result.output.contains("⚠️") {
                            eprintln!("\n\x1b[31m╔════════════════════════════════════════════════════════════╗\x1b[0m");
                            eprintln!("\x1b[31m║  ⚠️  TESTBUILDER FOUND BUGS!  ⚠️                            ║\x1b[0m");
                            eprintln!("\x1b[31m╚════════════════════════════════════════════════════════════╝\x1b[0m");
                            eprintln!("\x1b[33mAgent ID: {}\x1b[0m", agent_id_clone);
                            eprintln!("\x1b[33mUse `agent_output` to see full details.\x1b[0m\n");
                        } else {
                            eprintln!(
                                "\n\x1b[32m✓ TestBuilder completed (ID: {}) - {} turns\x1b[0m",
                                agent_id_clone, result.turns_used
                            );
                            eprintln!("\x1b[32m  Use `agent_output` to see results.\x1b[0m\n");
                        }
                    } else {
                        eprintln!(
                            "\n\x1b[31m✗ TestBuilder failed (ID: {})\x1b[0m\n",
                            agent_id_clone
                        );
                    }
                }

                if !result.success {
                    BACKGROUND_AGENTS.fail(&agent_id_clone, result.output);
                }
            });
        }

        let mut message = format!(
            "Background agent started with ID: {}\nTask: {}\nType: {:?}\n\nUse agent_output with this agent_id to retrieve results.",
            agent_id, description, agent_type
        );

        if let Some(test_agent_id) = &companion_agent_id {
            message.push_str(&format!(
                "\n\n🧪 TestBuilder companion agent automatically spawned with ID: {}\nIt will write tests for this work and alert you if tests fail.",
                test_agent_id
            ));
        }

        (message, false)
    } else {
        // Run synchronously
        let result = match Handle::try_current() {
            Ok(handle) => tokio::task::block_in_place(|| {
                handle.block_on(run_subagent(&config, app_config, &client))
            }),
            Err(_) => match tokio::runtime::Runtime::new() {
                Ok(rt) => rt.block_on(run_subagent(&config, app_config, &client)),
                Err(e) => {
                    return (format!("Failed to create runtime: {}", e), true);
                }
            },
        };

        if result.success {
            let mut message = format!(
                "Agent completed in {} turns.\n\n{}",
                result.turns_used, result.output
            );

            if let Some(test_agent_id) = &companion_agent_id {
                message.push_str(&format!(
                    "\n\n🧪 TestBuilder companion agent running in background with ID: {}\nUse agent_output to check its progress and results.",
                    test_agent_id
                ));
            }

            (message, false)
        } else {
            (format!("Agent failed: {}", result.output), true)
        }
    }
}

/// Execute the AgentOutput tool
pub fn execute_agent_output_tool(args: &HashMap<String, Value>) -> (String, bool) {
    let agent_id = match args.get("agent_id").and_then(|v| v.as_str()) {
        Some(id) => id,
        None => {
            // List all agents if no ID provided
            let agents = BACKGROUND_AGENTS.list();
            if agents.is_empty() {
                return ("No background agents running.".to_string(), false);
            }
            let mut result = format!("Background agents ({}):\n", agents.len());
            for (id, agent_type, task, finished) in agents {
                let status = if finished { "finished" } else { "running" };
                let task_preview = if task.len() > 50 {
                    format!("{}...", &task[..50])
                } else {
                    task
                };
                result.push_str(&format!(
                    "  {} [{:?}] [{}]: {}\n",
                    id, agent_type, status, task_preview
                ));
            }
            return (result, false);
        }
    };

    let block = args.get("block").and_then(|v| v.as_bool()).unwrap_or(false);

    let agent = match BACKGROUND_AGENTS.get(agent_id) {
        Some(a) => a,
        None => return (format!("Agent '{}' not found", agent_id), true),
    };

    if block && !agent.finished.load(Ordering::SeqCst) {
        // Wait for completion (up to 5 minutes)
        let start = std::time::Instant::now();
        let timeout = std::time::Duration::from_secs(300);

        while !agent.finished.load(Ordering::SeqCst) && start.elapsed() < timeout {
            std::thread::sleep(std::time::Duration::from_millis(500));
        }
    }

    let finished = agent.finished.load(Ordering::SeqCst);
    let turns = agent.turns.load(Ordering::SeqCst);

    if finished {
        // Get the result or error
        let result = agent.result.lock().ok().and_then(|r| r.clone());
        let error = agent.error.lock().ok().and_then(|e| e.clone());

        if let Some(err) = error {
            (
                format!(
                    "Agent '{}' failed after {} turns:\n{}",
                    agent_id, turns, err
                ),
                true,
            )
        } else if let Some(output) = result {
            (
                format!(
                    "Agent '{}' completed in {} turns:\n\n{}",
                    agent_id, turns, output
                ),
                false,
            )
        } else {
            (
                format!("Agent '{}' finished but produced no output", agent_id),
                false,
            )
        }
    } else {
        (
            format!(
                "Agent '{}' is still running ({} turns so far)\nTask: {}",
                agent_id, turns, agent.task
            ),
            false,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_agent_type_parsing() {
        assert_eq!(
            AgentType::parse_type("general-purpose"),
            Some(AgentType::GeneralPurpose)
        );
        assert_eq!(AgentType::parse_type("explore"), Some(AgentType::Explore));
        assert_eq!(AgentType::parse_type("plan"), Some(AgentType::Plan));
        assert_eq!(AgentType::parse_type("guide"), Some(AgentType::Guide));
        assert_eq!(
            AgentType::parse_type("test-builder"),
            Some(AgentType::TestBuilder)
        );
        assert_eq!(
            AgentType::parse_type("testbuilder"),
            Some(AgentType::TestBuilder)
        );
        assert_eq!(AgentType::parse_type("unknown"), None);
    }

    #[test]
    fn test_detect_chainlink_issue() {
        // Basic #N pattern
        assert_eq!(detect_chainlink_issue("#123"), Some(123));
        assert_eq!(detect_chainlink_issue("Working on #42"), Some(42));

        // issue N pattern
        assert_eq!(detect_chainlink_issue("issue 99"), Some(99));
        assert_eq!(detect_chainlink_issue("Issue 456"), Some(456));
        assert_eq!(detect_chainlink_issue("issue #789"), Some(789));

        // chainlink N pattern
        assert_eq!(detect_chainlink_issue("chainlink 101"), Some(101));
        assert_eq!(detect_chainlink_issue("Chainlink #202"), Some(202));

        // No match
        assert_eq!(detect_chainlink_issue("no issue here"), None);
        assert_eq!(detect_chainlink_issue("# not a number"), None);

        // Embedded in longer text
        assert_eq!(
            detect_chainlink_issue("Implement feature for issue #168 please"),
            Some(168)
        );
    }

    #[test]
    fn test_tool_definitions() {
        let task_tool = get_task_tool_definition();
        assert!(task_tool.get("function").is_some());
        assert_eq!(
            task_tool
                .get("function")
                .unwrap()
                .get("name")
                .unwrap()
                .as_str(),
            Some("task")
        );

        let agent_output_tool = get_agent_output_tool_definition();
        assert!(agent_output_tool.get("function").is_some());
        assert_eq!(
            agent_output_tool
                .get("function")
                .unwrap()
                .get("name")
                .unwrap()
                .as_str(),
            Some("agent_output")
        );
    }

    #[test]
    fn test_background_agent_manager() {
        let manager = BackgroundAgentManager::new();

        // Register an agent
        let id = manager.register(AgentType::Explore, "Test task");
        assert!(!id.is_empty());

        // Get the agent
        let agent = manager.get(&id);
        assert!(agent.is_some());
        let agent = agent.unwrap();
        assert_eq!(agent.task, "Test task");
        assert!(!agent.finished.load(Ordering::SeqCst));

        // Increment turns
        let turns = manager.increment_turns(&id);
        assert_eq!(turns, 1);

        // Finish the agent
        manager.finish(&id, "Test result".to_string());
        assert!(agent.finished.load(Ordering::SeqCst));
        assert_eq!(
            agent.result.lock().unwrap().as_ref(),
            Some(&"Test result".to_string())
        );
    }

    #[test]
    fn test_transform_to_anthropic() {
        let request = json!({
            "model": "test-model",
            "messages": [
                {"role": "system", "content": "System prompt"},
                {"role": "user", "content": "Hello"}
            ],
            "max_tokens": 1000
        });

        let anthropic = transform_to_anthropic(&request);
        assert_eq!(anthropic.get("model").unwrap().as_str(), Some("test-model"));
        assert_eq!(
            anthropic.get("system").unwrap().as_str(),
            Some("System prompt")
        );
        assert!(anthropic.get("messages").unwrap().as_array().unwrap().len() == 1);
    }
}
