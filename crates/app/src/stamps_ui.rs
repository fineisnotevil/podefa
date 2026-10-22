// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 FINE Association <su@fa.org.tr>

//! Watermark, page numbers and crop (Plan 0005 Phase 3).
//!
//! The watermark and the page numbers come from one panel; the Crop tool draws a rectangle on a
//! page, shown with the ink stroke's path, and a small panel applies it to that page or to all
//! pages. Each is one structural edit (ADR 0009): one undo step.

use crate::{AppState, MainWindow, Tool};
use pdf_core::annot::{Colour, EditOp};
use pdf_core::pages::PageOp;
use pdf_core::stamp::{Corner, Stamp};
use pdf_core::tools::parse_ranges;
use slint::SharedString;
use std::fmt::Write as _;

/// The stamp panel's colours: grey, red, blue, black.
pub(crate) const STAMP_COLOURS: [Colour; 4] = [
    Colour::new(0.5, 0.5, 0.5),
    Colour::new(0.8, 0.1, 0.1),
    Colour::new(0.1, 0.25, 0.7),
    Colour::new(0.0, 0.0, 0.0),
];

/// A crop rectangle smaller than this, in points, on either side is taken for a click.
const CROP_MIN_PT: f32 = 4.0;

/// The rectangle the Crop tool is drawing, or has drawn and waits to apply, in the page's points.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct CropDraw {
    pub(crate) page: u32,
    pub(crate) from: (f32, f32),
    pub(crate) to: (f32, f32),
    /// The pointer came up: the crop panel is open.
    pub(crate) done: bool,
}

impl CropDraw {
    pub(crate) fn rect(&self) -> [f32; 4] {
        [
            self.from.0.min(self.to.0),
            self.from.1.min(self.to.1),
            self.from.0.max(self.to.0),
            self.from.1.max(self.to.1),
        ]
    }
}

impl AppState {
    /// Tools > Watermark (`mode` 0) or Page Numbers (1).
    pub(crate) fn stamp_open(&mut self, ui: &MainWindow, mode: i32) {
        if !self.editable || self.page_sizes.is_empty() {
            return;
        }
        ui.set_stamp_mode(mode);
        ui.set_stamp_error(SharedString::new());
        ui.set_stamp_visible(true);
    }

    /// The pages the stamp panel names: all of them when its field is empty.
    pub(crate) fn stamp_pages(&self, text: &str) -> Result<Vec<u32>, String> {
        let count = self.page_sizes.len() as u32;
        if text.trim().is_empty() {
            return Ok((0..count).collect());
        }
        let mut pages: Vec<u32> = parse_ranges(text, count)
            .map_err(|e| e.to_string())?
            .into_iter()
            .flatten()
            .collect();
        pages.sort_unstable();
        pages.dedup();
        Ok(pages)
    }

    /// The stamp the panel describes.
    fn stamp_from_panel(ui: &MainWindow) -> Result<Stamp, String> {
        let colour = STAMP_COLOURS
            .get(ui.get_stamp_colour().max(0) as usize)
            .copied()
            .unwrap_or(STAMP_COLOURS[0]);
        let stamp = if ui.get_stamp_mode() == 0 {
            Stamp::Watermark {
                text: ui.get_stamp_text().to_string(),
                colour,
                opacity: (ui.get_stamp_opacity() / 100.0).clamp(0.0, 1.0),
                diagonal: ui.get_stamp_diagonal(),
            }
        } else {
            let first = ui
                .get_stamp_first()
                .trim()
                .parse()
                .map_err(|_| "Start at which number? Type a whole number.".to_owned())?;
            let size = ui
                .get_stamp_size()
                .trim()
                .replace(',', ".")
                .parse()
                .map_err(|_| "Which font size? Type a number of points.".to_owned())?;
            Stamp::PageNumbers {
                format: ui.get_stamp_format().to_string(),
                first,
                corner: Corner::ALL
                    .get(ui.get_stamp_corner().max(0) as usize)
                    .copied()
                    .unwrap_or(Corner::BottomCentre),
                size,
                colour,
            }
        };
        stamp.check().map_err(|e| {
            let text = e.to_string();
            let mut chars = text.chars();
            chars.next().map_or(text.clone(), |c| {
                c.to_uppercase().collect::<String>() + chars.as_str()
            }) + "."
        })?;
        Ok(stamp)
    }

    /// Apply in the stamp panel: one undo step over the chosen pages.
    pub(crate) fn stamp_apply(&mut self, ui: &MainWindow) {
        let stamp = Self::stamp_from_panel(ui);
        let pages = self.stamp_pages(&ui.get_stamp_pages());
        match (stamp, pages) {
            (Ok(stamp), Ok(pages)) => {
                ui.set_stamp_visible(false);
                self.send_edit(EditOp::Pages(PageOp::Stamp { pages, stamp }));
            }
            (Err(message), _) | (_, Err(message)) => {
                ui.set_stamp_error(SharedString::from(message));
            }
        }
    }

    /// Tools > Crop Pages: the Crop tool, or back to Select.
    pub(crate) fn crop_tool(&mut self, ui: &MainWindow) {
        if !self.editable || self.page_sizes.is_empty() {
            return;
        }
        self.crop_cancel(ui);
        self.choose_tool(ui, Tool::Crop);
        if self.tool == Tool::Crop {
            ui.set_status_text(SharedString::from(
                "Crop: drag a rectangle over the part of the page to keep",
            ));
        }
    }

    /// Pointer input with the Crop tool: a drag draws the rectangle, kept on its page.
    pub(crate) fn crop_pointer(&mut self, ui: &MainWindow, kind: i32, x: f32, y: f32) {
        match kind {
            // Down: a new rectangle, from the page under the pointer.
            0 => {
                let Some(point) = self.page_point(ui, x, y) else {
                    return;
                };
                let Some(size) = self.page_sizes.get(point.page as usize) else {
                    return;
                };
                let at = (
                    point.x.clamp(0.0, size.width),
                    point.y.clamp(0.0, size.height),
                );
                ui.set_crop_visible(false);
                self.crop = Some(CropDraw {
                    page: point.page,
                    from: at,
                    to: at,
                    done: false,
                });
                self.rebuild_crop(ui);
            }
            // Moved.
            2 => {
                let Some(draw) = self.crop.filter(|d| !d.done) else {
                    return;
                };
                let (Some((px, py)), Some(size)) = (
                    self.page_local(draw.page, x, y),
                    self.page_sizes.get(draw.page as usize),
                ) else {
                    return;
                };
                if let Some(draw) = self.crop.as_mut() {
                    draw.to = (px.clamp(0.0, size.width), py.clamp(0.0, size.height));
                }
                self.rebuild_crop(ui);
            }
            // Up: a rectangle large enough waits in the crop panel; a click drops it.
            1 => {
                let Some(draw) = self.crop.filter(|d| !d.done) else {
                    return;
                };
                let [x0, y0, x1, y1] = draw.rect();
                if x1 - x0 < CROP_MIN_PT || y1 - y0 < CROP_MIN_PT {
                    self.crop_cancel(ui);
                    return;
                }
                if let Some(draw) = self.crop.as_mut() {
                    draw.done = true;
                }
                ui.set_crop_size(SharedString::from(format!(
                    "{:.0} x {:.0} pt",
                    x1 - x0,
                    y1 - y0
                )));
                ui.set_crop_visible(true);
            }
            // Taken away.
            4 => self.crop_cancel(ui),
            _ => {}
        }
    }

    /// The crop rectangle's outline, in viewport logical pixels, drawn with the ink stroke's path:
    /// rebuilt on each move and scroll.
    pub(crate) fn rebuild_crop(&mut self, ui: &MainWindow) {
        let Some(draw) = self.crop else {
            return;
        };
        let Some(slot) = self.layout.slot(draw.page as usize) else {
            return;
        };
        let s = self.scale_milli as f64 / 1000.0;
        let sf = self.sf();
        let (ox, oy) = (
            (slot.x as f64 - self.pos.0) / sf,
            (slot.y as f64 - self.pos.1) / sf,
        );
        let k = s / sf;
        let [x0, y0, x1, y1] = draw.rect().map(f64::from);
        let (l, t, r, b) = (ox + x0 * k, oy + y0 * k, ox + x1 * k, oy + y1 * k);
        self.ink_commands.clear();
        let _ = write!(
            self.ink_commands,
            "M{l:.1} {t:.1} L{r:.1} {t:.1} L{r:.1} {b:.1} L{l:.1} {b:.1} Z"
        );
        ui.set_ink_colour(ui.get_crop_colour());
        ui.set_ink_stroke_width(1.5);
        ui.set_ink_commands(SharedString::from(self.ink_commands.as_str()));
    }

    /// The crop panel's "This page" (`all` false) or "All pages".
    pub(crate) fn crop_apply(&mut self, ui: &MainWindow, all: bool) {
        let Some(draw) = self.crop.filter(|d| d.done) else {
            return;
        };
        let pages = if all {
            (0..self.page_sizes.len() as u32).collect()
        } else {
            vec![draw.page]
        };
        let rect = draw.rect();
        self.crop_cancel(ui);
        if self.tool == Tool::Crop {
            self.choose_tool(ui, Tool::Crop);
        }
        self.send_edit(EditOp::Pages(PageOp::Crop { pages, rect }));
    }

    /// A page point as a viewport point (logical pixels), for the bench's pointer.
    pub(crate) fn viewport_point(&self, page: u32, px: f32, py: f32) -> Option<(f32, f32)> {
        let slot = self.layout.slot(page as usize)?;
        let s = self.scale_milli as f64 / 1000.0;
        let sf = self.sf();
        Some((
            ((slot.x as f64 + px as f64 * s - self.pos.0) / sf) as f32,
            ((slot.y as f64 + py as f64 * s - self.pos.1) / sf) as f32,
        ))
    }

    /// Drops the crop rectangle and closes its panel.
    pub(crate) fn crop_cancel(&mut self, ui: &MainWindow) {
        if self.crop.take().is_some() {
            self.ink_commands.clear();
            ui.set_ink_commands(SharedString::new());
        }
        ui.set_crop_visible(false);
    }
}
