//! Shared local tool execution service.
//!
//! This centralizes the common "run an OpenClaudia tool locally" mechanics
//! that were duplicated across TUI, legacy REPL, ACP local tools, subagents,
//! and intercepted XML tools: optional enterprise tool cap, session id guard,
//! active ledger installation, permission checked-vs-unchecked dispatch, and
//! task-manager-aware execution.

use crate::config::AppConfig;
use crate::memory::MemoryDb;
use crate::permissions::PermissionManager;
use crate::services::policy::{PolicyEnforcer, ToolExecutionPolicy};
use crate::session::TaskManager;
use crate::tools::{self, ToolCall, ToolResult};

/// Inputs for one local tool execution.
pub struct ToolExecutorRequest<'a> {
    /// Tool call to execute.
    pub tool_call: &'a ToolCall,
    /// Optional memory database for memory tools.
    pub memory_db: Option<&'a MemoryDb>,
    /// Optional app config for subagent tools.
    pub app_config: Option<&'a AppConfig>,
    /// Optional task manager for task_* tools.
    pub task_mgr: Option<&'a mut TaskManager>,
    /// Permission manager to consult when `permission_already_checked` is false.
    pub permission_mgr: Option<&'a PermissionManager>,
    /// Set true when an outer interactive prompt already made the permission
    /// decision and the dispatcher should not prompt/check again.
    pub permission_already_checked: bool,
    /// Session id to bind for session-scoped tools and ledger observations.
    pub session_id: Option<&'a str>,
    /// Optional enterprise policy enforcer. When supplied with `session_id`,
    /// the tool cap is checked and recorded before dispatch.
    pub policy_enforcer: Option<&'a PolicyEnforcer>,
}

/// Shared local tool executor.
pub struct ToolExecutor;

impl ToolExecutor {
    /// Execute a local tool call.
    ///
    /// # Errors
    ///
    /// Tool failures are returned inside [`ToolResult::is_error`], matching the
    /// historical dispatcher contract.
    #[must_use]
    pub fn execute(request: ToolExecutorRequest<'_>) -> ToolResult {
        let ToolExecutorRequest {
            tool_call,
            memory_db,
            app_config,
            task_mgr,
            permission_mgr,
            permission_already_checked,
            session_id,
            policy_enforcer,
        } = request;

        let tool_policy = ToolExecutionPolicy::new(policy_enforcer, session_id);
        if let Err(err) = tool_policy.check_and_record_tool(&tool_call.function.name) {
            return ToolResult {
                tool_call_id: tool_call.id.clone(),
                content: format!("Blocked by policy: {err}"),
                is_error: true,
            };
        }

        let _session_guard = session_id.map(tools::SessionIdGuard::set);
        let _ledger_guard =
            session_id.and_then(crate::grounded_loop::install_active_project_ledger_for_session);

        if permission_already_checked {
            tools::execute_tool_with_tasks_unchecked(tool_call, memory_db, app_config, task_mgr)
        } else {
            tools::execute_tool_with_tasks(
                tool_call,
                memory_db,
                app_config,
                task_mgr,
                permission_mgr,
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::policy::{EnterprisePolicy, PolicyEnforcer, ToolCaps};
    use crate::tools::{FunctionCall, ToolCall};

    fn bash_call(command: &str) -> ToolCall {
        ToolCall {
            id: "call_bash".to_string(),
            call_type: "function".to_string(),
            function: FunctionCall {
                name: "bash".to_string(),
                arguments: serde_json::json!({ "command": command }).to_string(),
            },
        }
    }

    #[test]
    fn tool_executor_enforces_policy_before_dispatch() {
        let mut caps = ToolCaps::new();
        caps.insert("bash".to_string(), 0);
        let enforcer = PolicyEnforcer::new(EnterprisePolicy {
            tool_caps: caps,
            ..Default::default()
        });
        let call = bash_call("printf tool-executor-should-not-run");

        let result = ToolExecutor::execute(ToolExecutorRequest {
            tool_call: &call,
            memory_db: None,
            app_config: None,
            task_mgr: None,
            permission_mgr: None,
            permission_already_checked: false,
            session_id: Some("s1"),
            policy_enforcer: Some(&enforcer),
        });

        assert!(result.is_error);
        assert!(result.content.contains("Blocked by policy"));
        assert!(!result.content.contains("tool-executor-should-not-run"));
    }

    #[test]
    fn tool_executor_uses_checked_dispatch_without_nested_permission() {
        let call = bash_call("printf tool-executor-ok");

        let result = ToolExecutor::execute(ToolExecutorRequest {
            tool_call: &call,
            memory_db: None,
            app_config: None,
            task_mgr: None,
            permission_mgr: None,
            permission_already_checked: true,
            session_id: Some("s2"),
            policy_enforcer: None,
        });

        assert!(!result.is_error, "unexpected error: {}", result.content);
        assert!(result.content.contains("tool-executor-ok"));
    }
}
