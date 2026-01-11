### Scala Best Practices

#### Code Style
- Follow Scala Style Guide
- Prefer immutability (`val` over `var`)
- Use case classes for data
- Leverage pattern matching

```scala
// GOOD: Idiomatic Scala
case class User(id: String, name: String)

class UserService(repository: UserRepository) {
  def findUser(id: String): Option[User] =
    repository.find(id)

  def processUser(id: String): Either[Error, Result] =
    findUser(id) match {
      case Some(user) => Right(process(user))
      case None       => Left(UserNotFound(id))
    }
}
```

#### Error Handling
- Use `Option` for missing values
- Use `Either` or `Try` for operations that can fail
- Avoid throwing exceptions in pure code

```scala
// GOOD: Using Either for errors
def parseConfig(json: String): Either[ParseError, Config] =
  decode[Config](json).left.map(e => ParseError(e.getMessage))

// Pattern match on result
parseConfig(input) match {
  case Right(config) => useConfig(config)
  case Left(error)   => logger.error(s"Parse failed: $error")
}
```

#### Security
- Use prepared statements for database queries
- Validate input with refined types when possible
- Never interpolate user input into queries
