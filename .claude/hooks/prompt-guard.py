#!/usr/bin/env python3
"""
Crosslink behavioral hook for Claude Code.
Injects best practice reminders on every prompt submission.
Loads rules from .crosslink/rules/ markdown files.
"""

import json
import sys
import os
import io
import hashlib
from datetime import datetime

# Fix Windows encoding issues with Unicode characters
sys.stdout = io.TextIOWrapper(sys.stdout.buffer, encoding='utf-8')

# Add hooks directory to path for shared module import
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from crosslink_config import (
    find_crosslink_dir,
    get_project_root,
    is_agent_context,
    load_config_merged,
    load_guard_state,
    load_tracking_mode,
    save_guard_state,
)


def load_rule_file(rules_dir, filename, rules_local_dir=None):
    """Load a rule file, preferring rules.local/ override if present."""
    if not rules_dir:
        return ""
    # Check rules.local/ first for an override
    if rules_local_dir:
        local_path = os.path.join(rules_local_dir, filename)
        try:
            with open(local_path, 'r', encoding='utf-8') as f:
                return f.read().strip()
        except (OSError, IOError):
            pass
    # Fall back to rules/
    path = os.path.join(rules_dir, filename)
    try:
        with open(path, 'r', encoding='utf-8') as f:
            return f.read().strip()
    except (OSError, IOError):
        return ""


def load_all_rules(crosslink_dir):
    """Load all rule files from .crosslink/rules/, with .crosslink/rules.local/ overrides.

    Auto-discovers all .md files in the rules directory. Files are categorized as:
    - Well-known names: global.md, project.md, knowledge.md, quality.md
    - Language files: matched by known language filename patterns
    - Extra rules: any other .md file (loaded as additional general rules)

    Files in rules.local/ override same-named files in rules/.
    """
    if not crosslink_dir:
        return {}, "", "", "", ""

    rules_dir = os.path.join(crosslink_dir, 'rules')
    rules_local_dir = os.path.join(crosslink_dir, 'rules.local')
    if not os.path.isdir(rules_dir) and not os.path.isdir(rules_local_dir):
        return {}, "", "", "", ""

    if not os.path.isdir(rules_local_dir):
        rules_local_dir = None

    # Well-known non-language files (loaded into specific return values)
    WELL_KNOWN = {'global.md', 'project.md', 'knowledge.md', 'quality.md'}

    # Internal/structural files (not injected as rules)
    SKIP_FILES = {
        'sanitize-patterns.txt',
        'tracking-strict.md', 'tracking-normal.md', 'tracking-relaxed.md',
    }

    # Language filename -> display name mapping
    LANGUAGE_MAP = {
        'rust.md': 'Rust', 'python.md': 'Python',
        'javascript.md': 'JavaScript', 'typescript.md': 'TypeScript',
        'typescript-react.md': 'TypeScript/React',
        'javascript-react.md': 'JavaScript/React',
        'go.md': 'Go', 'java.md': 'Java', 'c.md': 'C', 'cpp.md': 'C++',
        'csharp.md': 'C#', 'ruby.md': 'Ruby', 'php.md': 'PHP',
        'swift.md': 'Swift', 'kotlin.md': 'Kotlin', 'scala.md': 'Scala',
        'zig.md': 'Zig', 'odin.md': 'Odin',
        'elixir.md': 'Elixir', 'elixir-phoenix.md': 'Elixir/Phoenix',
        'shell.md': 'Shell',
        'web.md': 'Web',
    }

    # Load well-known files
    global_rules = load_rule_file(rules_dir, 'global.md', rules_local_dir)
    project_rules = load_rule_file(rules_dir, 'project.md', rules_local_dir)
    knowledge_rules = load_rule_file(rules_dir, 'knowledge.md', rules_local_dir)
    quality_rules = load_rule_file(rules_dir, 'quality.md', rules_local_dir)

    # Auto-discover all files from both directories
    language_rules = {}
    all_files = set()

    try:
        if os.path.isdir(rules_dir):
            for entry in os.listdir(rules_dir):
                if entry.endswith('.md') or entry.endswith('.txt'):
                    all_files.add(entry)
    except OSError:
        pass

    if rules_local_dir:
        try:
            for entry in os.listdir(rules_local_dir):
                if entry.endswith('.md') or entry.endswith('.txt'):
                    all_files.add(entry)
        except OSError:
            pass

    for filename in sorted(all_files):
        if filename in WELL_KNOWN or filename in SKIP_FILES:
            continue
        if filename in LANGUAGE_MAP:
            content = load_rule_file(rules_dir, filename, rules_local_dir)
            if content:
                language_rules[LANGUAGE_MAP[filename]] = content
        elif filename.endswith('.md'):
            content = load_rule_file(rules_dir, filename, rules_local_dir)
            if content:
                lang_name = os.path.splitext(filename)[0].replace('-', '/').title()
                language_rules[lang_name] = content

    return language_rules, global_rules, project_rules, knowledge_rules, quality_rules


# Detect language from common file extensions in the working directory
def detect_languages():
    """Scan for common source files to determine active languages."""
    extensions = {
        '.rs': 'Rust',
        '.py': 'Python',
        '.js': 'JavaScript',
        '.ts': 'TypeScript',
        '.tsx': 'TypeScript/React',
        '.jsx': 'JavaScript/React',
        '.go': 'Go',
        '.java': 'Java',
        '.c': 'C',
        '.cpp': 'C++',
        '.cs': 'C#',
        '.rb': 'Ruby',
        '.php': 'PHP',
        '.swift': 'Swift',
        '.kt': 'Kotlin',
        '.scala': 'Scala',
        '.zig': 'Zig',
        '.odin': 'Odin',
        '.ex': 'Elixir',
        '.exs': 'Elixir',
        '.heex': 'Elixir/Phoenix',
        '.sh': 'Shell',
        '.bash': 'Shell',
    }

    found = set()
    cwd = get_project_root()

    # Check for project config files first (more reliable than scanning)
    config_indicators = {
        'Cargo.toml': 'Rust',
        'package.json': 'JavaScript',
        'tsconfig.json': 'TypeScript',
        'pyproject.toml': 'Python',
        'requirements.txt': 'Python',
        'go.mod': 'Go',
        'pom.xml': 'Java',
        'build.gradle': 'Java',
        'Gemfile': 'Ruby',
        'composer.json': 'PHP',
        'Package.swift': 'Swift',
        'mix.exs': 'Elixir',
        '.shellcheckrc': 'Shell',
    }

    # Check cwd and immediate subdirs for config files
    check_dirs = [cwd]
    try:
        for entry in os.listdir(cwd):
            subdir = os.path.join(cwd, entry)
            if os.path.isdir(subdir) and not entry.startswith('.'):
                check_dirs.append(subdir)
    except (PermissionError, OSError):
        pass

    for check_dir in check_dirs:
        for config_file, lang in config_indicators.items():
            if os.path.exists(os.path.join(check_dir, config_file)):
                found.add(lang)

    # Also scan for source files in src/ directories
    scan_dirs = [cwd]
    src_dir = os.path.join(cwd, 'src')
    if os.path.isdir(src_dir):
        scan_dirs.append(src_dir)
    # Check nested project src dirs too
    for check_dir in check_dirs:
        nested_src = os.path.join(check_dir, 'src')
        if os.path.isdir(nested_src):
            scan_dirs.append(nested_src)

    for scan_dir in scan_dirs:
        try:
            for entry in os.listdir(scan_dir):
                ext = os.path.splitext(entry)[1].lower()
                if ext in extensions:
                    found.add(extensions[ext])
        except (PermissionError, OSError):
            pass

    return list(found) if found else ['the project']


def get_language_section(languages, language_rules):
    """Build language-specific best practices section from loaded rules."""
    sections = []
    for lang in languages:
        if lang in language_rules:
            content = language_rules[lang]
            # If the file doesn't start with a header, add one
            if not content.startswith('#'):
                sections.append(f"### {lang} Best Practices\n{content}")
            else:
                sections.append(content)

    if not sections:
        return ""

    return "\n\n".join(sections)


# Directories to skip when building project tree
SKIP_DIRS = {
    '.git', 'node_modules', 'target', 'venv', '.venv', 'env', '.env',
    '__pycache__', '.crosslink', '.claude', 'dist', 'build', '.next',
    '.nuxt', 'vendor', '.idea', '.vscode', 'coverage', '.pytest_cache',
    '.mypy_cache', '.tox', 'eggs', '*.egg-info', '.sass-cache',
    '_build', 'deps', '.elixir_ls', '.fetch'
}


def get_project_tree(max_depth=3, max_entries=50):
    """Generate a compact project tree to prevent path hallucinations."""
    cwd = get_project_root()
    entries = []

    def should_skip(name):
        if name.startswith('.') and name not in ('.github', '.claude'):
            return True
        return name in SKIP_DIRS or name.endswith('.egg-info')

    def walk_dir(path, prefix="", depth=0):
        if depth > max_depth or len(entries) >= max_entries:
            return

        try:
            items = sorted(os.listdir(path))
        except (PermissionError, OSError):
            return

        # Separate dirs and files
        dirs = [i for i in items if os.path.isdir(os.path.join(path, i)) and not should_skip(i)]
        files = [i for i in items if os.path.isfile(os.path.join(path, i)) and not i.startswith('.')]

        # Add files first (limit per directory)
        for f in files[:10]:  # Max 10 files per dir shown
            if len(entries) >= max_entries:
                return
            entries.append(f"{prefix}{f}")

        if len(files) > 10:
            entries.append(f"{prefix}... ({len(files) - 10} more files)")

        # Then recurse into directories
        for d in dirs:
            if len(entries) >= max_entries:
                return
            entries.append(f"{prefix}{d}/")
            walk_dir(os.path.join(path, d), prefix + "  ", depth + 1)

    walk_dir(cwd)

    if not entries:
        return ""

    if len(entries) >= max_entries:
        entries.append(f"... (tree truncated at {max_entries} entries)")

    return "\n".join(entries)



def get_lock_file_hash(lock_path):
    """Get a hash of the lock file for cache invalidation."""
    try:
        mtime = os.path.getmtime(lock_path)
        return hashlib.sha256(f"{lock_path}:{mtime}".encode()).hexdigest()[:12]
    except OSError:
        return None



def get_dependencies(max_deps=30):
    """Get installed dependencies with versions. Uses caching based on lock file mtime."""
    cwd = get_project_root()
    deps = []

    # Check for Rust (Cargo.toml)
    cargo_toml = os.path.join(cwd, 'Cargo.toml')
    if os.path.exists(cargo_toml):
        # Parse Cargo.toml for direct dependencies (faster than cargo tree)
        try:
            with open(cargo_toml, 'r') as f:
                content = f.read()
                in_deps = False
                for line in content.split('\n'):
                    if line.strip().startswith('[dependencies]'):
                        in_deps = True
                        continue
                    if line.strip().startswith('[') and in_deps:
                        break
                    if in_deps and '=' in line and not line.strip().startswith('#'):
                        parts = line.split('=', 1)
                        name = parts[0].strip()
                        rest = parts[1].strip() if len(parts) > 1 else ''
                        if rest.startswith('{'):
                            # Handle { version = "x.y", features = [...] } format
                            import re
                            match = re.search(r'version\s*=\s*"([^"]+)"', rest)
                            if match:
                                deps.append(f"  {name} = \"{match.group(1)}\"")
                        elif rest.startswith('"') or rest.startswith("'"):
                            version = rest.strip('"').strip("'")
                            deps.append(f"  {name} = \"{version}\"")
                        if len(deps) >= max_deps:
                            break
        except (OSError, Exception):
            pass
        if deps:
            return "Rust (Cargo.toml):\n" + "\n".join(deps[:max_deps])

    # Check for Node.js (package.json)
    package_json = os.path.join(cwd, 'package.json')
    if os.path.exists(package_json):
        try:
            with open(package_json, 'r') as f:
                pkg = json.load(f)
                for dep_type in ['dependencies', 'devDependencies']:
                    if dep_type in pkg:
                        for name, version in list(pkg[dep_type].items())[:max_deps]:
                            deps.append(f"  {name}: {version}")
                            if len(deps) >= max_deps:
                                break
        except (OSError, json.JSONDecodeError, Exception):
            pass
        if deps:
            return "Node.js (package.json):\n" + "\n".join(deps[:max_deps])

    # Check for Python (requirements.txt or pyproject.toml)
    requirements = os.path.join(cwd, 'requirements.txt')
    if os.path.exists(requirements):
        try:
            with open(requirements, 'r') as f:
                for line in f:
                    line = line.strip()
                    if line and not line.startswith('#') and not line.startswith('-'):
                        deps.append(f"  {line}")
                        if len(deps) >= max_deps:
                            break
        except (OSError, Exception):
            pass
        if deps:
            return "Python (requirements.txt):\n" + "\n".join(deps[:max_deps])

    # Check for Elixir (mix.exs)
    mix_exs = os.path.join(cwd, 'mix.exs')
    if os.path.exists(mix_exs):
        try:
            import re
            with open(mix_exs, 'r') as f:
                content = f.read()
                # Match {:dep_name, "~> x.y"} or {:dep_name, ">= x.y"} patterns
                for match in re.finditer(r'\{:(\w+),\s*"([^"]+)"', content):
                    deps.append(f"  {match.group(1)}: {match.group(2)}")
                    if len(deps) >= max_deps:
                        break
        except (OSError, Exception):
            pass
        if deps:
            return "Elixir (mix.exs):\n" + "\n".join(deps[:max_deps])

    # Check for Go (go.mod)
    go_mod = os.path.join(cwd, 'go.mod')
    if os.path.exists(go_mod):
        try:
            with open(go_mod, 'r') as f:
                in_require = False
                for line in f:
                    line = line.strip()
                    if line.startswith('require ('):
                        in_require = True
                        continue
                    if line == ')' and in_require:
                        break
                    if in_require and line:
                        deps.append(f"  {line}")
                        if len(deps) >= max_deps:
                            break
        except (OSError, Exception):
            pass
        if deps:
            return "Go (go.mod):\n" + "\n".join(deps[:max_deps])

    return ""


def build_reminder(languages, project_tree, dependencies, language_rules, global_rules, project_rules, tracking_mode="strict", crosslink_dir=None, knowledge_rules="", quality_rules=""):
    """Build the full reminder context."""
    lang_section = get_language_section(languages, language_rules)
    lang_list = ", ".join(languages) if languages else "this project"
    current_year = datetime.now().year

    # Build tree section if available
    tree_section = ""
    if project_tree:
        tree_section = f"""
### Project Structure (use these exact paths)
```
{project_tree}
```
"""

    # Build dependencies section if available
    deps_section = ""
    if dependencies:
        deps_section = f"""
### Installed Dependencies (use these exact versions)
```
{dependencies}
```
"""

    # Build global rules section (from .crosslink/rules/global.md)
    # Then append/replace the tracking section based on tracking_mode
    global_section = ""
    if global_rules:
        global_section = f"\n{global_rules}\n"
    else:
        # Fallback to hardcoded defaults if no rules file
        global_section = f"""
### Pre-Coding Grounding (PREVENT HALLUCINATIONS)
Before writing code that uses external libraries, APIs, or unfamiliar patterns:
1. **VERIFY IT EXISTS**: Use WebSearch to confirm the crate/package/module exists and check its actual API
2. **CHECK THE DOCS**: Fetch documentation to see real function signatures, not imagined ones
3. **CONFIRM SYNTAX**: If unsure about language features or library usage, search first
4. **USE LATEST VERSIONS**: Always check for and use the latest stable version of dependencies (security + features)
5. **NO GUESSING**: If you can't verify it, tell the user you need to research it

Examples of when to search:
- Using a crate/package you haven't used recently → search "[package] [language] docs {current_year}"
- Uncertain about function parameters → search for actual API reference
- New language feature or syntax → verify it exists in the version being used
- System calls or platform-specific code → confirm the correct API
- Adding a dependency → search "[package] latest version {current_year}" to get current release

### General Requirements
1. **NO STUBS - ABSOLUTE RULE**:
   - NEVER write `TODO`, `FIXME`, `pass`, `...`, `unimplemented!()` as implementation
   - NEVER write empty function bodies or placeholder returns
   - NEVER say "implement later" or "add logic here"
   - If logic is genuinely too complex for one turn, use `raise NotImplementedError("Descriptive reason: what needs to be done")` and create a crosslink issue
   - The PostToolUse hook WILL detect and flag stub patterns - write real code the first time
2. **NO DEAD CODE**: Discover if dead code is truly dead or if it's an incomplete feature. If incomplete, complete it. If truly dead, remove it.
3. **FULL FEATURES**: Implement the complete feature as requested. Don't stop partway or suggest "you could add X later."
4. **ERROR HANDLING**: Proper error handling everywhere. No panics/crashes on bad input.
5. **SECURITY**: Validate input, use parameterized queries, no command injection, no hardcoded secrets.
6. **READ BEFORE WRITE**: Always read a file before editing it. Never guess at contents.

### Conciseness Protocol
Minimize chattiness. Your output should be:
- **Code blocks** with implementation
- **Tool calls** to accomplish tasks
- **Brief explanations** only when the code isn't self-explanatory

NEVER output:
- "Here is the code" / "Here's how to do it" (just show the code)
- "Let me know if you need anything else" / "Feel free to ask"
- "I'll now..." / "Let me..." (just do it)
- Restating what the user asked
- Explaining obvious code
- Multiple paragraphs when one sentence suffices

When writing code: write it. When making changes: make them. Skip the narration.

### Large File Management (500+ lines)
If you need to write or modify code that will exceed 500 lines:
1. Create a parent issue for the overall feature: `crosslink issue create "<feature name>" -p high`
2. Break down into subissues: `crosslink issue subissue <parent_id> "<component 1>"`, etc.
3. Inform the user: "This implementation will require multiple files/components. I've created issue #X with Y subissues to track progress."
4. Work on one subissue at a time, marking each complete before moving on.

### Context Window Management
If the conversation is getting long OR the task requires many more steps:
1. Create a crosslink issue to track remaining work: `crosslink issue create "Continue: <task summary>" -p high`
2. Add detailed notes as a comment: `crosslink issue comment <id> "<what's done, what's next>"`
3. Inform the user: "This task will require additional turns. I've created issue #X to track progress."

Use `crosslink session work <id>` to mark what you're working on.
"""

    # Inject tracking rules from per-mode markdown file
    tracking_rules = load_tracking_rules(crosslink_dir, tracking_mode) if crosslink_dir else ""
    tracking_section = f"\n{tracking_rules}\n" if tracking_rules else ""

    # Build project rules section (from .crosslink/rules/project.md)
    project_section = ""
    if project_rules:
        project_section = f"\n### Project-Specific Rules\n{project_rules}\n"

    # Build knowledge section (from .crosslink/rules/knowledge.md)
    knowledge_section = ""
    if knowledge_rules:
        knowledge_section = f"\n{knowledge_rules}\n"

    # Build quality section (from .crosslink/rules/quality.md)
    quality_section = ""
    if quality_rules:
        quality_section = f"\n{quality_rules}\n"

    reminder = f"""<crosslink-behavioral-guard>
## Code Quality Requirements

You are working on a {lang_list} project. Follow these requirements strictly:
{tree_section}{deps_section}{global_section}{tracking_section}{quality_section}{lang_section}{project_section}{knowledge_section}
</crosslink-behavioral-guard>"""

    return reminder


def get_guard_marker_path(crosslink_dir):
    """Get the path to the guard-full-sent marker file."""
    if not crosslink_dir:
        return None
    cache_dir = os.path.join(crosslink_dir, '.cache')
    return os.path.join(cache_dir, 'guard-full-sent')


def should_send_full_guard(crosslink_dir):
    """Check if this is the first prompt (no marker) or marker is stale."""
    marker = get_guard_marker_path(crosslink_dir)
    if not marker:
        return True
    if not os.path.exists(marker):
        return True
    # Re-send full guard if marker is older than 4 hours (new session likely)
    try:
        age = datetime.now().timestamp() - os.path.getmtime(marker)
        if age > 4 * 3600:
            return True
    except OSError:
        return True
    return False


def mark_full_guard_sent(crosslink_dir):
    """Create marker file indicating full guard has been sent this session."""
    marker = get_guard_marker_path(crosslink_dir)
    if not marker:
        return
    try:
        cache_dir = os.path.dirname(marker)
        os.makedirs(cache_dir, exist_ok=True)
        with open(marker, 'w') as f:
            f.write(str(datetime.now().timestamp()))
    except OSError:
        pass


def load_tracking_rules(crosslink_dir, tracking_mode):
    """Load the tracking rules markdown file for the given mode.

    Checks rules.local/ first for a local override, then falls back to rules/.
    """
    if not crosslink_dir:
        return ""
    filename = f"tracking-{tracking_mode}.md"
    # Check rules.local/ first
    local_path = os.path.join(crosslink_dir, "rules.local", filename)
    try:
        with open(local_path, "r", encoding="utf-8") as f:
            return f.read().strip()
    except (OSError, IOError):
        pass
    # Fall back to rules/
    path = os.path.join(crosslink_dir, "rules", filename)
    try:
        with open(path, "r", encoding="utf-8") as f:
            return f.read().strip()
    except (OSError, IOError):
        return ""


# Condensed reminders kept short — these don't need full markdown files
CONDENSED_REMINDERS = {
    "strict": (
        "- **MANDATORY — Crosslink Issue Tracking**: You MUST create a crosslink issue BEFORE writing ANY code. "
        "NO EXCEPTIONS. Use `crosslink quick \"title\" -p <priority> -l <label>` BEFORE your first Write/Edit/Bash. "
        "If you skip this, the PreToolUse hook WILL block you. Do NOT treat this as optional.\n"
        "- **Session**: ALWAYS use `crosslink session work <id>` to mark focus. "
        "End with `crosslink session end --notes \"...\"`. This is NOT optional."
    ),
    "normal": (
        "- **Crosslink**: Create issues before work. Use `crosslink quick` for create+label+work. Close with `crosslink close`.\n"
        "- **Session**: Use `crosslink session work <id>`. End with `crosslink session end --notes \"...\"`."
    ),
    "relaxed": "",
}


def build_condensed_reminder(languages, tracking_mode):
    """Build a short reminder for subsequent prompts (after full guard already sent)."""
    lang_list = ", ".join(languages) if languages else "this project"
    tracking_lines = CONDENSED_REMINDERS.get(tracking_mode, "")

    return f"""<crosslink-behavioral-guard>
## Quick Reminder ({lang_list})

{tracking_lines}
- **Security**: Use `mcp__crosslink-safe-fetch__safe_fetch` for web requests. Parameterized queries only.
- **Quality**: No stubs/TODOs. Read before write. Complete features fully. Proper error handling.
- **Testing**: Run tests after changes. Fix warnings, don't suppress them.

Full rules were injected on first prompt. Use `crosslink issue list -s open` to see current issues.
</crosslink-behavioral-guard>"""


def estimate_prompt_chars(input_data):
    """Estimate characters consumed by this prompt turn.

    The hook only sees the user prompt, not tool outputs or model responses.
    We apply a multiplier (5x) to account for the full turn cost:
    user prompt + tool calls + tool results + model response.
    """
    TURN_MULTIPLIER = 5
    try:
        prompt_text = input_data.get("prompt", "")
        if isinstance(prompt_text, str):
            return len(prompt_text) * TURN_MULTIPLIER
        return 2000 * TURN_MULTIPLIER
    except (AttributeError, TypeError):
        return 2000 * TURN_MULTIPLIER


def check_context_budget(crosslink_dir, state, prompt_chars):
    """Check if estimated context usage has exceeded the budget.

    Returns True if the budget is exceeded and full reinjection is needed.
    Default budget: 1,000,000 chars ~ 250k tokens.
    """
    config = load_config_merged(crosslink_dir) if crosslink_dir else {}
    budget = int(config.get("context_budget_chars", 1_000_000))
    if budget <= 0:
        return False

    current = state.get("estimated_context_chars", 0)
    current += prompt_chars
    state["estimated_context_chars"] = current

    return current >= budget


def build_context_budget_warning(languages, tracking_mode):
    """Build the compression directive when context budget is exceeded."""
    lang_list = ", ".join(languages) if languages else "this project"
    tracking_lines = CONDENSED_REMINDERS.get(tracking_mode, "")

    return f"""<crosslink-context-budget-exceeded>
## CONTEXT BUDGET EXCEEDED — COMPRESSION REQUIRED

Your estimated context usage has exceeded 250k tokens. Research shows instruction
adherence degrades significantly past this point. You MUST take the following steps
IMMEDIATELY, before doing anything else:

1. **Record your current state**: Run `crosslink session action "Context budget reached. Working on: <current task summary>"`
2. **Save any in-progress work context** as a crosslink comment: `crosslink issue comment <id> "Progress: <what's done, what's next>" --kind observation`
3. **The system will compress context automatically.** After compression, re-read any files you need and continue working.

## Re-injected Rules ({lang_list})

{tracking_lines}
- **Security**: Use `mcp__crosslink-safe-fetch__safe_fetch` for web requests. Parameterized queries only.
- **Quality**: No stubs/TODOs. Read before write. Complete features fully. Proper error handling.
- **Testing**: Run tests after changes. Fix warnings, don't suppress them.
- **Documentation**: Add typed crosslink comments (--kind plan/decision/observation/result) at every step.
</crosslink-context-budget-exceeded>"""


def main():
    input_data = {}
    try:
        # Read input from stdin (Claude Code passes prompt info)
        input_data = json.load(sys.stdin)
    except json.JSONDecodeError:
        pass
    except Exception:
        pass

    # Find crosslink directory and load rules
    crosslink_dir = find_crosslink_dir()
    tracking_mode = load_tracking_mode(crosslink_dir)

    # Agents always get condensed reminders — skip expensive tree/deps scanning
    if is_agent_context(crosslink_dir):
        languages = detect_languages()
        print(build_condensed_reminder(languages, tracking_mode))
        sys.exit(0)

    # Check if we should send full or condensed guard
    if not should_send_full_guard(crosslink_dir):
        config = load_config_merged(crosslink_dir)
        interval = int(config.get("reminder_drift_threshold", 3))

        state = load_guard_state(crosslink_dir)
        state["total_prompts"] = state.get("total_prompts", 0) + 1

        # Check context budget — if exceeded, reinject full guard + compression directive
        prompt_chars = estimate_prompt_chars(input_data)
        if check_context_budget(crosslink_dir, state, prompt_chars):
            languages = detect_languages()
            language_rules, global_rules, project_rules, knowledge_rules, quality_rules = load_all_rules(crosslink_dir)
            project_tree = get_project_tree()
            dependencies = get_dependencies()
            print(build_reminder(languages, project_tree, dependencies, language_rules, global_rules, project_rules, tracking_mode, crosslink_dir, knowledge_rules, quality_rules))
            print(build_context_budget_warning(languages, tracking_mode))
            state["estimated_context_chars"] = 0
            state["context_budget_reinjections"] = state.get("context_budget_reinjections", 0) + 1
            save_guard_state(crosslink_dir, state)
            sys.exit(0)

        # Normal condensed reminder at interval
        if interval == 0 or state["total_prompts"] % interval == 0:
            languages = detect_languages()
            print(build_condensed_reminder(languages, tracking_mode))

        save_guard_state(crosslink_dir, state)
        sys.exit(0)

    language_rules, global_rules, project_rules, knowledge_rules, quality_rules = load_all_rules(crosslink_dir)

    # Detect languages in the project
    languages = detect_languages()

    # Generate project tree to prevent path hallucinations
    project_tree = get_project_tree()

    # Get installed dependencies to prevent version hallucinations
    dependencies = get_dependencies()

    # Output the full reminder
    print(build_reminder(languages, project_tree, dependencies, language_rules, global_rules, project_rules, tracking_mode, crosslink_dir, knowledge_rules, quality_rules))

    # Mark that we've sent the full guard this session
    mark_full_guard_sent(crosslink_dir)

    # Initialize context budget tracking for this session
    state = load_guard_state(crosslink_dir)
    state["estimated_context_chars"] = 0
    save_guard_state(crosslink_dir, state)

    sys.exit(0)


if __name__ == "__main__":
    main()
