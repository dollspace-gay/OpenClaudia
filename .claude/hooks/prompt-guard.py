#!/usr/bin/env python3
"""
Chainlink behavioral hook for Claude Code.
Injects best practice reminders on every prompt submission.
Loads rules from .chainlink/rules/ markdown files.
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


def find_chainlink_dir():
    """Find the .chainlink directory by walking up from cwd."""
    current = os.getcwd()
    for _ in range(10):
        candidate = os.path.join(current, '.chainlink')
        if os.path.isdir(candidate):
            return candidate
        parent = os.path.dirname(current)
        if parent == current:
            break
        current = parent
    return None


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


def load_all_rules(chainlink_dir):
    """Load all rule files from .chainlink/rules/."""
    if not chainlink_dir:
        return {}, "", ""

    rules_dir = os.path.join(chainlink_dir, 'rules')
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
    cwd = os.getcwd()

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
    '__pycache__', '.chainlink', '.claude', 'dist', 'build', '.next',
    '.nuxt', 'vendor', '.idea', '.vscode', 'coverage', '.pytest_cache',
    '.mypy_cache', '.tox', 'eggs', '*.egg-info', '.sass-cache'
}


def get_project_tree(max_depth=3, max_entries=50):
    """Generate a compact project tree to prevent path hallucinations."""
    cwd = os.getcwd()
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


# Cache directory for dependency snapshots
CACHE_DIR = os.path.join(os.getcwd(), '.chainlink', '.cache')


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
    cwd = os.getcwd()
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


def build_reminder(languages, project_tree, dependencies, language_rules, global_rules, project_rules):
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

    # Build global rules section (from .chainlink/rules/global.md)
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
   - If logic is genuinely too complex for one turn, use `raise NotImplementedError("Descriptive reason: what needs to be done")` and create a chainlink issue
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
1. Create a parent issue for the overall feature: `chainlink create "<feature name>" -p high`
2. Break down into subissues: `chainlink subissue <parent_id> "<component 1>"`, etc.
3. Inform the user: "This implementation will require multiple files/components. I've created issue #X with Y subissues to track progress."
4. Work on one subissue at a time, marking each complete before moving on.

### Context Window Management
If the conversation is getting long OR the task requires many more steps:
1. Create a chainlink issue to track remaining work: `chainlink create "Continue: <task summary>" -p high`
2. Add detailed notes as a comment: `chainlink comment <id> "<what's done, what's next>"`
3. Inform the user: "This task will require additional turns. I've created issue #X to track progress."

Use `chainlink session work <id>` to mark what you're working on.
"""

    # Build project rules section (from .chainlink/rules/project.md)
    project_section = ""
    if project_rules:
        project_section = f"\n### Project-Specific Rules\n{project_rules}\n"

    reminder = f"""<chainlink-behavioral-guard>
## Code Quality Requirements

You are working on a {lang_list} project. Follow these requirements strictly:
{tree_section}{deps_section}{global_section}{lang_section}{project_section}
</chainlink-behavioral-guard>"""

    return reminder


def main():
    try:
        # Read input from stdin (Claude Code passes prompt info)
        input_data = json.load(sys.stdin)
    except json.JSONDecodeError:
        # If no valid JSON, still inject reminder
        pass
    except Exception:
        pass

    # Find chainlink directory and load rules
    chainlink_dir = find_chainlink_dir()
    language_rules, global_rules, project_rules = load_all_rules(chainlink_dir)

    # Detect languages in the project
    languages = detect_languages()

    # Generate project tree to prevent path hallucinations
    project_tree = get_project_tree()

    # Get installed dependencies to prevent version hallucinations
    dependencies = get_dependencies()

    # Output the reminder as plain text (gets injected as context)
    print(build_reminder(languages, project_tree, dependencies, language_rules, global_rules, project_rules))
    sys.exit(0)


if __name__ == "__main__":
    main()
