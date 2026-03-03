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
import subprocess
import hashlib
from datetime import datetime

# Fix Windows encoding issues with Unicode characters
sys.stdout = io.TextIOWrapper(sys.stdout.buffer, encoding='utf-8')

# Add hooks directory to path for shared module import
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from crosslink_config import (
    find_crosslink_dir,
    get_project_root,
    load_tracking_mode,
)


def load_rule_file(rules_dir, filename):
    """Load a rule file and return its content, or empty string if not found."""
    if not rules_dir:
        return ""
    path = os.path.join(rules_dir, filename)
    try:
        with open(path, 'r', encoding='utf-8') as f:
            return f.read().strip()
    except (OSError, IOError):
        return ""


def load_all_rules(crosslink_dir):
    """Load all rule files from .crosslink/rules/."""
    if not crosslink_dir:
        return {}, "", ""

    rules_dir = os.path.join(crosslink_dir, 'rules')
    if not os.path.isdir(rules_dir):
        return {}, "", ""

    # Load global rules
    global_rules = load_rule_file(rules_dir, 'global.md')

    # Load project rules
    project_rules = load_rule_file(rules_dir, 'project.md')

    # Load language-specific rules
    language_rules = {}
    language_files = [
        ('rust.md', 'Rust'),
        ('python.md', 'Python'),
        ('javascript.md', 'JavaScript'),
        ('typescript.md', 'TypeScript'),
        ('typescript-react.md', 'TypeScript/React'),
        ('javascript-react.md', 'JavaScript/React'),
        ('go.md', 'Go'),
        ('java.md', 'Java'),
        ('c.md', 'C'),
        ('cpp.md', 'C++'),
        ('csharp.md', 'C#'),
        ('ruby.md', 'Ruby'),
        ('php.md', 'PHP'),
        ('swift.md', 'Swift'),
        ('kotlin.md', 'Kotlin'),
        ('scala.md', 'Scala'),
        ('zig.md', 'Zig'),
        ('odin.md', 'Odin'),
    ]

    for filename, lang_name in language_files:
        content = load_rule_file(rules_dir, filename)
        if content:
            language_rules[lang_name] = content

    return language_rules, global_rules, project_rules


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
    '.mypy_cache', '.tox', 'eggs', '*.egg-info', '.sass-cache'
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
        return hashlib.md5(f"{lock_path}:{mtime}".encode()).hexdigest()[:12]
    except OSError:
        return None


def run_command(cmd, timeout=5):
    """Run a command and return output, or None on failure."""
    try:
        result = subprocess.run(
            cmd,
            capture_output=True,
            text=True,
            timeout=timeout,
            shell=True
        )
        if result.returncode == 0:
            return result.stdout.strip()
    except (subprocess.TimeoutExpired, OSError, Exception):
        pass
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


def build_reminder(languages, project_tree, dependencies, language_rules, global_rules, project_rules, tracking_mode="strict", crosslink_dir=None):
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
1. Create a parent issue for the overall feature: `crosslink create "<feature name>" -p high`
2. Break down into subissues: `crosslink subissue <parent_id> "<component 1>"`, etc.
3. Inform the user: "This implementation will require multiple files/components. I've created issue #X with Y subissues to track progress."
4. Work on one subissue at a time, marking each complete before moving on.

### Context Window Management
If the conversation is getting long OR the task requires many more steps:
1. Create a crosslink issue to track remaining work: `crosslink create "Continue: <task summary>" -p high`
2. Add detailed notes as a comment: `crosslink comment <id> "<what's done, what's next>"`
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

    reminder = f"""<crosslink-behavioral-guard>
## Code Quality Requirements

You are working on a {lang_list} project. Follow these requirements strictly:
{tree_section}{deps_section}{global_section}{tracking_section}{lang_section}{project_section}
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
    """Load the tracking rules markdown file for the given mode."""
    if not crosslink_dir:
        return ""
    rules_dir = os.path.join(crosslink_dir, "rules")
    filename = f"tracking-{tracking_mode}.md"
    path = os.path.join(rules_dir, filename)
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

Full rules were injected on first prompt. Use `crosslink list -s open` to see current issues.
</crosslink-behavioral-guard>"""


def main():
    try:
        # Read input from stdin (Claude Code passes prompt info)
        input_data = json.load(sys.stdin)
    except json.JSONDecodeError:
        # If no valid JSON, still inject reminder
        pass
    except Exception:
        pass

    # Find crosslink directory and load rules
    crosslink_dir = find_crosslink_dir()
    tracking_mode = load_tracking_mode(crosslink_dir)

    # Check if we should send full or condensed guard
    if not should_send_full_guard(crosslink_dir):
        languages = detect_languages()
        print(build_condensed_reminder(languages, tracking_mode))
        sys.exit(0)

    language_rules, global_rules, project_rules = load_all_rules(crosslink_dir)

    # Detect languages in the project
    languages = detect_languages()

    # Generate project tree to prevent path hallucinations
    project_tree = get_project_tree()

    # Get installed dependencies to prevent version hallucinations
    dependencies = get_dependencies()

    # Output the full reminder
    print(build_reminder(languages, project_tree, dependencies, language_rules, global_rules, project_rules, tracking_mode, crosslink_dir))

    # Mark that we've sent the full guard this session
    mark_full_guard_sent(crosslink_dir)
    sys.exit(0)


if __name__ == "__main__":
    main()
