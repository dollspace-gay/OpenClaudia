//! Skills system for OpenClaudia.
//!
//! Loads user-defined skills from `.openclaudia/skills/` directories.
//! Skills are markdown files with YAML frontmatter that define
//! reusable prompts invokable as slash commands.
//!
//! Skill file format (SKILL.md or <name>.md):
//! ```markdown
//! ---
//! name: my-skill
//! description: Does something useful
//! allowed_tools: [bash, read_file, edit_file]
//! ---
//!
//! You are a specialized agent that...
//! ```

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillDefinition {
    pub name: String,
    pub description: String,
    #[serde(default)]
    pub allowed_tools: Option<Vec<String>>,
    /// The prompt content (markdown body after frontmatter)
    #[serde(skip)]
    pub prompt: String,
    /// Path to the skill file
    #[serde(skip)]
    pub path: PathBuf,
}

/// Parse a skill file (YAML frontmatter + markdown body)
pub fn parse_skill_file(path: &Path) -> Option<SkillDefinition> {
    let content = std::fs::read_to_string(path).ok()?;

    // Split frontmatter from body
    if !content.starts_with("---") {
        return None;
    }

    let rest = &content[3..];
    let end = rest.find("---")?;
    let frontmatter = rest[..end].trim();
    let body = rest[end + 3..].trim();

    let mut skill: SkillDefinition = serde_yaml::from_str(frontmatter).ok()?;
    skill.prompt = body.to_string();
    skill.path = path.to_path_buf();

    Some(skill)
}

/// Scan directories for skill files
pub fn load_skills() -> Vec<SkillDefinition> {
    let mut skills = Vec::new();
    let mut dirs_to_scan: Vec<PathBuf> = Vec::new();

    // Project skills: .openclaudia/skills/
    let project_dir = PathBuf::from(".openclaudia/skills");
    if project_dir.exists() {
        dirs_to_scan.push(project_dir);
    }

    // User skills: ~/.openclaudia/skills/
    if let Some(home) = dirs::home_dir() {
        let user_dir = home.join(".openclaudia/skills");
        if user_dir.exists() {
            dirs_to_scan.push(user_dir);
        }
    }

    for dir in dirs_to_scan {
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for entry in entries.flatten() {
                let path = entry.path();

                if path.is_dir() {
                    // Look for SKILL.md inside subdirectory
                    let skill_file = path.join("SKILL.md");
                    if skill_file.exists() {
                        if let Some(mut skill) = parse_skill_file(&skill_file) {
                            // Use directory name as skill name if not set
                            if skill.name.is_empty() {
                                skill.name = path
                                    .file_name()
                                    .and_then(|n| n.to_str())
                                    .unwrap_or("unknown")
                                    .to_string();
                            }
                            skills.push(skill);
                        }
                    }
                } else if path.extension().and_then(|e| e.to_str()) == Some("md") {
                    // Direct .md file in skills dir
                    if let Some(mut skill) = parse_skill_file(&path) {
                        if skill.name.is_empty() {
                            skill.name = path
                                .file_stem()
                                .and_then(|n| n.to_str())
                                .unwrap_or("unknown")
                                .to_string();
                        }
                        skills.push(skill);
                    }
                }
            }
        }
    }

    // Deduplicate by name (project skills take priority over user skills)
    let mut seen = std::collections::HashSet::new();
    skills.retain(|s| seen.insert(s.name.clone()));

    skills
}

/// Get a skill by name
pub fn get_skill(name: &str) -> Option<SkillDefinition> {
    load_skills().into_iter().find(|s| s.name == name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_skill_file() {
        let content =
            "---\nname: test-skill\ndescription: A test skill\n---\n\nYou are a test agent.";
        let tmp = std::env::temp_dir().join("test_skill.md");
        std::fs::write(&tmp, content).unwrap();

        let skill = parse_skill_file(&tmp).unwrap();
        assert_eq!(skill.name, "test-skill");
        assert_eq!(skill.description, "A test skill");
        assert_eq!(skill.prompt, "You are a test agent.");

        std::fs::remove_file(&tmp).ok();
    }

    #[test]
    fn test_parse_skill_no_frontmatter() {
        let tmp = std::env::temp_dir().join("no_fm.md");
        std::fs::write(&tmp, "Just plain text").unwrap();
        assert!(parse_skill_file(&tmp).is_none());
        std::fs::remove_file(&tmp).ok();
    }

    #[test]
    fn test_parse_skill_with_tools() {
        let content = "---\nname: coder\ndescription: Codes stuff\nallowed_tools:\n  - bash\n  - edit_file\n---\n\nWrite code.";
        let tmp = std::env::temp_dir().join("tools_skill.md");
        std::fs::write(&tmp, content).unwrap();

        let skill = parse_skill_file(&tmp).unwrap();
        assert_eq!(
            skill.allowed_tools,
            Some(vec!["bash".to_string(), "edit_file".to_string()])
        );

        std::fs::remove_file(&tmp).ok();
    }

    #[test]
    fn test_load_skills_empty() {
        // Should not panic even if dirs don't exist
        let _skills = load_skills();
    }
}
