// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 FINE Association <su@fa.org.tr>

//! Protection (Plan 0009 Phase 2, §4.2): a document's permissions followed (§0 question 4) and
//! unlocked with its permissions password, and the Protection dialog that writes AES-256 with new
//! passwords and permissions over the open file, or removes it.
//!
//! The engine refuses what the permissions withhold too (`MupdfError::NotPermitted`); the app
//! refuses first, so a drag or a typed field is not wasted, and says why.

use crate::platform::{Answer, Buttons, FileDialog, Level, MessageDialog};
use crate::{AppState, MainWindow, Tool, with_state, with_state_ret};
use pdf_core::EngineCmd;
use pdf_core::annot::EditOp;
use pdf_core::secure::{Encryption, Need, Permissions, Protection, Security};
use slint::{ComponentHandle as _, SharedString};
use std::path::PathBuf;

impl AppState {
    /// The document's protection arrived: with `Opened`, an unlock, or a save with new protection.
    pub(crate) fn set_security(&mut self, ui: &MainWindow, security: Security) {
        self.security = security;
        self.editable = self.pdf_editable && self.security.permissions.allows_any_edit();
        if !self.editable && self.tool != Tool::Select {
            self.crop_cancel(ui);
            self.tool = Tool::Select;
            ui.set_tool(self.tool.index());
        }
        self.show_security(ui);
    }

    /// The buttons that follow the protection, for this tab.
    pub(crate) fn show_security(&self, ui: &MainWindow) {
        ui.set_editable(self.editable);
        ui.set_can_undo(self.editable && self.can_undo);
        ui.set_can_redo(self.editable && self.can_redo);
        ui.set_protectable(self.pdf_editable);
        ui.set_secure_encrypted(self.security.encrypted);
        ui.set_secure_restricted(!self.security.permissions.is_all());
    }

    /// Whether the document allows `need`; when it does not, the status bar says so.
    pub(crate) fn permits(&self, ui: &MainWindow, need: Need) -> bool {
        if self.security.permissions.allows(need) {
            return true;
        }
        ui.set_status_text(SharedString::from(format!(
            "The document's permissions do not allow {}; the lock beside the page number unlocks them",
            need.words()
        )));
        false
    }
}

/// What the dialog says about the document as it is.
fn summary(security: &Security) -> String {
    let mut text = if security.encrypted {
        format!("This document is encrypted with {}.", security.method)
    } else {
        "This document is not protected.".to_owned()
    };
    let withheld = security.permissions.withheld();
    if !withheld.is_empty() {
        text.push_str(&format!(
            " It withholds {}; its permissions password allows them.",
            join(&withheld)
        ));
    } else if security.encrypted {
        text.push_str(" Every permission is allowed as it is open now.");
    }
    if security.signed > 0 {
        text.push_str(&format!(
            " It has {} signature{}; changing the protection writes the file anew, which makes them \
             invalid.",
            security.signed,
            if security.signed == 1 { "" } else { "s" }
        ));
    }
    text
}

fn join(words: &[&str]) -> String {
    match words {
        [] => String::new(),
        [one] => (*one).to_owned(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

/// File > Protection, or the lock beside the page number: the dialog, filled from the document.
pub(crate) fn open(ui: &MainWindow) {
    let Some(security) = with_state_ret(|state| {
        (state.pdf_editable && !state.page_sizes.is_empty()).then(|| state.security.clone())
    })
    .flatten() else {
        return;
    };
    ui.set_protect_summary(SharedString::from(summary(&security)));
    ui.set_protect_error(SharedString::new());
    ui.set_protect_busy(false);
    for set in [
        MainWindow::set_protect_user,
        MainWindow::set_protect_user_again,
        MainWindow::set_protect_owner,
        MainWindow::set_protect_owner_again,
    ] {
        set(ui, SharedString::new());
    }
    let p = security.permissions;
    ui.set_protect_print(p.print);
    ui.set_protect_copy(p.copy);
    ui.set_protect_pages(p.assemble);
    ui.set_protect_edit(p.modify);
    ui.set_protect_comment(p.annotate);
    ui.set_protect_fill(p.fill);
    ui.set_protect_visible(true);
}

/// Unlock... in the dialog: the password dialog, asking for the permissions password.
pub(crate) fn unlock(ui: &MainWindow) {
    let Some(name) = with_state_ret(|state| state.doc_name()) else {
        return;
    };
    ui.set_protect_visible(false);
    ui.set_password_unlock(true);
    ui.set_password_file(SharedString::from(name));
    ui.set_password_wrong(false);
    ui.set_password_busy(false);
    ui.set_password_tries(0);
    ui.set_password_visible(true);
}

/// The password dialog's Unlock, in its unlock mode.
pub(crate) fn unlock_submit(ui: &MainWindow, password: &str) {
    if password.is_empty() {
        ui.set_password_wrong(true);
        ui.set_password_tries(ui.get_password_tries() + 1);
        return;
    }
    let sent = with_state_ret(|state| {
        state
            .cmd_tx
            .send(EngineCmd::Unlock {
                password: password.to_owned(),
                generation: state.doc_generation,
            })
            .is_ok()
    })
    .unwrap_or(false);
    if sent {
        ui.set_password_busy(true);
    }
}

/// The password dialog closed in its unlock mode: nothing changes.
pub(crate) fn unlock_cancel(ui: &MainWindow) {
    ui.set_password_visible(false);
    ui.set_password_busy(false);
    ui.set_password_unlock(false);
}

/// The engine answered an unlock or wrote new protection.
pub(crate) fn security_changed(ui: &MainWindow, generation: u64, security: Security, wrong: bool) {
    let current = with_state_ret(|state| state.doc_generation == generation).unwrap_or(false);
    if !current {
        return;
    }
    if ui.get_password_unlock() && ui.get_password_visible() {
        if wrong {
            ui.set_password_busy(false);
            ui.set_password_wrong(true);
            ui.set_password_tries(ui.get_password_tries() + 1);
            return;
        }
        unlock_cancel(ui);
        ui.set_status_text(SharedString::from(
            "Unlocked: every permission is allowed until the document is closed",
        ));
    }
    with_state(|state| state.set_security(ui, security));
}

/// The dialog's choices as the permissions to write.
fn chosen_permissions(ui: &MainWindow) -> Permissions {
    let print = ui.get_protect_print();
    Permissions {
        print,
        print_hq: print,
        copy: ui.get_protect_copy(),
        modify: ui.get_protect_edit(),
        annotate: ui.get_protect_comment(),
        fill: ui.get_protect_fill(),
        assemble: ui.get_protect_pages(),
        accessibility: true,
    }
}

/// Protect in the dialog: checks the passwords, then writes.
pub(crate) fn apply(ui: &MainWindow) {
    let (user, owner) = (ui.get_protect_user(), ui.get_protect_owner());
    let error = if user != ui.get_protect_user_again() {
        Some("The two passwords to open differ.".to_owned())
    } else if owner != ui.get_protect_owner_again() {
        Some("The two permissions passwords differ.".to_owned())
    } else {
        None
    };
    if let Some(error) = error {
        ui.set_protect_error(SharedString::from(error));
        return;
    }
    let encryption = Encryption {
        user: user.to_string(),
        owner: owner.to_string(),
        permissions: chosen_permissions(ui),
    };
    match encryption.checked() {
        Ok(encryption) => confirm(ui, Protection::Encrypt(encryption)),
        Err(e) => {
            let mut text = e.to_string();
            if let Some(first) = text.get(..1) {
                text = format!("{}{}.", first.to_uppercase(), &text[1..]);
            }
            ui.set_protect_error(SharedString::from(text));
        }
    }
}

/// Remove Protection in the dialog.
pub(crate) fn remove(ui: &MainWindow) {
    confirm(ui, Protection::Remove);
}

/// Asks before a signed document is written anew (§3.5), then writes.
fn confirm(ui: &MainWindow, protection: Protection) {
    let signed = with_state_ret(|state| state.security.signed).unwrap_or(0);
    if signed == 0 {
        write(ui, protection);
        return;
    }
    let weak = ui.as_weak();
    MessageDialog::new(signed_warning(signed, "Changing the protection"))
        .level(Level::Warning)
        .buttons(Buttons::YesNo)
        .show(move |answer| {
            if let (Some(ui), Answer::Yes) = (weak.upgrade(), answer) {
                write(&ui, protection);
            }
        });
}

/// The question before an action that writes a signed document anew (Plan 0009 §3.5).
pub(crate) fn signed_warning(signed: u32, action: &str) -> String {
    let (count, them) = if signed == 1 {
        ("a signature".to_owned(), "it")
    } else {
        (format!("{signed} signatures"), "them")
    };
    format!(
        "This document has {count}.\n\n{action} writes the document anew, and {them} will no \
         longer be valid in what is written. Go on?"
    )
}

/// The first edit of a certified document that its certifying signature does not allow (§3.5):
/// asked about once, from the event loop, since the state is borrowed here. No asks again at the
/// next such edit.
pub(crate) fn warn_certified(op: EditOp) {
    slint::Timer::single_shot(std::time::Duration::ZERO, move || {
        let text = "This document is certified, and its certifying signature does not allow this \
                    change: other readers will show the signature as broken by a change that is \
                    not allowed once it is saved.\n\nMake the change?";
        MessageDialog::new(text)
            .level(Level::Warning)
            .buttons(Buttons::YesNo)
            .show(move |answer| {
                with_state(|state| {
                    if answer == Answer::Yes {
                        state.send_edit(op);
                    } else {
                        state.certified_warned = false;
                    }
                });
            });
    });
}

/// Writes the protection over the open file; a document made from pages is first given a file.
fn write(ui: &MainWindow, protection: Protection) {
    let Some(untitled) = with_state_ret(|state| {
        state
            .untitled
            .clone()
            .map(|title| (title, state.origin_dir.clone()))
    }) else {
        return;
    };
    match untitled {
        None => send(ui, None, protection),
        Some((title, dir)) => {
            let mut dialog = FileDialog::new()
                .add_filter("PDF document", &["pdf"])
                .set_file_name(format!("{title}.pdf"));
            if let Some(dir) = dir {
                dialog = dialog.set_directory(dir);
            }
            let weak = ui.as_weak();
            dialog.save_file(move |path| {
                if let (Some(ui), Some(path)) = (weak.upgrade(), path) {
                    send(&ui, Some(path), protection);
                }
            });
        }
    }
}

fn send(ui: &MainWindow, path: Option<PathBuf>, protection: Protection) {
    let removing = protection == Protection::Remove;
    let sent = with_state_ret(|state| {
        if state.saving {
            return false;
        }
        state.saving = true;
        let sent = state
            .cmd_tx
            .send(EngineCmd::Protect {
                path,
                protection,
                generation: state.doc_generation,
            })
            .is_ok();
        state.saving = sent;
        sent
    })
    .unwrap_or(false);
    if sent {
        ui.set_protect_busy(true);
        ui.set_status_text(SharedString::from(if removing {
            "Removing the protection..."
        } else {
            "Protecting..."
        }));
    }
}

/// A save finished: the dialog closes when it was the protection's.
pub(crate) fn saved(ui: &MainWindow, ok: bool) {
    if !ui.get_protect_busy() {
        return;
    }
    ui.set_protect_busy(false);
    if ok {
        ui.set_protect_visible(false);
    } else {
        ui.set_protect_error(ui.get_status_text());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_summary_names_what_is_withheld() {
        let print_only = Security {
            encrypted: true,
            method: "AES-256".to_owned(),
            permissions: Permissions {
                print: true,
                print_hq: true,
                ..Permissions::from_bits(0)
            },
            signed: 1,
            certified: None,
        };
        let text = summary(&print_only);
        assert!(text.starts_with("This document is encrypted with AES-256."));
        assert!(text.contains("copying text and images, editing the content"));
        assert!(text.contains("and changing pages;"));
        assert!(text.contains("1 signature;"));
        assert_eq!(
            summary(&Security::default()),
            "This document is not protected."
        );
        assert_eq!(join(&["a", "b", "c"]), "a, b and c");
    }
}
