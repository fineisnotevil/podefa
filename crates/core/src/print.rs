// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 FINE Association <su@fa.org.tr>

//! Printing (ADR 0013): where a page goes on the sheet, and how it reaches a printer that takes
//! pixels. The engine draws; a [`PrintSink`] takes the bands; this module does the arithmetic, so
//! it holds no renderer and no system call.

use std::fmt;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// The resolution a document that withholds high-quality printing is printed at: the "low
/// resolution" of ISO 32000-1 table 22 is not a number, and this is the one Acrobat uses.
pub const LOW_RES_DPI: u32 = 150;

/// The finest a page is drawn at; a finer printer gets it stretched. Each doubling costs four
/// times the pixels, for detail paper hardly shows.
pub const MAX_DPI: u32 = 600;

/// How many bytes of pixels one band holds at most: a page is drawn in bands, so a poster on a
/// plotter costs no more memory than a letter.
pub const BAND_BYTES: usize = 32 * 1024 * 1024;

/// A printer's sheet in its own pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sheet {
    pub dpi: (u32, u32),
    /// The whole paper.
    pub paper: (u32, u32),
    /// The part it can print on, and where that starts on the paper. A band's `dest` counts from
    /// this corner, the way a Windows printer's device context does.
    pub printable: (u32, u32),
    pub printable_at: (u32, u32),
}

/// Where one page goes: drawn upright or turned, in device pixels of the printable area, from a
/// raster of `render` pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Placement {
    /// Turned a quarter clockwise, since the page and the sheet lie differently.
    pub rotated: bool,
    /// `(x, y, width, height)` in the printable area's device pixels.
    pub dest: (i32, i32, u32, u32),
    /// The raster's size: `dest`'s, or less for a resolution limit.
    pub render: (u32, u32),
}

/// Places a page of `page` points on `sheet`, drawn at no more than `limit_dpi`: at its own size
/// when it fits, shrunk to fit when it does not, never enlarged; turned when it lies the other way
/// than the printable area; centred on the paper. `None` for a page or sheet with no size.
pub fn place(page: (f32, f32), sheet: &Sheet, limit_dpi: u32) -> Option<Placement> {
    let (dpi_x, dpi_y) = (sheet.dpi.0.max(1) as f64, sheet.dpi.1.max(1) as f64);
    let (printable_w, printable_h) = (sheet.printable.0 as f64, sheet.printable.1 as f64);
    let (page_w, page_h) = (page.0 as f64 / 72.0, page.1 as f64 / 72.0);
    let usable = page_w > 0.0
        && page_h > 0.0
        && printable_w > 0.0
        && printable_h > 0.0
        && page_w.is_finite()
        && page_h.is_finite();
    if !usable {
        return None;
    }
    let (sheet_w, sheet_h) = (printable_w / dpi_x, printable_h / dpi_y);
    let rotated = page_w != page_h && (page_w > page_h) != (sheet_w > sheet_h);
    let (w, h) = if rotated {
        (page_h, page_w)
    } else {
        (page_w, page_h)
    };
    let fit = (sheet_w / w).min(sheet_h / h).min(1.0);
    let dest_w = (w * fit * dpi_x).round().clamp(1.0, printable_w);
    let dest_h = (h * fit * dpi_y).round().clamp(1.0, printable_h);
    // Centred on the paper, which may print more on one side than on the other, then kept inside
    // what it can print.
    let centred = |paper: u32, size: f64, at: u32, printable: f64| {
        ((paper as f64 - size) / 2.0 - at as f64)
            .round()
            .clamp(0.0, (printable - size).max(0.0)) as i32
    };
    let x = centred(sheet.paper.0, dest_w, sheet.printable_at.0, printable_w);
    let y = centred(sheet.paper.1, dest_h, sheet.printable_at.1, printable_h);
    let limit = limit_dpi.max(1) as f64;
    let render = |size: f64, dpi: f64| (size * (limit / dpi).min(1.0)).ceil().max(1.0) as u32;
    Some(Placement {
        rotated,
        dest: (x, y, dest_w as u32, dest_h as u32),
        render: (render(dest_w, dpi_x), render(dest_h, dpi_y)),
    })
}

impl Placement {
    /// The matrix `[a, b, c, d, e, f]` that takes a page with `bounds` `(x0, y0, x1, y1)` to the
    /// raster's pixels (`x' = a x + c y + e`, `y' = b x + d y + f`).
    pub fn matrix(&self, bounds: (f32, f32, f32, f32)) -> [f32; 6] {
        let (x0, y0, x1, y1) = bounds;
        let (w, h) = (
            (x1 - x0).max(f32::MIN_POSITIVE),
            (y1 - y0).max(f32::MIN_POSITIVE),
        );
        let (render_w, render_h) = (self.render.0 as f32, self.render.1 as f32);
        if self.rotated {
            let (sx, sy) = (render_w / h, render_h / w);
            [0.0, sy, -sx, 0.0, sx * y1, -sy * x0]
        } else {
            let (sx, sy) = (render_w / w, render_h / h);
            [sx, 0.0, 0.0, sy, -sx * x0, -sy * y0]
        }
    }

    /// The bands that draw this page: top to bottom, each at most `budget` bytes of RGB8 (at least
    /// one row), their destinations sharing edges, so no row of the sheet is left out or drawn
    /// twice however the raster and the sheet differ.
    pub fn bands(&self, budget: usize) -> Vec<BandSpan> {
        let (width, height) = self.render;
        let rows = (budget / (width.max(1) as usize * 3)).clamp(1, u32::MAX as usize) as u32;
        let (dest_y, dest_h) = (self.dest.1 as i64, self.dest.3 as u64);
        let edge = |y: u32| {
            dest_y + ((y as u64 * dest_h + height as u64 / 2) / height.max(1) as u64) as i64
        };
        let mut out = Vec::new();
        let mut y = 0;
        while y < height {
            let h = rows.min(height - y);
            let (top, bottom) = (edge(y), edge(y + h));
            out.push(BandSpan {
                y,
                height: h,
                dest_y: top as i32,
                dest_height: (bottom - top) as u32,
            });
            y += h;
        }
        out
    }
}

/// One band of a page's raster and the rows of the sheet it goes to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BandSpan {
    pub y: u32,
    pub height: u32,
    pub dest_y: i32,
    pub dest_height: u32,
}

/// One band's pixels on their way to the printer: `width` x `height` tightly packed RGB8.
#[derive(Debug, Clone, Copy)]
pub struct PrintBand<'a> {
    pub rgb: &'a [u8],
    pub width: u32,
    pub height: u32,
    /// `(x, y, width, height)` in the printable area's device pixels.
    pub dest: (i32, i32, u32, u32),
}

/// A printer that takes pixels (the Windows device context): the tool thread calls it, in order,
/// and always ends with `finish` or `abort` once `start` has succeeded.
pub trait PrintSink: Send + fmt::Debug {
    fn start(&mut self) -> Result<Sheet, String>;
    fn begin_page(&mut self) -> Result<(), String>;
    fn band(&mut self, band: PrintBand<'_>) -> Result<(), String>;
    fn end_page(&mut self) -> Result<(), String>;
    fn finish(&mut self) -> Result<(), String>;
    /// Stops the job: the printer drops what it was given.
    fn abort(&mut self);
}

/// A sink in a job. Jobs are compared and cloned, so this compares by identity.
#[derive(Clone, Debug)]
pub struct SharedSink(pub Arc<Mutex<Box<dyn PrintSink>>>);

impl SharedSink {
    pub fn new(sink: impl PrintSink + 'static) -> Self {
        Self(Arc::new(Mutex::new(Box::new(sink))))
    }
}

impl PartialEq for SharedSink {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl Eq for SharedSink {}

/// Where a print job's pages go.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PrintTarget {
    /// A printer that takes pixels.
    Device(SharedSink),
    /// A PDF for a system that prints PDFs (CUPS, Android): the pages themselves, or their images
    /// when the document withholds high-quality printing.
    File(PathBuf),
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A4 at 600 dpi with a margin of 100 pixels all round.
    fn a4() -> Sheet {
        Sheet {
            dpi: (600, 600),
            paper: (4960, 7016),
            printable: (4760, 6816),
            printable_at: (100, 100),
        }
    }

    #[test]
    fn a_page_that_fits_keeps_its_size_and_is_centred() {
        // A5: 420 x 595 pt is 3500 x 4958 px.
        let at = place((420.0, 595.0), &a4(), MAX_DPI).unwrap();
        assert!(!at.rotated);
        assert_eq!(at.dest, (630, 929, 3500, 4958));
        assert_eq!(at.render, (3500, 4958));
    }

    #[test]
    fn a_page_too_large_shrinks_and_keeps_its_shape() {
        // A3 on A4's printable area: the width is what limits it.
        let at = place((842.0, 1191.0), &a4(), MAX_DPI).unwrap();
        assert!(!at.rotated);
        assert_eq!(at.dest.2, 4760);
        let shape = at.dest.2 as f64 / at.dest.3 as f64;
        assert!((shape - 842.0 / 1191.0).abs() < 0.001, "{shape}");
    }

    #[test]
    fn a_page_lying_the_other_way_is_turned() {
        let at = place((842.0, 595.0), &a4(), MAX_DPI).unwrap();
        assert!(at.rotated);
        assert!(at.dest.3 > at.dest.2);
    }

    #[test]
    fn a_square_page_is_not_turned() {
        assert!(!place((300.0, 300.0), &a4(), MAX_DPI).unwrap().rotated);
    }

    #[test]
    fn the_limit_lowers_the_raster_not_the_place() {
        let fine = place((420.0, 595.0), &a4(), MAX_DPI).unwrap();
        let low = place((420.0, 595.0), &a4(), LOW_RES_DPI).unwrap();
        assert_eq!(low.dest, fine.dest);
        assert_eq!(low.render, (875, 1240));
    }

    #[test]
    fn a_printer_coarser_than_the_limit_sets_the_raster() {
        let sheet = Sheet {
            dpi: (100, 100),
            paper: (827, 1169),
            printable: (827, 1169),
            printable_at: (0, 0),
        };
        let at = place((595.0, 842.0), &sheet, LOW_RES_DPI).unwrap();
        assert_eq!(at.render, (at.dest.2, at.dest.3));
    }

    #[test]
    fn nothing_to_place() {
        assert_eq!(place((0.0, 100.0), &a4(), MAX_DPI), None);
        let none = Sheet {
            printable: (0, 0),
            ..a4()
        };
        assert_eq!(place((100.0, 100.0), &none, MAX_DPI), None);
    }

    #[test]
    fn a_lopsided_margin_stays_inside_the_printable_area() {
        let sheet = Sheet {
            printable_at: (800, 0),
            printable: (4160, 7016),
            ..a4()
        };
        let at = place((595.0, 842.0), &sheet, MAX_DPI).unwrap();
        assert_eq!(at.dest.0, 0);
        assert!(at.dest.2 <= 4160);
    }

    #[test]
    fn the_matrix_takes_the_page_to_the_raster() {
        let apply =
            |m: [f32; 6], x: f32, y: f32| (m[0] * x + m[2] * y + m[4], m[1] * x + m[3] * y + m[5]);
        let upright = Placement {
            rotated: false,
            dest: (0, 0, 100, 200),
            render: (100, 200),
        };
        let m = upright.matrix((10.0, 20.0, 60.0, 120.0));
        assert_eq!(apply(m, 10.0, 20.0), (0.0, 0.0));
        assert_eq!(apply(m, 60.0, 120.0), (100.0, 200.0));
        // Turned: the page's top left goes to the raster's top right, its bottom left to the top
        // left.
        let turned = Placement {
            rotated: true,
            dest: (0, 0, 200, 100),
            render: (200, 100),
        };
        let m = turned.matrix((10.0, 20.0, 60.0, 120.0));
        assert_eq!(apply(m, 10.0, 20.0), (200.0, 0.0));
        assert_eq!(apply(m, 10.0, 120.0), (0.0, 0.0));
        assert_eq!(apply(m, 60.0, 120.0), (0.0, 100.0));
    }

    #[test]
    fn bands_cover_the_page_and_the_sheet_without_a_gap() {
        for (render_h, dest_h, budget) in [
            (4958, 4958, BAND_BYTES),
            (1240, 4958, 300_000),
            (7, 100, 1),
            (100, 7, 3 * 875),
            (1, 1, BAND_BYTES),
        ] {
            let at = Placement {
                rotated: false,
                dest: (0, 11, 3500, dest_h),
                render: (875, render_h),
            };
            let bands = at.bands(budget);
            let (mut y, mut dest) = (0, 11);
            for band in &bands {
                assert_eq!(band.y, y);
                assert_eq!(band.dest_y, dest);
                assert!(band.height as usize * 875 * 3 <= budget.max(875 * 3));
                y += band.height;
                dest += band.dest_height as i32;
            }
            assert_eq!(y, render_h);
            assert_eq!(dest, 11 + dest_h as i32);
        }
    }

    #[derive(Debug)]
    struct Nothing;

    impl PrintSink for Nothing {
        fn start(&mut self) -> Result<Sheet, String> {
            Err(String::new())
        }
        fn begin_page(&mut self) -> Result<(), String> {
            Ok(())
        }
        fn band(&mut self, _: PrintBand<'_>) -> Result<(), String> {
            Ok(())
        }
        fn end_page(&mut self) -> Result<(), String> {
            Ok(())
        }
        fn finish(&mut self) -> Result<(), String> {
            Ok(())
        }
        fn abort(&mut self) {}
    }

    #[test]
    fn sinks_compare_by_identity() {
        let one = SharedSink::new(Nothing);
        assert_eq!(one, one.clone());
        assert_ne!(one, SharedSink::new(Nothing));
    }
}
