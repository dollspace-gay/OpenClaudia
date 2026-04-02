mod kill;
mod output;

pub use kill::{execute_kill_shell, terminate_process_tree};
pub use output::execute_bash_output;

use crate::tools::safe_truncate;
use serde_json::Value;
use std::collections::HashMap;
use std::io::{BufRead, BufReader};
#[cfg(unix)]
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use uuid::Uuid;

/// Maximum number of background shells allowed before refusing new ones
const MAX_BACKGROUND_SHELLS: usize = 50;

/// Background shell process with captured output
struct BackgroundShell {
    stdout_buffer: Arc<Mutex<Vec<String>>>,
    stderr_buffer: Arc<Mutex<Vec<String>>>,
    command: String,
    finished: Arc<AtomicBool>,
    exit_status: Arc<Mutex<Option<i32>>>,
    /// PID of the spawned process, used to send SIGTERM on kill
    pid: u32,
    /// Whether output has been retrieved at least once after the process finished
    output_retrieved_after_finish: AtomicBool,
}

/// Manager for background shell processes
pub struct BackgroundShellManager {
    shells: Mutex<HashMap<String, BackgroundShell>>,
}

impl BackgroundShellManager {
    fn new() -> Self {
        Self {
            shells: Mutex::new(HashMap::new()),
        }
    }

    /// Spawn a new background shell and return its ID
    pub(crate) fn spawn(&self, command: &str) -> Result<String, String> {
        let shell_id = safe_truncate(&Uuid::new_v4().to_string(), 8).to_string();
        // IMPORTANT: Set current_dir to ensure bash runs in the same directory as the process
        let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));

        #[cfg(windows)]
        let child = {
            match find_git_bash() {
                Some(git_bash) => Command::new(git_bash)
                    .args(["-c", command])
                    .current_dir(&cwd)
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped())
                    .spawn(),
                None => Command::new("bash")
                    .args(["-c", command])
                    .current_dir(&cwd)
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped())
                    .spawn(),
            }
        };

        #[cfg(not(windows))]
        let child = {
            let mut cmd = Command::new("bash");
            cmd.args(["-c", command])
                .current_dir(&cwd)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .process_group(0); // Put child in its own process group for clean kill
            cmd.spawn()
        };

        // Enforce maximum shell limit BEFORE spawning the process
        if let Ok(mut shells) = self.shells.lock() {
            // GC sweep: remove finished shells whose output has been retrieved at least once
            shells.retain(|_id, s| {
                let is_finished = s.finished.load(Ordering::SeqCst);
                let output_retrieved = s.output_retrieved_after_finish.load(Ordering::SeqCst);
                !is_finished || !output_retrieved
            });

            if shells.len() >= MAX_BACKGROUND_SHELLS {
                return Err(format!(
                    "Maximum background shell limit ({MAX_BACKGROUND_SHELLS}) reached. Kill or wait for existing shells to finish."
                ));
            }
        }

        let mut child = child.map_err(|e| format!("Failed to spawn background shell: {e}"))?;

        // Capture PID before moving the child handle into the wait thread
        let pid = child.id();

        let stdout_buffer = Arc::new(Mutex::new(Vec::new()));
        let stderr_buffer = Arc::new(Mutex::new(Vec::new()));
        let finished = Arc::new(AtomicBool::new(false));
        let exit_status = Arc::new(Mutex::new(None));

        // Spawn thread to read stdout
        if let Some(stdout) = child.stdout.take() {
            let buffer = Arc::clone(&stdout_buffer);
            let finished_clone = Arc::clone(&finished);
            thread::spawn(move || {
                let reader = BufReader::new(stdout);
                for line in reader.lines().map_while(Result::ok) {
                    if let Ok(mut buf) = buffer.lock() {
                        buf.push(line);
                    }
                }
                finished_clone.store(true, Ordering::SeqCst);
            });
        }

        // Spawn thread to read stderr
        if let Some(stderr) = child.stderr.take() {
            let buffer = Arc::clone(&stderr_buffer);
            thread::spawn(move || {
                let reader = BufReader::new(stderr);
                for line in reader.lines().map_while(Result::ok) {
                    if let Ok(mut buf) = buffer.lock() {
                        buf.push(line);
                    }
                }
            });
        }

        // Spawn thread to wait for process and capture exit status
        let exit_status_clone = Arc::clone(&exit_status);
        let finished_clone = Arc::clone(&finished);
        let mut child_for_wait = child;
        thread::spawn(move || {
            if let Ok(status) = child_for_wait.wait() {
                if let Ok(mut es) = exit_status_clone.lock() {
                    *es = status.code();
                }
                finished_clone.store(true, Ordering::SeqCst);
            }
        });

        let shell = BackgroundShell {
            stdout_buffer,
            stderr_buffer,
            command: command.to_string(),
            finished,
            exit_status,
            pid,
            output_retrieved_after_finish: AtomicBool::new(false),
        };

        if let Ok(mut shells) = self.shells.lock() {
            shells.insert(shell_id.clone(), shell);
        }

        Ok(shell_id)
    }

    /// Get output from a background shell (returns new output since last call)
    #[allow(clippy::significant_drop_tightening)] // shells lock must be held while accessing shell
    pub(crate) fn get_output(&self, shell_id: &str) -> Result<(String, bool, Option<i32>), String> {
        let shells = self
            .shells
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let shell = shells
            .get(shell_id)
            .ok_or_else(|| format!("Shell '{shell_id}' not found"))?;

        let mut output = String::new();

        // Swap buffers atomically — take all lines, leave empty vec.
        // This minimizes lock hold time and prevents data loss from
        // concurrent writer threads.
        let stdout_lines: Vec<String> = shell
            .stdout_buffer
            .lock()
            .map(|mut buf| std::mem::take(&mut *buf))
            .unwrap_or_default();

        let stderr_lines: Vec<String> = shell
            .stderr_buffer
            .lock()
            .map(|mut buf| std::mem::take(&mut *buf))
            .unwrap_or_default();

        // Join outside the lock
        if !stdout_lines.is_empty() {
            output.push_str(&stdout_lines.join("\n"));
        }
        if !stderr_lines.is_empty() {
            if !output.is_empty() {
                output.push('\n');
            }
            output.push_str("stderr:\n");
            output.push_str(&stderr_lines.join("\n"));
        }

        let is_finished = shell.finished.load(Ordering::SeqCst);
        let is_running = !is_finished;
        let exit_code = shell.exit_status.lock().ok().and_then(|es| *es);

        // Mark that output has been retrieved after process finished (for GC eligibility)
        if is_finished {
            shell
                .output_retrieved_after_finish
                .store(true, Ordering::SeqCst);
        }

        Ok((output, is_running, exit_code))
    }

    /// Kill a background shell by terminating the OS process and its process group.
    ///
    /// Sends SIGTERM first, waits for graceful exit, then escalates to SIGKILL
    /// if needed. Only removes the shell from tracking after the process has
    /// been terminated.
    pub(crate) fn kill(&self, shell_id: &str) -> Result<String, String> {
        let mut shells = self
            .shells
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        if let Some(shell) = shells.remove(shell_id) {
            if !shell.finished.load(Ordering::SeqCst) {
                // Terminate the process group (SIGTERM -> wait -> SIGKILL)
                terminate_process_tree(shell.pid);
            }
            shell.finished.store(true, Ordering::SeqCst);
            Ok(format!(
                "Shell '{}' terminated (command: {}, pid: {})",
                shell_id, shell.command, shell.pid
            ))
        } else {
            Err(format!("Shell '{shell_id}' not found"))
        }
    }

    /// List all background shells
    pub(crate) fn list(&self) -> Vec<(String, String, bool)> {
        let shells = self
            .shells
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        shells
            .iter()
            .map(|(id, shell)| {
                (
                    id.clone(),
                    shell.command.clone(),
                    !shell.finished.load(Ordering::SeqCst),
                )
            })
            .collect()
    }
}

/// Global background shell manager
pub static BACKGROUND_SHELLS: std::sync::LazyLock<BackgroundShellManager> =
    std::sync::LazyLock::new(BackgroundShellManager::new);

/// Find Git Bash on Windows
#[cfg(windows)]
pub(crate) fn find_git_bash() -> Option<std::path::PathBuf> {
    // Common Git Bash locations on Windows
    let paths = [
        r"C:\Program Files\Git\bin\bash.exe",
        r"C:\Program Files (x86)\Git\bin\bash.exe",
        r"C:\Git\bin\bash.exe",
    ];

    for path in &paths {
        let p = std::path::PathBuf::from(path);
        if p.exists() {
            return Some(p);
        }
    }

    // Try to find via 'where git' and derive bash path
    if let Ok(output) = Command::new("where").arg("git").output() {
        if output.status.success() {
            let git_path = String::from_utf8_lossy(&output.stdout);
            if let Some(first_line) = git_path.lines().next() {
                // git.exe is usually in cmd/ or bin/, bash is in bin/
                let git_dir = std::path::Path::new(first_line.trim())
                    .parent()
                    .and_then(|p| p.parent());
                if let Some(git_root) = git_dir {
                    let bash = git_root.join("bin").join("bash.exe");
                    if bash.exists() {
                        return Some(bash);
                    }
                }
            }
        }
    }

    None
}

/// Execute a bash command
pub fn execute_bash(args: &HashMap<String, Value>) -> (String, bool) {
    let Some(command) = args.get("command").and_then(|v| v.as_str()) else {
        return ("Missing 'command' argument".to_string(), true);
    };

    // Check if this should run in background
    let run_in_background = args
        .get("run_in_background")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);

    if run_in_background {
        // Spawn background shell and return shell_id
        match BACKGROUND_SHELLS.spawn(command) {
            Ok(shell_id) => {
                (format!("Background shell started with ID: {shell_id}\nUse bash_output with this shell_id to retrieve output."), false)
            }
            Err(e) => (e, true),
        }
    } else {
        // Run synchronously (original behavior)
        // On Windows, use Git Bash explicitly (not WSL bash)
        // On Unix, use system bash
        // IMPORTANT: Set current_dir to ensure bash runs in the same directory as the process
        let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));

        #[cfg(windows)]
        let output = {
            match find_git_bash() {
                Some(git_bash) => Command::new(git_bash)
                    .args(["-c", command])
                    .current_dir(&cwd)
                    .output(),
                None => Command::new("bash")
                    .args(["-c", command])
                    .current_dir(&cwd)
                    .output(),
            }
        };

        #[cfg(not(windows))]
        let output = Command::new("bash")
            .args(["-c", command])
            .current_dir(&cwd)
            .output();

        match output {
            Ok(output) => {
                let stdout = String::from_utf8_lossy(&output.stdout);
                let stderr = String::from_utf8_lossy(&output.stderr);

                let mut result = String::new();
                if !stdout.is_empty() {
                    result.push_str(&stdout);
                }
                if !stderr.is_empty() {
                    if !result.is_empty() {
                        result.push('\n');
                    }
                    result.push_str("stderr: ");
                    result.push_str(&stderr);
                }
                if result.is_empty() {
                    result = "(command completed with no output)".to_string();
                }

                // Truncate if too long
                if result.len() > 50000 {
                    result = format!(
                        "{}...\n(output truncated, {} total chars)",
                        safe_truncate(&result, 50000),
                        result.len()
                    );
                }

                (result, !output.status.success())
            }
            Err(e) => (format!("Failed to execute command: {e}"), true),
        }
    }
}
