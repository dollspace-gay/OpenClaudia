//! Guardrails module for coding safety enforcement
//!
//! Provides three guardrail mechanisms:
//! - **Blast radius limiting**: constrains file/scope access per request
//! - **Diff size monitoring**: flags when changes exceed expected scope
//! - **Quality gates**: automated code quality checks
//!
//! Also provides language detection utilities shared with the VDD engine.

use regex::Regex;
use std::collections::HashSet;
use std::path::Path;
use std::process::Command;
use std::sync::Mutex;
use tracing::{debug, info, warn};

use crate::config::{
    BlastRadiusConfig, DiffMonitorConfig, GuardrailAction, GuardrailMode, GuardrailsConfig,
    QualityGatesConfig,
};

// ==========================================================================
// Global guardrails instance
// ==========================================================================

static GUARDRAILS: std::sync::LazyLock<Mutex<Option<GuardrailsEngine>>> =
    std::sync::LazyLock::new(|| Mutex::new(None));

/// Initialize the guardrails engine from config. Called once at startup.
pub fn configure(config: &GuardrailsConfig) {
    if let Ok(mut guard) = GUARDRAILS.lock() {
        *guard = Some(GuardrailsEngine::from_config(config));
        info!("Guardrails engine configured");
    }
}

/// Check if a file path is allowed by blast radius rules.
/// Returns Ok(()) if allowed, Err(message) if blocked in strict mode.
pub fn check_file_access(path: &str) -> Result<(), String> {
    if let Ok(guard) = GUARDRAILS.lock() {
        if let Some(engine) = guard.as_ref() {
            return engine.check_file_access(path);
        }
    }
    Ok(())
}

/// Record a file modification for diff monitoring.
/// Call after successful write_file or edit_file.
pub fn record_file_modification(path: &str, lines_added: u32, lines_removed: u32) {
    if let Ok(guard) = GUARDRAILS.lock() {
        if let Some(engine) = guard.as_ref() {
            engine.record_modification(path, lines_added, lines_removed);
        }
    }
}

/// Check diff thresholds. Returns a warning if thresholds exceeded.
pub fn check_diff_thresholds() -> Option<DiffWarning> {
    if let Ok(guard) = GUARDRAILS.lock() {
        if let Some(engine) = guard.as_ref() {
            return engine.check_diff_thresholds();
        }
    }
    None
}

/// Run quality gate checks. Returns results for each configured check.
pub fn run_quality_gates() -> Vec<QualityCheckResult> {
    if let Ok(guard) = GUARDRAILS.lock() {
        if let Some(engine) = guard.as_ref() {
            return engine.run_quality_gates();
        }
    }
    Vec::new()
}

/// Reset per-turn tracking (blast radius file count).
pub fn reset_turn() {
    if let Ok(guard) = GUARDRAILS.lock() {
        if let Some(engine) = guard.as_ref() {
            engine.reset_turn();
        }
    }
}

/// Get current diff stats summary.
pub fn get_diff_summary() -> Option<DiffStats> {
    if let Ok(guard) = GUARDRAILS.lock() {
        if let Some(engine) = guard.as_ref() {
            return engine.get_diff_stats();
        }
    }
    None
}

// ==========================================================================
// Public Types
// ==========================================================================

/// Warning emitted when diff thresholds are exceeded
#[derive(Debug, Clone)]
pub struct DiffWarning {
    pub message: String,
    pub stats: DiffStats,
    pub action: GuardrailAction,
}

/// Accumulated diff statistics for the session
#[derive(Debug, Clone, Default)]
pub struct DiffStats {
    pub lines_added: u32,
    pub lines_removed: u32,
    pub lines_changed: u32,
    pub files_changed: u32,
    pub file_list: Vec<String>,
}

/// Result of running a single quality gate check
#[derive(Debug, Clone)]
pub struct QualityCheckResult {
    pub name: String,
    pub command: String,
    pub passed: bool,
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
    pub required: bool,
}

// ==========================================================================
// GuardrailsEngine
// ==========================================================================

struct GuardrailsEngine {
    blast_radius: Option<BlastRadiusGuard>,
    diff_monitor: Option<DiffMonitor>,
    quality_gates: Option<QualityGateRunner>,
}

impl GuardrailsEngine {
    fn from_config(config: &GuardrailsConfig) -> Self {
        let blast_radius = config.blast_radius.as_ref().filter(|c| c.enabled).map(|c| {
            info!(
                mode = %c.mode,
                allowed = c.allowed_paths.len(),
                denied = c.denied_paths.len(),
                max_files = c.max_files_per_turn,
                "Blast radius guard enabled"
            );
            BlastRadiusGuard::new(c.clone())
        });

        let diff_monitor = config.diff_monitor.as_ref().filter(|c| c.enabled).map(|c| {
            info!(
                max_lines = c.max_lines_changed,
                max_files = c.max_files_changed,
                action = %c.action,
                "Diff monitor enabled"
            );
            DiffMonitor::new(c.clone())
        });

        let quality_gates = config
            .quality_gates
            .as_ref()
            .filter(|c| c.enabled)
            .map(|c| {
                info!(
                    checks = c.checks.len(),
                    run_after = %c.run_after,
                    "Quality gates enabled"
                );
                QualityGateRunner::new(c.clone())
            });

        Self {
            blast_radius,
            diff_monitor,
            quality_gates,
        }
    }

    fn check_file_access(&self, path: &str) -> Result<(), String> {
        if let Some(br) = &self.blast_radius {
            br.check_path(path)?;
            br.record_access(path)?;
        }
        Ok(())
    }

    fn record_modification(&self, path: &str, lines_added: u32, lines_removed: u32) {
        if let Some(dm) = &self.diff_monitor {
            dm.record(path, lines_added, lines_removed);
        }
    }

    fn check_diff_thresholds(&self) -> Option<DiffWarning> {
        self.diff_monitor
            .as_ref()
            .and_then(|dm| dm.check_thresholds())
    }

    fn run_quality_gates(&self) -> Vec<QualityCheckResult> {
        self.quality_gates
            .as_ref()
            .map(|qg| qg.run())
            .unwrap_or_default()
    }

    fn reset_turn(&self) {
        if let Some(br) = &self.blast_radius {
            br.reset_turn();
        }
    }

    fn get_diff_stats(&self) -> Option<DiffStats> {
        self.diff_monitor.as_ref().map(|dm| dm.get_stats())
    }
}

// ==========================================================================
// Blast Radius Guard
// ==========================================================================

struct BlastRadiusGuard {
    config: BlastRadiusConfig,
    allowed_patterns: Vec<Regex>,
    denied_patterns: Vec<Regex>,
    files_this_turn: Mutex<HashSet<String>>,
}

impl BlastRadiusGuard {
    fn new(config: BlastRadiusConfig) -> Self {
        let allowed_patterns = config
            .allowed_paths
            .iter()
            .filter_map(|p| {
                glob_to_regex(p)
                    .map_err(|e| warn!("Invalid allowed glob '{}': {}", p, e))
                    .ok()
            })
            .collect();

        let denied_patterns = config
            .denied_paths
            .iter()
            .filter_map(|p| {
                glob_to_regex(p)
                    .map_err(|e| warn!("Invalid denied glob '{}': {}", p, e))
                    .ok()
            })
            .collect();

        Self {
            config,
            allowed_patterns,
            denied_patterns,
            files_this_turn: Mutex::new(HashSet::new()),
        }
    }

    fn check_path(&self, path: &str) -> Result<(), String> {
        let normalized = normalize_path(path);

        // Denied paths take priority
        for pattern in &self.denied_patterns {
            if pattern.is_match(&normalized) {
                let msg = format!("Blast radius: path '{}' matches deny list pattern", path);
                return match self.config.mode {
                    GuardrailMode::Strict => {
                        warn!("{} (BLOCKED)", msg);
                        Err(msg)
                    }
                    GuardrailMode::Advisory => {
                        warn!("{} (advisory)", msg);
                        Ok(())
                    }
                };
            }
        }

        // If allowed_paths configured, path must match at least one
        if !self.allowed_patterns.is_empty() {
            let allowed = self
                .allowed_patterns
                .iter()
                .any(|p| p.is_match(&normalized));
            if !allowed {
                let msg = format!("Blast radius: path '{}' not in allowed list", path);
                return match self.config.mode {
                    GuardrailMode::Strict => {
                        warn!("{} (BLOCKED)", msg);
                        Err(msg)
                    }
                    GuardrailMode::Advisory => {
                        warn!("{} (advisory)", msg);
                        Ok(())
                    }
                };
            }
        }

        Ok(())
    }

    fn record_access(&self, path: &str) -> Result<(), String> {
        if self.config.max_files_per_turn == 0 {
            return Ok(());
        }

        let normalized = normalize_path(path);
        if let Ok(mut files) = self.files_this_turn.lock() {
            files.insert(normalized);
            if files.len() as u32 > self.config.max_files_per_turn {
                let msg = format!(
                    "Blast radius: exceeded max files per turn ({}/{})",
                    files.len(),
                    self.config.max_files_per_turn
                );
                return match self.config.mode {
                    GuardrailMode::Strict => {
                        warn!("{} (BLOCKED)", msg);
                        Err(msg)
                    }
                    GuardrailMode::Advisory => {
                        warn!("{} (advisory)", msg);
                        Ok(())
                    }
                };
            }
        }
        Ok(())
    }

    fn reset_turn(&self) {
        if let Ok(mut files) = self.files_this_turn.lock() {
            files.clear();
        }
    }
}

// ==========================================================================
// Diff Monitor
// ==========================================================================

struct DiffMonitor {
    config: DiffMonitorConfig,
    stats: Mutex<DiffStatsInternal>,
}

struct DiffStatsInternal {
    lines_added: u32,
    lines_removed: u32,
    files: HashSet<String>,
}

impl DiffMonitor {
    fn new(config: DiffMonitorConfig) -> Self {
        Self {
            config,
            stats: Mutex::new(DiffStatsInternal {
                lines_added: 0,
                lines_removed: 0,
                files: HashSet::new(),
            }),
        }
    }

    fn record(&self, path: &str, lines_added: u32, lines_removed: u32) {
        if let Ok(mut stats) = self.stats.lock() {
            stats.lines_added += lines_added;
            stats.lines_removed += lines_removed;
            stats.files.insert(normalize_path(path));
            debug!(
                path = path,
                added = lines_added,
                removed = lines_removed,
                total_files = stats.files.len(),
                "Diff monitor: recorded modification"
            );
        }
    }

    fn check_thresholds(&self) -> Option<DiffWarning> {
        if let Ok(stats) = self.stats.lock() {
            let total_lines = stats.lines_added + stats.lines_removed;
            let total_files = stats.files.len() as u32;

            let mut warnings = Vec::new();

            if self.config.max_lines_changed > 0 && total_lines > self.config.max_lines_changed {
                warnings.push(format!(
                    "lines changed {}/{}",
                    total_lines, self.config.max_lines_changed
                ));
            }

            if self.config.max_files_changed > 0 && total_files > self.config.max_files_changed {
                warnings.push(format!(
                    "files changed {}/{}",
                    total_files, self.config.max_files_changed
                ));
            }

            if warnings.is_empty() {
                return None;
            }

            let message = format!("Diff size threshold exceeded: {}", warnings.join(", "));
            warn!("{}", message);

            Some(DiffWarning {
                message,
                stats: DiffStats {
                    lines_added: stats.lines_added,
                    lines_removed: stats.lines_removed,
                    lines_changed: total_lines,
                    files_changed: total_files,
                    file_list: stats.files.iter().cloned().collect(),
                },
                action: self.config.action.clone(),
            })
        } else {
            None
        }
    }

    fn get_stats(&self) -> DiffStats {
        if let Ok(stats) = self.stats.lock() {
            DiffStats {
                lines_added: stats.lines_added,
                lines_removed: stats.lines_removed,
                lines_changed: stats.lines_added + stats.lines_removed,
                files_changed: stats.files.len() as u32,
                file_list: stats.files.iter().cloned().collect(),
            }
        } else {
            DiffStats::default()
        }
    }
}

// ==========================================================================
// Quality Gate Runner
// ==========================================================================

struct QualityGateRunner {
    config: QualityGatesConfig,
}

impl QualityGateRunner {
    fn new(config: QualityGatesConfig) -> Self {
        Self { config }
    }

    fn run(&self) -> Vec<QualityCheckResult> {
        let mut results = Vec::new();

        for check in &self.config.checks {
            info!(name = %check.name, "Running quality gate");

            let (exit_code, stdout, stderr) =
                run_shell_command_sync(&check.command, self.config.timeout_seconds);

            let passed = exit_code == 0;
            if !passed && check.required {
                warn!(name = %check.name, exit_code, "Required quality gate FAILED");
            } else if passed {
                debug!(name = %check.name, "Quality gate passed");
            }

            results.push(QualityCheckResult {
                name: check.name.clone(),
                command: check.command.clone(),
                passed,
                exit_code,
                stdout,
                stderr,
                required: check.required,
            });
        }

        results
    }
}

/// Run a shell command synchronously with timeout.
fn run_shell_command_sync(command: &str, _timeout_seconds: u64) -> (i32, String, String) {
    let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));

    #[cfg(windows)]
    let output = {
        let paths = [
            r"C:\Program Files\Git\bin\bash.exe",
            r"C:\Program Files (x86)\Git\bin\bash.exe",
        ];
        let bash = paths
            .iter()
            .find(|p| Path::new(p).exists())
            .map(|&p| std::path::PathBuf::from(p))
            .unwrap_or_else(|| std::path::PathBuf::from("bash"));

        Command::new(bash)
            .args(["-c", command])
            .current_dir(&cwd)
            .output()
    };

    #[cfg(not(windows))]
    let output = Command::new("bash")
        .args(["-c", command])
        .current_dir(&cwd)
        .output();

    match output {
        Ok(output) => {
            let exit_code = output.status.code().unwrap_or(-1);
            let stdout = String::from_utf8_lossy(&output.stdout).to_string();
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            (exit_code, stdout, stderr)
        }
        Err(e) => (-1, String::new(), format!("Failed to execute: {}", e)),
    }
}

// ==========================================================================
// Language Detection (shared with VDD)
// ==========================================================================

/// Detected project language
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ProjectLanguage {
    Rust,
    JavaScript,
    TypeScript,
    Python,
    Go,
    Java,
    Kotlin,
    Ruby,
    PHP,
    CSharp,
    Cpp,
    C,
}

impl std::fmt::Display for ProjectLanguage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Rust => write!(f, "Rust"),
            Self::JavaScript => write!(f, "JavaScript"),
            Self::TypeScript => write!(f, "TypeScript"),
            Self::Python => write!(f, "Python"),
            Self::Go => write!(f, "Go"),
            Self::Java => write!(f, "Java"),
            Self::Kotlin => write!(f, "Kotlin"),
            Self::Ruby => write!(f, "Ruby"),
            Self::PHP => write!(f, "PHP"),
            Self::CSharp => write!(f, "C#"),
            Self::Cpp => write!(f, "C++"),
            Self::C => write!(f, "C"),
        }
    }
}

/// Detect project languages by checking for marker files in the working directory.
pub fn detect_project_languages() -> Vec<ProjectLanguage> {
    let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    detect_languages_in_dir(&cwd)
}

/// Detect languages in a specific directory.
pub fn detect_languages_in_dir(dir: &Path) -> Vec<ProjectLanguage> {
    let mut languages = Vec::new();

    let markers: &[(ProjectLanguage, &[&str])] = &[
        (ProjectLanguage::Rust, &["Cargo.toml"]),
        (ProjectLanguage::TypeScript, &["tsconfig.json"]),
        (ProjectLanguage::JavaScript, &["package.json"]),
        (
            ProjectLanguage::Python,
            &["pyproject.toml", "setup.py", "requirements.txt", "Pipfile"],
        ),
        (ProjectLanguage::Go, &["go.mod"]),
        (
            ProjectLanguage::Java,
            &["pom.xml", "build.gradle", "build.gradle.kts"],
        ),
        (ProjectLanguage::Ruby, &["Gemfile"]),
        (ProjectLanguage::PHP, &["composer.json"]),
        (ProjectLanguage::Cpp, &["CMakeLists.txt"]),
    ];

    for (lang, files) in markers {
        for file in *files {
            if dir.join(file).exists() {
                if !languages.contains(lang) {
                    languages.push(lang.clone());
                }
                break;
            }
        }
    }

    // TypeScript detection: if we found package.json but also have tsconfig,
    // the TypeScript entry was already added by the marker check above.
    // If we found package.json but NOT tsconfig, it's JavaScript.
    // Remove JavaScript if TypeScript is already detected (tsconfig present).
    if languages.contains(&ProjectLanguage::TypeScript)
        && languages.contains(&ProjectLanguage::JavaScript)
    {
        languages.retain(|l| l != &ProjectLanguage::JavaScript);
    }

    // Kotlin: if build.gradle.kts exists, add Kotlin alongside Java
    if dir.join("build.gradle.kts").exists() && !languages.contains(&ProjectLanguage::Kotlin) {
        languages.push(ProjectLanguage::Kotlin);
    }

    // C# detection: .sln or .csproj files
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.ends_with(".sln") || name.ends_with(".csproj") {
                if !languages.contains(&ProjectLanguage::CSharp) {
                    languages.push(ProjectLanguage::CSharp);
                }
                break;
            }
        }
    }

    // C detection: Makefile with .c/.h files but no CMakeLists
    if languages.is_empty() && dir.join("Makefile").exists() {
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                if name.ends_with(".c") || name.ends_with(".h") {
                    if !languages.contains(&ProjectLanguage::C) {
                        languages.push(ProjectLanguage::C);
                    }
                    break;
                }
                if name.ends_with(".cpp") || name.ends_with(".cc") || name.ends_with(".hpp") {
                    if !languages.contains(&ProjectLanguage::Cpp) {
                        languages.push(ProjectLanguage::Cpp);
                    }
                    break;
                }
            }
        }
    }

    debug!("Detected project languages: {:?}", languages);
    languages
}

/// Get default static analysis commands for a detected language.
/// Returns Vec<(name, command)>.
pub fn get_default_analysis_commands(lang: &ProjectLanguage) -> Vec<(String, String)> {
    match lang {
        ProjectLanguage::Rust => vec![
            (
                "clippy".to_string(),
                "cargo clippy -- -D warnings".to_string(),
            ),
            ("test".to_string(), "cargo test --no-fail-fast".to_string()),
        ],
        ProjectLanguage::JavaScript => {
            vec![("eslint".to_string(), "npx eslint .".to_string())]
        }
        ProjectLanguage::TypeScript => {
            let mut cmds = vec![("tsc".to_string(), "npx tsc --noEmit".to_string())];
            cmds.push(("eslint".to_string(), "npx eslint .".to_string()));
            cmds
        }
        ProjectLanguage::Python => {
            vec![
                ("ruff".to_string(), "ruff check .".to_string()),
                ("pytest".to_string(), "pytest --tb=short -q".to_string()),
            ]
        }
        ProjectLanguage::Go => vec![
            ("vet".to_string(), "go vet ./...".to_string()),
            ("test".to_string(), "go test ./...".to_string()),
        ],
        ProjectLanguage::Java => {
            if Path::new("pom.xml").exists() {
                vec![("maven".to_string(), "mvn compile -q".to_string())]
            } else {
                vec![("gradle".to_string(), "gradle build -q".to_string())]
            }
        }
        ProjectLanguage::Kotlin => {
            vec![("gradle".to_string(), "gradle build -q".to_string())]
        }
        ProjectLanguage::Ruby => {
            vec![("rubocop".to_string(), "rubocop".to_string())]
        }
        ProjectLanguage::PHP => {
            vec![("phpstan".to_string(), "phpstan analyse".to_string())]
        }
        ProjectLanguage::CSharp => {
            vec![(
                "dotnet".to_string(),
                "dotnet build --no-restore".to_string(),
            )]
        }
        ProjectLanguage::Cpp | ProjectLanguage::C => {
            if Path::new("CMakeLists.txt").exists() {
                vec![("cmake".to_string(), "cmake --build build".to_string())]
            } else if Path::new("Makefile").exists() {
                vec![("make".to_string(), "make".to_string())]
            } else {
                Vec::new()
            }
        }
    }
}

/// Get auto-detected static analysis commands for the current project.
/// Used by VDD when auto_detect is enabled and no explicit commands are configured.
pub fn get_auto_detected_commands() -> Vec<String> {
    let languages = detect_project_languages();
    let mut commands = Vec::new();

    for lang in &languages {
        for (_name, cmd) in get_default_analysis_commands(lang) {
            if !commands.contains(&cmd) {
                commands.push(cmd);
            }
        }
    }

    if !commands.is_empty() {
        info!(
            languages = ?languages.iter().map(|l| l.to_string()).collect::<Vec<_>>(),
            commands = ?commands,
            "Auto-detected static analysis commands"
        );
    }

    commands
}

// ==========================================================================
// Glob Pattern Matching Utilities
// ==========================================================================

/// Convert a glob pattern to a regex.
fn glob_to_regex(pattern: &str) -> Result<Regex, regex::Error> {
    let mut regex = String::from("^");
    let chars: Vec<char> = pattern.chars().collect();
    let mut i = 0;

    while i < chars.len() {
        match chars[i] {
            '*' => {
                if i + 1 < chars.len() && chars[i + 1] == '*' {
                    if i + 2 < chars.len() && chars[i + 2] == '/' {
                        // **/ matches zero or more directories
                        regex.push_str("(.*/)?");
                        i += 3;
                    } else {
                        // ** at end matches everything
                        regex.push_str(".*");
                        i += 2;
                    }
                } else {
                    // * matches everything except /
                    regex.push_str("[^/]*");
                    i += 1;
                }
            }
            '?' => {
                regex.push_str("[^/]");
                i += 1;
            }
            '.' | '(' | ')' | '[' | ']' | '{' | '}' | '+' | '^' | '$' | '|' | '\\' => {
                regex.push('\\');
                regex.push(chars[i]);
                i += 1;
            }
            c => {
                regex.push(c);
                i += 1;
            }
        }
    }

    regex.push('$');
    Regex::new(&regex)
}

/// Normalize a file path for matching (forward slashes, no leading ./).
fn normalize_path(path: &str) -> String {
    let normalized = path.replace('\\', "/");
    let normalized = normalized.strip_prefix("./").unwrap_or(&normalized);
    normalized.to_string()
}

// ==========================================================================
// Tests
// ==========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::QualityCheck;

    // ====== Glob matching tests ======

    #[test]
    fn test_glob_exact_match() {
        let re = glob_to_regex("src/main.rs").unwrap();
        assert!(re.is_match("src/main.rs"));
        assert!(!re.is_match("src/lib.rs"));
    }

    #[test]
    fn test_glob_star() {
        let re = glob_to_regex("src/*.rs").unwrap();
        assert!(re.is_match("src/main.rs"));
        assert!(re.is_match("src/lib.rs"));
        assert!(!re.is_match("src/sub/mod.rs"));
        assert!(!re.is_match("tests/test.rs"));
    }

    #[test]
    fn test_glob_double_star() {
        let re = glob_to_regex("src/**").unwrap();
        assert!(re.is_match("src/main.rs"));
        assert!(re.is_match("src/sub/mod.rs"));
        assert!(re.is_match("src/a/b/c.rs"));
    }

    #[test]
    fn test_glob_double_star_prefix() {
        let re = glob_to_regex("**/*.rs").unwrap();
        assert!(re.is_match("src/main.rs"));
        assert!(re.is_match("tests/test.rs"));
        assert!(re.is_match("a/b/c.rs"));
    }

    #[test]
    fn test_glob_dot_env() {
        let re = glob_to_regex(".env*").unwrap();
        assert!(re.is_match(".env"));
        assert!(re.is_match(".env.local"));
        assert!(re.is_match(".envrc"));
        assert!(!re.is_match("src/.env"));
    }

    #[test]
    fn test_normalize_path() {
        assert_eq!(normalize_path("src\\main.rs"), "src/main.rs");
        assert_eq!(normalize_path("./src/main.rs"), "src/main.rs");
        assert_eq!(normalize_path("src/main.rs"), "src/main.rs");
    }

    // ====== Blast radius tests ======

    #[test]
    fn test_blast_radius_denied_strict() {
        let config = BlastRadiusConfig {
            enabled: true,
            mode: GuardrailMode::Strict,
            allowed_paths: vec![],
            denied_paths: vec![".env*".to_string(), ".git/**".to_string()],
            max_files_per_turn: 0,
        };
        let guard = BlastRadiusGuard::new(config);

        assert!(guard.check_path("src/main.rs").is_ok());
        assert!(guard.check_path(".env").is_err());
        assert!(guard.check_path(".env.local").is_err());
        assert!(guard.check_path(".git/config").is_err());
    }

    #[test]
    fn test_blast_radius_allowed_strict() {
        let config = BlastRadiusConfig {
            enabled: true,
            mode: GuardrailMode::Strict,
            allowed_paths: vec!["src/**".to_string(), "tests/**".to_string()],
            denied_paths: vec![],
            max_files_per_turn: 0,
        };
        let guard = BlastRadiusGuard::new(config);

        assert!(guard.check_path("src/main.rs").is_ok());
        assert!(guard.check_path("tests/test.rs").is_ok());
        assert!(guard.check_path("config.yaml").is_err());
    }

    #[test]
    fn test_blast_radius_advisory_allows() {
        let config = BlastRadiusConfig {
            enabled: true,
            mode: GuardrailMode::Advisory,
            allowed_paths: vec!["src/**".to_string()],
            denied_paths: vec![],
            max_files_per_turn: 0,
        };
        let guard = BlastRadiusGuard::new(config);

        // Advisory mode warns but doesn't block
        assert!(guard.check_path("config.yaml").is_ok());
    }

    #[test]
    fn test_blast_radius_max_files() {
        let config = BlastRadiusConfig {
            enabled: true,
            mode: GuardrailMode::Strict,
            allowed_paths: vec![],
            denied_paths: vec![],
            max_files_per_turn: 2,
        };
        let guard = BlastRadiusGuard::new(config);

        assert!(guard.record_access("file1.rs").is_ok());
        assert!(guard.record_access("file2.rs").is_ok());
        assert!(guard.record_access("file3.rs").is_err());
    }

    #[test]
    fn test_blast_radius_reset_turn() {
        let config = BlastRadiusConfig {
            enabled: true,
            mode: GuardrailMode::Strict,
            allowed_paths: vec![],
            denied_paths: vec![],
            max_files_per_turn: 1,
        };
        let guard = BlastRadiusGuard::new(config);

        assert!(guard.record_access("file1.rs").is_ok());
        assert!(guard.record_access("file2.rs").is_err());

        guard.reset_turn();
        assert!(guard.record_access("file3.rs").is_ok());
    }

    // ====== Diff monitor tests ======

    #[test]
    fn test_diff_monitor_basic() {
        let config = DiffMonitorConfig {
            enabled: true,
            max_lines_changed: 100,
            max_files_changed: 5,
            action: GuardrailAction::Warn,
        };
        let monitor = DiffMonitor::new(config);

        monitor.record("file1.rs", 10, 5);
        monitor.record("file2.rs", 20, 10);

        let stats = monitor.get_stats();
        assert_eq!(stats.lines_added, 30);
        assert_eq!(stats.lines_removed, 15);
        assert_eq!(stats.lines_changed, 45);
        assert_eq!(stats.files_changed, 2);
    }

    #[test]
    fn test_diff_monitor_threshold_not_exceeded() {
        let config = DiffMonitorConfig {
            enabled: true,
            max_lines_changed: 100,
            max_files_changed: 5,
            action: GuardrailAction::Warn,
        };
        let monitor = DiffMonitor::new(config);

        monitor.record("file1.rs", 10, 5);
        assert!(monitor.check_thresholds().is_none());
    }

    #[test]
    fn test_diff_monitor_threshold_exceeded() {
        let config = DiffMonitorConfig {
            enabled: true,
            max_lines_changed: 20,
            max_files_changed: 5,
            action: GuardrailAction::Warn,
        };
        let monitor = DiffMonitor::new(config);

        monitor.record("file1.rs", 15, 10);

        let warning = monitor.check_thresholds();
        assert!(warning.is_some());
        let w = warning.unwrap();
        assert!(w.message.contains("lines changed"));
        assert_eq!(w.stats.lines_changed, 25);
    }

    #[test]
    fn test_diff_monitor_files_threshold() {
        let config = DiffMonitorConfig {
            enabled: true,
            max_lines_changed: 0,
            max_files_changed: 2,
            action: GuardrailAction::Block,
        };
        let monitor = DiffMonitor::new(config);

        monitor.record("a.rs", 1, 0);
        monitor.record("b.rs", 1, 0);
        assert!(monitor.check_thresholds().is_none());

        monitor.record("c.rs", 1, 0);
        let warning = monitor.check_thresholds();
        assert!(warning.is_some());
        assert!(warning.unwrap().message.contains("files changed"));
    }

    // ====== Quality gates tests ======

    #[test]
    fn test_quality_gate_passing_command() {
        let config = QualityGatesConfig {
            enabled: true,
            run_after: crate::config::RunAfter::EveryTurn,
            fail_action: GuardrailAction::Warn,
            checks: vec![QualityCheck {
                name: "echo".to_string(),
                command: "echo ok".to_string(),
                required: true,
            }],
            timeout_seconds: 30,
        };
        let runner = QualityGateRunner::new(config);
        let results = runner.run();

        assert_eq!(results.len(), 1);
        assert!(results[0].passed);
        assert_eq!(results[0].exit_code, 0);
    }

    #[test]
    fn test_quality_gate_failing_command() {
        let config = QualityGatesConfig {
            enabled: true,
            run_after: crate::config::RunAfter::EveryTurn,
            fail_action: GuardrailAction::Warn,
            checks: vec![QualityCheck {
                name: "fail".to_string(),
                command: "exit 1".to_string(),
                required: false,
            }],
            timeout_seconds: 30,
        };
        let runner = QualityGateRunner::new(config);
        let results = runner.run();

        assert_eq!(results.len(), 1);
        assert!(!results[0].passed);
    }

    // ====== Language detection tests ======

    #[test]
    fn test_detect_rust_project() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), "[package]\nname = \"test\"").unwrap();

        let langs = detect_languages_in_dir(dir.path());
        assert!(langs.contains(&ProjectLanguage::Rust));
    }

    #[test]
    fn test_detect_python_project() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(dir.path().join("requirements.txt"), "flask\n").unwrap();

        let langs = detect_languages_in_dir(dir.path());
        assert!(langs.contains(&ProjectLanguage::Python));
    }

    #[test]
    fn test_detect_typescript_project() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(dir.path().join("tsconfig.json"), "{}").unwrap();
        std::fs::write(dir.path().join("package.json"), "{}").unwrap();

        let langs = detect_languages_in_dir(dir.path());
        assert!(langs.contains(&ProjectLanguage::TypeScript));
        // JavaScript should be deduped when TypeScript is present
        assert!(!langs.contains(&ProjectLanguage::JavaScript));
    }

    #[test]
    fn test_detect_javascript_only() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(dir.path().join("package.json"), "{}").unwrap();

        let langs = detect_languages_in_dir(dir.path());
        assert!(langs.contains(&ProjectLanguage::JavaScript));
        assert!(!langs.contains(&ProjectLanguage::TypeScript));
    }

    #[test]
    fn test_detect_go_project() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(dir.path().join("go.mod"), "module test").unwrap();

        let langs = detect_languages_in_dir(dir.path());
        assert!(langs.contains(&ProjectLanguage::Go));
    }

    #[test]
    fn test_detect_empty_dir() {
        let dir = tempfile::TempDir::new().unwrap();
        let langs = detect_languages_in_dir(dir.path());
        assert!(langs.is_empty());
    }

    #[test]
    fn test_detect_multi_language() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), "").unwrap();
        std::fs::write(dir.path().join("package.json"), "{}").unwrap();

        let langs = detect_languages_in_dir(dir.path());
        assert!(langs.contains(&ProjectLanguage::Rust));
        assert!(langs.contains(&ProjectLanguage::JavaScript));
    }

    #[test]
    fn test_default_commands_rust() {
        let cmds = get_default_analysis_commands(&ProjectLanguage::Rust);
        assert_eq!(cmds.len(), 2);
        assert!(cmds[0].1.contains("clippy"));
        assert!(cmds[1].1.contains("cargo test"));
    }

    #[test]
    fn test_default_commands_python() {
        let cmds = get_default_analysis_commands(&ProjectLanguage::Python);
        assert!(!cmds.is_empty());
        assert!(cmds.iter().any(|(name, _)| name == "ruff"));
    }

    #[test]
    fn test_project_language_display() {
        assert_eq!(ProjectLanguage::Rust.to_string(), "Rust");
        assert_eq!(ProjectLanguage::TypeScript.to_string(), "TypeScript");
        assert_eq!(ProjectLanguage::CSharp.to_string(), "C#");
        assert_eq!(ProjectLanguage::Cpp.to_string(), "C++");
    }

    // ====== Global API tests ======

    #[test]
    fn test_unconfigured_guardrails_allow_all() {
        // Without configuration, all operations should pass
        assert!(check_file_access("any/file.rs").is_ok());
        assert!(check_diff_thresholds().is_none());
        assert!(run_quality_gates().is_empty());
    }
}
