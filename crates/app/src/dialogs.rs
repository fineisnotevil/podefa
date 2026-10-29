// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 FINE Association <su@fa.org.tr>

//! The window's own dialogs (Plan 0007 Phase 5): message boxes on the desktop in the window's
//! style, About, the password of a protected document, and the "Reduce motion" setting.
//!
//! A message box is asked for through [`platform::MessageDialog`] as before; on the desktop it is
//! shown in the window (`main.slint`, `message-*`) instead of by the system, one at a time, the
//! next waiting until the one shown is answered. Android keeps its own dialogs.

use crate::platform::{self, Answer, Buttons, Level, MessageDialog};
use crate::{MainWindow, UiTheme, with_state, with_state_ret};
use pdf_core::EngineCmd;
use slint::{ComponentHandle, SharedString};
use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;

type Waiting = Box<dyn FnOnce(Answer)>;

thread_local! {
    /// Message boxes asked for while one is shown.
    static QUEUE: RefCell<VecDeque<(MessageDialog, Waiting)>> = const { RefCell::new(VecDeque::new()) };
    /// What the message box shown waits to be told.
    static WAITING: RefCell<Option<Waiting>> = const { RefCell::new(None) };
}

/// The dialogs' callbacks, and the settings they show.
pub(crate) fn wire(ui: &MainWindow, reduce_motion: bool) {
    ui.set_app_version(SharedString::from(env!("CARGO_PKG_VERSION")));
    show_reduce_motion(ui, reduce_motion);

    if !cfg!(target_os = "android") {
        let weak = ui.as_weak();
        platform::set_message_presenter(Rc::new(move |spec, then| {
            QUEUE.with(|queue| queue.borrow_mut().push_back((spec, then)));
            if let Some(ui) = weak.upgrade() {
                show_next(&ui);
            }
        }));
    }

    let weak = ui.as_weak();
    ui.on_message_answered(move |answer| {
        let Some(ui) = weak.upgrade() else {
            return;
        };
        ui.set_message_visible(false);
        if let Some(then) = WAITING.with(|cell| cell.borrow_mut().take()) {
            then(match answer {
                0 => Answer::Yes,
                1 => Answer::No,
                _ => Answer::Cancel,
            });
        }
        show_next(&ui);
    });

    let weak = ui.as_weak();
    ui.on_set_reduce_motion(move |on| {
        if let Some(ui) = weak.upgrade() {
            show_reduce_motion(&ui, on);
            with_state(|state| {
                state.settings.reduce_motion = on;
                state.save_settings();
            });
        }
    });

    let weak = ui.as_weak();
    ui.on_password_submit(move |text| {
        if let Some(ui) = weak.upgrade() {
            password_submit(&ui, text.as_str());
        }
    });
    let weak = ui.as_weak();
    ui.on_password_cancel(move || {
        if let Some(ui) = weak.upgrade() {
            password_cancel(&ui);
        }
    });
}

fn show_reduce_motion(ui: &MainWindow, on: bool) {
    ui.set_reduce_motion(on);
    ui.global::<UiTheme>().set_reduce_motion(on);
}

/// Shows the next message box, unless one is shown.
fn show_next(ui: &MainWindow) {
    if WAITING.with(|cell| cell.borrow().is_some()) {
        return;
    }
    let Some((spec, then)) = QUEUE.with(|queue| queue.borrow_mut().pop_front()) else {
        return;
    };
    ui.set_message_text(SharedString::from(spec.description()));
    ui.set_message_level(match spec.level_of() {
        Level::Info => 0,
        Level::Warning => 1,
        Level::Error => 2,
    });
    ui.set_message_buttons(match spec.buttons_of() {
        Buttons::Ok => 0,
        Buttons::YesNo => 1,
        Buttons::YesNoCancel => 2,
    });
    WAITING.with(|cell| *cell.borrow_mut() = Some(then));
    ui.set_message_visible(true);
}

/// The engine asked for the password of the document being opened; `wrong` when the one given
/// was not right.
pub(crate) fn needs_password(ui: &MainWindow, wrong: bool) {
    let name = with_state_ret(|state| {
        state.opening.as_ref().map(|path| {
            path.file_name().map_or_else(
                || path.display().to_string(),
                |n| n.to_string_lossy().into_owned(),
            )
        })
    })
    .flatten();
    let Some(name) = name else {
        return;
    };
    ui.set_is_loading(false);
    ui.set_password_file(SharedString::from(name.as_str()));
    ui.set_password_busy(false);
    ui.set_password_wrong(wrong);
    if wrong {
        ui.set_password_tries(ui.get_password_tries() + 1);
    }
    ui.set_status_text(SharedString::from(format!(
        "\"{name}\" is password-protected"
    )));
    ui.set_password_visible(true);
}

fn password_submit(ui: &MainWindow, password: &str) {
    if password.is_empty() {
        ui.set_password_wrong(true);
        ui.set_password_tries(ui.get_password_tries() + 1);
        return;
    }
    let sent = with_state_ret(|state| {
        let path = state.opening.clone()?;
        state
            .cmd_tx
            .send(EngineCmd::Open {
                path,
                password: Some(password.to_owned()),
            })
            .ok()
    })
    .flatten();
    if sent.is_some() {
        ui.set_password_busy(true);
        ui.set_is_loading(true);
    } else {
        password_cancel(ui);
    }
}

/// The password dialog was closed: the document is not opened, the one before stays.
fn password_cancel(ui: &MainWindow) {
    ui.set_password_visible(false);
    ui.set_password_busy(false);
    ui.set_is_loading(false);
    ui.set_status_text(SharedString::from("Opening cancelled"));
    with_state(|state| state.open_failed(ui));
}

/// The document opened, or failed for another reason: the password dialog has done its work.
pub(crate) fn password_done(ui: &MainWindow) {
    if ui.get_password_visible() {
        ui.set_password_visible(false);
        ui.set_password_busy(false);
        ui.set_password_tries(0);
    }
}
