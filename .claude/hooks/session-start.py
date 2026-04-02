#!/usr/bin/env python3
"""
Session start hook that loads crosslink context and auto-starts sessions.
"""

import json
import re
import subprocess
import sys
import os
from datetime import datetime, timezone


# Sessions older than this (in hours) are considered stale and auto-ended
STALE_SESSION_HOURS = 4


def run_crosslink(args):
    """Run a crosslink command and return output."""
    try:
        result = subprocess.run(
            ["crosslink"] + args,
            capture_output=True,
            text=True,
            timeout=5
        )
        return result.stdout.strip() if result.returncode == 0 else None
    except (subprocess.TimeoutExpired, FileNotFoundError, Exception):
        return None


def check_crosslink_initialized():
    """Check if .crosslink directory exists.

    Prefers the project root derived from the hook script's own path
    (reliable even when cwd is a subdirectory), falling back to walking
    up from cwd.
    """
    # Primary: resolve from script location (.claude/hooks/ -> project root)
    try:
        root = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
        if os.path.isdir(os.path.join(root, ".crosslink")):
            return True
    except (NameError, OSError):
        pass

    # Fallback: walk up from cwd
    current = os.getcwd()
    while True:
        candidate = os.path.join(current, ".crosslink")
        if os.path.isdir(candidate):
            return True
        parent = os.path.dirname(current)
        if parent == current:
            break
        current = parent

    return False


def get_session_age_minutes():
    """Parse session status to get duration in minutes. Returns None if no active session."""
    result = run_crosslink(["session", "status"])
    if not result or "Session #" not in result:
        return None
    match = re.search(r'Duration:\s*(\d+)\s*minutes', result)
    if match:
        return int(match.group(1))
    return None


def has_active_session():
    """Check if there's an active crosslink session."""
    result = run_crosslink(["session", "status"])
    if result and "Session #" in result and "(started" in result:
        return True
    return False


def auto_end_stale_session():
    """End session if it's been open longer than STALE_SESSION_HOURS."""
    age_minutes = get_session_age_minutes()
    if age_minutes is not None and age_minutes > STALE_SESSION_HOURS * 60:
        run_crosslink([
            "session", "end", "--notes",
            f"Session auto-ended (stale after {age_minutes} minutes). No handoff notes provided."
        ])
        return True
    return False


def detect_resume_event():
    """Detect if this is a resume (context compression) vs fresh startup.

    If there's already an active session, this is a resume event.
    """
    return has_active_session()


def get_last_action_from_status(status_text):
    """Extract last action from session status output."""
    if not status_text:
        return None
    match = re.search(r'Last action:\s*(.+)', status_text)
    if match:
        return match.group(1).strip()
    return None


def auto_comment_on_resume(session_status):
    """Add auto-comment on active issue when resuming after context compression."""
    if not session_status:
        return
    # Extract working issue ID
    match = re.search(r'Working on: #(\d+)', session_status)
    if not match:
        return
    issue_id = match.group(1)

    last_action = get_last_action_from_status(session_status)
    if last_action:
        comment = f"[auto] Session resumed after context compression. Last action: {last_action}"
    else:
        comment = "[auto] Session resumed after context compression."

    run_crosslink(["comment", issue_id, comment])


def get_working_issue_id(session_status):
    """Extract the working issue ID from session status text."""
    if not session_status:
        return None
    match = re.search(r'Working on: #(\d+)', session_status)
    return match.group(1) if match else None


def get_issue_labels(issue_id):
    """Get labels for an issue via crosslink issue show --json."""
    output = run_crosslink(["show", issue_id, "--json"])
    if not output:
        return []
    try:
        data = json.loads(output)
        return data.get("labels", [])
    except (json.JSONDecodeError, KeyError):
        return []


def extract_design_doc_slugs(labels):
    """Extract knowledge page slugs from design-doc:<slug> labels."""
    prefix = "design-doc:"
    return [label[len(prefix):] for label in labels if label.startswith(prefix)]


def build_design_context(session_status):
    """Build auto-injected design context from issue labels.

    Returns a formatted string block, or None if no design docs found.
    """
    issue_id = get_working_issue_id(session_status)
    if not issue_id:
        return None

    labels = get_issue_labels(issue_id)
    slugs = extract_design_doc_slugs(labels)
    if not slugs:
        return None

    parts = ["## Design Context (auto-injected)"]

    # Limit to 3 pages to respect hook timeout
    for slug in slugs[:3]:
        content = run_crosslink(["knowledge", "show", slug])
        if not content:
            parts.append(f"### {slug}\n*Page not found. Run `crosslink knowledge show {slug}` to check.*")
            continue

        if len(content) <= 8000:
            parts.append(f"### {slug}\n{content}")
        else:
            # Too large — inject summary only
            meta = run_crosslink(["knowledge", "show", slug, "--json"])
            if meta:
                try:
                    data = json.loads(meta)
                    title = data.get("title", slug)
                    tags = ", ".join(data.get("tags", []))
                    parts.append(
                        f"### {slug}\n"
                        f"**{title}** (tags: {tags})\n"
                        f"*Content too large for auto-injection ({len(content)} chars). "
                        f"View with: `crosslink knowledge show {slug}`*"
                    )
                except json.JSONDecodeError:
                    parts.append(
                        f"### {slug}\n"
                        f"*Content too large ({len(content)} chars). "
                        f"View with: `crosslink knowledge show {slug}`*"
                    )
            else:
                parts.append(
                    f"### {slug}\n"
                    f"*Content too large ({len(content)} chars). "
                    f"View with: `crosslink knowledge show {slug}`*"
                )

    if len(parts) == 1:
        return None

    return "\n\n".join(parts)


def main():
    if not check_crosslink_initialized():
        # No crosslink repo, skip
        sys.exit(0)

    context_parts = ["<crosslink-session-context>"]

    is_resume = detect_resume_event()

    # Check for stale session and auto-end it
    stale_ended = False
    if is_resume:
        stale_ended = auto_end_stale_session()
        if stale_ended:
            is_resume = False
            context_parts.append(
                "## Stale Session Warning\nPrevious session was auto-ended (open > "
                f"{STALE_SESSION_HOURS} hours). Handoff notes may be incomplete."
            )

    # Get handoff notes from previous session before starting new one
    last_handoff = run_crosslink(["session", "last-handoff"])

    # Auto-start session if none active
    if not has_active_session():
        run_crosslink(["session", "start"])

    # If resuming, add breadcrumb comment and context
    if is_resume:
        session_status = run_crosslink(["session", "status"])
        auto_comment_on_resume(session_status)

        last_action = get_last_action_from_status(session_status)
        if last_action:
            context_parts.append(
                f"## Context Compression Breadcrumb\n"
                f"This session resumed after context compression.\n"
                f"Last recorded action: {last_action}"
            )
        else:
            context_parts.append(
                "## Context Compression Breadcrumb\n"
                "This session resumed after context compression.\n"
                "No last action was recorded. Use `crosslink session action \"...\"` to track progress."
            )

    # Include previous session handoff notes if available
    if last_handoff and "No previous" not in last_handoff:
        context_parts.append(f"## Previous Session Handoff\n{last_handoff}")

    # Try to get session status
    session_status = run_crosslink(["session", "status"])
    if session_status:
        context_parts.append(f"## Current Session\n{session_status}")

    # Show agent identity if in multi-agent mode
    agent_status = run_crosslink(["agent", "status"])
    if agent_status and "No agent configured" not in agent_status:
        context_parts.append(f"## Agent Identity\n{agent_status}")

    # Sync lock state and hydrate shared issues (best-effort, non-blocking)
    sync_result = run_crosslink(["sync"])
    if sync_result:
        context_parts.append(f"## Coordination Sync\n{sync_result}")

    # Show lock assignments
    locks_result = run_crosslink(["locks", "list"])
    if locks_result and "No locks" not in locks_result:
        context_parts.append(f"## Active Locks\n{locks_result}")

    # Show knowledge repo summary
    knowledge_list = run_crosslink(["knowledge", "list", "--quiet"])
    if knowledge_list is not None:
        # --quiet outputs one slug per line; count non-empty lines
        page_count = len([line for line in knowledge_list.splitlines() if line.strip()])
        if page_count > 0:
            context_parts.append(
                f"## Knowledge Repo\n{page_count} page(s) available. "
                "Search with `crosslink knowledge search '<query>'` before researching a topic."
            )

    # Auto-inject design docs from issue labels
    design_context = build_design_context(session_status)
    if design_context:
        context_parts.append(design_context)

    # Get ready issues (unblocked work)
    ready_issues = run_crosslink(["ready"])
    if ready_issues:
        context_parts.append(f"## Ready Issues (unblocked)\n{ready_issues}")

    # Get open issues summary
    open_issues = run_crosslink(["list", "-s", "open"])
    if open_issues:
        context_parts.append(f"## Open Issues\n{open_issues}")

    context_parts.append("""
## Crosslink Workflow Reminder
- Use `crosslink session start` at the beginning of work
- Use `crosslink session work <id>` to mark current focus
- Use `crosslink session action "..."` to record breadcrumbs before context compression
- Add comments as you discover things: `crosslink issue comment <id> "..."`
- End with handoff notes: `crosslink session end --notes "..."`
- Use `crosslink locks list` to see which issues are claimed by agents
- Use `crosslink sync` to refresh lock state from the coordination branch
</crosslink-session-context>""")

    print("\n\n".join(context_parts))
    sys.exit(0)


if __name__ == "__main__":
    main()
