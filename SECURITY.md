<!--
SPDX-License-Identifier: AGPL-3.0-or-later
SPDX-FileCopyrightText: 2026 FINE Association <su@fa.org.tr>
-->

# Security Policy

## Threat Model & Untrusted File Handling

`PODEFA` is a PDF viewer and editor. By definition, **PDF viewers process complex, untrusted binary files** sourced from arbitrary origins (e.g. web downloads, email attachments). The PDF specification is vast, historically error-prone, and frequently targeted by malicious documents attempting parser exploitation, memory corruption, infinite parsing loops, or resource exhaustion.

Our security posture relies on:
1. **Memory Safety in Rust**: Core abstractions, data manipulation, and UI logic are implemented in safe Rust.
2. **Hardened Engine Integration**: The underlying MuPDF parsing and rasterization engine is maintained upstream and integrated behind sandboxed boundaries where possible.
3. **Rigorous Auditing**: Dependencies are continuously audited for vulnerabilities via `cargo-deny` and RustSec advisory tracking.

## Supported Versions

During active development (pre-1.0.0), only the latest commit on the `main` branch receives security fixes.

| Version | Supported          |
| ------- | ------------------ |
| `0.1.x` | :white_check_mark: |
| `< 0.1` | :x:                |

## Reporting a Vulnerability

**Please do not report security vulnerabilities through public GitHub issues.**

If you discover a vulnerability, suspect a memory corruption issue, or identify an exploit vector in file parsing or rendering:

1. Send an email directly to the project maintainers at: **su@fa.org.tr**.
2. Include the following details:
   - A description of the vulnerability and its potential impact.
   - Minimal proof-of-concept (PoC) document or reproduction steps.
   - Operating system and environment details.
   - Any proposed remediation or patch if available.
3. You will receive an acknowledgment within 48 hours.
4. We will coordinate a coordinated disclosure timeline, providing time for a patch and advisory release before public announcement.
