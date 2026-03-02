#!/usr/bin/env python3
"""
PreToolUse hook that blocks Write|Edit|Bash unless a crosslink issue
is being actively worked on. Forces issue creation before code changes.
"""

import json
import sys
import os
import io

# Fix Windows encoding issues
sys.stdout = io.TextIOWrapper(sys.stdout.buffer, encoding='utf-8')

# Add hooks directory to path for shared module import
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from crosslink_config import (
    find_crosslink_dir,
    load_config_merged,
    run_crosslink,
)

# Defaults — overridden by .crosslink/hook-config.json if present
DEFAULT_BLOCKED_GIT = [
    "git push", "git merge", "git rebase", "git cherry-pick",
    "git reset", "git checkout .", "git restore .", "git clean",
    "git stash", "git tag", "git am", "git apply",
    "git branch -d", "git branch -D", "git branch -m",
]

# Git commands that are blocked UNLESS there is an active crosslink issue.
# This allows the /commit skill to work while still preventing unsolicited commits.
DEFAULT_GATED_GIT = [
    "git commit",
]

DEFAULT_ALLOWED_BASH = [
    "crosslink ",
    "git status", "git diff", "git log", "git branch", "git show",
    "cargo test", "cargo build", "cargo check", "cargo clippy", "cargo fmt",
    "npm test", "npm run", "npx ",
    "tsc", "node ", "python ",
    "ls", "dir", "pwd", "echo",
]


def load_config(crosslink_dir):
    """Load hook config from .crosslink/hook-config.json (with .local override), falling back to defaults.

    Returns (tracking_mode, blocked_git, gated_git, allowed_bash).
    tracking_mode is one of: "strict", "normal", "relaxed".
      strict  — block Write/Edit/Bash without an active issue
      normal  — remind (print warning) but don't block
      relaxed — no issue-tracking enforcement, only git blocks
    """
    blocked = list(DEFAULT_BLOCKED_GIT)
    gated = list(DEFAULT_GATED_GIT)
    allowed = list(DEFAULT_ALLOWED_BASH)
    mode = "strict"

    config = load_config_merged(crosslink_dir)
    if not config:
        return mode, blocked, gated, allowed

    if config.get("tracking_mode") in ("strict", "normal", "relaxed"):
        mode = config["tracking_mode"]
    if "blocked_git_commands" in config:
        blocked = config["blocked_git_commands"]
    if "gated_git_commands" in config:
        gated = config["gated_git_commands"]
    if "allowed_bash_prefixes" in config:
        allowed = config["allowed_bash_prefixes"]

    return mode, blocked, gated, allowed


def _matches_command_list(command, cmd_list):
    """Check if a command matches any entry in the list (direct or chained)."""
    for entry in cmd_list:
        if command.startswith(entry):
            return True
    for entry in cmd_list:
        if f"&& {entry}" in command or f"; {entry}" in command or f"| {entry}" in command:
            return True
    return False


def is_blocked_git(input_data, blocked_list):
    """Check if a Bash command is a permanently blocked git mutation."""
    command = input_data.get("tool_input", {}).get("command", "").strip()
    return _matches_command_list(command, blocked_list)


def is_gated_git(input_data, gated_list):
    """Check if a Bash command is a gated git command (allowed with active issue)."""
    command = input_data.get("tool_input", {}).get("command", "").strip()
    return _matches_command_list(command, gated_list)


def is_allowed_bash(input_data, allowed_list):
    """Check if a Bash command is on the allow list (read-only/infra)."""
    command = input_data.get("tool_input", {}).get("command", "").strip()
    for prefix in allowed_list:
        if command.startswith(prefix):
            return True
    return False


def is_claude_memory_path(input_data):
    """Check if a Write/Edit targets Claude Code's own memory/config directory (~/.claude/)."""
    file_path = input_data.get("tool_input", {}).get("file_path", "")
    if not file_path:
        return False
    home = os.path.expanduser("~")
    claude_dir = os.path.join(home, ".claude")
    try:
        return os.path.normcase(os.path.abspath(file_path)).startswith(
            os.path.normcase(os.path.abspath(claude_dir))
        )
    except (ValueError, OSError):
        return False


def main():
    try:
        input_data = json.load(sys.stdin)
        tool_name = input_data.get('tool_name', '')
    except (json.JSONDecodeError, Exception):
        tool_name = ''

    # Only check on Write, Edit, Bash
    if tool_name not in ('Write', 'Edit', 'Bash'):
        sys.exit(0)

    # Allow Claude Code to manage its own memory/config in ~/.claude/
    if tool_name in ('Write', 'Edit') and is_claude_memory_path(input_data):
        sys.exit(0)

    crosslink_dir = find_crosslink_dir()
    tracking_mode, blocked_git, gated_git, allowed_bash = load_config(crosslink_dir)

    # PERMANENT BLOCK: git mutation commands are never allowed (all modes)
    if tool_name == 'Bash' and is_blocked_git(input_data, blocked_git):
        print(
            "MANDATORY COMPLIANCE — DO NOT ATTEMPT TO WORK AROUND THIS BLOCK.\n\n"
            "Git mutation commands (push, merge, rebase, reset, etc.) are "
            "PERMANENTLY FORBIDDEN. The human performs all git write operations.\n\n"
            "You MUST NOT:\n"
            "  - Retry this command\n"
            "  - Rewrite the command to achieve the same effect\n"
            "  - Use a different tool to perform git mutations\n"
            "  - Ask the user if you should bypass this restriction\n\n"
            "You MUST instead:\n"
            "  - Inform the user that this is a manual step for them\n"
            "  - Continue with your other work\n\n"
            "Read-only git commands (status, diff, log, show, branch) are allowed.\n\n"
            "--- INTERVENTION LOGGING ---\n"
            "Log this blocked action for the audit trail:\n"
            "  crosslink intervene <issue-id> \"Attempted: <command>\" "
            "--trigger tool_blocked --context \"<what you were trying to accomplish>\""
        )
        sys.exit(2)

    # GATED GIT: commands like `git commit` require an active crosslink issue
    if tool_name == 'Bash' and is_gated_git(input_data, gated_git):
        if not crosslink_dir:
            # No crosslink dir — allow through (no enforcement possible)
            sys.exit(0)
        status = run_crosslink(["session", "status"], crosslink_dir)
        if status and "Working on: #" in status:
            sys.exit(0)
        print(
            "Git commit requires an active crosslink issue.\n\n"
            "Create one first:\n"
            "  crosslink quick \"<describe the work>\" -p <priority> -l <label>\n\n"
            "Or pick an existing issue:\n"
            "  crosslink list -s open\n"
            "  crosslink session work <id>\n\n"
            "--- INTERVENTION LOGGING ---\n"
            "If a human redirected you here, log the intervention:\n"
            "  crosslink intervene <issue-id> \"Redirected to create issue before commit\" "
            "--trigger redirect --context \"Attempted git commit without active issue\""
        )
        sys.exit(2)

    # Allow read-only / infrastructure Bash commands through
    if tool_name == 'Bash' and is_allowed_bash(input_data, allowed_bash):
        sys.exit(0)

    # Relaxed mode: no issue-tracking enforcement
    if tracking_mode == "relaxed":
        sys.exit(0)

    if not crosslink_dir:
        sys.exit(0)

    # Check session status
    status = run_crosslink(["session", "status"], crosslink_dir)
    if not status:
        # crosslink not available — don't block
        sys.exit(0)

    # If already working on an issue, allow
    if "Working on: #" in status:
        sys.exit(0)

    # No active work item — behavior depends on mode
    strict_msg = (
        "MANDATORY COMPLIANCE — DO NOT ATTEMPT TO WORK AROUND THIS BLOCK.\n\n"
        "You cannot Write, Edit, or run Bash commands without an active crosslink issue. "
        "This is a hard requirement enforced by the project, not a suggestion.\n\n"
        "You MUST NOT:\n"
        "  - Retry the same tool call hoping it will work\n"
        "  - Try alternative tools or approaches to bypass this check\n"
        "  - Proceed without creating an issue first\n"
        "  - Tell the user the hook is preventing you from working\n\n"
        "You MUST do exactly this, right now, before anything else:\n\n"
        "  crosslink quick \"<describe what you are about to do>\" -p <priority> -l <label>\n\n"
        "This single command creates an issue AND sets it as your active work item. "
        "After running it, your tool calls will succeed.\n\n"
        "If an issue already exists for this work, use:\n"
        "  crosslink list -s open\n"
        "  crosslink session work <id>\n\n"
        "This is how the project tracks work. Comply, then continue.\n\n"
        "--- INTERVENTION LOGGING ---\n"
        "After creating the issue, log this intervention:\n"
        "  crosslink intervene <issue-id> \"Blocked: no active issue\" "
        "--trigger tool_blocked --context \"<what you were about to do>\""
    )

    normal_msg = (
        "Reminder: No active crosslink issue. You should create one before making changes.\n\n"
        "  crosslink quick \"<describe what you are about to do>\" -p <priority> -l <label>\n\n"
        "Or pick an existing issue:\n"
        "  crosslink list -s open\n"
        "  crosslink session work <id>"
    )

    if tracking_mode == "strict":
        print(strict_msg)
        sys.exit(2)
    else:
        # normal mode: remind but allow
        print(normal_msg)
        sys.exit(0)


if __name__ == "__main__":
    main()
