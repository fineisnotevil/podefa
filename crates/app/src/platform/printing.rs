// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 FINE Association <su@fa.org.tr>

//! How each system prints (ADR 0013). Windows takes pixels, through a device context
//! (`print_win`); CUPS (Linux, macOS) takes a PDF, through `lp`; Android takes a PDF too, and has
//! its own dialog, so the app shows none there.

use pdf_core::print::PrintTarget;
#[cfg(windows)]
use pdf_core::print::SharedSink;
use std::path::{Path, PathBuf};

/// A choice of printers, and the one to start from.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Printers {
    pub names: Vec<String>,
    pub default: usize,
}

impl Printers {
    fn new(names: Vec<String>, default: Option<&str>) -> Self {
        let default = default
            .and_then(|name| names.iter().position(|n| n == name))
            .unwrap_or(0);
        Self { names, default }
    }
}

/// The system shows its own print dialog, so the app asks nothing (Android).
pub const SYSTEM_DIALOG: bool = cfg!(target_os = "android");

/// The driver's settings dialog exists (Windows).
pub const PRINTER_PROPERTIES: bool = cfg!(windows);

/// Where a job goes: a printer's device context, or a PDF in the temporary folder for
/// [`send_file`]. `settings` are the driver's, from [`printer_properties`].
#[cfg(windows)]
pub fn target(printer: &str, title: &str, settings: Option<&[u8]>, _job: u64) -> PrintTarget {
    PrintTarget::Device(SharedSink::new(super::print_win::GdiSink::new(
        printer, title, settings,
    )))
}

#[cfg(not(windows))]
pub fn target(_printer: &str, _title: &str, _settings: Option<&[u8]>, job: u64) -> PrintTarget {
    PrintTarget::File(spool_path(job))
}

/// Where a job's PDF waits for the system to take it.
#[cfg(not(windows))]
fn spool_path(job: u64) -> PathBuf {
    let name = format!("PODEFA-print-{}-{job}.pdf", std::process::id());
    super::temp_dir().join(name)
}

#[cfg(windows)]
pub fn printers() -> Printers {
    Printers::new(
        super::print_win::printers(),
        super::print_win::default_printer().as_deref(),
    )
}

#[cfg(windows)]
pub fn printer_properties(printer: &str, settings: &mut Option<Vec<u8>>) -> bool {
    super::print_win::printer_properties(printer, settings)
}

#[cfg(not(windows))]
pub fn printer_properties(_printer: &str, _settings: &mut Option<Vec<u8>>) -> bool {
    false
}

/// The names `lpstat -e` lists, one to a line.
#[cfg_attr(any(windows, target_os = "android"), allow(dead_code))]
fn parse_lpstat_names(out: &str) -> Vec<String> {
    out.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect()
}

/// The name in `lpstat -d`'s "system default destination: name", in the C locale.
#[cfg_attr(any(windows, target_os = "android"), allow(dead_code))]
fn parse_lpstat_default(out: &str) -> Option<&str> {
    out.lines()
        .find_map(|line| line.strip_prefix("system default destination:"))
        .map(str::trim)
        .filter(|name| !name.is_empty())
}

/// The arguments of `lp`: `--` ends the options, so a file name cannot be taken for one.
#[cfg_attr(any(windows, target_os = "android"), allow(dead_code))]
fn lp_args(printer: &str, copies: u32, title: &str, file: &Path) -> Vec<std::ffi::OsString> {
    let mut args: Vec<std::ffi::OsString> = vec![
        "-d".into(),
        printer.into(),
        "-n".into(),
        copies.max(1).to_string().into(),
        "-t".into(),
        title.into(),
        "--".into(),
    ];
    args.push(file.into());
    args
}

#[cfg(all(unix, not(target_os = "android")))]
fn lpstat(flag: &str) -> String {
    std::process::Command::new("lpstat")
        .arg(flag)
        .env("LC_ALL", "C")
        .output()
        .map(|out| String::from_utf8_lossy(&out.stdout).into_owned())
        .unwrap_or_default()
}

#[cfg(all(unix, not(target_os = "android")))]
pub fn printers() -> Printers {
    Printers::new(
        parse_lpstat_names(&lpstat("-e")),
        parse_lpstat_default(&lpstat("-d")),
    )
}

#[cfg(target_os = "android")]
pub fn printers() -> Printers {
    Printers::default()
}

/// Hands a finished PDF to the system's queue and answers on the event loop; the file is gone or
/// the system's when `then` runs.
#[cfg(all(unix, not(target_os = "android")))]
pub fn send_file(
    printer: &str,
    copies: u32,
    title: &str,
    file: PathBuf,
    then: impl FnOnce(Result<(), String>) + Send + 'static,
) {
    let args = lp_args(printer, copies, title, &file);
    std::thread::spawn(move || {
        let sent = std::process::Command::new("lp")
            .args(args)
            .env("LC_ALL", "C")
            .output();
        let _ = std::fs::remove_file(&file);
        let result = match sent {
            Ok(out) if out.status.success() => Ok(()),
            Ok(out) => Err(String::from_utf8_lossy(&out.stderr).trim().to_owned()),
            Err(err) => Err(format!("could not run lp: {err}")),
        };
        let _ = slint::invoke_from_event_loop(move || then(result));
    });
}

#[cfg(target_os = "android")]
pub fn send_file(
    _printer: &str,
    _copies: u32,
    title: &str,
    file: PathBuf,
    then: impl FnOnce(Result<(), String>) + Send + 'static,
) {
    then(super::android::print_file(&file, title));
}

/// Windows prints to its device context and has no file to send.
#[cfg(windows)]
pub fn send_file(
    _printer: &str,
    _copies: u32,
    _title: &str,
    file: PathBuf,
    then: impl FnOnce(Result<(), String>) + Send + 'static,
) {
    let _ = std::fs::remove_file(file);
    then(Ok(()));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lpstat_is_read_as_it_prints() {
        assert_eq!(
            parse_lpstat_names("office\nPDF_printer\n\n"),
            ["office", "PDF_printer"]
        );
        assert_eq!(
            parse_lpstat_default("system default destination: office\n"),
            Some("office")
        );
        assert_eq!(
            parse_lpstat_default("no system default destination\n"),
            None
        );
    }

    #[test]
    fn the_default_printer_is_found_among_the_names() {
        let names = vec!["a".to_owned(), "b".to_owned()];
        assert_eq!(Printers::new(names.clone(), Some("b")).default, 1);
        assert_eq!(Printers::new(names, Some("gone")).default, 0);
    }

    #[test]
    fn a_file_name_cannot_be_an_option_of_lp() {
        let args = lp_args("office", 0, "Report", Path::new("-odd.pdf"));
        let args: Vec<_> = args.iter().map(|a| a.to_string_lossy()).collect();
        assert_eq!(
            args,
            ["-d", "office", "-n", "1", "-t", "Report", "--", "-odd.pdf"]
        );
    }
}
