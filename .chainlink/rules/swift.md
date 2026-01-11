### Swift Best Practices

#### Code Style
- Follow Swift API Design Guidelines
- Use `camelCase` for variables/functions, `PascalCase` for types
- Prefer `let` over `var` when possible
- Use optionals properly; avoid force unwrapping

```swift
// GOOD: Safe optional handling
func findUser(id: String) -> User? {
    guard let user = repository.find(id) else {
        return nil
    }
    return user
}

// Using optional binding
if let user = findUser(id: "123") {
    print(user.name)
}

// BAD: Force unwrapping
let user = findUser(id: "123")!  // Crash if nil
```

#### Error Handling
- Use `throws` for recoverable errors
- Use `Result<T, Error>` for async operations
- Handle all error cases explicitly

```swift
// GOOD: Proper error handling
func loadConfig() throws -> Config {
    let data = try Data(contentsOf: configURL)
    return try JSONDecoder().decode(Config.self, from: data)
}

do {
    let config = try loadConfig()
} catch {
    print("Failed to load config: \(error)")
}
```

#### Security
- Use Keychain for sensitive data
- Validate all user input
- Use App Transport Security (HTTPS)
- Never hardcode secrets
