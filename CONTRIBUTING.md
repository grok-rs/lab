# Contributing to lab

## Development Setup

```bash
git clone https://github.com/grok-rs/lab.git
cd lab
make setup    # Configure git hooks (fmt + clippy + tests on commit)
make check    # Run all quality checks
```

## Workflow

1. Fork and create a branch from `main`
2. Make your changes
3. Run `make check` (or just commit — the pre-commit hook runs it)
4. Open a PR

## Code Quality

The pre-commit hook enforces:
- `cargo fmt` — consistent formatting
- `cargo clippy -D warnings` — zero warnings
- `cargo test` — all tests pass

## Project Structure

```
crates/
├── lab-core/    # Library: parsing, planning, execution, analysis
└── lab-cli/     # Binary: CLI, display, MCP server
```

## Adding a GitLab CI Keyword

1. Add the field to the appropriate struct in `crates/lab-core/src/model/job.rs`
2. If it needs execution logic, add it in `crates/lab-core/src/runner/`
3. Add a test in `crates/lab-core/tests/keywords.rs`
4. Update `docs/gitlab-ci-reference.md`
5. Update `README.md` coverage checklist

## Adding an Analysis Rule

1. Add a check function in `crates/lab-core/src/analyze.rs`
2. Call it from `analyze()`
3. Add a fix suggestion in `crates/lab-cli/src/mcp.rs` → `tool_suggest_fix()`

## Running Tests

```bash
cargo test                                    # All tests
cargo test -p lab-core                        # Core library only
cargo test -p lab-core --test keywords        # Keyword tests
cargo test -p lab-core --test spec_examples   # Spec-derived tests
```
