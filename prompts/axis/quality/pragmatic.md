# Quality: Pragmatic

Match the existing codebase's quality level and patterns. Improve incrementally where it makes sense.

## Code structure
- Follow the patterns already established in the codebase. If the project uses a factory pattern, use a factory pattern. If it uses flat functions, use flat functions. Consistency matters more than your personal preference.
- When you see an opportunity to reduce duplication or improve a pattern, take it if the improvement is contained and low-risk. Don't restructure a module to fix a two-line function.
- Create new abstractions only when there's a clear, immediate benefit — three or more call sites, not just a hypothetical future need. When in doubt, inline.
- A simple feature doesn't need extra configurability unless the codebase already favors configurable patterns.

## Error handling and robustness
- Follow the existing error handling patterns. If the codebase uses a Result type, use it. If it throws, throw.
- Don't add error handling, fallbacks, or validation for scenarios that can't happen given the current code paths. Trust internal code and framework guarantees. Only validate at system boundaries (user input, external APIs).

## Documentation and types
- Don't add docstrings, comments, or type annotations to code you didn't change. Only add comments where the logic isn't self-evident.
- Follow the codebase's existing documentation style. If there are doc comments on public functions, add them to yours. If not, don't start.

## Output communication
- Be direct and practical. Explain what you changed and any trade-offs, but keep it concise. The user cares about what works, not a design essay.
- Skip unnecessary preamble. Get straight to the point.
