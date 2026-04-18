//! Plugin manager for discovery, loading, and lifecycle management.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use tracing::{debug, info, warn};

use super::git::{copy_dir_recursive, git_clone, git_pull};
use super::install::{InstallScope, InstalledPlugins, PluginInstallEntry};
use super::marketplace::{MarketplaceManifest, MarketplacePlugin, PluginSource, PluginSourceDef};
use super::{Plugin, PluginCommand, PluginError, PluginHook, PluginMcpServer};

/// Manages plugin discovery, loading, and lifecycle
pub struct PluginManager {
    /// Loaded plugins by name
    plugins: HashMap<String, Plugin>,
    /// Search paths for plugins
    search_paths: Vec<PathBuf>,
    /// Installation tracking
    installed: InstalledPlugins,
}

impl PluginManager {
    /// Create a new plugin manager with default search paths
    #[must_use]
    pub fn new() -> Self {
        let mut search_paths = Vec::new();

        // User plugins directory
        if let Some(home) = dirs::home_dir() {
            search_paths.push(home.join(".openclaudia").join("plugins"));
            // Also search Claude Code's plugin cache for compatibility
            search_paths.push(home.join(".claude").join("plugins"));
        }

        // Project plugins directory
        search_paths.push(PathBuf::from(".openclaudia/plugins"));

        Self {
            plugins: HashMap::new(),
            search_paths,
            installed: InstalledPlugins::load(),
        }
    }

    /// Create a plugin manager with custom search paths
    #[must_use]
    pub fn with_paths(paths: Vec<PathBuf>) -> Self {
        Self {
            plugins: HashMap::new(),
            search_paths: paths,
            installed: InstalledPlugins::default(),
        }
    }

    /// Discover and load all plugins from search paths and `installed_plugins.json`
    pub fn discover(&mut self) -> Vec<PluginError> {
        let mut errors = Vec::new();

        // Load from search paths (convention-based discovery)
        for search_path in &self.search_paths.clone() {
            if !search_path.exists() {
                debug!(path = ?search_path, "Plugin search path does not exist");
                continue;
            }

            let entries = match fs::read_dir(search_path) {
                Ok(entries) => entries,
                Err(e) => {
                    warn!(path = ?search_path, error = %e, "Failed to read plugin directory");
                    continue;
                }
            };

            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    match Plugin::load(&path) {
                        Ok(plugin) => {
                            info!(
                                name = %plugin.name(),
                                version = ?plugin.manifest.version,
                                path = ?path,
                                commands = plugin.command_paths.len(),
                                hooks = plugin.hook_definitions.len(),
                                mcp = plugin.mcp_configs.len(),
                                "Loaded plugin"
                            );
                            self.plugins.insert(plugin.name().to_string(), plugin);
                        }
                        Err(PluginError::ManifestNotFound(_)) => {
                            // Not a plugin directory, skip silently
                            debug!(path = ?path, "Directory has no plugin manifest, skipping");
                        }
                        Err(e) => {
                            warn!(path = ?path, error = %e, "Failed to load plugin");
                            errors.push(e);
                        }
                    }
                }
            }
        }

        // Load from installed_plugins.json (tracked installations)
        for (plugin_id, entries) in &self.installed.plugins {
            for entry in entries {
                let install_path = PathBuf::from(&entry.install_path);
                if !install_path.exists() {
                    debug!(plugin = %plugin_id, path = ?install_path, "Installed plugin path missing");
                    continue;
                }
                // Skip if already loaded from search paths
                let name = plugin_id.split('@').next().unwrap_or(plugin_id);
                if self.plugins.contains_key(name) {
                    continue;
                }
                match Plugin::load(&install_path) {
                    Ok(mut plugin) => {
                        plugin.id.clone_from(plugin_id);
                        if let Some(marketplace) = plugin_id.split('@').nth(1) {
                            plugin.source = marketplace.to_string();
                        }
                        info!(
                            id = %plugin_id,
                            name = %plugin.name(),
                            scope = %entry.scope,
                            "Loaded installed plugin"
                        );
                        self.plugins.insert(plugin.name().to_string(), plugin);
                    }
                    Err(e) => {
                        warn!(plugin = %plugin_id, error = %e, "Failed to load installed plugin");
                        errors.push(e);
                    }
                }
            }
        }

        errors
    }

    /// Get a plugin by name
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&Plugin> {
        self.plugins.get(name)
    }

    /// Get all loaded plugins
    pub fn all(&self) -> impl Iterator<Item = &Plugin> {
        self.plugins.values()
    }

    /// Get the number of loaded plugins
    #[must_use]
    pub fn count(&self) -> usize {
        self.plugins.len()
    }

    /// Get all hooks from all enabled plugins
    #[must_use]
    pub fn all_hooks(&self) -> Vec<(&Plugin, PluginHook)> {
        self.plugins
            .values()
            .filter(|p| p.enabled)
            .flat_map(|plugin| {
                plugin
                    .resolved_hooks()
                    .into_iter()
                    .map(move |hook| (plugin, hook))
            })
            .collect()
    }

    /// Get hooks for a specific event
    #[must_use]
    pub fn hooks_for_event(&self, event: &str) -> Vec<(&Plugin, PluginHook)> {
        self.all_hooks()
            .into_iter()
            .filter(|(_, hook)| hook.event == event)
            .collect()
    }

    /// Get all commands from all enabled plugins
    #[must_use]
    pub fn all_commands(&self) -> Vec<(&Plugin, PluginCommand)> {
        self.plugins
            .values()
            .filter(|p| p.enabled)
            .flat_map(|plugin| {
                plugin
                    .resolved_commands()
                    .into_iter()
                    .map(move |cmd| (plugin, cmd))
            })
            .collect()
    }

    /// Get all MCP servers from all enabled plugins
    #[must_use]
    pub fn all_mcp_servers(&self) -> Vec<(&Plugin, PluginMcpServer)> {
        self.plugins
            .values()
            .filter(|p| p.enabled)
            .flat_map(|plugin| {
                plugin
                    .resolved_mcp_servers()
                    .into_iter()
                    .map(move |server| (plugin, server))
            })
            .collect()
    }

    /// Get the installation tracker
    #[must_use]
    pub const fn installed(&self) -> &InstalledPlugins {
        &self.installed
    }

    /// Get mutable installation tracker
    pub const fn installed_mut(&mut self) -> &mut InstalledPlugins {
        &mut self.installed
    }

    /// Enable a plugin
    ///
    /// # Errors
    ///
    /// Returns `PluginError::NotFound` if no plugin with the given name is loaded.
    pub fn enable(&mut self, name: &str) -> Result<(), PluginError> {
        if let Some(plugin) = self.plugins.get_mut(name) {
            plugin.enabled = true;
            Ok(())
        } else {
            Err(PluginError::NotFound(name.to_string()))
        }
    }

    /// Disable a plugin
    ///
    /// # Errors
    ///
    /// Returns `PluginError::NotFound` if no plugin with the given name is loaded.
    pub fn disable(&mut self, name: &str) -> Result<(), PluginError> {
        if let Some(plugin) = self.plugins.get_mut(name) {
            plugin.enabled = false;
            Ok(())
        } else {
            Err(PluginError::NotFound(name.to_string()))
        }
    }

    /// Reload all plugins
    pub fn reload(&mut self) -> Vec<PluginError> {
        self.plugins.clear();
        self.installed = InstalledPlugins::load();
        self.discover()
    }

    /// Get the marketplaces directory (~/.claude/marketplaces/)
    fn marketplaces_dir() -> PathBuf {
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".claude")
            .join("marketplaces")
    }

    /// List installed marketplaces
    #[must_use]
    pub fn list_marketplaces(&self) -> Vec<(String, MarketplaceManifest)> {
        let dir = Self::marketplaces_dir();
        let mut marketplaces = Vec::new();
        if !dir.exists() {
            return marketplaces;
        }
        if let Ok(entries) = fs::read_dir(&dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if !path.is_dir() {
                    continue;
                }
                // Try loading marketplace manifest
                let manifest_path = path.join(".claude-plugin").join("marketplace.json");
                let alt_manifest_path = path.join("marketplace.json");
                let mp = if manifest_path.exists() {
                    &manifest_path
                } else if alt_manifest_path.exists() {
                    &alt_manifest_path
                } else {
                    continue;
                };
                if let Ok(content) = fs::read_to_string(mp) {
                    if let Ok(manifest) = serde_json::from_str::<MarketplaceManifest>(&content) {
                        let name = entry.file_name().to_string_lossy().to_string();
                        marketplaces.push((name, manifest));
                    }
                }
            }
        }
        marketplaces
    }

    /// Add a marketplace from a local directory path
    ///
    /// # Errors
    ///
    /// Returns an error if the directory has no marketplace manifest, IO fails,
    /// or the marketplace already exists.
    pub fn add_marketplace_from_directory(
        &self,
        source_path: &Path,
    ) -> Result<MarketplaceManifest, PluginError> {
        // Validate source has a marketplace manifest
        let manifest_path = source_path.join(".claude-plugin").join("marketplace.json");
        let alt_manifest_path = source_path.join("marketplace.json");
        let mp = if manifest_path.exists() {
            &manifest_path
        } else if alt_manifest_path.exists() {
            &alt_manifest_path
        } else {
            return Err(PluginError::InvalidManifest(
                "No marketplace.json found in directory".to_string(),
            ));
        };
        let content = fs::read_to_string(mp).map_err(|e| PluginError::IoError(e.to_string()))?;
        let manifest: MarketplaceManifest = serde_json::from_str(&content)
            .map_err(|e| PluginError::InvalidManifest(e.to_string()))?;

        // Copy to marketplaces directory
        let dest = Self::marketplaces_dir().join(&manifest.name);
        if dest.exists() {
            return Err(PluginError::InvalidManifest(format!(
                "Marketplace '{}' already exists. Remove it first.",
                manifest.name
            )));
        }
        copy_dir_recursive(source_path, &dest).map_err(|e| PluginError::IoError(e.to_string()))?;

        info!(name = %manifest.name, plugins = manifest.plugins.len(), "Added marketplace");
        Ok(manifest)
    }

    /// Add a marketplace from a git repository URL
    ///
    /// # Errors
    /// Returns an error if the git clone fails or the manifest cannot be parsed.
    pub fn add_marketplace_from_git(
        &self,
        url: &str,
        git_ref: Option<&str>,
    ) -> Result<MarketplaceManifest, PluginError> {
        // Validate URL up front — git_clone also validates, but failing here
        // avoids an early mkdir when the URL is going to be rejected.
        super::validate::validate_source_url(url)?;

        let dest = Self::marketplaces_dir();
        fs::create_dir_all(&dest).map_err(|e| PluginError::IoError(e.to_string()))?;

        // Derive the destination name from the URL with the centralized
        // validator — rejects `..`, empty segments, path separators, leading
        // dots, NUL, and control chars. Closes crosslink #248.
        let name = super::validate::derive_dir_name_from_url(url)?;

        let clone_dest = dest.join(&name);
        if clone_dest.exists() {
            return Err(PluginError::InvalidManifest(format!(
                "Marketplace '{name}' already exists. Remove it first."
            )));
        }

        // Clone the repository
        git_clone(url, &clone_dest, git_ref)?;

        // Validate the cloned repo has a marketplace manifest
        let manifest_path = clone_dest.join(".claude-plugin").join("marketplace.json");
        let alt_path = clone_dest.join("marketplace.json");
        let mp = if manifest_path.exists() {
            &manifest_path
        } else if alt_path.exists() {
            &alt_path
        } else {
            // Clean up if no manifest
            let _ = fs::remove_dir_all(&clone_dest);
            return Err(PluginError::InvalidManifest(
                "Cloned repository has no marketplace.json".to_string(),
            ));
        };

        let content = fs::read_to_string(mp).map_err(|e| PluginError::IoError(e.to_string()))?;
        let manifest: MarketplaceManifest = serde_json::from_str(&content)
            .map_err(|e| PluginError::InvalidManifest(e.to_string()))?;

        info!(name = %manifest.name, url = %url, plugins = manifest.plugins.len(), "Added git marketplace");
        Ok(manifest)
    }

    /// Remove a marketplace by name
    ///
    /// # Errors
    /// Returns an error if the marketplace is not found or cannot be removed.
    pub fn remove_marketplace(&self, name: &str) -> Result<(), PluginError> {
        let dir = Self::marketplaces_dir().join(name);
        if !dir.exists() {
            return Err(PluginError::NotFound(format!(
                "Marketplace '{name}' not found"
            )));
        }
        fs::remove_dir_all(&dir).map_err(|e| PluginError::IoError(e.to_string()))?;
        info!(name = %name, "Removed marketplace");
        Ok(())
    }

    /// Update a marketplace (git pull or re-copy)
    ///
    /// # Errors
    /// Returns an error if the marketplace is not found or the update fails.
    pub fn update_marketplace(&self, name: &str) -> Result<MarketplaceManifest, PluginError> {
        let dir = Self::marketplaces_dir().join(name);
        if !dir.exists() {
            return Err(PluginError::NotFound(format!(
                "Marketplace '{name}' not found"
            )));
        }

        // Check if it's a git repo
        if dir.join(".git").exists() {
            git_pull(&dir)?;
        } else {
            return Err(PluginError::InvalidManifest(
                "Non-git marketplaces cannot be updated automatically. Remove and re-add."
                    .to_string(),
            ));
        }

        // Re-read manifest
        let manifest_path = dir.join(".claude-plugin").join("marketplace.json");
        let alt_path = dir.join("marketplace.json");
        let mp = if manifest_path.exists() {
            &manifest_path
        } else if alt_path.exists() {
            &alt_path
        } else {
            return Err(PluginError::InvalidManifest(
                "Marketplace manifest missing after update".to_string(),
            ));
        };

        let content = fs::read_to_string(mp).map_err(|e| PluginError::IoError(e.to_string()))?;
        let manifest: MarketplaceManifest = serde_json::from_str(&content)
            .map_err(|e| PluginError::InvalidManifest(e.to_string()))?;
        Ok(manifest)
    }

    /// Install a plugin from a marketplace
    ///
    /// # Errors
    /// Returns an error if the plugin is not found in the marketplace or installation fails.
    #[allow(clippy::too_many_lines)] // Complex installer, splitting would reduce readability
    pub fn install_from_marketplace(
        &mut self,
        plugin_name: &str,
        marketplace_name: &str,
    ) -> Result<String, PluginError> {
        // Find the marketplace
        let marketplaces = self.list_marketplaces();
        let (_name, manifest) = marketplaces
            .iter()
            .find(|(n, _)| n == marketplace_name)
            .ok_or_else(|| {
                PluginError::NotFound(format!("Marketplace '{marketplace_name}' not found"))
            })?;

        // Find the plugin in the marketplace
        let mp_plugin = manifest
            .plugins
            .iter()
            .find(|p| p.name == plugin_name)
            .ok_or_else(|| {
                PluginError::NotFound(format!(
                    "Plugin '{plugin_name}' not found in marketplace '{marketplace_name}'"
                ))
            })?;

        // Determine install path — validate plugin name to prevent path traversal
        if plugin_name.contains("..") || plugin_name.contains('/') || plugin_name.contains('\\') {
            return Err(PluginError::InvalidManifest(format!(
                "Plugin name '{plugin_name}' contains invalid path characters"
            )));
        }
        let plugins_dir = PathBuf::from(".openclaudia/plugins");
        let dest = plugins_dir.join(plugin_name);
        if dest.exists() {
            return Err(PluginError::InvalidManifest(format!(
                "Plugin '{}' already exists at {}",
                plugin_name,
                dest.display()
            )));
        }

        // Install based on source type
        let marketplace_dir = Self::marketplaces_dir().join(marketplace_name);
        let source_path = match &mp_plugin.source {
            PluginSource::Path(rel_path) => {
                let full = marketplace_dir.join(rel_path);
                if !full.exists() {
                    return Err(PluginError::IoError(format!(
                        "Plugin source path not found: {}",
                        full.display()
                    )));
                }
                // Verify the canonical path is still within the marketplace directory
                // to prevent path traversal attacks (e.g., rel_path = "../../etc/passwd")
                let canonical = full.canonicalize().map_err(|e| {
                    PluginError::IoError(format!(
                        "Failed to canonicalize plugin path {}: {}",
                        full.display(),
                        e
                    ))
                })?;
                let canonical_marketplace = marketplace_dir.canonicalize().map_err(|e| {
                    PluginError::IoError(format!(
                        "Failed to canonicalize marketplace dir {}: {}",
                        marketplace_dir.display(),
                        e
                    ))
                })?;
                if !canonical.starts_with(&canonical_marketplace) {
                    return Err(PluginError::IoError(format!(
                        "Plugin path traversal detected: {} escapes marketplace directory {}",
                        full.display(),
                        marketplace_dir.display()
                    )));
                }
                canonical
            }
            PluginSource::Structured(def) => {
                // For structured sources, clone/download directly to dest
                match def {
                    PluginSourceDef::Url { url, git_ref } => {
                        fs::create_dir_all(&plugins_dir)
                            .map_err(|e| PluginError::IoError(e.to_string()))?;
                        git_clone(url, &dest, git_ref.as_deref())?;
                    }
                    PluginSourceDef::GitHub { repo, git_ref } => {
                        let resolved_url = format!("https://github.com/{repo}.git");
                        fs::create_dir_all(&plugins_dir)
                            .map_err(|e| PluginError::IoError(e.to_string()))?;
                        git_clone(&resolved_url, &dest, git_ref.as_deref())?;
                    }
                    _ => {
                        return Err(PluginError::InvalidManifest(
                            "npm/pip sources not yet supported. Use git or path sources."
                                .to_string(),
                        ));
                    }
                }
                // Track and return (dest already populated by git clone)
                let plugin_id = format!("{plugin_name}@{marketplace_name}");
                let mut installed = InstalledPlugins::load();
                installed.upsert(
                    &plugin_id,
                    PluginInstallEntry {
                        scope: InstallScope::Project,
                        project_path: Some(
                            std::env::current_dir()
                                .unwrap_or_default()
                                .to_string_lossy()
                                .to_string(),
                        ),
                        install_path: dest.to_string_lossy().to_string(),
                        version: mp_plugin.version.clone(),
                        installed_at: Some(chrono::Utc::now().to_rfc3339()),
                        last_updated: None,
                        git_commit_sha: None,
                    },
                );
                if let Err(e) = installed.save() {
                    warn!("Failed to save install tracking: {}", e);
                }
                let _ = self.reload();
                info!(plugin = %plugin_name, marketplace = %marketplace_name, "Installed plugin from marketplace (git)");
                return Ok(plugin_id);
            }
        };

        // Copy plugin to install directory
        fs::create_dir_all(&plugins_dir).map_err(|e| PluginError::IoError(e.to_string()))?;
        copy_dir_recursive(&source_path, &dest).map_err(|e| PluginError::IoError(e.to_string()))?;

        // Track installation
        let plugin_id = format!("{plugin_name}@{marketplace_name}");
        let mut installed = InstalledPlugins::load();
        installed.upsert(
            &plugin_id,
            PluginInstallEntry {
                scope: InstallScope::Project,
                project_path: Some(
                    std::env::current_dir()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .to_string(),
                ),
                install_path: dest.to_string_lossy().to_string(),
                version: mp_plugin.version.clone(),
                installed_at: Some(chrono::Utc::now().to_rfc3339()),
                last_updated: None,
                git_commit_sha: None,
            },
        );
        if let Err(e) = installed.save() {
            warn!("Failed to save install tracking: {}", e);
        }

        // Reload to pick up the new plugin
        let _ = self.reload();

        info!(plugin = %plugin_name, marketplace = %marketplace_name, "Installed plugin from marketplace");
        Ok(plugin_id)
    }

    /// Install a plugin directly from a git repository
    ///
    /// # Errors
    /// Returns an error if the git clone fails or the plugin manifest is invalid.
    pub fn install_from_git(
        &mut self,
        url: &str,
        git_ref: Option<&str>,
    ) -> Result<String, PluginError> {
        // Reject disallowed URL schemes (http://, file://, git://, inline
        // credentials) before any filesystem work. git_clone will validate
        // again — deliberately redundant, cheap defense-in-depth.
        super::validate::validate_source_url(url)?;

        // Derive the plugins/ subdir name from the URL's last segment with
        // full traversal protection — closes crosslink #248. Previously the
        // url-last-segment extraction was raw and accepted `..`, leading
        // dots, etc., so a crafted URL could place the clone outside the
        // `.openclaudia/plugins/` jail.
        let name = super::validate::derive_dir_name_from_url(url)?;

        let plugins_dir = PathBuf::from(".openclaudia/plugins");
        let dest = plugins_dir.join(&name);
        if dest.exists() {
            return Err(PluginError::InvalidManifest(format!(
                "Plugin '{}' already exists at {}",
                name,
                dest.display()
            )));
        }

        fs::create_dir_all(&plugins_dir).map_err(|e| PluginError::IoError(e.to_string()))?;

        // Clone the repo
        git_clone(url, &dest, git_ref)?;

        // Validate it's a valid plugin
        match Plugin::load(&dest) {
            Ok(plugin) => {
                let actual_name = plugin.name().to_string();
                // Track installation
                let mut installed = InstalledPlugins::load();
                installed.upsert(
                    &actual_name,
                    PluginInstallEntry {
                        scope: InstallScope::Project,
                        project_path: Some(
                            std::env::current_dir()
                                .unwrap_or_default()
                                .to_string_lossy()
                                .to_string(),
                        ),
                        install_path: dest.to_string_lossy().to_string(),
                        version: plugin.manifest.version,
                        installed_at: Some(chrono::Utc::now().to_rfc3339()),
                        last_updated: None,
                        git_commit_sha: None,
                    },
                );
                if let Err(e) = installed.save() {
                    warn!("Failed to save install tracking: {}", e);
                }
                let _ = self.reload();
                info!(plugin = %actual_name, url = %url, "Installed plugin from git");
                Ok(actual_name)
            }
            Err(e) => {
                // Clean up invalid clone
                let _ = fs::remove_dir_all(&dest);
                Err(e)
            }
        }
    }

    /// List plugins available from all installed marketplaces
    #[must_use]
    pub fn list_available_plugins(&self) -> Vec<(String, MarketplacePlugin)> {
        let mut available = Vec::new();
        for (marketplace_name, manifest) in self.list_marketplaces() {
            for plugin in &manifest.plugins {
                available.push((marketplace_name.clone(), plugin.clone()));
            }
        }
        available
    }
}

impl Default for PluginManager {
    fn default() -> Self {
        Self::new()
    }
}
