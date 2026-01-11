### C Best Practices

#### Memory Safety
- Always check return values of malloc/calloc
- Free all allocated memory (use tools like valgrind)
- Initialize all variables before use
- Use sizeof() with the variable, not the type

```c
// GOOD: Safe memory allocation
int *arr = malloc(n * sizeof(*arr));
if (arr == NULL) {
    return -1;  // Handle allocation failure
}
// ... use arr ...
free(arr);

// BAD: Unchecked allocation
int *arr = malloc(n * sizeof(int));
arr[0] = 1;  // Crash if malloc failed
```

#### Buffer Safety
- Always bounds-check array access
- Use `strncpy`/`snprintf` instead of `strcpy`/`sprintf`
- Validate string lengths before copying

```c
// GOOD: Safe string copy
char dest[64];
strncpy(dest, src, sizeof(dest) - 1);
dest[sizeof(dest) - 1] = '\0';

// BAD: Buffer overflow risk
char dest[64];
strcpy(dest, src);  // No bounds check
```

#### Security
- Never use `gets()` (use `fgets()`)
- Validate all external input
- Use constant-time comparison for secrets
- Avoid integer overflow in size calculations
