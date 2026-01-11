### Zig Best Practices

#### Code Style
- Follow Zig Style Guide
- Use `const` by default; `var` only when mutation needed
- Prefer slices over pointers when possible
- Use meaningful names; avoid single-letter variables

```zig
// GOOD: Clear, idiomatic Zig
const User = struct {
    id: []const u8,
    name: []const u8,
};

fn findUser(allocator: std.mem.Allocator, id: []const u8) !?User {
    const user = try repository.find(allocator, id);
    return user;
}
```

#### Error Handling
- Use error unions (`!T`) for fallible operations
- Handle errors with `try`, `catch`, or explicit checks
- Create meaningful error sets

```zig
// GOOD: Proper error handling
const ConfigError = error{
    FileNotFound,
    ParseError,
    OutOfMemory,
};

fn loadConfig(allocator: std.mem.Allocator) ConfigError!Config {
    const file = std.fs.cwd().openFile("config.json", .{}) catch |err| {
        return ConfigError.FileNotFound;
    };
    defer file.close();
    // ...
}
```

#### Memory Safety
- Always pair allocations with deallocations
- Use `defer` for cleanup
- Prefer stack allocation when size is known
- Use allocators explicitly; never use global state
