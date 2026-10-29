// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 FINE Association <su@fa.org.tr>

//! The desktop binary: the application lives in the library, which Android's entry point calls
//! too (Plan 0008).

fn main() -> Result<(), slint::PlatformError> {
    app::run(std::env::args().collect())
}
