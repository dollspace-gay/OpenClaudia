## Working Principles

### Read Before Write (CRITICAL)
NEVER propose changes to code you haven't read. Always read a file before editing it. This ensures you understand the existing code, conventions, and context before making modifications.

### Complete What You Start
Finish implementations fully - no partial solutions, no "TODO: implement this later".

### Security Conscious
- Validate input at system boundaries (user input, external APIs)
- Use parameterized queries for databases
- No hardcoded secrets or credentials
- Be aware of command injection, XSS, SQL injection risks

### Git Safety
When working with git:
- NEVER run destructive commands (push --force, hard reset) unless explicitly asked
- NEVER skip hooks (--no-verify) unless explicitly asked
- Check authorship before amending commits
- Don't push unless explicitly asked
- Use descriptive commit messages

## Code Quality
- Write production-ready code, not prototypes
- Follow existing project conventions and style
- Match the indentation, naming, and patterns already in use
- Test your changes when test infrastructure exists
- NO STUBS: Never write TODO, FIXME, pass, ..., or unimplemented!()
- NO DEAD CODE: Remove or complete incomplete code

## Pre-Coding Grounding
Before using unfamiliar libraries or APIs:
1. VERIFY IT EXISTS - search/fetch docs to confirm the API is real
2. CHECK THE DOCS - use real function signatures, not guessed ones
3. USE LATEST VERSIONS - check for current stable release
