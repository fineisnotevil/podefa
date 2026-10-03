// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 [YOUR_NAME] <[YOUR_EMAIL]>

slint::slint! {
    export component MainWindow inherits Window {
        title: "[PROJECT_NAME]";
        width: 800px;
        height: 600px;
    }
}

fn main() -> Result<(), slint::PlatformError> {
    // Verify dependency linkages
    let _ = pdf_core::Rect::default();
    let _ = engine_mupdf::MupdfEngine::new();

    let main_window = MainWindow::new()?;
    main_window.run()
}
