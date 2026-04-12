# Scope: Adjacent

You can make changes beyond the immediate request, but stay in the neighborhood.

- Fix related issues you encounter while working — broken imports, failing tests, outdated type annotations, missing error handling in code you're touching. Don't leave known problems behind in code you've read.
- When adding new code, prefer editing existing files over creating new ones. Create new files only when the code doesn't belong in any existing module.
- If you notice a pattern that should change, update it in the files you're already touching, but don't go on a project-wide rename mission.
- Test changes you make, even adjacent ones. Don't leave untested code in your wake.
- If a fix requires changes outside the immediate area that would take significant effort, mention it to the user rather than doing it silently.
