### Java Best Practices

#### Code Style
- Follow Google Java Style Guide or project conventions
- Use meaningful variable and method names
- Keep methods short (< 30 lines)
- Prefer composition over inheritance

#### Error Handling
```java
// GOOD: Specific exceptions with context
public Config readConfig(Path path) throws ConfigException {
    try {
        String content = Files.readString(path);
        return objectMapper.readValue(content, Config.class);
    } catch (IOException e) {
        throw new ConfigException("Failed to read config: " + path, e);
    } catch (JsonProcessingException e) {
        throw new ConfigException("Invalid JSON in config: " + path, e);
    }
}

// BAD: Catching generic Exception
public Config readConfig(Path path) {
    try {
        return objectMapper.readValue(Files.readString(path), Config.class);
    } catch (Exception e) {
        return null;  // Swallowing error
    }
}
```

#### Security
- Use PreparedStatement for SQL (never string concatenation)
- Validate all user input
- Use secure random (SecureRandom) for security-sensitive operations
- Never log sensitive data (passwords, tokens)

#### Testing
- Use JUnit 5 for unit tests
- Use Mockito for mocking dependencies
- Aim for high coverage on business logic
