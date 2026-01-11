#!/usr/bin/env python3
"""
Session start hook that loads chainlink context and reminds about session workflow.
"""

import json
import subprocess
import sys
import os


def run_chainlink(args):
    """Run a chainlink command and return output."""
    try:
        result = subprocess.run(
            ["chainlink"] + args,
            capture_output=True,
            text=True,
            timeout=5
        )
        return result.stdout.strip() if result.returncode == 0 else None
    except (subprocess.TimeoutExpired, FileNotFoundError, Exception):
        return None


def check_chainlink_initialized():
    """Check if .chainlink directory exists."""
    cwd = os.getcwd()
    current = cwd

    while True:
        candidate = os.path.join(current, ".chainlink")
        if os.path.isdir(candidate):
            return True
        parent = os.path.dirname(current)
        if parent == current:
            break
        current = parent

    return False


def main():
    if not check_chainlink_initialized():
        # No chainlink repo, skip
        sys.exit(0)

    context_parts = ["<chainlink-session-context>"]

    # Try to get session status
    session_status = run_chainlink(["session", "status"])
    if session_status:
        context_parts.append(f"## Current Session\n{session_status}")

    # Get ready issues (unblocked work)
    ready_issues = run_chainlink(["ready"])
    if ready_issues:
        context_parts.append(f"## Ready Issues (unblocked)\n{ready_issues}")

    # Get open issues summary
    open_issues = run_chainlink(["list", "-s", "open"])
    if open_issues:
        context_parts.append(f"## Open Issues\n{open_issues}")

    context_parts.append("""
## Chainlink Workflow Reminder
- Use `chainlink session start` at the beginning of work
- Use `chainlink session work <id>` to mark current focus
- Add comments as you discover things: `chainlink comment <id> "..."`
- End with handoff notes: `chainlink session end --notes "..."`
</chainlink-session-context>""")

    print("\n\n".join(context_parts))
    sys.exit(0)


if __name__ == "__main__":
    main()
