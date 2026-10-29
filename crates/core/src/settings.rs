// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 FINE Association <su@fa.org.tr>

//! The user's settings: theme and recent files (Plan 0003, §3.8 and §4.7), and the name written
//! into new annotations (Plan 0004, §4.3).
//!
//! Plain text, one `key=value` per line, so no parser dependency is needed and a user can read or
//! fix the file by hand. Parsing never fails: an unknown key, a malformed line or a missing file
//! all fall back to the defaults, because a settings file must never stop the viewer from opening.
//! Where the file lives is the app's business; this module only turns text into [`Settings`] and
//! back.

use std::path::{Path, PathBuf};

/// Recent files kept, newest first.
pub const RECENT_MAX: usize = 10;

/// The colour scheme the user chose.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Theme {
    /// Follow the operating system.
    #[default]
    System,
    /// Always light.
    Light,
    /// Always dark.
    Dark,
}

impl Theme {
    fn as_str(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::Light => "light",
            Self::Dark => "dark",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "system" => Some(Self::System),
            "light" => Some(Self::Light),
            "dark" => Some(Self::Dark),
            _ => None,
        }
    }
}

/// Everything the app remembers between runs.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Settings {
    /// The chosen colour scheme.
    pub theme: Theme,
    /// Recently opened documents, newest first, at most [`RECENT_MAX`].
    pub recent: Vec<PathBuf>,
    /// Whether the sidebar (thumbnails and outline) is shown.
    pub sidebar: bool,
    /// Whether form fields are left unshaded. By default they are shaded on the page, so they can
    /// be seen before they are touched (Plan 0006).
    pub plain_fields: bool,
    /// The author written into new annotations. `None` writes none: nothing personal goes into a
    /// shared file unless the user typed it here (Plan 0004 §0, question 5).
    pub author: Option<String>,
    /// Whether animations are switched off (Plan 0007 Phase 5, row AX).
    pub reduce_motion: bool,
}

impl Settings {
    /// Reads settings from the file's text. Unknown keys, malformed lines and values that do not
    /// parse are skipped; whatever is missing keeps its default.
    pub fn parse(text: &str) -> Self {
        let mut settings = Self::default();
        for line in text.lines() {
            let line = line.trim_end_matches('\r');
            if line.trim_start().starts_with('#') {
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            match key.trim() {
                "sidebar" => match value.trim() {
                    "1" | "true" => settings.sidebar = true,
                    "0" | "false" => settings.sidebar = false,
                    _ => {}
                },
                "reduce_motion" => match value.trim() {
                    "1" | "true" => settings.reduce_motion = true,
                    "0" | "false" => settings.reduce_motion = false,
                    _ => {}
                },
                "field_highlight" => match value.trim() {
                    "1" | "true" => settings.plain_fields = false,
                    "0" | "false" => settings.plain_fields = true,
                    _ => {}
                },
                "theme" => {
                    if let Some(theme) = Theme::parse(value.trim()) {
                        settings.theme = theme;
                    }
                }
                "author" => {
                    let name = value.trim();
                    settings.author = (!name.is_empty()).then(|| name.to_owned());
                }
                // The value is taken as is: a path may start or end with spaces.
                "recent" if !value.is_empty() && settings.recent.len() < RECENT_MAX => {
                    let path = PathBuf::from(value);
                    if !settings.recent.contains(&path) {
                        settings.recent.push(path);
                    }
                }
                _ => {}
            }
        }
        settings
    }

    /// The file's text. A path that cannot be written on one line - not valid UTF-8, or containing
    /// a line break - is left out rather than written in a form [`Self::parse`] would misread.
    pub fn serialize(&self) -> String {
        let mut out = String::from("# PODEFA settings\n");
        out.push_str("theme=");
        out.push_str(self.theme.as_str());
        out.push('\n');
        out.push_str(if self.sidebar {
            "sidebar=1\n"
        } else {
            "sidebar=0\n"
        });
        out.push_str(if self.plain_fields {
            "field_highlight=0\n"
        } else {
            "field_highlight=1\n"
        });
        if self.reduce_motion {
            out.push_str("reduce_motion=1\n");
        }
        if let Some(author) = self.author.as_deref().filter(|a| !a.contains(['\n', '\r'])) {
            out.push_str("author=");
            out.push_str(author);
            out.push('\n');
        }
        for path in &self.recent {
            let Some(text) = path.to_str() else {
                continue;
            };
            if text.contains(['\n', '\r']) {
                continue;
            }
            out.push_str("recent=");
            out.push_str(text);
            out.push('\n');
        }
        out
    }

    /// Records that `path` was opened: it moves to the front, appears once, and the list keeps
    /// at most [`RECENT_MAX`] entries.
    pub fn opened(&mut self, path: &Path) {
        self.recent.retain(|p| p != path);
        self.recent.insert(0, path.to_path_buf());
        self.recent.truncate(RECENT_MAX);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_round_trip_keeps_theme_and_order() {
        let mut settings = Settings {
            theme: Theme::Dark,
            sidebar: true,
            plain_fields: true,
            ..Settings::default()
        };
        settings.opened(Path::new("C:\\docs\\a.pdf"));
        settings.opened(Path::new("/home/user/b c.pdf"));
        let text = settings.serialize();
        assert_eq!(Settings::parse(&text), settings);
        assert_eq!(settings.recent[0], PathBuf::from("/home/user/b c.pdf"));
    }

    #[test]
    fn reduce_motion_is_kept_and_off_by_default() {
        assert!(!Settings::default().reduce_motion);
        assert!(!Settings::default().serialize().contains("reduce_motion"));
        let reduced = Settings {
            reduce_motion: true,
            ..Settings::default()
        };
        assert_eq!(Settings::parse(&reduced.serialize()), reduced);
        assert!(
            !Settings::parse(
                "reduce_motion=0
"
            )
            .reduce_motion
        );
    }

    #[test]
    fn the_author_is_written_only_when_set() {
        let settings = Settings::default();
        assert!(!settings.serialize().contains("author"));
        assert_eq!(Settings::parse("author=\n").author, None);
        assert_eq!(Settings::parse("author=   \n").author, None);
        let named = Settings {
            author: Some("Ada Lovelace".into()),
            ..Settings::default()
        };
        assert_eq!(Settings::parse(&named.serialize()), named);
    }

    #[test]
    fn missing_empty_and_corrupt_files_give_the_defaults() {
        assert_eq!(Settings::parse(""), Settings::default());
        assert_eq!(
            Settings::parse("\u{0}\u{1}garbage\nno equals sign\n=\ntheme=purple\n"),
            Settings::default()
        );
    }

    #[test]
    fn unknown_keys_and_comments_are_skipped() {
        let text = "# hi\nfuture_key=1\ntheme=light\r\nrecent=/a.pdf\r\n";
        let settings = Settings::parse(text);
        assert_eq!(settings.theme, Theme::Light);
        assert_eq!(settings.recent, vec![PathBuf::from("/a.pdf")]);
    }

    #[test]
    fn opening_moves_a_file_to_the_front_once_and_caps_the_list() {
        let mut settings = Settings::default();
        for i in 0..15 {
            settings.opened(Path::new(&format!("/{i}.pdf")));
        }
        assert_eq!(settings.recent.len(), RECENT_MAX);
        assert_eq!(settings.recent[0], PathBuf::from("/14.pdf"));
        settings.opened(Path::new("/10.pdf"));
        assert_eq!(settings.recent[0], PathBuf::from("/10.pdf"));
        assert_eq!(
            settings
                .recent
                .iter()
                .filter(|p| p.as_path() == Path::new("/10.pdf"))
                .count(),
            1
        );
        assert_eq!(settings.recent.len(), RECENT_MAX);
    }

    #[test]
    fn a_file_with_more_than_the_cap_or_duplicates_is_trimmed() {
        let mut text = String::new();
        for i in 0..20 {
            text.push_str(&format!("recent=/{}.pdf\n", i % 12));
        }
        let settings = Settings::parse(&text);
        assert_eq!(settings.recent.len(), RECENT_MAX);
        let mut unique = settings.recent.clone();
        unique.dedup();
        assert_eq!(unique.len(), RECENT_MAX);
    }

    #[test]
    fn a_path_with_a_line_break_is_not_written() {
        let settings = Settings {
            recent: vec![PathBuf::from("/ok.pdf"), PathBuf::from("/bad\nname.pdf")],
            ..Settings::default()
        };
        let parsed = Settings::parse(&settings.serialize());
        assert_eq!(parsed.recent, vec![PathBuf::from("/ok.pdf")]);
    }
}
