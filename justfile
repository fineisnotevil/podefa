# SPDX-License-Identifier: CC0-1.0
# SPDX-FileCopyrightText: 2026 [YOUR_NAME] <[YOUR_EMAIL]>

# Default recipe lists available commands
default:
    @just --list

# Format all workspace code
fmt:
    cargo fmt --all

# Check formatting without modifying files
fmt-check:
    cargo fmt --all -- --check

# Run Clippy with warnings treated as errors
lint:
    cargo clippy --workspace --all-targets -- -D warnings

# Run all tests across workspace
test:
    cargo test --workspace

# Run dependency and license checks
deny:
    cargo deny check

# Check REUSE licensing compliance
reuse:
    reuse lint

# Run all verification checks (fmt, lint, test, deny, reuse)
check: fmt-check lint test deny reuse

# Run the desktop application
run:
    cargo run -p app
