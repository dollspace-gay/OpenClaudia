//! `OpenClaudia` - Open-source universal agent harness
//!
//! Provides Claude Code-like capabilities for any AI agent.
//!
//! This library exposes the core functionality of `OpenClaudia` for both
//! the CLI binary and integration testing.

#![recursion_limit = "256"]

/// Default max output tokens for chat completions when not specified by config.
pub const DEFAULT_MAX_TOKENS: u32 = 4096;

pub mod acp;
pub mod auto_learn;
pub mod claude_credentials;
pub mod compaction;
pub mod config;
pub mod context;
pub mod guardrails;
pub mod hooks;
pub mod mcp;
pub mod memory;
pub mod modes;
pub mod oauth;
pub mod output_style;
pub mod permissions;
pub mod pipeline;
pub mod plugins;
pub mod prompt;
pub mod providers;
pub mod proxy;
pub mod rules;
pub mod session;
pub mod skills;
pub mod streaming;
pub mod subagent;
pub mod tool_intercept;
pub mod tools;
pub mod tui;
pub mod vdd;
pub mod web;
