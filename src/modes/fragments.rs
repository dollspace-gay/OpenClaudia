//! Embedded prompt fragments compiled into the binary.
//!
//! All markdown files from `prompts/` are included at compile time via
//! `include_str!`. No filesystem reads at runtime.
//!
//! # Single-source-of-truth registries
//!
//! The four axis/modifier registries (`AGENCY_FRAGMENTS`, `QUALITY_FRAGMENTS`,
//! `SCOPE_FRAGMENTS`, `MODIFIERS`) are the *only* mapping between an enum
//! variant and its prompt content. Accessors and tests iterate these
//! constants; adding a new variant requires touching exactly one entry plus
//! the enum definition itself (which the `every_*_variant_appears_*` tests
//! enforce).

use super::{Agency, Modifier, Quality, Scope};

// =========================================================================
// Base fragments (always included in every prompt)
// =========================================================================

pub const BASE_IDENTITY: &str = include_str!("../../prompts/base/identity.md");
pub const BASE_TOOLS: &str = include_str!("../../prompts/base/tools.md");
pub const BASE_PRINCIPLES: &str = include_str!("../../prompts/base/principles.md");
pub const BASE_COMMS: &str = include_str!("../../prompts/base/comms.md");

/// Base fragments paired with their human-readable name (for diagnostics).
pub const BASE_FRAGMENTS: &[(&str, &str)] = &[
    ("BASE_IDENTITY", BASE_IDENTITY),
    ("BASE_TOOLS", BASE_TOOLS),
    ("BASE_PRINCIPLES", BASE_PRINCIPLES),
    ("BASE_COMMS", BASE_COMMS),
];

// =========================================================================
// Single-source registries: enum variant -> embedded markdown
// =========================================================================
//
// Each table below is the *only* place that wires a variant to its content.
// Tests enforce that every enum variant appears exactly once; accessors and
// listing functions iterate these tables.

/// Agency axis: variant -> embedded fragment.
pub const AGENCY_FRAGMENTS: &[(Agency, &str)] = &[
    (
        Agency::Autonomous,
        include_str!("../../prompts/axis/agency/autonomous.md"),
    ),
    (
        Agency::Collaborative,
        include_str!("../../prompts/axis/agency/collaborative.md"),
    ),
    (
        Agency::Surgical,
        include_str!("../../prompts/axis/agency/surgical.md"),
    ),
];

/// Quality axis: variant -> embedded fragment.
pub const QUALITY_FRAGMENTS: &[(Quality, &str)] = &[
    (
        Quality::Architect,
        include_str!("../../prompts/axis/quality/architect.md"),
    ),
    (
        Quality::Pragmatic,
        include_str!("../../prompts/axis/quality/pragmatic.md"),
    ),
    (
        Quality::Minimal,
        include_str!("../../prompts/axis/quality/minimal.md"),
    ),
];

/// Scope axis: variant -> embedded fragment.
pub const SCOPE_FRAGMENTS: &[(Scope, &str)] = &[
    (
        Scope::Unrestricted,
        include_str!("../../prompts/axis/scope/unrestricted.md"),
    ),
    (
        Scope::Adjacent,
        include_str!("../../prompts/axis/scope/adjacent.md"),
    ),
    (
        Scope::Narrow,
        include_str!("../../prompts/axis/scope/narrow.md"),
    ),
];

/// A single modifier's metadata.
///
/// Bundles the enum variant with its canonical kebab-case name (must match
/// `Display`), its embedded prompt fragment, and a one-line human
/// description for `list_modifiers()` / help output.
#[derive(Debug, Clone, Copy)]
pub struct ModifierEntry {
    pub variant: Modifier,
    pub name: &'static str,
    pub fragment: &'static str,
    pub description: &'static str,
}

/// Behavioral modifiers: the single source of truth pairing each `Modifier`
/// variant with its name, embedded fragment, and description.  Accessors,
/// listing functions, and tests all iterate this table.
pub const MODIFIERS: &[ModifierEntry] = &[
    ModifierEntry {
        variant: Modifier::Bold,
        name: "bold",
        fragment: include_str!("../../prompts/modifiers/bold.md"),
        description: "Confident, idiomatic code — no hedging",
    },
    ModifierEntry {
        variant: Modifier::Debug,
        name: "debug",
        fragment: include_str!("../../prompts/modifiers/debug.md"),
        description: "Investigation-first debugging",
    },
    ModifierEntry {
        variant: Modifier::Methodical,
        name: "methodical",
        fragment: include_str!("../../prompts/modifiers/methodical.md"),
        description: "Step-by-step precision",
    },
    ModifierEntry {
        variant: Modifier::Director,
        name: "director",
        fragment: include_str!("../../prompts/modifiers/director.md"),
        description: "Orchestrate subagents, delegate implementation",
    },
    ModifierEntry {
        variant: Modifier::Readonly,
        name: "readonly",
        fragment: include_str!("../../prompts/modifiers/readonly.md"),
        description: "No file modifications — read and explain only",
    },
    ModifierEntry {
        variant: Modifier::ContextPacing,
        name: "context-pacing",
        fragment: include_str!("../../prompts/modifiers/context-pacing.md"),
        description: "Pace work to context limits — clean pause points",
    },
];

// =========================================================================
// Accessor functions — single table lookup per axis.
// =========================================================================
//
// The accessors are no longer `const fn` because slice iteration is not yet
// stable in const context.  They are still trivial, branch-free linear
// scans over a six-or-fewer-element array — effectively free at runtime.

/// Get the prompt fragment for an agency value.
///
/// # Panics
/// Panics if the `AGENCY_FRAGMENTS` table is missing an entry for `agency`.
/// The `every_agency_variant_appears_in_table` test enforces this at build
/// time of the test suite.
#[must_use]
pub fn agency_fragment(agency: Agency) -> &'static str {
    AGENCY_FRAGMENTS
        .iter()
        .find(|(a, _)| *a == agency)
        .map(|(_, s)| *s)
        .expect("AGENCY_FRAGMENTS is missing an entry for an Agency variant")
}

/// Get the prompt fragment for a quality value.
///
/// # Panics
/// Panics if the `QUALITY_FRAGMENTS` table is missing an entry for `quality`.
#[must_use]
pub fn quality_fragment(quality: Quality) -> &'static str {
    QUALITY_FRAGMENTS
        .iter()
        .find(|(q, _)| *q == quality)
        .map(|(_, s)| *s)
        .expect("QUALITY_FRAGMENTS is missing an entry for a Quality variant")
}

/// Get the prompt fragment for a scope value.
///
/// # Panics
/// Panics if the `SCOPE_FRAGMENTS` table is missing an entry for `scope`.
#[must_use]
pub fn scope_fragment(scope: Scope) -> &'static str {
    SCOPE_FRAGMENTS
        .iter()
        .find(|(s, _)| *s == scope)
        .map(|(_, s)| *s)
        .expect("SCOPE_FRAGMENTS is missing an entry for a Scope variant")
}

/// Get the prompt fragment for a modifier.
///
/// # Panics
/// Panics if the `MODIFIERS` table is missing an entry for `modifier`.
#[must_use]
pub fn modifier_fragment(modifier: Modifier) -> &'static str {
    MODIFIERS
        .iter()
        .find(|e| e.variant == modifier)
        .map(|e| e.fragment)
        .expect("MODIFIERS is missing an entry for a Modifier variant")
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
    ///
    /// Iterates the four single-source registries (`BASE_FRAGMENTS` plus the
    /// three axis tables plus `MODIFIERS`) so adding a new variant
    /// automatically extends coverage — no hand-maintained vec to forget.
    #[test]
    fn no_unsubstituted_template_variables() {
        let re = regex::Regex::new(r"\{\{[A-Z_]+\}\}").unwrap();
        let check = |name: &str, content: &str| {
            assert!(
                !re.is_match(content),
                "fragment {name} contains unsubstituted template variable: {:?}",
                re.find(content).map(|m| m.as_str())
            );
        };

        for (name, content) in BASE_FRAGMENTS {
            check(name, content);
        }
        for (variant, content) in AGENCY_FRAGMENTS {
            check(&format!("agency/{variant}"), content);
        }
        for (variant, content) in QUALITY_FRAGMENTS {
            check(&format!("quality/{variant}"), content);
        }
        for (variant, content) in SCOPE_FRAGMENTS {
            check(&format!("scope/{variant}"), content);
        }
        for entry in MODIFIERS {
            check(&format!("mod/{}", entry.variant), entry.fragment);
        }
    }

    /// Modifier fragments must each have unique opening content — no two
    /// modifiers should share the same first heading line, which would
    /// indicate a copy-paste duplication.
    #[test]
    fn modifier_fragments_have_unique_first_lines() {
        let first_lines: Vec<(Modifier, &str)> = MODIFIERS
            .iter()
            .map(|e| {
                let first_heading = e
                    .fragment
                    .lines()
                    .find(|l| l.starts_with('#'))
                    .unwrap_or("<no heading>");
                (e.variant, first_heading)
            })
            .collect();

        for i in 0..first_lines.len() {
            for j in (i + 1)..first_lines.len() {
                assert_ne!(
                    first_lines[i].1, first_lines[j].1,
                    "modifiers {} and {} share the same first heading: {:?}",
                    first_lines[i].0, first_lines[j].0, first_lines[i].1
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

    // =====================================================================
    // Single-source-of-truth invariants (the point of issue #383).
    // =====================================================================

    /// Every `Modifier` enum variant must appear in `MODIFIERS` exactly once.
    ///
    /// The enumerated array on the left is the *enum's* witness — if a new
    /// variant is added to `Modifier` without a corresponding `MODIFIERS`
    /// row, this assertion (and the exhaustive `match` below) both fail.
    #[test]
    fn every_modifier_variant_appears_in_table_exactly_once() {
        let all_variants = [
            Modifier::Bold,
            Modifier::Debug,
            Modifier::Methodical,
            Modifier::Director,
            Modifier::Readonly,
            Modifier::ContextPacing,
        ];

        // Exhaustive match: new variant => compile error here.  This is the
        // mechanism that forces a developer to update both this list AND
        // the MODIFIERS table when the enum grows.
        for v in all_variants {
            match v {
                Modifier::Bold
                | Modifier::Debug
                | Modifier::Methodical
                | Modifier::Director
                | Modifier::Readonly
                | Modifier::ContextPacing => {}
            }
        }

        for v in all_variants {
            let count = MODIFIERS.iter().filter(|e| e.variant == v).count();
            assert_eq!(
                count, 1,
                "Modifier::{v} must appear in MODIFIERS exactly once, found {count}"
            );
        }
        assert_eq!(
            MODIFIERS.len(),
            all_variants.len(),
            "MODIFIERS contains entries for variants not in the canonical list"
        );

        // Each entry's `name` field must round-trip through FromStr to the
        // same variant, proving the embedded name is the canonical Display
        // string and not a typo that bypasses parsing.
        for entry in MODIFIERS {
            let parsed: Modifier = entry
                .name
                .parse()
                .expect("MODIFIERS entry name must be a parseable Modifier");
            assert_eq!(
                parsed, entry.variant,
                "MODIFIERS entry name {:?} parses to {parsed} but is paired with {}",
                entry.name, entry.variant
            );
            assert_eq!(
                entry.variant.to_string(),
                entry.name,
                "MODIFIERS entry name {:?} disagrees with Display for {}",
                entry.name,
                entry.variant
            );
        }
    }

    /// Same invariant for the three axis tables — every variant present
    /// exactly once.  Catches duplicate rows and missing rows.
    #[test]
    fn every_axis_variant_appears_in_its_table_exactly_once() {
        for v in [Agency::Autonomous, Agency::Collaborative, Agency::Surgical] {
            let count = AGENCY_FRAGMENTS.iter().filter(|(a, _)| *a == v).count();
            assert_eq!(
                count, 1,
                "Agency::{v} must appear in AGENCY_FRAGMENTS exactly once, found {count}"
            );
        }
        assert_eq!(AGENCY_FRAGMENTS.len(), 3);

        for v in [Quality::Architect, Quality::Pragmatic, Quality::Minimal] {
            let count = QUALITY_FRAGMENTS.iter().filter(|(q, _)| *q == v).count();
            assert_eq!(
                count, 1,
                "Quality::{v} must appear in QUALITY_FRAGMENTS exactly once, found {count}"
            );
        }
        assert_eq!(QUALITY_FRAGMENTS.len(), 3);

        for v in [Scope::Unrestricted, Scope::Adjacent, Scope::Narrow] {
            let count = SCOPE_FRAGMENTS.iter().filter(|(s, _)| *s == v).count();
            assert_eq!(
                count, 1,
                "Scope::{v} must appear in SCOPE_FRAGMENTS exactly once, found {count}"
            );
        }
        assert_eq!(SCOPE_FRAGMENTS.len(), 3);
    }

    /// Round-trip: `list_modifiers()` must enumerate exactly the same set
    /// of variants as `MODIFIERS`, in the same order.  If they ever drift
    /// the single-source guarantee is broken.
    #[test]
    fn list_modifiers_roundtrip_matches_table() {
        use crate::modes::list_modifiers;
        let listed = list_modifiers();
        assert_eq!(
            listed.len(),
            MODIFIERS.len(),
            "list_modifiers().len() must equal MODIFIERS.len()"
        );
        for (i, entry) in MODIFIERS.iter().enumerate() {
            // list_modifiers returns (name, desc) where name == entry.name
            assert_eq!(
                listed[i].0, entry.name,
                "list_modifiers()[{i}] name mismatch with MODIFIERS table"
            );
            assert_eq!(
                listed[i].1, entry.description,
                "list_modifiers()[{i}] description mismatch with MODIFIERS table"
            );
        }
    }
}
