### Ruby Best Practices

#### Code Style
- Follow Ruby Style Guide (use RuboCop)
- Use 2 spaces for indentation
- Prefer symbols over strings for hash keys
- Use `snake_case` for methods and variables

```ruby
# GOOD: Idiomatic Ruby
class UserService
  def initialize(repository)
    @repository = repository
  end

  def find_user(id)
    @repository.find(id)
  rescue ActiveRecord::RecordNotFound
    nil
  end
end

# BAD: Non-idiomatic
class UserService
  def initialize(repository)
    @repository = repository
  end
  def findUser(id)  # Wrong naming
    begin
      @repository.find(id)
    rescue
      return nil
    end
  end
end
```

#### Error Handling
- Use specific exception classes
- Don't rescue `Exception` (too broad)
- Use `ensure` for cleanup

#### Security
- Use parameterized queries (ActiveRecord does this by default)
- Sanitize user input in views (Rails does this by default)
- Never use `eval` or `send` with user input
- Use `strong_parameters` in Rails controllers
