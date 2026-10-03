# SPDX-License-Identifier: AGPL-3.0-or-later
# SPDX-FileCopyrightText: 2026 FINE Association <su@fa.org.tr>

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
