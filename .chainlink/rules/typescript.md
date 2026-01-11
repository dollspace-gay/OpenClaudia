### TypeScript Best Practices

#### Warnings Are Errors - ABSOLUTE RULE
- **ALL warnings must be fixed, NEVER silenced**
- No `// @ts-ignore`, `// @ts-expect-error`, or `eslint-disable` without explicit justification
- No `any` type - use `unknown` and narrow with type guards
- Fix the root cause, don't suppress the symptom

```typescript
// FORBIDDEN: Silencing warnings
// @ts-ignore
// eslint-disable-next-line
const data: any = response;

// REQUIRED: Fix the actual issue
const data: unknown = response;
if (isValidUser(data)) {
    console.log(data.name);  // Type narrowed safely
}
```

#### Code Style
- Use strict mode (`"strict": true` in tsconfig.json)
- Prefer `interface` over `type` for object shapes
- Use `const` by default, `let` when needed, never `var`
- Enable `noImplicitAny`, `strictNullChecks`, `noUnusedLocals`, `noUnusedParameters`

#### Type Safety
```typescript
// GOOD: Explicit types and null handling
function getUser(id: string): User | undefined {
    return users.get(id);
}

const user = getUser(id);
if (user) {
    console.log(user.name);  // TypeScript knows user is defined
}

// BAD: Type assertions to bypass safety
const user = getUser(id) as User;  // Dangerous if undefined
```

#### Error Handling
- Use try/catch for async operations
- Define custom error types for domain errors
- Never swallow errors silently
- Log errors with context before re-throwing

#### Security - CRITICAL
- **Validate ALL user input** at API boundaries (use zod, yup, or io-ts)
- **Sanitize output** - use DOMPurify for HTML, escape for SQL
- **Never use**: `eval()`, `Function()`, `innerHTML` with user data
- **Use parameterized queries** - never string concatenation for SQL
- **Set security headers**: CSP, X-Content-Type-Options, X-Frame-Options
- **Avoid prototype pollution** - validate object keys from user input

```typescript
// GOOD: Input validation with zod
import { z } from 'zod';
const UserInput = z.object({
    email: z.string().email(),
    age: z.number().min(0).max(150),
});
const validated = UserInput.parse(untrustedInput);

// BAD: Trust user input
const { email, age } = req.body;  // No validation
```

#### Dependency Security - MANDATORY
- Run `npm audit` before every commit - **zero vulnerabilities allowed**
- Run `npm audit fix` to patch, `npm audit fix --force` only with review
- Use `npm outdated` weekly to check for updates
- Pin exact versions in production (`"lodash": "4.17.21"` not `"^4.17.21"`)
- Review changelogs before major version upgrades
- Remove unused dependencies (`npx depcheck`)

```bash
# Required checks before commit
npm audit              # Must pass with 0 vulnerabilities
npm outdated           # Review and update regularly
npx depcheck           # Remove unused deps
```

#### Forbidden Patterns
| Pattern | Why | Fix |
|---------|-----|-----|
| `any` | Disables type checking | Use `unknown` + type guards |
| `@ts-ignore` | Hides real errors | Fix the type error |
| `eslint-disable` | Hides code issues | Fix the lint error |
| `eval()` | Code injection risk | Use safe alternatives |
| `innerHTML = userInput` | XSS vulnerability | Use `textContent` or sanitize |
