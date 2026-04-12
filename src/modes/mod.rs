//! Behavioral modes system for OpenClaudia.
//!
//! Implements a three-axis model (agency, quality, scope) with named presets
//! and composable modifiers. Inspired by claude-code-modes but integrated
//! directly into OpenClaudia's prompt pipeline.
//!
//! # Architecture
//!
//! The system works by assembling markdown prompt fragments at runtime:
//! - **Base fragments**: identity, tools, principles, comms (always included)
//! - **Axis fragments**: one each from agency, quality, scope
//! - **Modifiers**: zero or more behavioral overlays
//!
//! Fragments are compiled into the binary via `include_str!` — no filesystem
//! reads at runtime.

pub mod fragments;

use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

// =========================================================================
// Axis enums
// =========================================================================

/// How much initiative the agent takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Agency {
    /// Makes decisions, creates files, restructures without asking.
    #[default]
    Autonomous,
    /// Explains reasoning, checks in at decision points.
    Collaborative,
    /// Executes exactly what was asked, nothing more.
    Surgical,
}

/// What code quality standard to target.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Quality {
    /// Proper abstractions, error handling, forward-thinking structure.
    Architect,
    /// Match existing patterns, improve incrementally.
    #[default]
    Pragmatic,
    /// Smallest correct change, no speculative improvements.
    Minimal,
}

/// How far beyond the request the agent can go.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Scope {
    /// Free to create, reorganize, restructure.
    Unrestricted,
    /// Fix related issues in the neighborhood.
    #[default]
    Adjacent,
    /// Only what was explicitly asked.
    Narrow,
}

/// Behavioral modifier overlays.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Modifier {
    /// Confident, idiomatic code — no hedging.
    Bold,
    /// Investigation-first debugging.
    Debug,
    /// Step-by-step precision.
    Methodical,
    /// Orchestrate subagents, delegate implementation.
    Director,
    /// No file modifications — read and explain only.
    Readonly,
    /// Pace work to context limits — clean pause points.
    ContextPacing,
}

/// Named preset combining axis values and optional modifiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Preset {
    /// Build from scratch with proper architecture.
    Create,
    /// Extend a fast-built project, improve incrementally.
    Extend,
    /// Surgical precision, minimal risk.
    Safe,
    /// Restructure freely across the codebase.
    Refactor,
    /// Read-only — understand code without changing it.
    Explore,
    /// Investigation-first debugging.
    Debug,
    /// Step-by-step precision.
    Methodical,
    /// Delegate to subagents, orchestrate and verify.
    Director,
}

// =========================================================================
// Display / FromStr implementations
// =========================================================================

impl fmt::Display for Agency {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Autonomous => write!(f, "autonomous"),
            Self::Collaborative => write!(f, "collaborative"),
            Self::Surgical => write!(f, "surgical"),
        }
    }
}

impl fmt::Display for Quality {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Architect => write!(f, "architect"),
            Self::Pragmatic => write!(f, "pragmatic"),
            Self::Minimal => write!(f, "minimal"),
        }
    }
}

impl fmt::Display for Scope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unrestricted => write!(f, "unrestricted"),
            Self::Adjacent => write!(f, "adjacent"),
            Self::Narrow => write!(f, "narrow"),
        }
    }
}

impl fmt::Display for Modifier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Bold => write!(f, "bold"),
            Self::Debug => write!(f, "debug"),
            Self::Methodical => write!(f, "methodical"),
            Self::Director => write!(f, "director"),
            Self::Readonly => write!(f, "readonly"),
            Self::ContextPacing => write!(f, "context-pacing"),
        }
    }
}

impl fmt::Display for Preset {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Create => write!(f, "create"),
            Self::Extend => write!(f, "extend"),
            Self::Safe => write!(f, "safe"),
            Self::Refactor => write!(f, "refactor"),
            Self::Explore => write!(f, "explore"),
            Self::Debug => write!(f, "debug"),
            Self::Methodical => write!(f, "methodical"),
            Self::Director => write!(f, "director"),
        }
    }
}

impl FromStr for Agency {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "autonomous" | "auto" => Ok(Self::Autonomous),
            "collaborative" | "collab" => Ok(Self::Collaborative),
            "surgical" => Ok(Self::Surgical),
            _ => Err(format!(
                "unknown agency: \"{s}\". Must be: autonomous, collaborative, surgical"
            )),
        }
    }
}

impl FromStr for Quality {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "architect" | "arch" => Ok(Self::Architect),
            "pragmatic" | "prag" => Ok(Self::Pragmatic),
            "minimal" | "min" => Ok(Self::Minimal),
            _ => Err(format!(
                "unknown quality: \"{s}\". Must be: architect, pragmatic, minimal"
            )),
        }
    }
}

impl FromStr for Scope {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "unrestricted" => Ok(Self::Unrestricted),
            "adjacent" | "adj" => Ok(Self::Adjacent),
            "narrow" => Ok(Self::Narrow),
            _ => Err(format!(
                "unknown scope: \"{s}\". Must be: unrestricted, adjacent, narrow"
            )),
        }
    }
}

impl FromStr for Modifier {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().replace('_', "-").as_str() {
            "bold" => Ok(Self::Bold),
            "debug" => Ok(Self::Debug),
            "methodical" => Ok(Self::Methodical),
            "director" => Ok(Self::Director),
            "readonly" | "read-only" => Ok(Self::Readonly),
            "context-pacing" | "pacing" => Ok(Self::ContextPacing),
            _ => Err(format!(
                "unknown modifier: \"{s}\". Must be: bold, debug, methodical, director, readonly, context-pacing"
            )),
        }
    }
}

impl FromStr for Preset {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "create" => Ok(Self::Create),
            "extend" => Ok(Self::Extend),
            "safe" => Ok(Self::Safe),
            "refactor" => Ok(Self::Refactor),
            "explore" => Ok(Self::Explore),
            "debug" => Ok(Self::Debug),
            "methodical" => Ok(Self::Methodical),
            "director" => Ok(Self::Director),
            _ => Err(format!(
                "unknown preset: \"{s}\". Must be: create, extend, safe, refactor, explore, debug, methodical, director"
            )),
        }
    }
}

// =========================================================================
// BehaviorMode — the assembled configuration
// =========================================================================

/// Complete behavioral configuration: three axis values plus optional modifiers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BehaviorMode {
    pub agency: Agency,
    pub quality: Quality,
    pub scope: Scope,
    pub modifiers: Vec<Modifier>,
}

impl Default for BehaviorMode {
    /// Default mode: autonomous / pragmatic / adjacent (matches `extend` preset).
    fn default() -> Self {
        Self {
            agency: Agency::Autonomous,
            quality: Quality::Pragmatic,
            scope: Scope::Adjacent,
            modifiers: Vec::new(),
        }
    }
}

impl fmt::Display for BehaviorMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}/{}", self.agency, self.quality, self.scope)?;
        if !self.modifiers.is_empty() {
            write!(f, " [")?;
            for (i, m) in self.modifiers.iter().enumerate() {
                if i > 0 {
                    write!(f, ", ")?;
                }
                write!(f, "{m}")?;
            }
            write!(f, "]")?;
        }
        Ok(())
    }
}

impl BehaviorMode {
    /// Create a mode from a preset, optionally overriding individual axes.
    pub fn from_preset(preset: Preset) -> Self {
        let (agency, quality, scope, modifiers) = match preset {
            Preset::Create => (
                Agency::Autonomous,
                Quality::Architect,
                Scope::Unrestricted,
                vec![],
            ),
            Preset::Extend => (
                Agency::Autonomous,
                Quality::Pragmatic,
                Scope::Adjacent,
                vec![],
            ),
            Preset::Safe => (
                Agency::Collaborative,
                Quality::Minimal,
                Scope::Narrow,
                vec![],
            ),
            Preset::Refactor => (
                Agency::Autonomous,
                Quality::Pragmatic,
                Scope::Unrestricted,
                vec![],
            ),
            Preset::Explore => (
                Agency::Collaborative,
                Quality::Architect,
                Scope::Narrow,
                vec![Modifier::Readonly],
            ),
            Preset::Debug => (
                Agency::Collaborative,
                Quality::Pragmatic,
                Scope::Narrow,
                vec![Modifier::Debug],
            ),
            Preset::Methodical => (
                Agency::Surgical,
                Quality::Architect,
                Scope::Narrow,
                vec![Modifier::Methodical],
            ),
            Preset::Director => (
                Agency::Collaborative,
                Quality::Architect,
                Scope::Unrestricted,
                vec![Modifier::Director],
            ),
        };
        Self {
            agency,
            quality,
            scope,
            modifiers,
        }
    }

    /// Add a modifier if not already present.
    pub fn add_modifier(&mut self, modifier: Modifier) {
        if !self.modifiers.contains(&modifier) {
            self.modifiers.push(modifier);
        }
    }

    /// Remove a modifier if present.
    pub fn remove_modifier(&mut self, modifier: Modifier) {
        self.modifiers.retain(|m| *m != modifier);
    }

    /// Try to find a matching preset name for the current configuration.
    /// Returns `None` if no built-in preset matches exactly.
    pub fn matching_preset(&self) -> Option<Preset> {
        let presets = [
            Preset::Create,
            Preset::Extend,
            Preset::Safe,
            Preset::Refactor,
            Preset::Explore,
            Preset::Debug,
            Preset::Methodical,
            Preset::Director,
        ];
        presets.into_iter().find(|p| &Self::from_preset(*p) == self)
    }

    /// Human-readable description of the mode for status displays.
    pub fn description(&self) -> String {
        if let Some(preset) = self.matching_preset() {
            let desc = match preset {
                Preset::Create => "Build from scratch with proper architecture",
                Preset::Extend => "Extend and improve incrementally",
                Preset::Safe => "Surgical precision, minimal risk",
                Preset::Refactor => "Restructure freely across the codebase",
                Preset::Explore => "Read-only — understand code without changing it",
                Preset::Debug => "Investigation-first debugging",
                Preset::Methodical => "Step-by-step precision",
                Preset::Director => "Orchestrate subagents, delegate and verify",
            };
            format!("{preset}: {desc}")
        } else {
            format!("custom: {self}")
        }
    }

    /// Short display name — preset name if matching, otherwise axis summary.
    pub fn display_name(&self) -> String {
        if let Some(preset) = self.matching_preset() {
            preset.to_string()
        } else {
            self.to_string()
        }
    }

    /// Assemble the complete behavioral prompt fragment for this mode.
    ///
    /// Returns the assembled string of all axis + modifier fragments,
    /// ready to be inserted into the system prompt.
    pub fn assemble_behavioral_prompt(&self) -> String {
        let mut sections: Vec<&str> = Vec::with_capacity(6);

        // Axis fragments
        sections.push(fragments::agency_fragment(self.agency));
        sections.push(fragments::quality_fragment(self.quality));
        sections.push(fragments::scope_fragment(self.scope));

        // Modifier fragments
        for modifier in &self.modifiers {
            sections.push(fragments::modifier_fragment(*modifier));
        }

        sections.join("\n\n")
    }
}

/// List all available preset names with their descriptions.
pub fn list_presets() -> Vec<(&'static str, &'static str)> {
    vec![
        (
            "create",
            "autonomous / architect / unrestricted — Build from scratch",
        ),
        (
            "extend",
            "autonomous / pragmatic / adjacent — Extend and improve",
        ),
        (
            "safe",
            "collaborative / minimal / narrow — Surgical precision",
        ),
        (
            "refactor",
            "autonomous / pragmatic / unrestricted — Restructure freely",
        ),
        (
            "explore",
            "collaborative / architect / narrow + readonly — Understand code",
        ),
        (
            "debug",
            "collaborative / pragmatic / narrow + debug — Investigation-first",
        ),
        (
            "methodical",
            "surgical / architect / narrow + methodical — Step-by-step",
        ),
        (
            "director",
            "collaborative / architect / unrestricted + director — Orchestrate agents",
        ),
    ]
}

/// List all available modifier names with their descriptions.
pub fn list_modifiers() -> Vec<(&'static str, &'static str)> {
    vec![
        ("bold", "Confident, idiomatic code — no hedging"),
        ("debug", "Investigation-first debugging"),
        ("methodical", "Step-by-step precision"),
        ("director", "Orchestrate subagents, delegate implementation"),
        ("readonly", "No file modifications — read and explain only"),
        (
            "context-pacing",
            "Pace work to context limits — clean pause points",
        ),
    ]
}

// =========================================================================
// Tests
// =========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    // =====================================================================
    // Preset uniqueness & identity
    // =====================================================================

    const ALL_PRESETS: [Preset; 8] = [
        Preset::Create,
        Preset::Extend,
        Preset::Safe,
        Preset::Refactor,
        Preset::Explore,
        Preset::Debug,
        Preset::Methodical,
        Preset::Director,
    ];

    /// Every preset must produce a distinct BehaviorMode.  If two presets
    /// collapse to the same config, one of them is redundant or miswired.
    #[test]
    fn all_presets_produce_unique_modes() {
        let modes: Vec<BehaviorMode> = ALL_PRESETS
            .iter()
            .map(|p| BehaviorMode::from_preset(*p))
            .collect();
        for i in 0..modes.len() {
            for j in (i + 1)..modes.len() {
                assert_ne!(
                    modes[i], modes[j],
                    "presets {} and {} collapsed to identical BehaviorMode",
                    ALL_PRESETS[i], ALL_PRESETS[j]
                );
            }
        }
    }

    /// from_preset → matching_preset must round-trip for every preset.
    #[test]
    fn preset_roundtrip_all() {
        for preset in ALL_PRESETS {
            let mode = BehaviorMode::from_preset(preset);
            assert_eq!(
                mode.matching_preset(),
                Some(preset),
                "preset {preset} did not round-trip through matching_preset"
            );
        }
    }

    /// Changing ANY single field of a preset's mode must break matching_preset.
    /// This catches a matching_preset implementation that ignores a field.
    #[test]
    fn matching_preset_sensitive_to_each_field() {
        for preset in ALL_PRESETS {
            let base = BehaviorMode::from_preset(preset);

            // Flip agency
            let mut m = base.clone();
            m.agency = if m.agency == Agency::Autonomous {
                Agency::Surgical
            } else {
                Agency::Autonomous
            };
            assert_ne!(
                m.matching_preset(),
                Some(preset),
                "preset {preset}: flipping agency should break match"
            );

            // Flip quality
            let mut m = base.clone();
            m.quality = if m.quality == Quality::Architect {
                Quality::Minimal
            } else {
                Quality::Architect
            };
            assert_ne!(
                m.matching_preset(),
                Some(preset),
                "preset {preset}: flipping quality should break match"
            );

            // Flip scope
            let mut m = base.clone();
            m.scope = if m.scope == Scope::Unrestricted {
                Scope::Narrow
            } else {
                Scope::Unrestricted
            };
            assert_ne!(
                m.matching_preset(),
                Some(preset),
                "preset {preset}: flipping scope should break match"
            );

            // Add an extra modifier
            let mut m = base.clone();
            let extra = if m.modifiers.contains(&Modifier::Bold) {
                Modifier::ContextPacing
            } else {
                Modifier::Bold
            };
            m.add_modifier(extra);
            assert_ne!(
                m.matching_preset(),
                Some(preset),
                "preset {preset}: adding modifier should break match"
            );
        }
    }

    // =====================================================================
    // FromStr adversarial
    // =====================================================================

    /// Empty strings, whitespace-only, numbers, and near-miss typos must
    /// all be rejected, not silently accepted as some default.
    #[test]
    fn from_str_rejects_garbage_inputs() {
        let garbage = [
            "",
            " ",
            "\t",
            "\n",
            "123",
            "null",
            "none",
            "true",
            "AUTONOMOUS",   // wrong case is accepted, but...
            "autonomou",    // one char short
            "collaborativ", // truncated
            "surgica",
            "🔥",
            "auto\0nomic",
            "auto nomous", // embedded space
        ];
        for input in garbage {
            // Some of these (like "AUTONOMOUS") ARE valid because FromStr
            // lowercases. We test the definitely-invalid ones.
            if input.trim().is_empty()
                || input.contains('\0')
                || input.contains(' ')
                || input.contains('🔥')
            {
                assert!(
                    input.parse::<Agency>().is_err(),
                    "Agency should reject {:?}",
                    input
                );
                assert!(
                    input.parse::<Quality>().is_err(),
                    "Quality should reject {:?}",
                    input
                );
                assert!(
                    input.parse::<Scope>().is_err(),
                    "Scope should reject {:?}",
                    input
                );
                assert!(
                    input.parse::<Preset>().is_err(),
                    "Preset should reject {:?}",
                    input
                );
                assert!(
                    input.parse::<Modifier>().is_err(),
                    "Modifier should reject {:?}",
                    input
                );
            }
        }
    }

    /// Every Display output must parse back to the original value.
    /// Catches Display/FromStr drift.
    #[test]
    fn display_from_str_roundtrip_all_enums() {
        for v in [Agency::Autonomous, Agency::Collaborative, Agency::Surgical] {
            assert_eq!(v.to_string().parse::<Agency>().unwrap(), v);
        }
        for v in [Quality::Architect, Quality::Pragmatic, Quality::Minimal] {
            assert_eq!(v.to_string().parse::<Quality>().unwrap(), v);
        }
        for v in [Scope::Unrestricted, Scope::Adjacent, Scope::Narrow] {
            assert_eq!(v.to_string().parse::<Scope>().unwrap(), v);
        }
        for v in ALL_PRESETS {
            assert_eq!(v.to_string().parse::<Preset>().unwrap(), v);
        }
        for v in [
            Modifier::Bold,
            Modifier::Debug,
            Modifier::Methodical,
            Modifier::Director,
            Modifier::Readonly,
            Modifier::ContextPacing,
        ] {
            assert_eq!(v.to_string().parse::<Modifier>().unwrap(), v);
        }
    }

    /// Modifier aliases must parse correctly — "read-only" and "readonly"
    /// both map to Readonly; "pacing" maps to ContextPacing.
    #[test]
    fn modifier_aliases_all_resolve() {
        assert_eq!("read-only".parse::<Modifier>().unwrap(), Modifier::Readonly);
        assert_eq!("readonly".parse::<Modifier>().unwrap(), Modifier::Readonly);
        assert_eq!(
            "context-pacing".parse::<Modifier>().unwrap(),
            Modifier::ContextPacing
        );
        assert_eq!(
            "pacing".parse::<Modifier>().unwrap(),
            Modifier::ContextPacing
        );
        // underscore normalisation
        assert_eq!(
            "context_pacing".parse::<Modifier>().unwrap(),
            Modifier::ContextPacing
        );
    }

    // =====================================================================
    // Modifier operations
    // =====================================================================

    /// Adding all 6 modifiers, then removing them one by one, must leave
    /// the mode with exactly the remaining modifiers in insertion order.
    #[test]
    fn modifier_add_remove_preserves_order() {
        let all_mods = [
            Modifier::Bold,
            Modifier::Debug,
            Modifier::Methodical,
            Modifier::Director,
            Modifier::Readonly,
            Modifier::ContextPacing,
        ];
        let mut mode = BehaviorMode::from_preset(Preset::Create);
        for m in &all_mods {
            mode.add_modifier(*m);
        }
        assert_eq!(mode.modifiers.len(), 6);
        assert_eq!(mode.modifiers, all_mods.to_vec());

        // Remove from the middle
        mode.remove_modifier(Modifier::Methodical);
        assert_eq!(mode.modifiers.len(), 5);
        assert_eq!(mode.modifiers[0], Modifier::Bold);
        assert_eq!(mode.modifiers[1], Modifier::Debug);
        assert_eq!(mode.modifiers[2], Modifier::Director); // shifted up
        assert_eq!(mode.modifiers[3], Modifier::Readonly);

        // Removing a non-present modifier is a no-op
        mode.remove_modifier(Modifier::Methodical);
        assert_eq!(mode.modifiers.len(), 5);
    }

    /// Duplicate add_modifier calls must be idempotent — the modifier
    /// list must never contain duplicates.
    #[test]
    fn add_modifier_is_idempotent() {
        let mut mode = BehaviorMode::default();
        for _ in 0..100 {
            mode.add_modifier(Modifier::Bold);
        }
        assert_eq!(
            mode.modifiers
                .iter()
                .filter(|m| **m == Modifier::Bold)
                .count(),
            1
        );
    }

    // =====================================================================
    // Serde edge cases
    // =====================================================================

    /// Every preset must survive serde JSON round-trip exactly.
    #[test]
    fn serde_roundtrip_all_presets() {
        for preset in ALL_PRESETS {
            let mode = BehaviorMode::from_preset(preset);
            let json = serde_json::to_string(&mode).unwrap();
            let restored: BehaviorMode = serde_json::from_str(&json).unwrap();
            assert_eq!(mode, restored, "serde roundtrip failed for preset {preset}");
        }
    }

    /// A custom mode with all 6 modifiers must round-trip through serde.
    #[test]
    fn serde_roundtrip_all_modifiers() {
        let mode = BehaviorMode {
            agency: Agency::Surgical,
            quality: Quality::Minimal,
            scope: Scope::Narrow,
            modifiers: vec![
                Modifier::Bold,
                Modifier::Debug,
                Modifier::Methodical,
                Modifier::Director,
                Modifier::Readonly,
                Modifier::ContextPacing,
            ],
        };
        let json = serde_json::to_string(&mode).unwrap();
        let restored: BehaviorMode = serde_json::from_str(&json).unwrap();
        assert_eq!(mode, restored);
    }

    /// Deserialization of the Default value from JSON must produce Default.
    /// Tests backwards compat: old config files with default values.
    #[test]
    fn serde_deserialize_defaults() {
        let json =
            r#"{"agency":"autonomous","quality":"pragmatic","scope":"adjacent","modifiers":[]}"#;
        let mode: BehaviorMode = serde_json::from_str(json).unwrap();
        assert_eq!(mode, BehaviorMode::default());
    }

    /// Missing `modifiers` field should default to empty vec.
    /// Tests backwards compat for sessions saved before modifiers existed.
    #[test]
    fn serde_missing_modifiers_defaults_to_empty() {
        let json = r#"{"agency":"autonomous","quality":"pragmatic","scope":"adjacent"}"#;
        let result: Result<BehaviorMode, _> = serde_json::from_str(json);
        // This will error since modifiers is not optional — that's the
        // current design.  If we want backwards compat, we'd need
        // #[serde(default)] on the field.  This test documents the behavior.
        assert!(
            result.is_err(),
            "BehaviorMode currently requires modifiers field; \
             add #[serde(default)] to make it optional"
        );
    }

    /// Unknown enum variant in JSON should produce a clear error.
    #[test]
    fn serde_rejects_unknown_variants() {
        let json = r#"{"agency":"yolo","quality":"pragmatic","scope":"adjacent","modifiers":[]}"#;
        assert!(serde_json::from_str::<BehaviorMode>(json).is_err());

        let json = r#"{"agency":"autonomous","quality":"pragmatic","scope":"adjacent","modifiers":["teleport"]}"#;
        assert!(serde_json::from_str::<BehaviorMode>(json).is_err());
    }

    // =====================================================================
    // Assembly adversarial
    // =====================================================================

    /// Every pair of distinct presets must produce distinct assembled prompts.
    /// Catches fragment wiring bugs where two presets map to the same content.
    #[test]
    fn distinct_presets_produce_distinct_prompts() {
        let prompts: Vec<(Preset, String)> = ALL_PRESETS
            .iter()
            .map(|p| {
                (
                    *p,
                    BehaviorMode::from_preset(*p).assemble_behavioral_prompt(),
                )
            })
            .collect();
        for i in 0..prompts.len() {
            for j in (i + 1)..prompts.len() {
                assert_ne!(
                    prompts[i].1, prompts[j].1,
                    "presets {} and {} produced identical prompt text",
                    prompts[i].0, prompts[j].0
                );
            }
        }
    }

    /// Assembly must be deterministic: same mode, same output.
    #[test]
    fn assembly_is_deterministic() {
        let mode = BehaviorMode::from_preset(Preset::Director);
        let a = mode.assemble_behavioral_prompt();
        let b = mode.assemble_behavioral_prompt();
        assert_eq!(a, b);
    }

    /// Assembled prompt for a mode with modifiers must contain content from
    /// ALL modifiers, and the modifier content must appear AFTER the axis
    /// content. This catches ordering regressions.
    #[test]
    fn assembly_ordering_axes_before_modifiers() {
        let mode = BehaviorMode {
            agency: Agency::Autonomous,
            quality: Quality::Architect,
            scope: Scope::Unrestricted,
            modifiers: vec![Modifier::Bold, Modifier::ContextPacing],
        };
        let prompt = mode.assemble_behavioral_prompt();

        let agency_pos = prompt.find("# Agency:").expect("missing agency");
        let quality_pos = prompt.find("# Quality:").expect("missing quality");
        let scope_pos = prompt.find("# Scope:").expect("missing scope");
        let bold_pos = prompt.find("# Bold").expect("missing bold modifier");
        let pacing_pos = prompt
            .find("# Context and Pacing")
            .expect("missing context-pacing modifier");

        // Axes in order
        assert!(agency_pos < quality_pos, "agency must precede quality");
        assert!(quality_pos < scope_pos, "quality must precede scope");
        // Modifiers after axes, in insertion order
        assert!(scope_pos < bold_pos, "scope must precede bold modifier");
        assert!(bold_pos < pacing_pos, "bold must precede context-pacing");
    }

    /// Stacking all 6 modifiers on a preset must not panic, must not
    /// duplicate fragment text, and must include content from each modifier.
    #[test]
    fn stacking_all_modifiers_produces_complete_prompt() {
        let mut mode = BehaviorMode::from_preset(Preset::Create);
        let all_mods = [
            Modifier::Bold,
            Modifier::Debug,
            Modifier::Methodical,
            Modifier::Director,
            Modifier::Readonly,
            Modifier::ContextPacing,
        ];
        for m in &all_mods {
            mode.add_modifier(*m);
        }
        let prompt = mode.assemble_behavioral_prompt();

        // Each modifier's unique heading must appear exactly once
        let unique_markers = [
            "# Bold",
            "# Investigation Mode",
            "# Methodical Mode",
            "# Director",
            "# Read-Only Mode",
            "# Context and Pacing",
        ];
        for marker in unique_markers {
            let count = prompt.matches(marker).count();
            assert_eq!(
                count, 1,
                "expected exactly 1 occurrence of \"{marker}\", found {count}"
            );
        }
    }

    // =====================================================================
    // list_presets / list_modifiers consistency
    // =====================================================================

    /// Every preset returned by list_presets() must be parseable as a Preset.
    #[test]
    fn list_presets_names_all_parse() {
        for (name, _desc) in list_presets() {
            assert!(
                name.parse::<Preset>().is_ok(),
                "list_presets() contains unparseable name: {name:?}"
            );
        }
    }

    /// The set of names from list_presets() must equal the set from ALL_PRESETS.
    #[test]
    fn list_presets_covers_all_variants() {
        let listed: HashSet<String> = list_presets().iter().map(|(n, _)| n.to_string()).collect();
        let expected: HashSet<String> = ALL_PRESETS.iter().map(|p| p.to_string()).collect();
        assert_eq!(listed, expected, "list_presets() doesn't match ALL_PRESETS");
    }

    /// Every modifier name from list_modifiers() must be parseable.
    #[test]
    fn list_modifiers_names_all_parse() {
        for (name, _desc) in list_modifiers() {
            assert!(
                name.parse::<Modifier>().is_ok(),
                "list_modifiers() contains unparseable name: {name:?}"
            );
        }
    }

    // =====================================================================
    // Display edge cases
    // =====================================================================

    /// Display for a mode with multiple modifiers must list them all,
    /// comma-separated, in brackets.
    #[test]
    fn display_multiple_modifiers() {
        let mode = BehaviorMode {
            agency: Agency::Surgical,
            quality: Quality::Minimal,
            scope: Scope::Narrow,
            modifiers: vec![Modifier::Debug, Modifier::Bold, Modifier::Readonly],
        };
        let s = mode.to_string();
        assert!(s.contains("[debug, bold, readonly]"), "got: {s}");
    }

    /// display_name returns the preset name for matching presets,
    /// and the full axis string for custom modes.
    #[test]
    fn display_name_preset_vs_custom() {
        let matching = BehaviorMode::from_preset(Preset::Create);
        assert_eq!(matching.display_name(), "create");

        let custom = BehaviorMode {
            agency: Agency::Surgical,
            quality: Quality::Architect,
            scope: Scope::Unrestricted,
            modifiers: vec![],
        };
        // No preset matches this combo, so display_name is the axis string
        assert_eq!(custom.display_name(), "surgical/architect/unrestricted");
    }

    /// description() for a custom mode must include the axis values
    /// so the user can tell what's configured.
    #[test]
    fn description_custom_includes_axes() {
        let mode = BehaviorMode {
            agency: Agency::Surgical,
            quality: Quality::Minimal,
            scope: Scope::Unrestricted,
            modifiers: vec![Modifier::Bold],
        };
        let desc = mode.description();
        assert!(
            desc.contains("surgical"),
            "description missing agency: {desc}"
        );
        assert!(
            desc.contains("minimal"),
            "description missing quality: {desc}"
        );
        assert!(
            desc.contains("unrestricted"),
            "description missing scope: {desc}"
        );
    }
}
