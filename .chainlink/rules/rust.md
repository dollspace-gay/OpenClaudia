### Rust Best Practices

#### Code Style
- Use `rustfmt` for formatting (run `cargo fmt` before committing)
- Use `clippy` for linting (run `cargo clippy -- -D warnings`)
- Prefer `?` operator over `.unwrap()` for error handling
- Use `anyhow::Result` for application errors, `thiserror` for library errors
- Avoid `.clone()` unless necessary - prefer references
- Use `&str` for function parameters, `String` for owned data

#### Error Handling
```rust
// GOOD: Propagate errors with context
fn read_config(path: &Path) -> Result<Config> {
    let content = fs::read_to_string(path)
        .context("Failed to read config file")?;
    serde_json::from_str(&content)
        .context("Failed to parse config")
}

// BAD: Panic on error
fn read_config(path: &Path) -> Config {
    let content = fs::read_to_string(path).unwrap();  // Don't do this
    serde_json::from_str(&content).unwrap()
}
```

#### Memory Safety
- Never use `unsafe` without explicit justification and review
- Prefer `Vec` over raw pointers
- Use `Arc<Mutex<T>>` for shared mutable state across threads
- Avoid `static mut` - use `lazy_static` or `once_cell` instead

#### Testing
- Write unit tests with `#[cfg(test)]` modules
- Use `tempfile` for tests involving filesystem
- Run `cargo test` before committing
- Use `cargo tarpaulin` for coverage reports

#### SQL Injection Prevention
Always use parameterized queries with `rusqlite::params![]`:
```rust
// GOOD
conn.execute("INSERT INTO users (name) VALUES (?1)", params![name])?;

// BAD - SQL injection vulnerability
conn.execute(&format!("INSERT INTO users (name) VALUES ('{}')", name), [])?;
```
