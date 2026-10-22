// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 FINE Association <su@fa.org.tr>

//! Tools that write new files (Plan 0005, §4.2): merging, splitting and extracting pages. They run
//! on the engine's tool thread, on documents of their own, never on the open one (§3.4); this
//! module describes the jobs and parses what the user types for them.

use std::path::PathBuf;

/// One document a merge takes its pages from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MergeInput {
    /// The open document as it is now, unsaved changes included.
    Current,
    /// A PDF file.
    File(PathBuf),
}

/// A job for the tool thread.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolJob {
    /// Every page of every input, in order, into one new file.
    Merge {
        inputs: Vec<MergeInput>,
        output: PathBuf,
    },
    /// Pages of the open document, as it is now, into new files: each part's pages, in their order,
    /// into its own file.
    Extract { parts: Vec<(Vec<u32>, PathBuf)> },
}

impl ToolJob {
    /// What the status bar calls the job.
    pub fn title(&self) -> &'static str {
        match self {
            Self::Merge { .. } => "Merging",
            Self::Extract { .. } => "Extracting pages",
        }
    }

    /// Every file the job writes.
    pub fn outputs(&self) -> Vec<PathBuf> {
        match self {
            Self::Merge { output, .. } => vec![output.clone()],
            Self::Extract { parts } => parts.iter().map(|(_, path)| path.clone()).collect(),
        }
    }
}

/// Why a page range cannot be used.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RangeError {
    #[error("\"{0}\" is not a page or a range of pages")]
    Syntax(String),
    #[error("page {page} does not exist (the document has {count})")]
    OutOfRange { page: u32, count: u32 },
    #[error("no pages given")]
    Empty,
}

/// Parses page ranges as people write them, one-based: `1-3, 5, 8-` is pages 1 to 3, page 5, and
/// page 8 to the end; `-4` is pages 1 to 4. Each comma-separated part is one group of zero-based
/// page indices, in the order written (`5-3` counts down).
pub fn parse_ranges(text: &str, count: u32) -> Result<Vec<Vec<u32>>, RangeError> {
    let mut groups = Vec::new();
    for part in text.split([',', ';']) {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let page = |s: &str, default: u32| -> Result<u32, RangeError> {
            let s = s.trim();
            if s.is_empty() {
                return Ok(default);
            }
            let n: u32 = s.parse().map_err(|_| RangeError::Syntax(part.to_owned()))?;
            if n == 0 || n > count {
                return Err(RangeError::OutOfRange { page: n, count });
            }
            Ok(n)
        };
        let (from, to) = match part.split_once('-') {
            Some((a, b)) => (page(a, 1)?, page(b, count)?),
            None => {
                let n = page(part, 0)?;
                (n, n)
            }
        };
        let group: Vec<u32> = if from <= to {
            (from - 1..to).collect()
        } else {
            (to - 1..from).rev().collect()
        };
        groups.push(group);
    }
    if groups.is_empty() {
        return Err(RangeError::Empty);
    }
    Ok(groups)
}

/// Splits a document of `count` pages into parts of `every` pages (the last may be shorter).
pub fn split_every(every: u32, count: u32) -> Vec<Vec<u32>> {
    let every = every.max(1);
    (0..count)
        .step_by(every as usize)
        .map(|start| (start..(start + every).min(count)).collect())
        .collect()
}

/// A file name for a part of `stem`: its first and last page, one-based (`report_1-3.pdf`,
/// `report_5.pdf`).
pub fn part_name(stem: &str, pages: &[u32]) -> String {
    match (pages.first(), pages.last()) {
        (Some(first), Some(last)) if first != last => {
            format!("{stem}_{}-{}.pdf", first + 1, last + 1)
        }
        (Some(only), _) => format!("{stem}_{}.pdf", only + 1),
        _ => format!("{stem}.pdf"),
    }
}

/// The document information a reader sees and the app edits (Plan 0005 Phase 2): the `/Info`
/// entries people fill in. `None` leaves an entry as it is; an empty string removes it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Metadata {
    pub title: Option<String>,
    pub author: Option<String>,
    pub subject: Option<String>,
    pub keywords: Option<String>,
}

/// What the properties dialog shows besides the editable fields.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DocumentInfo {
    pub fields: Metadata,
    pub creator: String,
    pub producer: String,
    pub created: String,
    pub modified: String,
    pub format: String,
    /// Whether the document has an XMP metadata stream, which the app does not rewrite: readers
    /// that prefer it may show its older values.
    pub has_xmp: bool,
}

/// A PDF date (`D:YYYYMMDDHHmmSSZ`) for a moment given as seconds since 1970, in UTC.
pub fn pdf_date(unix_seconds: i64) -> String {
    let days = unix_seconds.div_euclid(86_400);
    let secs = unix_seconds.rem_euclid(86_400);
    // Days since 1970-01-01 to a civil date (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "D:{year:04}{month:02}{day:02}{:02}{:02}{:02}Z",
        secs / 3600,
        secs % 3600 / 60,
        secs % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pdf_dates_are_utc_civil_dates() {
        assert_eq!(pdf_date(0), "D:19700101000000Z");
        // 2026-10-22 18:30:05 UTC.
        assert_eq!(pdf_date(1_792_693_805), "D:20261022183005Z");
        // A leap day.
        assert_eq!(pdf_date(951_782_400), "D:20000229000000Z");
    }

    #[test]
    fn ranges_parse_as_people_write_them() {
        assert_eq!(
            parse_ranges("1-3, 5, 8-", 10).unwrap(),
            vec![vec![0, 1, 2], vec![4], vec![7, 8, 9]]
        );
        assert_eq!(parse_ranges("-2;4", 5).unwrap(), vec![vec![0, 1], vec![3]]);
        assert_eq!(parse_ranges(" 3-1 ", 5).unwrap(), vec![vec![2, 1, 0]]);
        assert_eq!(parse_ranges("", 5), Err(RangeError::Empty));
        assert_eq!(
            parse_ranges("2-9", 5),
            Err(RangeError::OutOfRange { page: 9, count: 5 })
        );
        assert_eq!(
            parse_ranges("0", 5),
            Err(RangeError::OutOfRange { page: 0, count: 5 })
        );
        assert!(matches!(parse_ranges("a-b", 5), Err(RangeError::Syntax(_))));
    }

    #[test]
    fn split_every_covers_every_page_once() {
        assert_eq!(
            split_every(3, 7),
            vec![vec![0, 1, 2], vec![3, 4, 5], vec![6]]
        );
        assert_eq!(split_every(0, 2), vec![vec![0], vec![1]]);
        assert!(split_every(4, 0).is_empty());
    }

    #[test]
    fn parts_are_named_by_their_pages() {
        assert_eq!(part_name("report", &[0, 1, 2]), "report_1-3.pdf");
        assert_eq!(part_name("report", &[4]), "report_5.pdf");
    }
}
