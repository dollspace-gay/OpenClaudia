### Odin Best Practices

#### Code Style
- Follow Odin naming conventions
- Use `snake_case` for procedures and variables
- Use `Pascal_Case` for types
- Prefer explicit over implicit

```odin
// GOOD: Clear Odin code
User :: struct {
    id:   string,
    name: string,
}

find_user :: proc(id: string) -> (User, bool) {
    user, found := repository[id]
    return user, found
}
```

#### Error Handling
- Use multiple return values for errors
- Use `or_return` for early returns
- Create explicit error types when needed

```odin
// GOOD: Explicit error handling
Config_Error :: enum {
    File_Not_Found,
    Parse_Error,
}

load_config :: proc(path: string) -> (Config, Config_Error) {
    data, ok := os.read_entire_file(path)
    if !ok {
        return {}, .File_Not_Found
    }
    defer delete(data)

    config, parse_ok := parse_config(data)
    if !parse_ok {
        return {}, .Parse_Error
    }
    return config, nil
}
```

#### Memory Management
- Use explicit allocators
- Prefer temp allocator for short-lived allocations
- Use `defer` for cleanup
- Be explicit about ownership
