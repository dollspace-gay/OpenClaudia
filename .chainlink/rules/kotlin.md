### Kotlin Best Practices

#### Code Style
- Follow Kotlin coding conventions
- Use `val` over `var` when possible
- Use data classes for simple data holders
- Leverage null safety features

```kotlin
// GOOD: Idiomatic Kotlin
data class User(val id: String, val name: String)

class UserService(private val repository: UserRepository) {
    fun findUser(id: String): User? =
        repository.find(id)

    fun getOrCreateUser(id: String, name: String): User =
        findUser(id) ?: repository.create(User(id, name))
}
```

#### Null Safety
- Avoid `!!` (force non-null); use safe calls instead
- Use `?.let {}` for conditional execution
- Use Elvis operator `?:` for defaults

```kotlin
// GOOD: Safe null handling
val userName = user?.name ?: "Unknown"
user?.let { saveToDatabase(it) }

// BAD: Force unwrapping
val userName = user!!.name  // Crash if null
```

#### Coroutines
- Use structured concurrency with `CoroutineScope`
- Handle exceptions in coroutines properly
- Use `withContext` for context switching

#### Security
- Use parameterized queries
- Validate input at boundaries
- Use sealed classes for exhaustive error handling
