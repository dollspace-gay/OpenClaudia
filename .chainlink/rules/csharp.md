### C# Best Practices

#### Code Style
- Follow .NET naming conventions (PascalCase for public, camelCase for private)
- Use `var` when type is obvious from right side
- Use expression-bodied members for simple methods
- Enable nullable reference types

```csharp
// GOOD: Modern C# style
public class UserService
{
    private readonly IUserRepository _repository;

    public UserService(IUserRepository repository)
        => _repository = repository;

    public async Task<User?> GetUserAsync(string id)
        => await _repository.FindByIdAsync(id);
}
```

#### Error Handling
- Use specific exception types
- Never catch and swallow exceptions silently
- Use `try-finally` or `using` for cleanup

```csharp
// GOOD: Proper async error handling
public async Task<Result<User>> GetUserAsync(string id)
{
    try
    {
        var user = await _repository.FindByIdAsync(id);
        return user is null
            ? Result<User>.NotFound()
            : Result<User>.Ok(user);
    }
    catch (DbException ex)
    {
        _logger.LogError(ex, "Database error fetching user {Id}", id);
        throw;
    }
}
```

#### Security
- Use parameterized queries (never string interpolation for SQL)
- Validate all input with data annotations or FluentValidation
- Use ASP.NET's built-in anti-forgery tokens
- Store secrets in Azure Key Vault or similar
