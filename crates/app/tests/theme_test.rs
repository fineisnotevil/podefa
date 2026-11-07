// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 FINE Association <su@fa.org.tr>

//! Plan 0007 Phase 5, rows AW and AX, checked from the markup: the text colours' contrast on the
//! surfaces they are drawn on, in both themes, and that every animation takes a duration that
//! "Reduce motion" sets to 0.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

fn ui_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("ui")
}

/// The theme's colour tokens that differ by theme: name -> (dark, light).
fn tokens() -> HashMap<String, (String, String)> {
    let text = std::fs::read_to_string(ui_dir().join("theme.slint")).expect("theme.slint");
    let mut out = HashMap::new();
    for line in text.lines() {
        let line = line.trim();
        let Some(rest) = line.strip_prefix("out property <color> ") else {
            continue;
        };
        let Some((name, value)) = rest.split_once(':') else {
            continue;
        };
        let value = value.trim().trim_end_matches(';');
        if let Some(choice) = value.strip_prefix("dark ? ")
            && let Some((dark, light)) = choice.split_once(" : ")
        {
            out.insert(name.trim().to_owned(), (dark.to_owned(), light.to_owned()));
        }
    }
    out
}

/// `#rrggbb` or `#rrggbbaa` as linear-light channels and alpha.
fn parse(hex: &str) -> ([f64; 3], f64) {
    let hex = hex.trim_start_matches('#');
    let byte = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).expect("hex") as f64 / 255.0;
    let alpha = if hex.len() == 8 { byte(6) } else { 1.0 };
    ([byte(0), byte(2), byte(4)], alpha)
}

fn luminance(c: [f64; 3]) -> f64 {
    let lin = |v: f64| {
        if v <= 0.04045 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * lin(c[0]) + 0.7152 * lin(c[1]) + 0.0722 * lin(c[2])
}

/// WCAG's contrast ratio of `fg` drawn over the opaque `bg`.
fn contrast(fg: &str, bg: &str) -> f64 {
    let (f, alpha) = parse(fg);
    let (b, _) = parse(bg);
    let mixed = [0, 1, 2].map(|i| alpha * f[i] + (1.0 - alpha) * b[i]);
    let (l1, l2) = (luminance(mixed), luminance(b));
    (l1.max(l2) + 0.05) / (l1.min(l2) + 0.05)
}

#[test]
fn contrast_is_known_for_black_on_white() {
    assert!((contrast("#000000", "#ffffff") - 21.0).abs() < 1e-9);
    assert!((contrast("#777777", "#ffffff") - 4.48).abs() < 0.01);
}

#[test]
fn text_has_at_least_4_5_to_1_on_its_surfaces_in_both_themes() {
    let tokens = tokens();
    let mut pairs = Vec::new();
    for fg in ["text", "text-muted", "text-faint", "danger"] {
        for bg in ["shell", "canvas", "surface", "hover"] {
            pairs.push((fg, bg));
        }
    }
    // The active fill (a chosen tab, a pressed button) carries `text` only.
    pairs.push(("text", "active"));
    let mut failures = Vec::new();
    for (fg, bg) in pairs {
        let (fd, fl) = &tokens[fg];
        let (bd, bl) = &tokens[bg];
        for (theme, f, b) in [("dark", fd, bd), ("light", fl, bl)] {
            let ratio = contrast(f, b);
            if ratio < 4.5 {
                failures.push(format!("{theme}: {fg} on {bg} is {ratio:.2}:1"));
            }
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}

fn slint_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).expect("ui dir").flatten() {
        let path = entry.path();
        if path.is_dir() {
            slint_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "slint") {
            out.push(path);
        }
    }
}

#[test]
fn every_animation_takes_a_duration_that_reduce_motion_zeroes() {
    let mut files = Vec::new();
    slint_files(&ui_dir(), &mut files);
    let mut literal = Vec::new();
    for file in &files {
        let text = std::fs::read_to_string(file).expect("read");
        // An `animate` block's `duration` and `delay`, written on one line or several.
        let mut in_animate = false;
        for (n, line) in text.lines().enumerate() {
            let line = line.trim();
            if line.starts_with("animate ") {
                in_animate = true;
            }
            if in_animate {
                for key in ["duration:", "delay:"] {
                    if let Some(at) = line.find(key) {
                        let value = line[at + key.len()..].trim_start();
                        if !value.starts_with("Theme.") && !value.starts_with("root.order * Theme.")
                        {
                            literal.push(format!("{}:{}: {line}", file.display(), n + 1));
                        }
                    }
                }
            }
            if line.contains('}') {
                in_animate = false;
            }
        }
    }
    assert!(
        literal.is_empty(),
        "animations with their own times: {literal:#?}"
    );

    let theme = std::fs::read_to_string(ui_dir().join("theme.slint")).expect("theme.slint");
    let durations: Vec<&str> = theme
        .lines()
        .map(str::trim)
        .filter(|l| l.starts_with("out property <duration> "))
        .collect();
    assert!(durations.len() >= 5, "{durations:?}");
    for line in durations {
        assert!(
            line.contains(": reduce-motion ? 0ms : "),
            "not zeroed by reduce-motion: {line}"
        );
    }
}
