### PHP Best Practices

#### Code Style
- Follow PSR-12 coding standard
- Use strict types: `declare(strict_types=1);`
- Use type hints for parameters and return types
- Use Composer for dependency management

```php
<?php
declare(strict_types=1);

// GOOD: Typed, modern PHP
class UserService
{
    public function __construct(
        private readonly UserRepository $repository
    ) {}

    public function findUser(string $id): ?User
    {
        return $this->repository->find($id);
    }
}
```

#### Error Handling
- Use exceptions for error handling
- Create custom exception classes
- Never suppress errors with `@`

#### Security
- Use PDO with prepared statements (never string interpolation)
- Use `password_hash()` and `password_verify()` for passwords
- Validate and sanitize all user input
- Use CSRF tokens for forms
- Set secure cookie flags

```php
// GOOD: Prepared statement
$stmt = $pdo->prepare('SELECT * FROM users WHERE id = :id');
$stmt->execute(['id' => $id]);

// BAD: SQL injection vulnerability
$result = $pdo->query("SELECT * FROM users WHERE id = '$id'");
```
