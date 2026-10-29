// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 FINE Association <su@fa.org.tr>

//! Book mode and the phone's hidden islands (Plan 0008 Phase 5 review).
//!
//! Book mode shows one page at a time and turns pages sideways. The layout stays the continuous
//! column of Plan 0002, with room added above the first page and below the last
//! ([`BOOK_PAD_LOGICAL_PX`]) so that any page can sit in the middle of the view; the scroll
//! position is held to the shown page ([`AppState::book_range`]) and only that page is drawn.
//! A finger moves the page itself: up and down within it, and sideways past its edges, where it
//! turns once let go beyond [`TURN_SHARE`] of the view's width. The mouse wheel and the keys
//! scroll within the page and turn at its ends.
//!
//! In Read on a phone the islands fade out after [`CHROME_HIDE_MS`] without a touch and come
//! back on a tap, so only the page is left to read.

use crate::{AppState, MainWindow, TOUCH_SLOP_LOGICAL_PX, with_state};
use pdf_core::nav::{NavAction, page_top};
use slint::ComponentHandle as _;
use std::time::{Duration, Instant};

/// Room above the first page and below the last in book mode, in logical pixels: enough to put
/// any page in the middle of a view.
pub(crate) const BOOK_PAD_LOGICAL_PX: f64 = 4096.0;
/// The share of the view's width a page must be pulled sideways to turn.
const TURN_SHARE: f32 = 0.18;
/// How long a turned page takes to slide in, or a pulled one to slide back.
const SLIDE_MS: f32 = 220.0;
/// The shortest time between two turns by the mouse wheel: one flick of the wheel turns once.
const WHEEL_TURN_GAP_MS: u128 = 350;
/// How long Read waits without a touch before it hides the islands.
pub(crate) const CHROME_HIDE_MS: u64 = 4000;

/// A finger on the page in book mode: where it came down, the scroll position then, and whether
/// it has moved far enough to move the page (rather than to tap or press).
#[derive(Debug, Clone, Copy)]
pub(crate) struct BookDrag {
    start: (f32, f32),
    pos: (f64, f64),
    moving: bool,
}

impl AppState {
    /// The room added above the first page and below the last: none outside book mode.
    pub(crate) fn book_pad(&self) -> u32 {
        if self.book {
            (BOOK_PAD_LOGICAL_PX * self.sf()).round() as u32
        } else {
            0
        }
    }

    /// The scroll positions (lowest, highest) that keep the shown page in view: one position, the
    /// page in the middle of the room the islands leave, when it fits there.
    pub(crate) fn book_range(&self, ui: &MainWindow) -> Option<(f64, f64)> {
        let slot = self.layout.slot(self.book_page)?;
        let (_, free_h) = self.free_view(ui);
        let (_, view_h) = self.view_size(ui);
        let top = self.margins.top as f64;
        let gap = self.gap_px() as f64;
        let h = slot.h as f64;
        if h + 2.0 * gap <= free_h {
            let y = slot.y as f64 - top - (free_h - h) / 2.0;
            return Some((y, y));
        }
        Some((
            slot.y as f64 - gap - top,
            slot.bottom() as f64 + gap + self.margins.bottom as f64 - view_h,
        ))
    }

    /// Book mode on or off, on the current page. Turning it on fits that page in the view.
    pub(crate) fn set_book(&mut self, ui: &MainWindow, on: bool) {
        ui.set_book_mode(on);
        if on == self.book {
            return;
        }
        let page = self.current_page(ui);
        self.book = on;
        self.book_page = page;
        self.book_drag = None;
        self.book_slide = None;
        self.book_timer.stop();
        ui.set_book_shift(0.0);
        if self.layout.is_empty() {
            return;
        }
        self.layout = self.layout_for(ui, self.scale_milli);
        if on {
            self.fit(ui, true);
            return;
        }
        self.begin_epoch();
        self.pos.1 = page_top(&self.layout, page).unwrap_or(0.0);
        self.clamp_pos(ui);
        self.reanchor(ui);
        self.refresh(ui);
    }

    /// Turns to the next page (or the one before); false at the document's end (or start).
    pub(crate) fn turn(&mut self, ui: &MainWindow, forward: bool) -> bool {
        let next = if forward {
            self.book_page + 1
        } else {
            match self.book_page.checked_sub(1) {
                Some(page) => page,
                None => return false,
            }
        };
        if next >= self.layout.len() {
            return false;
        }
        self.go_to_page(ui, next);
        // Going back shows the end of the page before, as a reader leafing back would.
        if !forward && let Some((_, last)) = self.book_range(ui) {
            self.scroll_to(ui, (self.pos.0, last));
        }
        true
    }

    /// Slides the page from `from` logical pixels sideways back to its place.
    fn slide(&mut self, ui: &MainWindow, from: f32) {
        // With reduced motion the page is in its place at once.
        let from = if self.settings.reduce_motion {
            0.0
        } else {
            from
        };
        ui.set_book_shift(from);
        if from == 0.0 {
            return;
        }
        self.book_slide = Some((from, Instant::now()));
        let weak = ui.as_weak();
        self.book_timer.start(
            slint::TimerMode::Repeated,
            Duration::from_millis(16),
            move || {
                if let Some(ui) = weak.upgrade() {
                    with_state(|state| state.slide_step(&ui));
                }
            },
        );
    }

    fn slide_step(&mut self, ui: &MainWindow) {
        let Some((from, at)) = self.book_slide else {
            self.book_timer.stop();
            return;
        };
        let t = (at.elapsed().as_secs_f32() * 1000.0 / SLIDE_MS).min(1.0);
        let eased = 1.0 - (1.0 - t).powi(3);
        ui.set_book_shift(from * (1.0 - eased));
        if t >= 1.0 {
            self.book_slide = None;
            self.book_timer.stop();
        }
    }

    /// A finger on the page in book mode (`kind` as `select_pointer`'s; viewport logical
    /// pixels). True when it moved the page, so the event selects nothing.
    pub(crate) fn book_pointer(&mut self, ui: &MainWindow, kind: i32, x: f32, y: f32) -> bool {
        match kind {
            0 => {
                self.book_slide = None;
                self.book_timer.stop();
                ui.set_book_shift(0.0);
                self.book_drag = Some(BookDrag {
                    start: (x, y),
                    pos: self.pos,
                    moving: false,
                });
                false
            }
            2 => {
                let Some(mut drag) = self.book_drag else {
                    return false;
                };
                if self.touch_selecting || self.note_drag.is_some() {
                    return false;
                }
                let (dx, dy) = (x - drag.start.0, y - drag.start.1);
                if !drag.moving {
                    if dx.abs() <= TOUCH_SLOP_LOGICAL_PX && dy.abs() <= TOUCH_SLOP_LOGICAL_PX {
                        return false;
                    }
                    // Moving the page: no long press, no tap.
                    drag.moving = true;
                    self.long_press.stop();
                    self.touch_down = None;
                    self.book_drag = Some(drag);
                }
                let sf = self.sf();
                let want_x = drag.pos.0 - dx as f64 * sf;
                self.scroll_to(ui, (want_x, drag.pos.1 - dy as f64 * sf));
                // What the page could not pan sideways moves it, towards a turn.
                let over = ((want_x - self.pos.0) / sf) as f32;
                ui.set_book_shift(-over);
                true
            }
            _ => {
                let Some(drag) = self.book_drag.take() else {
                    return false;
                };
                if !drag.moving {
                    return false;
                }
                let shift = ui.get_book_shift();
                let width = ui.get_view_w();
                // Pulled left, the next page comes in from the right; pulled right, the one
                // before from the left.
                if kind == 1 && shift.abs() > width * TURN_SHARE && self.turn(ui, shift < 0.0) {
                    let from = if shift < 0.0 {
                        shift + width
                    } else {
                        shift - width
                    };
                    self.slide(ui, from);
                } else {
                    self.slide(ui, shift);
                }
                false
            }
        }
    }

    /// The mouse wheel in book mode (`delta` as Slint's: positive towards the top). Scrolls within
    /// the page, and at its end turns, once per flick of the wheel.
    pub(crate) fn book_wheel(&mut self, ui: &MainWindow, delta: f32) {
        let Some((first, last)) = self.book_range(ui) else {
            return;
        };
        let back = delta > 0.0;
        let at_end = if back {
            self.pos.1 <= first + 0.5
        } else {
            self.pos.1 >= last - 0.5
        };
        if at_end {
            let recent = self
                .last_book_wheel
                .is_some_and(|at| at.elapsed().as_millis() < WHEEL_TURN_GAP_MS);
            self.last_book_wheel = Some(Instant::now());
            if !recent {
                self.turn(ui, !back);
            }
            return;
        }
        let target = self.pos.1 - delta as f64 * self.sf();
        self.scroll_to(ui, (self.pos.0, target));
    }

    /// A navigation key in book mode: a page step scrolls within the page and turns at its end;
    /// Home and End go to the first and the last page.
    pub(crate) fn book_nav(&mut self, ui: &MainWindow, action: NavAction) {
        let Some((first, last)) = self.book_range(ui) else {
            return;
        };
        let (_, free_h) = self.free_view(ui);
        let line = crate::LINE_STEP_LOGICAL_PX * self.sf();
        match action {
            NavAction::Home => self.go_to_page(ui, 0),
            NavAction::End => self.go_to_page(ui, self.layout.len().saturating_sub(1)),
            NavAction::LineUp => self.scroll_to(ui, (self.pos.0, self.pos.1 - line)),
            NavAction::LineDown => self.scroll_to(ui, (self.pos.0, self.pos.1 + line)),
            NavAction::PageUp if self.pos.1 <= first + 0.5 => {
                self.turn(ui, false);
            }
            NavAction::PageDown if self.pos.1 >= last - 0.5 => {
                self.turn(ui, true);
            }
            NavAction::PageUp => self.scroll_to(ui, (self.pos.0, self.pos.1 - free_h * 0.9)),
            NavAction::PageDown => self.scroll_to(ui, (self.pos.0, self.pos.1 + free_h * 0.9)),
        }
    }

    /// A touch or a tap on the phone in Read: a tap brings the islands back; either starts the
    /// wait before they hide again.
    pub(crate) fn chrome_touched(&mut self, ui: &MainWindow, tap: bool) {
        if !ui.get_mobile() {
            return;
        }
        if tap && ui.get_chrome_hidden() {
            ui.set_chrome_hidden(false);
        }
        let weak = ui.as_weak();
        self.chrome_timer.start(
            slint::TimerMode::SingleShot,
            Duration::from_millis(CHROME_HIDE_MS),
            move || {
                if let Some(ui) = weak.upgrade() {
                    with_state(|state| state.chrome_wait_over(&ui));
                }
            },
        );
    }

    /// The wait is over: the islands hide unless the user is doing something with them, or the
    /// page is held.
    fn chrome_wait_over(&mut self, ui: &MainWindow) {
        let busy = ui.get_mode() != 0
            || ui.get_find_visible()
            || ui.get_mobile_busy()
            || ui.get_has_error()
            || ui.get_total_pages() == 0
            || ui.get_touch_selecting()
            || self.field_edit.is_some()
            || self.note_edit.is_some();
        if busy || self.touch_down.is_some() || self.book_drag.is_some() {
            self.chrome_touched(ui, false);
            return;
        }
        ui.set_chrome_hidden(true);
    }
}
