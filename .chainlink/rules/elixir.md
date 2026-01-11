# Elixir Core Rules

## Critical Mistakes to Avoid
- **No early returns**: Last expression in a block is always returned
- **No list indexing with brackets**: Use `Enum.at(list, i)`, not `list[i]`
- **No struct access syntax**: Use `struct.field`, not `struct[:field]` (structs don't implement Access)
- **Rebinding in blocks doesn't work**: `socket = if cond, do: assign(socket, :k, v)` - bind the result, not inside
- **`%{}` matches ANY map**: Use `map_size(map) == 0` guard for empty maps
- **No `String.to_atom/1` on user input**: Memory leak risk
- **No nested modules in same file**: Causes cyclic dependencies

## Pattern Matching & Functions
- Match on function heads over `if`/`case` in bodies
- Use guards: `when is_binary(name) and byte_size(name) > 0`
- Use `with` for chaining `{:ok, _}` / `{:error, _}` operations
- Predicates end with `?` (not `is_`): `valid?/1` not `is_valid/1`
- Reserve `is_thing` names for guard macros

## Data Structures
- Prepend to lists: `[new | list]` not `list ++ [new]`
- Structs for known shapes, maps for dynamic data, keyword lists for options
- Use `Enum` over recursion; use `Stream` for large collections

## OTP
- `GenServer.call/3` for sync (prefer for back-pressure), `cast/2` for fire-and-forget
- DynamicSupervisor/Registry require names: `{DynamicSupervisor, name: MyApp.MySup}`
- `Task.async_stream(coll, fn, timeout: :infinity)` for concurrent enumeration

## Testing & Debugging
- `mix test path/to/test.exs:123` - run specific test
- `mix test --failed` - rerun failures
- `dbg/1` for debugging output

## Documentation Lookup
```bash
mix usage_rules.docs Enum.zip/1              # Function docs
mix usage_rules.search_docs "query" -p pkg   # Search package docs
```

