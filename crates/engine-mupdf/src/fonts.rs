// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 FINE Association <su@fa.org.tr>

//! Fonts a document names but does not embed (Plan 0005 Phase 1, §3.8).
//!
//! mupdf-rs answers MuPDF's request for such a font through its loaders: a registered one first,
//! then the system fonts through `font-kit` (`patches/mupdf/src/font_loader.rs`). The system lookup
//! builds a new `font_kit::source::SystemSource` for every request (`src/system_font.rs:127`),
//! which costs 1.2-1.7 s on Windows - per font object, so per document, and again for every page
//! copy that brings its own copy of a font. The 14 standard PDF fonts (Helvetica, Times, Courier,
//! Symbol, ZapfDingbats and their styles) are the most common unembedded fonts, and MuPDF has
//! metric-compatible versions of all of them built in; this loader hands MuPDF those first, and
//! leaves every other name to the lookups behind it.
//!
//! MuPDF also asks the loaders for the generic names `serif`, `sans-serif` and `monospace` when it
//! draws a form field's text with letters the field's font cannot encode (Turkish ğ, ş, İ, ı in a
//! WinAnsi Helvetica): three requests per fill, each a system font search of 0.8-1.2 s (Plan 0006
//! §3.2). Those names get MuPDF's Times, Helvetica and Courier too.

use mupdf::{Font, FontHints, FontLoader};
use std::sync::Once;

/// The 14 standard fonts first, from MuPDF's own copies; on Android, the platform's fonts next, as
/// mupdf-rs' default loader there would (registering a loader replaces it).
struct Base14First;

impl FontLoader for Base14First {
    fn load_font(&self, name: &str, hints: FontHints) -> Option<Font> {
        if let Some(font) = base14(generic(name)) {
            return Some(font);
        }
        #[cfg(target_os = "android")]
        {
            mupdf::AndroidFontLoader.load_font(name, hints)
        }
        #[cfg(not(target_os = "android"))]
        {
            let _ = hints;
            None
        }
    }

    #[cfg(target_os = "android")]
    fn load_cjk_font(
        &self,
        name: &str,
        ordering: mupdf::CjkFontOrdering,
        serif: bool,
    ) -> Option<Font> {
        mupdf::AndroidFontLoader.load_cjk_font(name, ordering, serif)
    }

    #[cfg(target_os = "android")]
    fn load_fallback_font(&self, script: u32, language: u32, hints: FontHints) -> Option<Font> {
        mupdf::AndroidFontLoader.load_fallback_font(script, language, hints)
    }
}

/// The standard font a generic family name stands for; any other name as it is.
pub(crate) fn generic(name: &str) -> &str {
    match name {
        "serif" => "Times-Roman",
        "sans-serif" => "Helvetica",
        "monospace" => "Courier",
        other => other,
    }
}

/// MuPDF's built-in copy of a standard font, by any of the names MuPDF knows it by; `None` for any
/// other font.
pub(crate) fn base14(name: &str) -> Option<Font> {
    // `Font::new` looks the name up among the base 14 (`fz_lookup_base14_font`) and fails for any
    // other name.
    Font::new(name).ok()
}

/// Registers the loader, once per process, before the first document is opened.
pub(crate) fn install() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| mupdf::set_font_loader(Base14First));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_standard_fonts_come_from_mupdf_and_others_do_not() {
        for name in [
            "Helvetica",
            "Helvetica-Bold",
            "Times-Roman",
            "Courier",
            "Symbol",
        ] {
            assert!(base14(name).is_some(), "{name}");
        }
        assert!(base14("NoSuchFont-Regular").is_none());
    }

    #[test]
    fn generic_families_are_standard_fonts() {
        for name in ["serif", "sans-serif", "monospace"] {
            assert!(base14(generic(name)).is_some(), "{name}");
        }
        assert_eq!(generic("Arial"), "Arial");
    }
}
