### JavaScript Best Practices

#### Code Style
- Use `const` by default, `let` when needed, never `var`
- Use arrow functions for callbacks
- Use template literals over string concatenation
- Use destructuring for object/array access

#### Error Handling
```javascript
// GOOD: Proper async error handling
async function fetchUser(id) {
    try {
        const response = await fetch(`/api/users/${id}`);
        if (!response.ok) {
            throw new Error(`HTTP ${response.status}`);
        }
        return await response.json();
    } catch (error) {
        console.error('Failed to fetch user:', error);
        throw error;  // Re-throw or handle appropriately
    }
}

// BAD: Ignoring errors
async function fetchUser(id) {
    const response = await fetch(`/api/users/${id}`);
    return response.json();  // No error handling
}
```

#### Security
- Never use `eval()` or `innerHTML` with user input
- Validate all input on both client and server
- Use `textContent` instead of `innerHTML` when possible
- Sanitize URLs before navigation or fetch
