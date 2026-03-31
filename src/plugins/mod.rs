//! Plugin System - Claude Code-compatible plugin loading and management.
//!
//! Supports the Claude Code plugin format:
//! - `.claude-plugin/plugin.json` manifest
//! - `commands/` directory for slash commands (markdown files)
//! - `hooks/hooks.json` for lifecycle hooks
//! - `.mcp.json` for MCP server configurations
//! - `agents/` directory for agent definitions
//! - `skills/` directory for skill definitions
//!
//! Also supports legacy OpenClaudia `manifest.json` format for backward compatibility.
//!
//! Plugin ID format: `plugin-name@marketplace-name`
//!
//! Storage:
//! - `~/.openclaudia/plugins/` (user plugins)
//! - `.openclaudia/plugins/` (project plugins)
//! - Tracked in `~/.openclaudia/plugins/installed_plugins.json`

pub mod git;
pub mod install;
pub mod manager;
pub mod manifest;
pub mod marketplace;

// Re-export all public types for backward compatibility
pub use git::copy_dir_recursive;
pub use install::{InstallScope, InstalledPlugins, PluginInstallEntry};
pub use manager::PluginManager;
pub use manifest::{
    AgentsSpec, CommandMetadata, CommandsSpec, HookEntry, HooksDefinition, HooksSpec,
    HooksSpecEntry, McpServerConfig, McpServersSpec, McpServersSpecEntry, PluginAuthor,
    PluginManifest, SkillsSpec,
};
pub use marketplace::{
    MarketplaceManifest, MarketplaceMetadata, MarketplacePlugin, MarketplaceSource, PluginSource,
    PluginSourceDef,
};

use serde::Deserialize;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use tracing::{debug, warn};

// ---------------------------------------------------------------------------
// Plugin errors
// ---------------------------------------------------------------------------

/// Errors that can occur during plugin operations
#[derive(Debug, thiserror::Error)]
pub enum PluginError {
    #[error("Manifest not found: {0}")]
    ManifestNotFound(PathBuf),

    #[error("Invalid manifest: {0}")]
    InvalidManifest(String),

    #[error("IO error: {0}")]
    IoError(String),

    #[error("Plugin not found: {0}")]
    NotFound(String),

    #[error("Installation error: {0}")]
    InstallError(String),

    #[error("Marketplace error: {0}")]
    MarketplaceError(String),
}

// ---------------------------------------------------------------------------
// Resolved plugin types (for backward-compatible API)
// ---------------------------------------------------------------------------

/// A resolved hook from a plugin, ready for the hook engine
pub struct PluginHook {
    /// Hook event type (PreToolUse, PostToolUse, SessionStart, etc.)
    pub event: String,
    /// Matcher pattern for the hook
    pub matcher: Option<String>,
    /// Hook type (command or prompt)
    pub hook_type: String,
    /// Command to run (for command hooks)
    pub command: Option<String>,
    /// Prompt to inject (for prompt hooks)
    pub prompt: Option<String>,
    /// Timeout in seconds
    pub timeout: u64,
}

/// A resolved command from a plugin
pub struct PluginCommand {
    /// Command name (used as /plugin-name:command)
    pub name: String,
    /// Command description
    pub description: Option<String>,
    /// Markdown content (loaded from file, with front matter stripped)
    pub content: String,
    /// Allowed tools when running this command
    pub allowed_tools: Option<Vec<String>>,
    /// Argument hint (e.g., "<required-arg> [optional-arg]")
    pub argument_hint: Option<String>,
    /// Model override for this command
    pub model: Option<String>,
}

/// Parsed YAML front matter from a command markdown file
struct CommandFrontMatter {
    description: Option<String>,
    allowed_tools: Option<Vec<String>>,
    argument_hint: Option<String>,
    model: Option<String>,
    /// Content after front matter
    body: String,
}

/// Parse YAML front matter from a markdown command file.
/// Front matter is delimited by `---` on its own line at the start.
fn parse_command_front_matter(content: &str) -> CommandFrontMatter {
    let trimmed = content.trim_start();
    if !trimmed.starts_with("---") {
        return CommandFrontMatter {
            description: None,
            allowed_tools: None,
            argument_hint: None,
            model: None,
            body: content.to_string(),
        };
    }

    // Find the closing ---
    let after_first = &trimmed[3..].trim_start_matches(['\r', '\n']);
    if let Some(end_pos) = after_first.find("\n---") {
        let yaml_block = &after_first[..end_pos];
        let body_start = end_pos + 4; // skip \n---
        let body = after_first[body_start..]
            .trim_start_matches(['\r', '\n'])
            .to_string();

        let mut description = None;
        let mut allowed_tools = None;
        let mut argument_hint = None;
        let mut model = None;

        for line in yaml_block.lines() {
            let line = line.trim();
            if let Some(val) = line.strip_prefix("description:") {
                description = Some(val.trim().to_string());
            } else if let Some(val) = line.strip_prefix("allowed-tools:") {
                let val = val.trim();
                // Parse as YAML array [Tool1, Tool2] or comma-separated
                if val.starts_with('[') && val.ends_with(']') {
                    let inner = &val[1..val.len() - 1];
                    allowed_tools = Some(
                        inner
                            .split(',')
                            .map(|s| s.trim().to_string())
                            .filter(|s| !s.is_empty())
                            .collect(),
                    );
                } else {
                    // Comma-separated without brackets
                    allowed_tools = Some(
                        val.split(',')
                            .map(|s| s.trim().to_string())
                            .filter(|s| !s.is_empty())
                            .collect(),
                    );
                }
            } else if let Some(val) = line.strip_prefix("argument-hint:") {
                argument_hint = Some(val.trim().to_string());
            } else if let Some(val) = line.strip_prefix("model:") {
                model = Some(val.trim().trim_matches('"').to_string());
            }
        }

        CommandFrontMatter {
            description,
            allowed_tools,
            argument_hint,
            model,
            body,
        }
    } else {
        // No closing ---, treat entire content as body
        CommandFrontMatter {
            description: None,
            allowed_tools: None,
            argument_hint: None,
            model: None,
            body: content.to_string(),
        }
    }
}

/// A resolved MCP server from a plugin
pub struct PluginMcpServer {
    /// Server name
    pub name: String,
    /// Transport type (stdio or http)
    pub transport: String,
    /// Command to run (for stdio)
    pub command: Option<String>,
    /// Arguments for the command
    pub args: Vec<String>,
    /// URL (for http)
    pub url: Option<String>,
    /// Environment variables
    pub env: HashMap<String, String>,
}

// ---------------------------------------------------------------------------
// Plugin loading
// ---------------------------------------------------------------------------

/// A loaded plugin
#[derive(Debug, Clone)]
pub struct Plugin {
    /// Plugin manifest
    pub manifest: PluginManifest,
    /// Path to the plugin directory
    pub path: PathBuf,
    /// Whether the plugin is enabled
    pub enabled: bool,
    /// Plugin ID (plugin@marketplace or just plugin name for local)
    pub id: String,
    /// Source identifier (marketplace name or "local")
    pub source: String,
    /// Resolved command paths
    pub command_paths: Vec<PathBuf>,
    /// Resolved command metadata (from manifest object form)
    pub command_metadata: HashMap<String, CommandMetadata>,
    /// Resolved hook definitions
    pub hook_definitions: Vec<HooksDefinition>,
    /// Resolved MCP server configs
    pub mcp_configs: HashMap<String, McpServerConfig>,
    /// Resolved agent paths
    pub agent_paths: Vec<PathBuf>,
    /// Resolved skill paths
    pub skill_paths: Vec<PathBuf>,
}

impl Plugin {
    /// Load a plugin from a directory using Claude Code format (.claude-plugin/plugin.json)
    pub fn load(path: &Path) -> Result<Self, PluginError> {
        // Try Claude Code format first: .claude-plugin/plugin.json
        let cc_manifest_path = path.join(".claude-plugin").join("plugin.json");
        // Also try plugin.json at root (legacy Claude Code location)
        let root_plugin_json = path.join("plugin.json");
        // Legacy OpenClaudia format
        let legacy_manifest_path = path.join("manifest.json");

        let manifest: PluginManifest = if cc_manifest_path.exists() {
            debug!(path = ?cc_manifest_path, "Loading Claude Code plugin manifest");
            let content = fs::read_to_string(&cc_manifest_path)
                .map_err(|e| PluginError::IoError(e.to_string()))?;
            serde_json::from_str(&content).map_err(|e| {
                PluginError::InvalidManifest(format!("{}: {}", cc_manifest_path.display(), e))
            })?
        } else if root_plugin_json.exists() {
            debug!(path = ?root_plugin_json, "Loading plugin.json from root");
            let content = fs::read_to_string(&root_plugin_json)
                .map_err(|e| PluginError::IoError(e.to_string()))?;
            serde_json::from_str(&content).map_err(|e| {
                PluginError::InvalidManifest(format!("{}: {}", root_plugin_json.display(), e))
            })?
        } else if legacy_manifest_path.exists() {
            debug!(path = ?legacy_manifest_path, "Loading legacy manifest.json");
            Self::load_legacy_manifest(&legacy_manifest_path)?
        } else {
            return Err(PluginError::ManifestNotFound(path.to_path_buf()));
        };

        Self::validate_manifest(&manifest)?;

        let mut plugin = Self {
            id: manifest.name.clone(),
            source: "local".to_string(),
            manifest,
            path: path.to_path_buf(),
            enabled: true,
            command_paths: Vec::new(),
            command_metadata: HashMap::new(),
            hook_definitions: Vec::new(),
            mcp_configs: HashMap::new(),
            agent_paths: Vec::new(),
            skill_paths: Vec::new(),
        };

        // Resolve all components
        plugin.resolve_commands();
        plugin.resolve_hooks();
        plugin.resolve_mcp_servers();
        plugin.resolve_agents();
        plugin.resolve_skills();

        Ok(plugin)
    }

    /// Load a legacy OpenClaudia manifest.json and convert to PluginManifest
    fn load_legacy_manifest(path: &Path) -> Result<PluginManifest, PluginError> {
        let content = fs::read_to_string(path).map_err(|e| PluginError::IoError(e.to_string()))?;
        let legacy: serde_json::Value = serde_json::from_str(&content)
            .map_err(|e| PluginError::InvalidManifest(e.to_string()))?;

        let name = legacy["name"].as_str().unwrap_or("unknown").to_string();
        let version = legacy["version"].as_str().map(String::from);
        let description = legacy["description"].as_str().map(String::from);

        // Convert legacy MCP servers to new format
        let mcp_servers = if let Some(servers) = legacy["mcp_servers"].as_array() {
            let mut map = HashMap::new();
            for server in servers {
                let server_name = server["name"].as_str().unwrap_or("unknown").to_string();
                let transport = server["transport"].as_str().unwrap_or("stdio").to_string();
                map.insert(
                    server_name,
                    McpServerConfig {
                        command: server["command"].as_str().map(String::from),
                        args: server["args"]
                            .as_array()
                            .map(|a| {
                                a.iter()
                                    .filter_map(|v| v.as_str().map(String::from))
                                    .collect()
                            })
                            .unwrap_or_default(),
                        env: HashMap::new(),
                        transport,
                        url: server["url"].as_str().map(String::from),
                    },
                );
            }
            if map.is_empty() {
                None
            } else {
                Some(McpServersSpec::Map(map))
            }
        } else {
            None
        };

        Ok(PluginManifest {
            name,
            version,
            description,
            author: legacy["author"].as_str().map(|a| PluginAuthor {
                name: a.to_string(),
                ..Default::default()
            }),
            homepage: None,
            repository: None,
            license: None,
            keywords: None,
            hooks: None,    // Legacy hooks handled differently
            commands: None, // Legacy commands handled differently
            agents: None,
            skills: None,
            mcp_servers,
        })
    }

    /// Validate the plugin manifest
    fn validate_manifest(manifest: &PluginManifest) -> Result<(), PluginError> {
        if manifest.name.is_empty() {
            return Err(PluginError::InvalidManifest(
                "Plugin name cannot be empty".to_string(),
            ));
        }
        if manifest.name.contains(' ') {
            return Err(PluginError::InvalidManifest(
                "Plugin name cannot contain spaces. Use kebab-case (e.g., \"my-plugin\")"
                    .to_string(),
            ));
        }
        Ok(())
    }

    /// Resolve command paths and metadata from manifest + convention
    fn resolve_commands(&mut self) {
        // Convention: commands/ directory
        let commands_dir = self.path.join("commands");
        if commands_dir.exists() && self.manifest.commands.is_none() {
            if let Ok(entries) = fs::read_dir(&commands_dir) {
                for entry in entries.flatten() {
                    let p = entry.path();
                    if p.extension().is_some_and(|e| e == "md") {
                        self.command_paths.push(p);
                    }
                }
            }
        }

        // Manifest-specified commands
        if let Some(ref commands) = self.manifest.commands {
            match commands {
                CommandsSpec::Path(p) => {
                    let resolved = self.path.join(p);
                    if resolved.exists() {
                        if resolved.is_dir() {
                            if let Ok(entries) = fs::read_dir(&resolved) {
                                for entry in entries.flatten() {
                                    let ep = entry.path();
                                    if ep.extension().is_some_and(|e| e == "md") {
                                        self.command_paths.push(ep);
                                    }
                                }
                            }
                        } else {
                            self.command_paths.push(resolved);
                        }
                    } else {
                        warn!(path = %p, plugin = %self.manifest.name, "Command path not found");
                    }
                }
                CommandsSpec::Paths(paths) => {
                    for p in paths {
                        let resolved = self.path.join(p);
                        if resolved.exists() {
                            self.command_paths.push(resolved);
                        } else {
                            warn!(path = %p, plugin = %self.manifest.name, "Command path not found");
                        }
                    }
                }
                CommandsSpec::Map(map) => {
                    for (name, meta) in map {
                        if let Some(ref source) = meta.source {
                            let resolved = self.path.join(source);
                            if resolved.exists() {
                                self.command_paths.push(resolved);
                            } else {
                                warn!(path = %source, command = %name, plugin = %self.manifest.name, "Command source not found");
                            }
                        }
                        self.command_metadata.insert(name.clone(), meta.clone());
                    }
                }
            }
        }
    }

    /// Resolve hooks from manifest + convention
    fn resolve_hooks(&mut self) {
        // Convention: hooks/hooks.json
        let hooks_json = self.path.join("hooks").join("hooks.json");
        if hooks_json.exists() {
            match Self::load_hooks_file(&hooks_json) {
                Ok(def) => self.hook_definitions.push(def),
                Err(e) => warn!(
                    path = ?hooks_json,
                    error = %e,
                    plugin = %self.manifest.name,
                    "Failed to load hooks"
                ),
            }
        }

        // Manifest-specified hooks
        if let Some(ref hooks_spec) = self.manifest.hooks {
            match hooks_spec {
                HooksSpec::Path(p) => {
                    let resolved = self.path.join(p);
                    if resolved.exists() {
                        match Self::load_hooks_file(&resolved) {
                            Ok(def) => self.hook_definitions.push(def),
                            Err(e) => warn!(error = %e, "Failed to load hooks from {}", p),
                        }
                    }
                }
                HooksSpec::Inline(def) => {
                    self.hook_definitions.push(def.clone());
                }
                HooksSpec::Array(entries) => {
                    for entry in entries {
                        match entry {
                            HooksSpecEntry::Path(p) => {
                                let resolved = self.path.join(p);
                                if resolved.exists() {
                                    match Self::load_hooks_file(&resolved) {
                                        Ok(def) => self.hook_definitions.push(def),
                                        Err(e) => {
                                            warn!(error = %e, "Failed to load hooks from {}", p)
                                        }
                                    }
                                }
                            }
                            HooksSpecEntry::Inline(def) => {
                                self.hook_definitions.push(def.clone());
                            }
                        }
                    }
                }
            }
        }
    }

    /// Load a hooks JSON file
    fn load_hooks_file(path: &Path) -> Result<HooksDefinition, PluginError> {
        let content = fs::read_to_string(path).map_err(|e| PluginError::IoError(e.to_string()))?;
        // Try parsing as HooksDefinition directly, or as a wrapper with "hooks" key
        if let Ok(def) = serde_json::from_str::<HooksDefinition>(&content) {
            return Ok(def);
        }
        // Try wrapper format: { "description": "...", "hooks": { ... } }
        #[derive(Deserialize)]
        struct HooksWrapper {
            #[serde(default)]
            description: Option<String>,
            hooks: HooksDefinition,
        }
        let wrapper: HooksWrapper = serde_json::from_str(&content)
            .map_err(|e| PluginError::InvalidManifest(format!("Invalid hooks file: {}", e)))?;
        let mut def = wrapper.hooks;
        if def.description.is_none() {
            def.description = wrapper.description;
        }
        Ok(def)
    }

    /// Resolve MCP server configurations from manifest + convention
    fn resolve_mcp_servers(&mut self) {
        // Convention: .mcp.json at plugin root
        let mcp_json = self.path.join(".mcp.json");
        if mcp_json.exists() {
            if let Ok(content) = fs::read_to_string(&mcp_json) {
                // .mcp.json can be { "mcpServers": { ... } } or just { "server-name": { ... } }
                if let Ok(wrapper) =
                    serde_json::from_str::<HashMap<String, serde_json::Value>>(&content)
                {
                    if let Some(servers_val) = wrapper.get("mcpServers") {
                        if let Ok(servers) = serde_json::from_value::<
                            HashMap<String, McpServerConfig>,
                        >(servers_val.clone())
                        {
                            self.mcp_configs.extend(servers);
                        }
                    } else {
                        // Try as direct map
                        if let Ok(servers) =
                            serde_json::from_str::<HashMap<String, McpServerConfig>>(&content)
                        {
                            self.mcp_configs.extend(servers);
                        }
                    }
                }
            }
        }

        // Manifest-specified MCP servers
        if let Some(ref mcp_spec) = self.manifest.mcp_servers {
            match mcp_spec {
                McpServersSpec::Path(p) => {
                    let resolved = self.path.join(p);
                    if resolved.exists() {
                        if let Ok(content) = fs::read_to_string(&resolved) {
                            if let Ok(servers) =
                                serde_json::from_str::<HashMap<String, McpServerConfig>>(&content)
                            {
                                self.mcp_configs.extend(servers);
                            }
                        }
                    }
                }
                McpServersSpec::Map(map) => {
                    self.mcp_configs.extend(map.clone());
                }
                McpServersSpec::Array(entries) => {
                    for entry in entries {
                        match entry {
                            McpServersSpecEntry::Path(p) => {
                                let resolved = self.path.join(p);
                                if resolved.exists() {
                                    if let Ok(content) = fs::read_to_string(&resolved) {
                                        if let Ok(servers) = serde_json::from_str::<
                                            HashMap<String, McpServerConfig>,
                                        >(
                                            &content
                                        ) {
                                            self.mcp_configs.extend(servers);
                                        }
                                    }
                                }
                            }
                            McpServersSpecEntry::Map(map) => {
                                self.mcp_configs.extend(map.clone());
                            }
                        }
                    }
                }
            }
        }
    }

    /// Resolve agent paths from manifest + convention
    fn resolve_agents(&mut self) {
        let agents_dir = self.path.join("agents");
        if agents_dir.exists() && self.manifest.agents.is_none() {
            if let Ok(entries) = fs::read_dir(&agents_dir) {
                for entry in entries.flatten() {
                    let p = entry.path();
                    if p.extension().is_some_and(|e| e == "md") {
                        self.agent_paths.push(p);
                    }
                }
            }
        }
        if let Some(ref agents_spec) = self.manifest.agents {
            let paths = match agents_spec {
                AgentsSpec::Path(p) => vec![p.clone()],
                AgentsSpec::Paths(ps) => ps.clone(),
            };
            for p in paths {
                let resolved = self.path.join(&p);
                if resolved.exists() {
                    self.agent_paths.push(resolved);
                } else {
                    warn!(path = %p, plugin = %self.manifest.name, "Agent path not found");
                }
            }
        }
    }

    /// Resolve skill paths from manifest + convention
    fn resolve_skills(&mut self) {
        let skills_dir = self.path.join("skills");
        if skills_dir.exists() && self.manifest.skills.is_none() {
            if let Ok(entries) = fs::read_dir(&skills_dir) {
                for entry in entries.flatten() {
                    if entry.path().is_dir() {
                        self.skill_paths.push(entry.path());
                    }
                }
            }
        }
        if let Some(ref skills_spec) = self.manifest.skills {
            let paths = match skills_spec {
                SkillsSpec::Path(p) => vec![p.clone()],
                SkillsSpec::Paths(ps) => ps.clone(),
            };
            for p in paths {
                let resolved = self.path.join(&p);
                if resolved.exists() {
                    self.skill_paths.push(resolved);
                } else {
                    warn!(path = %p, plugin = %self.manifest.name, "Skill path not found");
                }
            }
        }
    }

    /// Get the plugin name
    pub fn name(&self) -> &str {
        &self.manifest.name
    }

    /// Get the plugin root path
    pub fn root(&self) -> &Path {
        &self.path
    }

    /// Get environment variables to set when running plugin scripts
    pub fn env_vars(&self) -> HashMap<String, String> {
        let mut vars = HashMap::new();
        vars.insert(
            "PLUGIN_ROOT".to_string(),
            self.path.to_string_lossy().to_string(),
        );
        vars.insert("PLUGIN_NAME".to_string(), self.manifest.name.clone());
        vars.insert(
            "PLUGIN_VERSION".to_string(),
            self.manifest
                .version
                .clone()
                .unwrap_or_else(|| "0.0.0".to_string()),
        );
        vars
    }

    /// Resolve a path relative to the plugin root
    pub fn resolve_path(&self, relative: &str) -> PathBuf {
        self.path.join(relative)
    }

    /// Get all resolved hooks as flat list
    pub fn resolved_hooks(&self) -> Vec<PluginHook> {
        let mut hooks = Vec::new();
        for def in &self.hook_definitions {
            for h in &def.pre_tool_use {
                hooks.push(PluginHook {
                    event: "PreToolUse".to_string(),
                    matcher: h.matcher.clone(),
                    hook_type: h.hook_type.clone(),
                    command: h.command.clone(),
                    prompt: None,
                    timeout: h.timeout.unwrap_or(30),
                });
            }
            for h in &def.post_tool_use {
                hooks.push(PluginHook {
                    event: "PostToolUse".to_string(),
                    matcher: h.matcher.clone(),
                    hook_type: h.hook_type.clone(),
                    command: h.command.clone(),
                    prompt: None,
                    timeout: h.timeout.unwrap_or(30),
                });
            }
            for h in &def.session_start {
                hooks.push(PluginHook {
                    event: "SessionStart".to_string(),
                    matcher: h.matcher.clone(),
                    hook_type: h.hook_type.clone(),
                    command: h.command.clone(),
                    prompt: None,
                    timeout: h.timeout.unwrap_or(30),
                });
            }
            for h in &def.notification {
                hooks.push(PluginHook {
                    event: "Notification".to_string(),
                    matcher: h.matcher.clone(),
                    hook_type: h.hook_type.clone(),
                    command: h.command.clone(),
                    prompt: None,
                    timeout: h.timeout.unwrap_or(30),
                });
            }
            for h in &def.stop {
                hooks.push(PluginHook {
                    event: "Stop".to_string(),
                    matcher: h.matcher.clone(),
                    hook_type: h.hook_type.clone(),
                    command: h.command.clone(),
                    prompt: None,
                    timeout: h.timeout.unwrap_or(30),
                });
            }
            for h in &def.user_prompt_submit {
                hooks.push(PluginHook {
                    event: "UserPromptSubmit".to_string(),
                    matcher: h.matcher.clone(),
                    hook_type: h.hook_type.clone(),
                    command: h.command.clone(),
                    prompt: None,
                    timeout: h.timeout.unwrap_or(30),
                });
            }
        }
        hooks
    }

    /// Get all resolved commands
    pub fn resolved_commands(&self) -> Vec<PluginCommand> {
        let mut commands = Vec::new();

        // Load commands from paths (markdown files)
        for path in &self.command_paths {
            let cmd_name = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("unknown")
                .to_string();

            let raw_content = fs::read_to_string(path).unwrap_or_default();
            let front_matter = parse_command_front_matter(&raw_content);

            let meta = self.command_metadata.get(&cmd_name);
            // Front matter values take precedence, then manifest metadata, then fallback
            let description = meta
                .and_then(|m| m.description.clone())
                .or(front_matter.description)
                .or_else(|| {
                    // Extract first non-empty line from body as description
                    front_matter
                        .body
                        .lines()
                        .find(|l| !l.trim().is_empty())
                        .map(|l| l.trim_start_matches('#').trim().to_string())
                });
            let allowed_tools = meta
                .and_then(|m| m.allowed_tools.clone())
                .or(front_matter.allowed_tools);

            commands.push(PluginCommand {
                name: cmd_name.clone(),
                description,
                content: front_matter.body,
                allowed_tools,
                argument_hint: front_matter.argument_hint,
                model: front_matter.model,
            });
        }

        // Load inline content commands (no file path)
        for (name, meta) in &self.command_metadata {
            if meta.source.is_none() {
                if let Some(ref content) = meta.content {
                    let front_matter = parse_command_front_matter(content);
                    commands.push(PluginCommand {
                        name: name.clone(),
                        description: meta.description.clone().or(front_matter.description),
                        content: front_matter.body,
                        allowed_tools: meta.allowed_tools.clone().or(front_matter.allowed_tools),
                        argument_hint: front_matter.argument_hint,
                        model: front_matter.model,
                    });
                }
            }
        }

        commands
    }

    /// Get all resolved MCP servers
    pub fn resolved_mcp_servers(&self) -> Vec<PluginMcpServer> {
        self.mcp_configs
            .iter()
            .map(|(name, config)| PluginMcpServer {
                name: name.clone(),
                transport: config.transport.clone(),
                command: config.command.clone(),
                args: config.args.clone(),
                url: config.url.clone(),
                env: config.env.clone(),
            })
            .collect()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// Create a Claude Code-style plugin in a temp directory
    fn create_cc_plugin(dir: &Path, name: &str) {
        let plugin_dir = dir.join(name);
        let cc_dir = plugin_dir.join(".claude-plugin");
        let commands_dir = plugin_dir.join("commands");
        let hooks_dir = plugin_dir.join("hooks");

        fs::create_dir_all(&cc_dir).unwrap();
        fs::create_dir_all(&commands_dir).unwrap();
        fs::create_dir_all(&hooks_dir).unwrap();

        // Write plugin.json manifest
        let manifest = serde_json::json!({
            "name": name,
            "version": "1.0.0",
            "description": "A test plugin",
            "author": {
                "name": "Test Author",
                "email": "test@example.com"
            },
            "keywords": ["test", "example"]
        });
        fs::write(
            cc_dir.join("plugin.json"),
            serde_json::to_string_pretty(&manifest).unwrap(),
        )
        .unwrap();

        // Write a command markdown file
        fs::write(
            commands_dir.join("greet.md"),
            "# Greet\nSay hello to the user in a friendly way.",
        )
        .unwrap();

        // Write hooks.json
        let hooks = serde_json::json!({
            "PreToolUse": [
                {
                    "matcher": "bash",
                    "type": "command",
                    "command": "echo checking bash"
                }
            ],
            "SessionStart": [
                {
                    "type": "command",
                    "command": "echo plugin loaded"
                }
            ]
        });
        fs::write(
            hooks_dir.join("hooks.json"),
            serde_json::to_string_pretty(&hooks).unwrap(),
        )
        .unwrap();
    }

    /// Create a legacy OpenClaudia-style plugin
    fn create_legacy_plugin(dir: &Path, name: &str) {
        let plugin_dir = dir.join(name);
        fs::create_dir_all(&plugin_dir).unwrap();

        let manifest = serde_json::json!({
            "name": name,
            "version": "1.0.0",
            "description": "Legacy test plugin",
            "hooks": [
                {
                    "event": "session_start",
                    "type": "command",
                    "command": "echo hello"
                }
            ],
            "commands": [
                {
                    "name": "test",
                    "description": "Test command",
                    "script": "echo test"
                }
            ],
            "mcp_servers": [
                {
                    "name": "test-server",
                    "transport": "stdio",
                    "command": "node",
                    "args": ["server.js"]
                }
            ]
        });

        fs::write(
            plugin_dir.join("manifest.json"),
            serde_json::to_string_pretty(&manifest).unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn test_cc_plugin_manifest_parsing() {
        let manifest_json = r#"{
            "name": "my-plugin",
            "version": "1.0.0",
            "description": "A test plugin",
            "author": {
                "name": "Test Author",
                "email": "test@example.com",
                "url": "https://example.com"
            },
            "keywords": ["test"],
            "commands": {
                "greet": {
                    "source": "./commands/greet.md",
                    "description": "Say hello"
                }
            },
            "mcpServers": {
                "my-server": {
                    "command": "node",
                    "args": ["server.js"],
                    "transport": "stdio"
                }
            }
        }"#;

        let manifest: PluginManifest = serde_json::from_str(manifest_json).unwrap();
        assert_eq!(manifest.name, "my-plugin");
        assert_eq!(manifest.version.as_deref(), Some("1.0.0"));
        assert!(manifest.commands.is_some());
        assert!(manifest.mcp_servers.is_some());
    }

    #[test]
    fn test_cc_plugin_load() {
        let dir = TempDir::new().unwrap();
        create_cc_plugin(dir.path(), "test-plugin");

        let plugin = Plugin::load(&dir.path().join("test-plugin")).unwrap();
        assert_eq!(plugin.name(), "test-plugin");
        assert_eq!(plugin.manifest.version.as_deref(), Some("1.0.0"));
        assert!(plugin.enabled);
        // Should find commands/greet.md
        assert_eq!(plugin.command_paths.len(), 1);
        // Should load hooks/hooks.json
        assert_eq!(plugin.hook_definitions.len(), 1);
    }

    #[test]
    fn test_cc_plugin_resolved_hooks() {
        let dir = TempDir::new().unwrap();
        create_cc_plugin(dir.path(), "hook-plugin");

        let plugin = Plugin::load(&dir.path().join("hook-plugin")).unwrap();
        let hooks = plugin.resolved_hooks();

        assert_eq!(hooks.len(), 2); // PreToolUse + SessionStart
        assert!(hooks.iter().any(|h| h.event == "PreToolUse"));
        assert!(hooks.iter().any(|h| h.event == "SessionStart"));
    }

    #[test]
    fn test_cc_plugin_resolved_commands() {
        let dir = TempDir::new().unwrap();
        create_cc_plugin(dir.path(), "cmd-plugin");

        let plugin = Plugin::load(&dir.path().join("cmd-plugin")).unwrap();
        let commands = plugin.resolved_commands();

        assert_eq!(commands.len(), 1);
        assert_eq!(commands[0].name, "greet");
        assert!(commands[0].content.contains("hello"));
    }

    #[test]
    fn test_legacy_plugin_load() {
        let dir = TempDir::new().unwrap();
        create_legacy_plugin(dir.path(), "legacy-plugin");

        let plugin = Plugin::load(&dir.path().join("legacy-plugin")).unwrap();
        assert_eq!(plugin.name(), "legacy-plugin");
        // Legacy MCP servers should be resolved
        assert_eq!(plugin.mcp_configs.len(), 1);
        assert!(plugin.mcp_configs.contains_key("test-server"));
    }

    #[test]
    fn test_plugin_env_vars() {
        let dir = TempDir::new().unwrap();
        create_cc_plugin(dir.path(), "env-test");

        let plugin = Plugin::load(&dir.path().join("env-test")).unwrap();
        let vars = plugin.env_vars();

        assert!(vars.contains_key("PLUGIN_ROOT"));
        assert_eq!(vars.get("PLUGIN_NAME"), Some(&"env-test".to_string()));
        assert_eq!(vars.get("PLUGIN_VERSION"), Some(&"1.0.0".to_string()));
    }

    #[test]
    fn test_plugin_manager_discover() {
        let dir = TempDir::new().unwrap();
        let plugins_dir = dir.path().join("plugins");
        fs::create_dir_all(&plugins_dir).unwrap();

        create_cc_plugin(&plugins_dir, "plugin-a");
        create_cc_plugin(&plugins_dir, "plugin-b");
        create_legacy_plugin(&plugins_dir, "plugin-c");

        let mut manager = PluginManager::with_paths(vec![plugins_dir]);
        let errors = manager.discover();

        assert!(errors.is_empty(), "Errors: {:?}", errors);
        assert_eq!(manager.count(), 3);
        assert!(manager.get("plugin-a").is_some());
        assert!(manager.get("plugin-b").is_some());
        assert!(manager.get("plugin-c").is_some());
    }

    #[test]
    fn test_plugin_manager_hooks() {
        let dir = TempDir::new().unwrap();
        let plugins_dir = dir.path().join("plugins");
        fs::create_dir_all(&plugins_dir).unwrap();

        create_cc_plugin(&plugins_dir, "hook-test");

        let mut manager = PluginManager::with_paths(vec![plugins_dir]);
        manager.discover();

        let hooks = manager.hooks_for_event("SessionStart");
        assert_eq!(hooks.len(), 1);

        let hooks = manager.hooks_for_event("PreToolUse");
        assert_eq!(hooks.len(), 1);
    }

    #[test]
    fn test_plugin_manager_commands() {
        let dir = TempDir::new().unwrap();
        let plugins_dir = dir.path().join("plugins");
        fs::create_dir_all(&plugins_dir).unwrap();

        create_cc_plugin(&plugins_dir, "cmd-test");

        let mut manager = PluginManager::with_paths(vec![plugins_dir]);
        manager.discover();

        let commands = manager.all_commands();
        assert_eq!(commands.len(), 1);
        assert_eq!(commands[0].1.name, "greet");
    }

    #[test]
    fn test_plugin_manager_mcp_servers() {
        let dir = TempDir::new().unwrap();
        let plugins_dir = dir.path().join("plugins");
        fs::create_dir_all(&plugins_dir).unwrap();

        create_legacy_plugin(&plugins_dir, "mcp-test");

        let mut manager = PluginManager::with_paths(vec![plugins_dir]);
        manager.discover();

        let servers = manager.all_mcp_servers();
        assert_eq!(servers.len(), 1);
        assert_eq!(servers[0].1.name, "test-server");
    }

    #[test]
    fn test_plugin_manager_enable_disable() {
        let dir = TempDir::new().unwrap();
        let plugins_dir = dir.path().join("plugins");
        fs::create_dir_all(&plugins_dir).unwrap();

        create_cc_plugin(&plugins_dir, "toggle-test");

        let mut manager = PluginManager::with_paths(vec![plugins_dir]);
        manager.discover();

        assert!(manager.get("toggle-test").unwrap().enabled);

        manager.disable("toggle-test").unwrap();
        assert!(!manager.get("toggle-test").unwrap().enabled);
        // Disabled plugin hooks should not appear
        assert!(manager.hooks_for_event("SessionStart").is_empty());

        manager.enable("toggle-test").unwrap();
        assert!(manager.get("toggle-test").unwrap().enabled);
        assert_eq!(manager.hooks_for_event("SessionStart").len(), 1);
    }

    #[test]
    fn test_invalid_manifest_empty_name() {
        let dir = TempDir::new().unwrap();
        let plugin_dir = dir.path().join("bad");
        let cc_dir = plugin_dir.join(".claude-plugin");
        fs::create_dir_all(&cc_dir).unwrap();
        fs::write(cc_dir.join("plugin.json"), r#"{"name": ""}"#).unwrap();

        let result = Plugin::load(&plugin_dir);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("empty"));
    }

    #[test]
    fn test_invalid_manifest_spaces_in_name() {
        let dir = TempDir::new().unwrap();
        let plugin_dir = dir.path().join("bad");
        let cc_dir = plugin_dir.join(".claude-plugin");
        fs::create_dir_all(&cc_dir).unwrap();
        fs::write(cc_dir.join("plugin.json"), r#"{"name": "my plugin"}"#).unwrap();

        let result = Plugin::load(&plugin_dir);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("spaces"));
    }

    #[test]
    fn test_no_manifest_error() {
        let dir = TempDir::new().unwrap();
        let plugin_dir = dir.path().join("empty");
        fs::create_dir_all(&plugin_dir).unwrap();

        let result = Plugin::load(&plugin_dir);
        assert!(matches!(result, Err(PluginError::ManifestNotFound(_))));
    }

    #[test]
    fn test_marketplace_manifest_parsing() {
        let json = r#"{
            "name": "my-marketplace",
            "owner": {
                "name": "Test Org",
                "email": "org@example.com"
            },
            "plugins": [
                {
                    "name": "cool-plugin",
                    "source": "./cool-plugin",
                    "category": "productivity",
                    "tags": ["cool"]
                },
                {
                    "name": "remote-plugin",
                    "source": {
                        "source": "github",
                        "repo": "user/repo"
                    },
                    "strict": true
                }
            ],
            "metadata": {
                "pluginRoot": ".",
                "version": "1.0.0"
            }
        }"#;

        let marketplace: MarketplaceManifest = serde_json::from_str(json).unwrap();
        assert_eq!(marketplace.name, "my-marketplace");
        assert_eq!(marketplace.plugins.len(), 2);
        assert_eq!(marketplace.plugins[0].name, "cool-plugin");
    }

    #[test]
    fn test_installed_plugins_tracking() {
        let mut installed = InstalledPlugins::default();
        assert_eq!(installed.version, 2);
        assert!(installed.plugins.is_empty());

        installed.upsert(
            "test-plugin@my-marketplace",
            PluginInstallEntry {
                scope: InstallScope::User,
                project_path: None,
                install_path: "/tmp/plugins/test-plugin".to_string(),
                version: Some("1.0.0".to_string()),
                installed_at: Some("2026-01-15T00:00:00Z".to_string()),
                last_updated: None,
                git_commit_sha: None,
            },
        );

        assert_eq!(installed.plugins.len(), 1);
        assert!(installed.plugins.contains_key("test-plugin@my-marketplace"));

        // Update same entry
        installed.upsert(
            "test-plugin@my-marketplace",
            PluginInstallEntry {
                scope: InstallScope::User,
                project_path: None,
                install_path: "/tmp/plugins/test-plugin".to_string(),
                version: Some("1.1.0".to_string()),
                installed_at: Some("2026-01-15T00:00:00Z".to_string()),
                last_updated: Some("2026-01-16T00:00:00Z".to_string()),
                git_commit_sha: None,
            },
        );
        // Should still be 1 entry, not 2
        assert_eq!(installed.plugins["test-plugin@my-marketplace"].len(), 1);
        assert_eq!(
            installed.plugins["test-plugin@my-marketplace"][0]
                .version
                .as_deref(),
            Some("1.1.0")
        );

        assert!(installed.remove("test-plugin@my-marketplace"));
        assert!(installed.plugins.is_empty());
    }

    #[test]
    fn test_plugin_with_mcp_json() {
        let dir = TempDir::new().unwrap();
        let plugin_dir = dir.path().join("mcp-plugin");
        let cc_dir = plugin_dir.join(".claude-plugin");
        fs::create_dir_all(&cc_dir).unwrap();

        fs::write(cc_dir.join("plugin.json"), r#"{"name": "mcp-plugin"}"#).unwrap();

        // Write .mcp.json at plugin root
        let mcp_config = serde_json::json!({
            "mcpServers": {
                "my-server": {
                    "command": "npx",
                    "args": ["-y", "@modelcontextprotocol/server-filesystem"],
                    "transport": "stdio"
                }
            }
        });
        fs::write(
            plugin_dir.join(".mcp.json"),
            serde_json::to_string(&mcp_config).unwrap(),
        )
        .unwrap();

        let plugin = Plugin::load(&plugin_dir).unwrap();
        assert_eq!(plugin.mcp_configs.len(), 1);
        assert!(plugin.mcp_configs.contains_key("my-server"));

        let servers = plugin.resolved_mcp_servers();
        assert_eq!(servers.len(), 1);
        assert_eq!(servers[0].name, "my-server");
        assert_eq!(servers[0].command.as_deref(), Some("npx"));
    }

    #[test]
    fn test_inline_commands() {
        let dir = TempDir::new().unwrap();
        let plugin_dir = dir.path().join("inline-cmd");
        let cc_dir = plugin_dir.join(".claude-plugin");
        fs::create_dir_all(&cc_dir).unwrap();

        let manifest = serde_json::json!({
            "name": "inline-cmd",
            "commands": {
                "hello": {
                    "content": "Say hello to the user warmly.",
                    "description": "Greet the user"
                }
            }
        });
        fs::write(
            cc_dir.join("plugin.json"),
            serde_json::to_string_pretty(&manifest).unwrap(),
        )
        .unwrap();

        let plugin = Plugin::load(&plugin_dir).unwrap();
        let commands = plugin.resolved_commands();
        assert_eq!(commands.len(), 1);
        assert_eq!(commands[0].name, "hello");
        assert_eq!(commands[0].content, "Say hello to the user warmly.");
    }

    #[test]
    fn test_plugin_error_variants() {
        let err = PluginError::InvalidManifest("missing field".to_string());
        assert!(err.to_string().contains("missing field"));

        let err = PluginError::NotFound("test-plugin".to_string());
        assert!(err.to_string().contains("test-plugin"));

        let err = PluginError::InstallError("download failed".to_string());
        assert!(err.to_string().contains("download failed"));

        let err = PluginError::MarketplaceError("not found".to_string());
        assert!(err.to_string().contains("not found"));
    }

    #[test]
    fn test_local_plugin_install_end_to_end() {
        // Create a source plugin directory (simulates what user provides)
        let source_dir = TempDir::new().unwrap();
        let plugin_src = source_dir.path().join("my-test-plugin");
        let cc_dir = plugin_src.join(".claude-plugin");
        let commands_dir = plugin_src.join("commands");
        fs::create_dir_all(&cc_dir).unwrap();
        fs::create_dir_all(&commands_dir).unwrap();

        fs::write(
            cc_dir.join("plugin.json"),
            r#"{
                "name": "my-test-plugin",
                "version": "2.0.0",
                "description": "End-to-end test plugin"
            }"#,
        )
        .unwrap();
        fs::write(
            commands_dir.join("hello.md"),
            "# Hello command\nSay hello to the user.",
        )
        .unwrap();
        fs::write(
            commands_dir.join("status.md"),
            "# Status check\nShow system status.",
        )
        .unwrap();

        // Create destination plugins directory (simulates .openclaudia/plugins/)
        let install_dir = TempDir::new().unwrap();
        let dest = install_dir.path().join("my-test-plugin");

        // Step 1: Load plugin from source (validates manifest)
        let loaded = Plugin::load(&plugin_src).unwrap();
        assert_eq!(loaded.name(), "my-test-plugin");
        assert_eq!(loaded.manifest.version.as_deref(), Some("2.0.0"));
        assert_eq!(loaded.command_paths.len(), 2);

        // Step 2: Copy to install directory
        copy_dir_recursive(&plugin_src, &dest).unwrap();
        assert!(dest.join(".claude-plugin/plugin.json").exists());
        assert!(dest.join("commands/hello.md").exists());
        assert!(dest.join("commands/status.md").exists());

        // Step 3: Load the installed copy and verify
        let installed_plugin = Plugin::load(&dest).unwrap();
        assert_eq!(installed_plugin.name(), "my-test-plugin");
        assert_eq!(installed_plugin.command_paths.len(), 2);

        // Step 4: Verify resolved commands
        let commands = installed_plugin.resolved_commands();
        assert_eq!(commands.len(), 2);
        let cmd_names: Vec<&str> = commands.iter().map(|c| c.name.as_str()).collect();
        assert!(cmd_names.contains(&"hello"));
        assert!(cmd_names.contains(&"status"));

        // Verify command content was preserved
        let hello_cmd = commands.iter().find(|c| c.name == "hello").unwrap();
        assert!(hello_cmd.content.contains("Say hello"));
        assert!(hello_cmd
            .description
            .as_deref()
            .unwrap()
            .contains("Hello command"));

        // Step 5: PluginManager discovers the installed plugin
        let mut manager = PluginManager::with_paths(vec![install_dir.path().to_path_buf()]);
        let errors = manager.discover();
        assert!(errors.is_empty(), "Discovery errors: {:?}", errors);
        assert_eq!(manager.count(), 1);

        let plugin = manager.get("my-test-plugin").unwrap();
        assert_eq!(plugin.name(), "my-test-plugin");
        assert!(plugin.enabled);

        // Step 6: all_commands returns the plugin's commands
        let all_cmds = manager.all_commands();
        assert_eq!(all_cmds.len(), 2);
        for (p, cmd) in &all_cmds {
            assert_eq!(p.name(), "my-test-plugin");
            assert!(cmd.name == "hello" || cmd.name == "status");
        }
    }

    #[test]
    fn test_marketplace_install_from_directory() {
        // Create a marketplace directory with plugins inside
        let marketplace_dir = TempDir::new().unwrap();
        let mp_root = marketplace_dir.path().join("test-marketplace");
        let mp_meta = mp_root.join(".claude-plugin");
        fs::create_dir_all(&mp_meta).unwrap();

        // Create marketplace manifest
        let marketplace_manifest = serde_json::json!({
            "name": "test-marketplace",
            "owner": { "name": "Test Owner" },
            "plugins": [
                {
                    "name": "cool-plugin",
                    "source": "cool-plugin",
                    "description": "A cool test plugin"
                }
            ]
        });
        fs::write(
            mp_meta.join("marketplace.json"),
            serde_json::to_string_pretty(&marketplace_manifest).unwrap(),
        )
        .unwrap();

        // Create the actual plugin inside the marketplace
        let plugin_dir = mp_root.join("cool-plugin");
        let plugin_cc_dir = plugin_dir.join(".claude-plugin");
        let plugin_cmds = plugin_dir.join("commands");
        fs::create_dir_all(&plugin_cc_dir).unwrap();
        fs::create_dir_all(&plugin_cmds).unwrap();

        fs::write(
            plugin_cc_dir.join("plugin.json"),
            r#"{"name": "cool-plugin", "version": "1.0.0"}"#,
        )
        .unwrap();
        fs::write(
            plugin_cmds.join("do-stuff.md"),
            "# Do stuff\nDo something cool.",
        )
        .unwrap();

        // Verify we can parse the marketplace manifest
        let content = fs::read_to_string(mp_meta.join("marketplace.json")).unwrap();
        let manifest: MarketplaceManifest = serde_json::from_str(&content).unwrap();
        assert_eq!(manifest.name, "test-marketplace");
        assert_eq!(manifest.plugins.len(), 1);
        assert_eq!(manifest.plugins[0].name, "cool-plugin");

        // Verify the plugin within the marketplace loads correctly
        let plugin = Plugin::load(&plugin_dir).unwrap();
        assert_eq!(plugin.name(), "cool-plugin");
        assert_eq!(plugin.resolved_commands().len(), 1);
    }

    #[test]
    fn test_copy_dir_recursive() {
        let src = TempDir::new().unwrap();
        let dst = TempDir::new().unwrap();

        // Create a nested structure
        let sub = src.path().join("sub");
        fs::create_dir_all(&sub).unwrap();
        fs::write(src.path().join("file1.txt"), "hello").unwrap();
        fs::write(sub.join("file2.txt"), "world").unwrap();

        let dest_path = dst.path().join("copy");
        copy_dir_recursive(src.path(), &dest_path).unwrap();

        assert!(dest_path.join("file1.txt").exists());
        assert!(dest_path.join("sub/file2.txt").exists());
        assert_eq!(
            fs::read_to_string(dest_path.join("file1.txt")).unwrap(),
            "hello"
        );
        assert_eq!(
            fs::read_to_string(dest_path.join("sub/file2.txt")).unwrap(),
            "world"
        );
    }

    #[test]
    fn test_plugin_enable_disable_flow() {
        let dir = TempDir::new().unwrap();
        create_cc_plugin(dir.path(), "toggle-plugin");

        let mut manager = PluginManager::with_paths(vec![dir.path().to_path_buf()]);
        let errors = manager.discover();
        assert!(errors.is_empty());

        // Initially enabled
        assert!(manager.get("toggle-plugin").unwrap().enabled);

        // Disable
        manager.disable("toggle-plugin").unwrap();
        assert!(!manager.get("toggle-plugin").unwrap().enabled);

        // all_commands should not return disabled plugin commands
        let cmds = manager.all_commands();
        for (p, _) in &cmds {
            assert_ne!(p.name(), "toggle-plugin");
        }

        // Re-enable
        manager.enable("toggle-plugin").unwrap();
        assert!(manager.get("toggle-plugin").unwrap().enabled);

        // Error on nonexistent plugin
        assert!(manager.enable("nonexistent").is_err());
        assert!(manager.disable("nonexistent").is_err());
    }

    #[test]
    fn test_plugin_reload() {
        let dir = TempDir::new().unwrap();
        create_cc_plugin(dir.path(), "reload-plugin");

        let mut manager = PluginManager::with_paths(vec![dir.path().to_path_buf()]);
        manager.discover();
        assert_eq!(manager.count(), 1);

        // Add another plugin to the directory
        create_cc_plugin(dir.path(), "new-plugin");

        // Reload should find it
        let errors = manager.reload();
        assert!(errors.is_empty());
        assert_eq!(manager.count(), 2);
        assert!(manager.get("reload-plugin").is_some());
        assert!(manager.get("new-plugin").is_some());
    }

    #[test]
    fn test_command_front_matter_parsing() {
        let content = r#"---
description: Create a git commit
allowed-tools: Bash(git add:*), Bash(git status:*), Bash(git commit:*)
---

## Context

Based on the above changes, create a single git commit.
"#;
        let fm = parse_command_front_matter(content);
        assert_eq!(fm.description.as_deref(), Some("Create a git commit"));
        assert!(fm.allowed_tools.is_some());
        let tools = fm.allowed_tools.unwrap();
        assert_eq!(tools.len(), 3);
        assert_eq!(tools[0], "Bash(git add:*)");
        assert!(fm.body.contains("## Context"));
        assert!(!fm.body.contains("---"));
    }

    #[test]
    fn test_command_front_matter_array_syntax() {
        let content = r#"---
description: An example command
argument-hint: <required-arg> [optional-arg]
allowed-tools: [Read, Glob, Grep, Bash]
model: haiku
---

# Example Command

Do something.
"#;
        let fm = parse_command_front_matter(content);
        assert_eq!(fm.description.as_deref(), Some("An example command"));
        assert_eq!(
            fm.argument_hint.as_deref(),
            Some("<required-arg> [optional-arg]")
        );
        assert_eq!(fm.model.as_deref(), Some("haiku"));
        let tools = fm.allowed_tools.unwrap();
        assert_eq!(tools, vec!["Read", "Glob", "Grep", "Bash"]);
        assert!(fm.body.starts_with("# Example Command"));
    }

    #[test]
    fn test_command_no_front_matter() {
        let content = "# Just a heading\n\nSome content.\n";
        let fm = parse_command_front_matter(content);
        assert!(fm.description.is_none());
        assert!(fm.allowed_tools.is_none());
        assert!(fm.argument_hint.is_none());
        assert!(fm.model.is_none());
        assert_eq!(fm.body, content);
    }

    #[test]
    fn test_real_plugin_front_matter_integration() {
        let dir = TempDir::new().unwrap();
        let plugin_dir = dir.path().join("commit-test");
        let cc_dir = plugin_dir.join(".claude-plugin");
        let commands_dir = plugin_dir.join("commands");
        fs::create_dir_all(&cc_dir).unwrap();
        fs::create_dir_all(&commands_dir).unwrap();

        fs::write(
            cc_dir.join("plugin.json"),
            r#"{"name": "commit-test", "description": "Test front matter"}"#,
        )
        .unwrap();

        // Write a command with front matter (matching real Claude plugin format)
        fs::write(
            commands_dir.join("commit.md"),
            r#"---
allowed-tools: Bash(git add:*), Bash(git status:*), Bash(git commit:*)
description: Create a git commit
---

## Context

- Current git status: !`git status`

## Your task

Based on the above changes, create a single git commit.
"#,
        )
        .unwrap();

        let plugin = Plugin::load(&plugin_dir).unwrap();
        let commands = plugin.resolved_commands();
        assert_eq!(commands.len(), 1);

        let cmd = &commands[0];
        assert_eq!(cmd.name, "commit");
        assert_eq!(cmd.description.as_deref(), Some("Create a git commit"));
        assert!(cmd.allowed_tools.is_some());
        assert_eq!(cmd.allowed_tools.as_ref().unwrap().len(), 3);
        // Content should NOT contain front matter
        assert!(!cmd.content.contains("allowed-tools:"));
        assert!(cmd.content.contains("## Context"));
        assert!(cmd.content.contains("git commit"));
    }
}
