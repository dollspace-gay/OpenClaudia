//! Embedded prompt fragments compiled into the binary.
//!
//! All markdown files from `prompts/` are included at compile time via
//! `include_str!`. No filesystem reads at runtime.

use super::{Agency, Modifier, Quality, Scope};

// =========================================================================
// Base fragments (always included in every prompt)
// =========================================================================

pub const BASE_IDENTITY: &str = include_str!("../../prompts/base/identity.md");
pub const BASE_TOOLS: &str = include_str!("../../prompts/base/tools.md");
pub const BASE_PRINCIPLES: &str = include_str!("../../prompts/base/principles.md");
pub const BASE_COMMS: &str = include_str!("../../prompts/base/comms.md");

// =========================================================================
// Agency axis fragments
// =========================================================================

const AGENCY_AUTONOMOUS: &str = include_str!("../../prompts/axis/agency/autonomous.md");
const AGENCY_COLLABORATIVE: &str = include_str!("../../prompts/axis/agency/collaborative.md");
const AGENCY_SURGICAL: &str = include_str!("../../prompts/axis/agency/surgical.md");

// =========================================================================
// Quality axis fragments
// =========================================================================

const QUALITY_ARCHITECT: &str = include_str!("../../prompts/axis/quality/architect.md");
const QUALITY_PRAGMATIC: &str = include_str!("../../prompts/axis/quality/pragmatic.md");
const QUALITY_MINIMAL: &str = include_str!("../../prompts/axis/quality/minimal.md");

// =========================================================================
// Scope axis fragments
// =========================================================================

const SCOPE_UNRESTRICTED: &str = include_str!("../../prompts/axis/scope/unrestricted.md");
const SCOPE_ADJACENT: &str = include_str!("../../prompts/axis/scope/adjacent.md");
const SCOPE_NARROW: &str = include_str!("../../prompts/axis/scope/narrow.md");

// =========================================================================
// Modifier fragments
// =========================================================================

const MOD_BOLD: &str = include_str!("../../prompts/modifiers/bold.md");
const MOD_DEBUG: &str = include_str!("../../prompts/modifiers/debug.md");
const MOD_METHODICAL: &str = include_str!("../../prompts/modifiers/methodical.md");
const MOD_DIRECTOR: &str = include_str!("../../prompts/modifiers/director.md");
const MOD_READONLY: &str = include_str!("../../prompts/modifiers/readonly.md");
const MOD_CONTEXT_PACING: &str = include_str!("../../prompts/modifiers/context-pacing.md");

// =========================================================================
// Accessor functions
// =========================================================================

/// Get the prompt fragment for an agency value.
pub const fn agency_fragment(agency: Agency) -> &'static str {
    match agency {
        Agency::Autonomous => AGENCY_AUTONOMOUS,
        Agency::Collaborative => AGENCY_COLLABORATIVE,
        Agency::Surgical => AGENCY_SURGICAL,
    }
}

/// Get the prompt fragment for a quality value.
pub const fn quality_fragment(quality: Quality) -> &'static str {
    match quality {
        Quality::Architect => QUALITY_ARCHITECT,
        Quality::Pragmatic => QUALITY_PRAGMATIC,
        Quality::Minimal => QUALITY_MINIMAL,
    }
}

/// Get the prompt fragment for a scope value.
pub const fn scope_fragment(scope: Scope) -> &'static str {
    match scope {
        Scope::Unrestricted => SCOPE_UNRESTRICTED,
        Scope::Adjacent => SCOPE_ADJACENT,
        Scope::Narrow => SCOPE_NARROW,
    }
}

/// Get the prompt fragment for a modifier.
pub const fn modifier_fragment(modifier: Modifier) -> &'static str {
    match modifier {
        Modifier::Bold => MOD_BOLD,
        Modifier::Debug => MOD_DEBUG,
        Modifier::Methodical => MOD_METHODICAL,
        Modifier::Director => MOD_DIRECTOR,
        Modifier::Readonly => MOD_READONLY,
        Modifier::ContextPacing => MOD_CONTEXT_PACING,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each agency fragment must NOT contain the heading of any other agency value.
    /// Catches copy-paste errors or accidental fragment concatenation.
    #[test]
    fn agency_fragments_do_not_cross_contaminate() {
        let pairs: &[(Agency, &[&str])] = &[
            (
                Agency::Autonomous,
                &["Agency: Collaborative", "Agency: Surgical"],
            ),
            (
                Agency::Collaborative,
                &["Agency: Autonomous", "Agency: Surgical"],
            ),
            (
                Agency::Surgical,
                &["Agency: Autonomous", "Agency: Collaborative"],
            ),
        ];
        for (variant, forbidden) in pairs {
            let frag = agency_fragment(*variant);
            for bad in *forbidden {
                assert!(
                    !frag.contains(bad),
                    "agency fragment for {variant} must not contain \"{bad}\""
                );
            }
        }
    }

    /// Each quality fragment must NOT contain the heading of any other quality value.
    #[test]
    fn quality_fragments_do_not_cross_contaminate() {
        let pairs: &[(Quality, &[&str])] = &[
            (
                Quality::Architect,
                &["Quality: Pragmatic", "Quality: Minimal"],
            ),
            (
                Quality::Pragmatic,
                &["Quality: Architect", "Quality: Minimal"],
            ),
            (
                Quality::Minimal,
                &["Quality: Architect", "Quality: Pragmatic"],
            ),
        ];
        for (variant, forbidden) in pairs {
            let frag = quality_fragment(*variant);
            for bad in *forbidden {
                assert!(
                    !frag.contains(bad),
                    "quality fragment for {variant} must not contain \"{bad}\""
                );
            }
        }
    }

    /// Each scope fragment must NOT contain the heading of any other scope value.
    #[test]
    fn scope_fragments_do_not_cross_contaminate() {
        let pairs: &[(Scope, &[&str])] = &[
            (Scope::Unrestricted, &["Scope: Adjacent", "Scope: Narrow"]),
            (Scope::Adjacent, &["Scope: Unrestricted", "Scope: Narrow"]),
            (Scope::Narrow, &["Scope: Unrestricted", "Scope: Adjacent"]),
        ];
        for (variant, forbidden) in pairs {
            let frag = scope_fragment(*variant);
            for bad in *forbidden {
                assert!(
                    !frag.contains(bad),
                    "scope fragment for {variant} must not contain \"{bad}\""
                );
            }
        }
    }

    /// Axis fragments must not contain headings from other axis dimensions.
    /// e.g. an agency fragment should never contain "# Quality:" or "# Scope:".
    #[test]
    fn axis_fragments_stay_in_their_dimension() {
        for agency in [Agency::Autonomous, Agency::Collaborative, Agency::Surgical] {
            let frag = agency_fragment(agency);
            assert!(
                !frag.contains("# Quality:"),
                "agency {agency} fragment contains quality heading"
            );
            assert!(
                !frag.contains("# Scope:"),
                "agency {agency} fragment contains scope heading"
            );
        }
        for quality in [Quality::Architect, Quality::Pragmatic, Quality::Minimal] {
            let frag = quality_fragment(quality);
            assert!(
                !frag.contains("# Agency:"),
                "quality {quality} fragment contains agency heading"
            );
            assert!(
                !frag.contains("# Scope:"),
                "quality {quality} fragment contains scope heading"
            );
        }
        for scope in [Scope::Unrestricted, Scope::Adjacent, Scope::Narrow] {
            let frag = scope_fragment(scope);
            assert!(
                !frag.contains("# Agency:"),
                "scope {scope} fragment contains agency heading"
            );
            assert!(
                !frag.contains("# Quality:"),
                "scope {scope} fragment contains quality heading"
            );
        }
    }

    /// No fragment should contain leftover template variables like {{VAR}}.
    #[test]
    fn no_unsubstituted_template_variables() {
        let all_fragments: Vec<(&str, &str)> = vec![
            ("BASE_IDENTITY", BASE_IDENTITY),
            ("BASE_TOOLS", BASE_TOOLS),
            ("BASE_PRINCIPLES", BASE_PRINCIPLES),
            ("BASE_COMMS", BASE_COMMS),
            ("agency/autonomous", agency_fragment(Agency::Autonomous)),
            (
                "agency/collaborative",
                agency_fragment(Agency::Collaborative),
            ),
            ("agency/surgical", agency_fragment(Agency::Surgical)),
            ("quality/architect", quality_fragment(Quality::Architect)),
            ("quality/pragmatic", quality_fragment(Quality::Pragmatic)),
            ("quality/minimal", quality_fragment(Quality::Minimal)),
            ("scope/unrestricted", scope_fragment(Scope::Unrestricted)),
            ("scope/adjacent", scope_fragment(Scope::Adjacent)),
            ("scope/narrow", scope_fragment(Scope::Narrow)),
            ("mod/bold", modifier_fragment(Modifier::Bold)),
            ("mod/debug", modifier_fragment(Modifier::Debug)),
            ("mod/methodical", modifier_fragment(Modifier::Methodical)),
            ("mod/director", modifier_fragment(Modifier::Director)),
            ("mod/readonly", modifier_fragment(Modifier::Readonly)),
            (
                "mod/context-pacing",
                modifier_fragment(Modifier::ContextPacing),
            ),
        ];
        let re = regex::Regex::new(r"\{\{[A-Z_]+\}\}").unwrap();
        for (name, content) in all_fragments {
            assert!(
                !re.is_match(content),
                "fragment {name} contains unsubstituted template variable: {:?}",
                re.find(content).map(|m| m.as_str())
            );
        }
    }

    /// Modifier fragments must each have unique opening content — no two
    /// modifiers should share the same first heading line, which would
    /// indicate a copy-paste duplication.
    #[test]
    fn modifier_fragments_have_unique_first_lines() {
        let all_mods = [
            Modifier::Bold,
            Modifier::Debug,
            Modifier::Methodical,
            Modifier::Director,
            Modifier::Readonly,
            Modifier::ContextPacing,
        ];
        let first_lines: Vec<(&str, &str)> = all_mods
            .iter()
            .map(|m| {
                let frag = modifier_fragment(*m);
                let first_heading = frag
                    .lines()
                    .find(|l| l.starts_with('#'))
                    .unwrap_or("<no heading>");
                (frag, first_heading)
            })
            .collect();

        for i in 0..first_lines.len() {
            for j in (i + 1)..first_lines.len() {
                assert_ne!(
                    first_lines[i].1, first_lines[j].1,
                    "modifiers {} and {} share the same first heading: {:?}",
                    all_mods[i], all_mods[j], first_lines[i].1
                );
            }
        }
    }

    /// Base fragments must not accidentally duplicate each other's sections.
    /// Identity must not contain tool definitions; tools must not contain
    /// communication style, etc.
    #[test]
    fn base_fragments_do_not_leak_into_each_other() {
        // Identity should not contain tool or principle sections
        assert!(
            !BASE_IDENTITY.contains("## Your Tools"),
            "identity fragment contains tool definitions"
        );
        assert!(
            !BASE_IDENTITY.contains("## Working Principles"),
            "identity fragment contains principles"
        );

        // Tools should not contain identity or comms
        assert!(
            !BASE_TOOLS.contains("Persona: Claudia"),
            "tools fragment contains identity"
        );
        assert!(
            !BASE_TOOLS.contains("## Communication Style"),
            "tools fragment contains comms"
        );

        // Comms should be self-contained
        assert!(
            !BASE_COMMS.contains("## Your Tools"),
            "comms fragment contains tools"
        );
        assert!(
            !BASE_COMMS.contains("Persona: Claudia"),
            "comms fragment contains identity"
        );
    }
}
