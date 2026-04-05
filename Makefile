.PHONY: build test lint format check clean install setup

# Build
build:
	cargo build

release:
	cargo build --release

# Quality
test:
	cargo test

lint:
	cargo clippy --all-targets -- -D warnings

format:
	cargo fmt

format-check:
	cargo fmt -- --check

# All checks (same as pre-commit hook)
check: format-check lint test
	@echo "All checks passed."

# Full PR checklist
pr: format-check lint test
	@echo "Ready for PR."

# Install
install:
	cargo install --path crates/lab-cli

# Setup git hooks
setup:
	git config core.hooksPath .githooks
	@echo "Git hooks configured. Pre-commit will run fmt + clippy + tests."

# Clean
clean:
	cargo clean
	rm -rf .lab/tmp .lab/locks .lab/artifacts .lab/cache
