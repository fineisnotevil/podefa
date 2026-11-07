// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 FINE Association <su@fa.org.tr>

//! Watermarks and page numbers (Plan 0005 Phase 3): what text goes on which page, and where, as
//! pure logic. The engine measures the text in its font and writes it into the page's content; the
//! placement here works in the page's visible space, in points, with the origin at the top left and
//! y growing downwards, whatever the page's rotation or crop.

use crate::annot::Colour;

/// Where page numbers go.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Corner {
    TopLeft,
    TopCentre,
    TopRight,
    BottomLeft,
    BottomCentre,
    BottomRight,
}

impl Corner {
    /// Every position, in the order the page-number panel offers them.
    pub const ALL: [Self; 6] = [
        Self::TopLeft,
        Self::TopCentre,
        Self::TopRight,
        Self::BottomLeft,
        Self::BottomCentre,
        Self::BottomRight,
    ];
}

/// Text added to pages as page content (not as annotations), so every reader prints it.
#[derive(Debug, Clone, PartialEq)]
pub enum Stamp {
    /// The same text across the middle of every page, sized to the page.
    Watermark {
        text: String,
        colour: Colour,
        /// 0 (invisible) to 1 (opaque).
        opacity: f32,
        /// Along the page's diagonal, bottom left to top right; otherwise horizontal.
        diagonal: bool,
    },
    /// A number on every page: `format` with `{n}` for the number and `{total}` for the last one.
    PageNumbers {
        format: String,
        /// The number the first stamped page gets.
        first: u32,
        corner: Corner,
        /// The font size, in points.
        size: f32,
        colour: Colour,
    },
}

/// Distance of page numbers from the page's edges, in points.
pub const NUMBER_MARGIN_PT: f32 = 24.0;
/// The share of the page's width (or diagonal) a watermark spans.
pub const WATERMARK_SPAN: f32 = 0.7;
/// The largest watermark, as a share of the page's shorter side: a short text on a large page does
/// not grow beyond this.
pub const WATERMARK_MAX_SIZE: f32 = 0.25;
/// Height of capital letters as a share of the font size, for centring (Helvetica's 718/1000).
pub const CAP_HEIGHT: f32 = 0.718;

/// Why a stamp cannot be applied.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum StampError {
    #[error("the text is empty")]
    Empty,
    #[error("the opacity {0} is not between 0 and 1")]
    Opacity(f32),
    #[error("the font size {0} pt is not usable")]
    Size(f32),
    #[error("the format has no {{n}}")]
    NoNumber,
}

impl Stamp {
    /// Checks the stamp's values.
    pub fn check(&self) -> Result<(), StampError> {
        match self {
            Self::Watermark { text, opacity, .. } => {
                if text.trim().is_empty() {
                    return Err(StampError::Empty);
                }
                if !(opacity.is_finite() && *opacity > 0.0 && *opacity <= 1.0) {
                    return Err(StampError::Opacity(*opacity));
                }
            }
            Self::PageNumbers { format, size, .. } => {
                if !format.contains("{n}") {
                    return Err(StampError::NoNumber);
                }
                if !(size.is_finite() && (1.0..=500.0).contains(size)) {
                    return Err(StampError::Size(*size));
                }
            }
        }
        Ok(())
    }

    /// The text for the `index`th of `count` stamped pages.
    pub fn text(&self, index: u32, count: u32) -> String {
        match self {
            Self::Watermark { text, .. } => text.trim().to_owned(),
            Self::PageNumbers { format, first, .. } => {
                let n = first.saturating_add(index);
                let total = first.saturating_add(count.saturating_sub(1));
                format
                    .replace("{n}", &n.to_string())
                    .replace("{total}", &total.to_string())
            }
        }
    }

    pub fn colour(&self) -> Colour {
        match self {
            Self::Watermark { colour, .. } | Self::PageNumbers { colour, .. } => *colour,
        }
    }

    /// The opacity the text is drawn with.
    pub fn opacity(&self) -> f32 {
        match self {
            Self::Watermark { opacity, .. } => *opacity,
            Self::PageNumbers { .. } => 1.0,
        }
    }

    /// Where the text goes on a page of `width` x `height` points, for a text `advance` em wide
    /// (its width at a font size of 1).
    pub fn placement(&self, width: f32, height: f32, advance: f32) -> Placement {
        match self {
            Self::Watermark { diagonal, .. } => {
                let angle = if *diagonal { height.atan2(width) } else { 0.0 };
                let span = if *diagonal {
                    width.hypot(height)
                } else {
                    width
                };
                let size = (WATERMARK_SPAN * span / advance.max(f32::EPSILON))
                    .min(WATERMARK_MAX_SIZE * width.min(height));
                // The baseline starts half the text's width before the centre and half the capital
                // height below it, both along the turned text.
                let (sin, cos) = angle.sin_cos();
                let (dx, dy) = (-advance * size / 2.0, CAP_HEIGHT * size / 2.0);
                Placement::turned(
                    size,
                    angle,
                    width / 2.0 + dx * cos + dy * sin,
                    height / 2.0 - dx * sin + dy * cos,
                )
            }
            Self::PageNumbers { corner, size, .. } => {
                let text_width = advance * size;
                let m = NUMBER_MARGIN_PT;
                let x = match corner {
                    Corner::TopLeft | Corner::BottomLeft => m,
                    Corner::TopCentre | Corner::BottomCentre => (width - text_width) / 2.0,
                    Corner::TopRight | Corner::BottomRight => width - m - text_width,
                };
                let y = match corner {
                    Corner::TopLeft | Corner::TopCentre | Corner::TopRight => m + CAP_HEIGHT * size,
                    _ => height - m,
                };
                Placement::turned(*size, 0.0, x, y)
            }
        }
    }
}

/// The text matrix of a stamp in the page's visible space: from glyph space (y up, font size 1) to
/// points with y down. `[a, b, c, d, e, f]` as PDF writes matrices; `(e, f)` is where the baseline
/// starts.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Placement {
    pub size: f32,
    pub matrix: [f32; 6],
}

impl Placement {
    /// Text of `size` points turned `angle` radians anticlockwise as seen on the page, its baseline
    /// starting at `(x, y)`.
    fn turned(size: f32, angle: f32, x: f32, y: f32) -> Self {
        let (sin, cos) = angle.sin_cos();
        Self {
            size,
            matrix: [size * cos, -size * sin, -size * sin, -size * cos, x, y],
        }
    }

    /// A point of the text, in glyph space at font size 1, on the page.
    pub fn apply(&self, x: f32, y: f32) -> (f32, f32) {
        let [a, b, c, d, e, f] = self.matrix;
        (x * a + y * c + e, x * b + y * d + f)
    }
}

/// A crop rectangle in a page's visible space, points from the top left: `[x0, y0, x1, y1]`,
/// clipped to the page. `None` when less than a point of it is on the page.
pub fn clip_crop(rect: [f32; 4], width: f32, height: f32) -> Option<[f32; 4]> {
    let x0 = rect[0].min(rect[2]).max(0.0);
    let y0 = rect[1].min(rect[3]).max(0.0);
    let x1 = rect[0].max(rect[2]).min(width);
    let y1 = rect[1].max(rect[3]).min(height);
    (x1 - x0 >= 1.0 && y1 - y0 >= 1.0).then_some([x0, y0, x1, y1])
}

#[cfg(test)]
mod tests {
    use super::*;

    const GREY: Colour = Colour::new(0.5, 0.5, 0.5);

    fn numbers(format: &str, corner: Corner) -> Stamp {
        Stamp::PageNumbers {
            format: format.to_owned(),
            first: 1,
            corner,
            size: 10.0,
            colour: GREY,
        }
    }

    fn watermark(diagonal: bool) -> Stamp {
        Stamp::Watermark {
            text: " TASLAK ".to_owned(),
            colour: GREY,
            opacity: 0.3,
            diagonal,
        }
    }

    #[test]
    fn page_numbers_count_from_the_first_number() {
        let stamp = numbers("Sayfa {n} / {total}", Corner::BottomCentre);
        assert_eq!(stamp.text(0, 5), "Sayfa 1 / 5");
        assert_eq!(stamp.text(4, 5), "Sayfa 5 / 5");
        let from_ten = Stamp::PageNumbers {
            format: "{n}".to_owned(),
            first: 10,
            corner: Corner::TopRight,
            size: 10.0,
            colour: GREY,
        };
        assert_eq!(from_ten.text(2, 3), "12");
        assert_eq!(watermark(true).text(3, 9), "TASLAK");
    }

    #[test]
    fn stamps_are_checked() {
        assert_eq!(
            numbers("page", Corner::TopLeft).check(),
            Err(StampError::NoNumber)
        );
        assert!(numbers("{n}", Corner::TopLeft).check().is_ok());
        let empty = Stamp::Watermark {
            text: "  ".to_owned(),
            colour: GREY,
            opacity: 0.3,
            diagonal: false,
        };
        assert_eq!(empty.check(), Err(StampError::Empty));
        let clear = Stamp::Watermark {
            text: "x".to_owned(),
            colour: GREY,
            opacity: 0.0,
            diagonal: false,
        };
        assert_eq!(clear.check(), Err(StampError::Opacity(0.0)));
    }

    #[test]
    fn page_numbers_sit_in_their_corner() {
        // A text 2 em wide at 10 pt is 20 pt wide.
        let at = |corner| numbers("{n}", corner).placement(600.0, 800.0, 2.0).matrix;
        let [a, b, c, d, x, y] = at(Corner::BottomCentre);
        assert_eq!([a, b, c, d], [10.0, 0.0, 0.0, -10.0]);
        assert_eq!((x, y), (290.0, 776.0));
        let [.., x, y] = at(Corner::TopLeft);
        assert_eq!(x, 24.0);
        assert!((y - (24.0 + 7.18)).abs() < 1e-4);
        let [.., x, _] = at(Corner::BottomRight);
        assert_eq!(x, 556.0);
    }

    #[test]
    fn a_watermark_is_centred_and_fits_the_page() {
        for diagonal in [false, true] {
            let stamp = watermark(diagonal);
            let p = stamp.placement(600.0, 800.0, 3.0);
            // The middle of the text's capital height at its half width is the page's centre.
            let (cx, cy) = p.apply(1.5, CAP_HEIGHT / 2.0);
            assert!(
                (cx - 300.0).abs() < 1e-3 && (cy - 400.0).abs() < 1e-3,
                "{cx} {cy}"
            );
            // Both ends of the baseline stay on the page.
            for (x, y) in [p.apply(0.0, 0.0), p.apply(3.0, 0.0)] {
                assert!((0.0..=600.0).contains(&x) && (0.0..=800.0).contains(&y));
            }
        }
        // Diagonal: the text rises from left to right, along the page's diagonal.
        let p = watermark(true).placement(600.0, 800.0, 3.0);
        let (x0, y0) = p.apply(0.0, 0.0);
        let (x1, y1) = p.apply(3.0, 0.0);
        assert!(x1 > x0 && y1 < y0);
        assert!(((y0 - y1) / (x1 - x0) - 800.0 / 600.0).abs() < 1e-3);
        // A short text does not grow beyond a quarter of the shorter side.
        let short = watermark(false).placement(600.0, 800.0, 0.5);
        assert_eq!(short.size, 150.0);
    }

    #[test]
    fn a_crop_is_clipped_to_the_page() {
        assert_eq!(
            clip_crop([500.0, 700.0, 50.0, -20.0], 600.0, 800.0),
            Some([50.0, 0.0, 500.0, 700.0])
        );
        assert_eq!(clip_crop([700.0, 0.0, 800.0, 100.0], 600.0, 800.0), None);
        assert_eq!(clip_crop([10.0, 10.0, 10.5, 100.0], 600.0, 800.0), None);
    }
}
