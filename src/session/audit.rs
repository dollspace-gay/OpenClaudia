//! JSONL audit logging for sessions.

use std::path::PathBuf;

/// JSONL audit logger that records events for a session
pub struct AuditLogger {
    file: Option<std::fs::File>,
}

impl AuditLogger {
    /// Create a new audit logger for a session. Creates the log directory
    /// and opens a `.jsonl` file for appending.
    pub fn new(session_id: &str) -> Self {
        let dir = PathBuf::from(".openclaudia/logs");
        std::fs::create_dir_all(&dir).ok();
        let path = dir.join(format!("{}.jsonl", session_id));
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .ok();
        Self { file }
    }

    /// Log an event with arbitrary JSON data
    pub fn log(&mut self, event_type: &str, data: &serde_json::Value) {
        if let Some(ref mut f) = self.file {
            let entry = serde_json::json!({
                "timestamp": chrono::Utc::now().to_rfc3339(),
                "event": event_type,
                "data": data,
            });
            if let Ok(line) = serde_json::to_string(&entry) {
                use std::io::Write;
                writeln!(f, "{}", line).ok();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_audit_logger() {
        let dir = TempDir::new().unwrap();
        let log_dir = dir.path().join(".openclaudia/logs");
        std::fs::create_dir_all(&log_dir).unwrap();

        // We just test that creation doesn't panic; actual file writing
        // depends on current directory which is tricky in tests
        let session_id = "test-session-123";
        let path = log_dir.join(format!("{}.jsonl", session_id));
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .ok();
        let mut logger = AuditLogger { file };
        logger.log("test_event", &serde_json::json!({"key": "value"}));

        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("test_event"));
        assert!(content.contains("\"key\":\"value\""));
    }
}
