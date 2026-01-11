### Python Best Practices

#### Code Style
- Follow PEP 8 style guide
- Use type hints for function signatures
- Use `black` for formatting, `ruff` or `flake8` for linting
- Prefer `pathlib.Path` over `os.path` for path operations
- Use context managers (`with`) for file operations

#### Error Handling
```python
# GOOD: Specific exceptions with context
def read_config(path: Path) -> dict:
    try:
        with open(path, 'r', encoding='utf-8') as f:
            return json.load(f)
    except FileNotFoundError:
        raise ConfigError(f"Config file not found: {path}")
    except json.JSONDecodeError as e:
        raise ConfigError(f"Invalid JSON in {path}: {e}")

# BAD: Bare except or swallowing errors
def read_config(path):
    try:
        return json.load(open(path))
    except:  # Don't do this
        return {}
```

#### Security
- Never use `eval()` or `exec()` on user input
- Use `subprocess.run()` with explicit args, never `shell=True` with user input
- Use parameterized queries for SQL (never f-strings)
- Validate and sanitize all external input

#### Dependencies
- Pin dependency versions in `requirements.txt`
- Use virtual environments (`venv` or `poetry`)
- Run `pip-audit` to check for vulnerabilities

#### Testing
- Use `pytest` for testing
- Aim for high coverage with `pytest-cov`
- Mock external dependencies with `unittest.mock`
