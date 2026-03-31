//! Installation tracking types for installed_plugins.json (V2 format).

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use tracing::{debug, warn};

use super::PluginError;

// ---------------------------------------------------------------------------
// Installation tracking (installed_plugins.json V2)
// ---------------------------------------------------------------------------

/// Installation scope for a plugin
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum InstallScope {
    Managed,
    User,
    Project,
    Local,
}

impl std::fmt::Display for InstallScope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Managed => write!(f, "managed"),
            Self::User => write!(f, "user"),
            Self::Project => write!(f, "project"),
            Self::Local => write!(f, "local"),
        }
    }
}

impl std::str::FromStr for InstallScope {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "managed" => Ok(Self::Managed),
            "user" => Ok(Self::User),
            "project" => Ok(Self::Project),
            "local" => Ok(Self::Local),
            _ => Err(format!(
                "Invalid scope '{}'. Must be: managed, user, project, local",
                s
            )),
        }
    }
}

/// A single installation entry for a plugin
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginInstallEntry {
    /// Installation scope
    pub scope: InstallScope,
    /// Project path (required for project/local scopes)
    #[serde(
        default,
        rename = "projectPath",
        skip_serializing_if = "Option::is_none"
    )]
    pub project_path: Option<String>,
    /// Absolute path to the installed plugin directory
    #[serde(rename = "installPath")]
    pub install_path: String,
    /// Currently installed version
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// ISO 8601 timestamp of installation
    #[serde(
        default,
        rename = "installedAt",
        skip_serializing_if = "Option::is_none"
    )]
    pub installed_at: Option<String>,
    /// ISO 8601 timestamp of last update
    #[serde(
        default,
        rename = "lastUpdated",
        skip_serializing_if = "Option::is_none"
    )]
    pub last_updated: Option<String>,
    /// Git commit SHA for git-based plugins
    #[serde(
        default,
        rename = "gitCommitSha",
        skip_serializing_if = "Option::is_none"
    )]
    pub git_commit_sha: Option<String>,
}

/// Installed plugins tracking file (V2 format)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstalledPlugins {
    /// Schema version (always 2)
    pub version: u32,
    /// Map of plugin IDs (plugin@marketplace) to installation entries
    pub plugins: HashMap<String, Vec<PluginInstallEntry>>,
}

impl Default for InstalledPlugins {
    fn default() -> Self {
        Self {
            version: 2,
            plugins: HashMap::new(),
        }
    }
}

impl InstalledPlugins {
    /// Load from disk, returning default if not found
    pub fn load() -> Self {
        let path = Self::file_path();
        if !path.exists() {
            return Self::default();
        }
        match std::fs::read_to_string(&path) {
            Ok(content) => match serde_json::from_str::<Self>(&content) {
                Ok(data) => {
                    debug!(
                        count = data.plugins.len(),
                        "Loaded installed plugins tracking"
                    );
                    data
                }
                Err(e) => {
                    warn!(error = %e, "Failed to parse installed_plugins.json, starting fresh");
                    Self::default()
                }
            },
            Err(e) => {
                warn!(error = %e, "Failed to read installed_plugins.json");
                Self::default()
            }
        }
    }

    /// Save to disk
    pub fn save(&self) -> Result<(), PluginError> {
        let path = Self::file_path();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| PluginError::IoError(e.to_string()))?;
        }
        let json =
            serde_json::to_string_pretty(self).map_err(|e| PluginError::IoError(e.to_string()))?;
        std::fs::write(&path, json).map_err(|e| PluginError::IoError(e.to_string()))?;
        debug!(path = ?path, count = self.plugins.len(), "Saved installed plugins");
        Ok(())
    }

    /// Add or update an installation entry
    pub fn upsert(&mut self, plugin_id: &str, entry: PluginInstallEntry) {
        let entries = self.plugins.entry(plugin_id.to_string()).or_default();
        if let Some(existing) = entries
            .iter_mut()
            .find(|e| e.scope == entry.scope && e.project_path == entry.project_path)
        {
            *existing = entry;
        } else {
            entries.push(entry);
        }
    }

    /// Remove a plugin by ID
    pub fn remove(&mut self, plugin_id: &str) -> bool {
        self.plugins.remove(plugin_id).is_some()
    }

    /// Get the file path for installed_plugins.json
    fn file_path() -> PathBuf {
        if let Some(home) = dirs::home_dir() {
            home.join(".openclaudia")
                .join("plugins")
                .join("installed_plugins.json")
        } else {
            PathBuf::from(".openclaudia/plugins/installed_plugins.json")
        }
    }

    /// Get all plugin IDs
    pub fn plugin_ids(&self) -> Vec<&str> {
        self.plugins.keys().map(|s| s.as_str()).collect()
    }
}
