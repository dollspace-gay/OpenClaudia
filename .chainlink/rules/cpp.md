### C++ Best Practices

#### Modern C++ (C++17+)
- Use smart pointers (`unique_ptr`, `shared_ptr`) over raw pointers
- Use RAII for resource management
- Prefer `std::string` and `std::vector` over C arrays
- Use `auto` for complex types, explicit types for clarity

```cpp
// GOOD: Modern C++ with smart pointers
auto config = std::make_unique<Config>();
auto users = std::vector<User>{};

// BAD: Manual memory management
Config* config = new Config();
// ... forgot to delete
```

#### Error Handling
- Use exceptions for exceptional cases
- Use `std::optional` for values that may not exist
- Use `std::expected` (C++23) or result types for expected failures

```cpp
// GOOD: Optional for missing values
std::optional<User> findUser(const std::string& id) {
    auto it = users.find(id);
    if (it == users.end()) {
        return std::nullopt;
    }
    return it->second;
}
```

#### Security
- Validate all input boundaries
- Use `std::string_view` for non-owning string references
- Avoid C-style casts; use `static_cast`, `dynamic_cast`
- Never use `sprintf`; use `std::format` or streams
