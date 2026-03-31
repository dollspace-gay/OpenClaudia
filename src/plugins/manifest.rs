//! Plugin manifest types and Claude Code-compatible structures.
//!
//! Contains all types related to `.claude-plugin/plugin.json` parsing,
//! including commands, hooks, MCP servers, agents, and skills specs.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// ---------------------------------------------------------------------------
// Claude Code-compatible plugin manifest (.claude-plugin/plugin.json)
// ---------------------------------------------------------------------------

/// Plugin author information
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PluginAuthor {
    /// Display name of the plugin author or organization
    pub name: String,
    /// Contact email
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    /// Website or GitHub profile URL
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

/// Command metadata in the manifest (object form)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandMetadata {
    /// Path to command markdown file, relative to plugin root
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// Inline markdown content for the command
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    /// Command description override
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Hint for command arguments (e.g., "[file]")
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[serde(rename = "argumentHint")]
    pub argument_hint: Option<String>,
    /// Default model for this command
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Tools allowed when command runs
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[serde(rename = "allowedTools")]
    pub allowed_tools: Option<Vec<String>>,
}

/// Commands field in manifest - can be a path string, array of paths, or object map
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum CommandsSpec {
    /// Single path to command file or directory
    Path(String),
    /// Array of paths to command files or directories
    Paths(Vec<String>),
    /// Object mapping of command names to their metadata
    Map(HashMap<String, CommandMetadata>),
}

/// Hooks field in manifest - can be a path string, inline object, or array
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum HooksSpec {
    /// Path to hooks JSON file relative to plugin root
    Path(String),
    /// Inline hooks object (same format as settings hooks)
    Inline(HooksDefinition),
    /// Array of paths or inline hooks
    Array(Vec<HooksSpecEntry>),
}

/// Single entry in a hooks array
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum HooksSpecEntry {
    /// Path to hooks JSON file
    Path(String),
    /// Inline hooks definition
    Inline(HooksDefinition),
}

/// Hooks definition matching Claude Code's hooks format
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct HooksDefinition {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Pre-tool-use hooks
    #[serde(default, rename = "PreToolUse", skip_serializing_if = "Vec::is_empty")]
    pub pre_tool_use: Vec<HookEntry>,
    /// Post-tool-use hooks
    #[serde(default, rename = "PostToolUse", skip_serializing_if = "Vec::is_empty")]
    pub post_tool_use: Vec<HookEntry>,
    /// Notification hooks
    #[serde(
        default,
        rename = "Notification",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub notification: Vec<HookEntry>,
    /// Stop hooks
    #[serde(default, rename = "Stop", skip_serializing_if = "Vec::is_empty")]
    pub stop: Vec<HookEntry>,
    /// Prompt submit hooks
    #[serde(
        default,
        rename = "UserPromptSubmit",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub user_prompt_submit: Vec<HookEntry>,
    /// Session start hooks
    #[serde(
        default,
        rename = "SessionStart",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub session_start: Vec<HookEntry>,
}

/// A single hook entry
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HookEntry {
    /// Matcher pattern (tool name regex for PreToolUse/PostToolUse)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub matcher: Option<String>,
    /// Hook type
    #[serde(rename = "type")]
    pub hook_type: String,
    /// Command to execute (for "command" type)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    /// Timeout in seconds
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout: Option<u64>,
}

/// MCP server configurations - can be a path, object map, or array
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum McpServersSpec {
    /// Path to MCP servers JSON configuration file
    Path(String),
    /// MCP server configurations keyed by server name
    Map(HashMap<String, McpServerConfig>),
    /// Array of configurations
    Array(Vec<McpServersSpecEntry>),
}

/// Single entry in MCP servers array
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum McpServersSpecEntry {
    /// Path to configuration file
    Path(String),
    /// Inline MCP server configurations
    Map(HashMap<String, McpServerConfig>),
}

/// MCP server configuration (matches Claude Code format)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerConfig {
    /// Command to execute
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    /// Command arguments
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    /// Environment variables
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub env: HashMap<String, String>,
    /// Transport type (stdio or http)
    #[serde(default = "default_transport")]
    pub transport: String,
    /// URL for HTTP transport
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

fn default_transport() -> String {
    "stdio".to_string()
}

/// Agents field - can be path string or array of paths
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum AgentsSpec {
    /// Single path to agent markdown file
    Path(String),
    /// Array of paths to agent markdown files
    Paths(Vec<String>),
}

/// Skills field - can be path string or array of paths
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum SkillsSpec {
    /// Single path to skill directory
    Path(String),
    /// Array of paths to skill directories
    Paths(Vec<String>),
}

/// Claude Code-compatible plugin manifest (.claude-plugin/plugin.json)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginManifest {
    /// Plugin name (kebab-case, unique identifier)
    pub name: String,
    /// Semantic version
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// Brief description of what the plugin provides
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Plugin author information
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<PluginAuthor>,
    /// Plugin homepage URL
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub homepage: Option<String>,
    /// Source code repository URL
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository: Option<String>,
    /// SPDX license identifier
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub license: Option<String>,
    /// Tags for discovery and categorization
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keywords: Option<Vec<String>>,
    /// Hook definitions
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hooks: Option<HooksSpec>,
    /// Command definitions
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commands: Option<CommandsSpec>,
    /// Agent definitions
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agents: Option<AgentsSpec>,
    /// Skill definitions
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skills: Option<SkillsSpec>,
    /// MCP server configurations
    #[serde(
        default,
        rename = "mcpServers",
        skip_serializing_if = "Option::is_none"
    )]
    pub mcp_servers: Option<McpServersSpec>,
}
