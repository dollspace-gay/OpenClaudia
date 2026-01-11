### Go Best Practices

#### Code Style
- Use `gofmt` for formatting
- Use `golint` and `go vet` for linting
- Follow effective Go guidelines
- Keep functions short and focused

#### Error Handling
```go
// GOOD: Check and handle errors
func readConfig(path string) (*Config, error) {
    data, err := os.ReadFile(path)
    if err != nil {
        return nil, fmt.Errorf("reading config: %w", err)
    }

    var config Config
    if err := json.Unmarshal(data, &config); err != nil {
        return nil, fmt.Errorf("parsing config: %w", err)
    }
    return &config, nil
}

// BAD: Ignoring errors
func readConfig(path string) *Config {
    data, _ := os.ReadFile(path)  // Don't ignore errors
    var config Config
    json.Unmarshal(data, &config)
    return &config
}
```

#### Concurrency
- Use channels for communication between goroutines
- Use `sync.WaitGroup` for waiting on multiple goroutines
- Use `context.Context` for cancellation and timeouts
- Avoid shared mutable state; prefer message passing

#### Security
- Use `html/template` for HTML output (auto-escaping)
- Use parameterized queries for SQL
- Validate all input at API boundaries
- Use `crypto/rand` for secure random numbers
