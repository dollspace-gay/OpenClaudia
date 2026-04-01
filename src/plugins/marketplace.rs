//! Marketplace schema types for plugin discovery and distribution.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use super::manifest::PluginAuthor;

// ---------------------------------------------------------------------------
// Marketplace schema
// ---------------------------------------------------------------------------

/// Source for a plugin within a marketplace
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum PluginSource {
    /// Relative path to plugin root within marketplace
    Path(String),
    /// Structured source definition
    Structured(PluginSourceDef),
}

/// Structured plugin source definition
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "source")]
pub enum PluginSourceDef {
    /// NPM package
    #[serde(rename = "npm")]
    Npm {
        package: String,
        #[serde(default)]
        version: Option<String>,
        #[serde(default)]
        registry: Option<String>,
    },
    /// Python package
    #[serde(rename = "pip")]
    Pip {
        package: String,
        #[serde(default)]
        version: Option<String>,
        #[serde(default)]
        registry: Option<String>,
    },
    /// Git repository URL
    #[serde(rename = "url")]
    Url {
        url: String,
        #[serde(default)]
        #[serde(rename = "ref")]
        git_ref: Option<String>,
    },
    /// GitHub repository
    #[serde(rename = "github")]
    GitHub {
        repo: String,
        #[serde(default)]
        #[serde(rename = "ref")]
        git_ref: Option<String>,
    },
}

/// A plugin entry within a marketplace
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketplacePlugin {
    /// Plugin name (kebab-case)
    pub name: String,
    /// Where to fetch the plugin from
    pub source: PluginSource,
    /// Category for organizing plugins
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    /// Tags for searchability
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tags: Option<Vec<String>>,
    /// Require plugin manifest to be present
    #[serde(default = "default_strict")]
    pub strict: bool,
    // Optional manifest fields inlined for non-strict plugins
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

const fn default_strict() -> bool {
    true
}

/// Marketplace metadata
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct MarketplaceMetadata {
    /// Base path for relative plugin sources
    #[serde(
        default,
        rename = "pluginRoot",
        skip_serializing_if = "Option::is_none"
    )]
    pub plugin_root: Option<String>,
    /// Marketplace version
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// Marketplace description
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// Marketplace manifest (marketplace.json)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarketplaceManifest {
    /// Marketplace name (kebab-case)
    pub name: String,
    /// Marketplace maintainer information
    pub owner: PluginAuthor,
    /// Collection of available plugins
    pub plugins: Vec<MarketplacePlugin>,
    /// Optional marketplace metadata
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<MarketplaceMetadata>,
}

/// Marketplace source configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "source")]
pub enum MarketplaceSource {
    /// GitHub repository
    #[serde(rename = "github")]
    GitHub {
        repo: String,
        #[serde(default)]
        #[serde(rename = "ref")]
        git_ref: Option<String>,
        #[serde(default)]
        path: Option<String>,
    },
    /// Git repository URL
    #[serde(rename = "git")]
    Git {
        url: String,
        #[serde(default)]
        #[serde(rename = "ref")]
        git_ref: Option<String>,
        #[serde(default)]
        path: Option<String>,
    },
    /// Direct URL to marketplace.json
    #[serde(rename = "url")]
    Url {
        url: String,
        #[serde(default)]
        headers: Option<HashMap<String, String>>,
    },
    /// Local file path
    #[serde(rename = "file")]
    File { path: String },
    /// Local directory containing .claude-plugin/marketplace.json
    #[serde(rename = "directory")]
    Directory { path: String },
}
