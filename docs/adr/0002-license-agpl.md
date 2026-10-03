<!--
SPDX-License-Identifier: CC0-1.0
SPDX-FileCopyrightText: 2026 [YOUR_NAME] <[YOUR_EMAIL]>
-->

# ADR 0002: Licensing (AGPL-3.0-or-later and DCO Contributions)

## Status

Accepted

## Context

Choosing a license for `[PROJECT_NAME]` involves balancing external dependency licensing constraints with our philosophy on open-source software:
1. **MuPDF Dependency**: MuPDF is published by Artifex Software under the GNU Affero General Public License (AGPL-3.0-or-later) or commercial licenses. Any binary linking against MuPDF without a commercial license must comply with AGPL-3.0 copyleft terms.
2. **Slint Dependency**: Slint is dual-licensed under GPLv3, a proprietary license, and a paid ambassador agreement. Utilizing Slint in an open-source project without a proprietary license requires adherence to GPLv3.
3. **Contribution Management**: We need a frictionless yet legally sound contribution model that ensures contributors retain copyright and certify their contributions without burdensome CLA paperwork.

## Decision

1. **License**: We license `[PROJECT_NAME]` under the **GNU Affero General Public License v3.0 or later (AGPL-3.0-or-later)**.
   - AGPL-3.0 satisfies MuPDF's AGPL requirements.
   - Slint is consumed under its GPLv3 option, which is compatible with AGPL-3.0-or-later (GPLv3 section 13 permits combining with AGPLv3 code).
   - Third-party dependencies must carry licenses compatible with AGPL-3.0 (enforced in CI by `cargo-deny`).
2. **Contribution Model**: We adopt the **Developer Certificate of Origin (DCO) Version 1.1** instead of a Contributor License Agreement (CLA).
   - Contributors sign off on their commits with `Signed-off-by:` trailers (`git commit -s`).
   - Contributors retain their own copyright.
3. **Compliance Standard**: We enforce the **REUSE 3.3 specification** across the entire repository to ensure every single source file, asset, and documentation file carries explicit copyright and license metadata.

## Consequences

### Positive
- Fully open, strong copyleft terms ensure that downstream modifications remain free and open.
- Legal compatibility between MuPDF (AGPLv3) and Slint (GPLv3) is guaranteed.
- Zero barrier to entry for contributors compared to legal CLA forms; standardized DCO sign-offs are widely understood and supported across git tooling.
- Automated compliance auditing via `cargo-deny` and `reuse lint` prevents accidental inclusion of incompatible code.

### Negative
- Proprietary commercial entities cannot link or embed `[PROJECT_NAME]` without complying with AGPL-3.0 obligations.
