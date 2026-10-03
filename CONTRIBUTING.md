<!--
SPDX-License-Identifier: CC0-1.0
SPDX-FileCopyrightText: 2026 [YOUR_NAME] <[YOUR_EMAIL]>
-->

# Contributing to [PROJECT_NAME]

Thank you for your interest in contributing to [PROJECT_NAME]! We welcome code contributions, issue reports, documentation improvements, and architectural feedback.

## Developer Certificate of Origin (DCO)

We do not require a Contributor License Agreement (CLA). Instead, all contributions are made under the **Developer Certificate of Origin (DCO), Version 1.1**.

By signing off a commit, you certify that you have the right to submit the work under the project's license ([AGPL-3.0-or-later](LICENSE)).

### Signing Off Commits

Every commit in a pull request must include a `Signed-off-by` trailer with your real name and email address. You can automate this by passing the `-s` or `--signoff` flag to `git commit`:

```bash
git commit -s -m "feat(core): add crop box support to Rect"
```

This appends a line like:
```text
Signed-off-by: Your Name <your.email@example.com>
```

Commits missing a valid sign-off will fail the automated pull request checks.

## Development Setup

1. **Rust Toolchain**: Install Rust via [rustup](https://rustup.rs/). The project specifies its toolchain channel and required components (`rustfmt`, `clippy`) in [`rust-toolchain.toml`](rust-toolchain.toml).
2. **Native Tooling**: Install platform prerequisites (C compiler, LLVM / `libclang`) as described in [README.md](README.md#prerequisites--build-instructions).
3. **Developer Utilities** (optional but recommended):
   - `cargo-deny`: For checking licenses and security advisories (`cargo install cargo-deny`).
   - `reuse`: For validating copyright and license compliance (`pip install reuse`).
   - `just`: For command shortcut automation (`cargo install just`).

## Branch and Pull Request Workflow

1. Fork the repository on GitHub: `[GITHUB_URL]`.
2. Clone your fork locally and create a feature branch:
   ```bash
   git checkout -b feature/my-new-feature
   ```
3. Make focused, minimal changes. Keep commits logically separated.
4. Follow the **Conventional Commits** specification:
   - `feat:` A new feature or user-visible enhancement
   - `fix:` A bug fix
   - `docs:` Documentation changes only
   - `chore:` Maintenance tasks, dependency bumps, or tooling adjustments
   - `ci:` Changes to CI configuration or automated workflows
   - `refactor:` Code restructuring that does not alter external behavior
   - `test:` Adding or correcting tests
5. Rebase on the upstream `main` branch before submitting your pull request:
   ```bash
   git fetch upstream
   git rebase upstream/main
   ```
6. Open a pull request against `main`. Ensure the PR template checklist is completed.

## Quality Checks

Before pushing your changes, run the local quality verification suite:

```bash
# 1. Format check
cargo fmt --all -- --check

# 2. Linting
cargo clippy --workspace --all-targets -- -D warnings

# 3. Tests
cargo test --workspace

# 4. License and advisory audit
cargo deny check

# 5. REUSE compliance
reuse lint
```

If you have `just` installed, you can run all checks with:
```bash
just check
```

## Architecture and Dependency Rules

To maintain our lightweight, modular architecture:
- `crates/core` must remain strictly engine-agnostic and UI-agnostic. It must **never** depend on `slint`, `mupdf`, or any GUI runtime.
- `crates/render` depends only on `crates/core`.
- `crates/engine-mupdf` depends only on `crates/core` and the `mupdf` crate.
- `crates/app` integrates the frontend UI and coordinates dependencies.
- Changes that violate this dependency direction will be rejected.
