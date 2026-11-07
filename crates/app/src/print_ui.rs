// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 FINE Association <su@fa.org.tr>

//! Print (ADR 0013): the dialog on the desktop, the job on the tool thread. The document's
//! permissions decide: no print without `print`, a low-resolution raster without `print_hq` (the
//! engine holds to both whatever the app asks).

use crate::platform::printing::{self, Printers};
use crate::{MainWindow, with_state_ret};
use pdf_core::secure::Need;
use pdf_core::tools::{ToolJob, parse_ranges};
use slint::{ComponentHandle as _, ModelRc, SharedString, VecModel};
use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

/// What is left to do when a job's PDF is ready (CUPS and Android), or what to say (a device).
#[derive(Debug, Clone)]
pub(crate) struct Printing {
    pub(crate) printer: String,
    pub(crate) copies: u32,
    pub(crate) title: String,
    pub(crate) pages: usize,
}

/// What the dialog keeps for the next time, in any tab.
#[derive(Default)]
struct Memory {
    printers: Printers,
    /// The driver's settings, for this printer.
    settings: Option<(String, Vec<u8>)>,
    /// The last choice, when the same printer is still there.
    last: Option<String>,
}

thread_local! {
    static MEMORY: RefCell<Memory> = RefCell::new(Memory::default());
}

/// The most copies one job takes: more is a typing slip, and each copy is drawn anew on Windows.
const MAX_COPIES: u32 = 999;

/// The pages typed, one-based and comma-separated, for this many copies; an empty field is every
/// page. Each copy is the whole list again, so copies come out collated.
pub(crate) fn pages_to_print(text: &str, count: u32, copies: u32) -> Result<Vec<u32>, String> {
    let once: Vec<u32> = if text.trim().is_empty() {
        (0..count).collect()
    } else {
        parse_ranges(text, count)
            .map_err(|e| e.to_string())?
            .concat()
    };
    if once.is_empty() {
        return Err("no pages to print".to_owned());
    }
    let mut all = Vec::with_capacity(once.len() * copies as usize);
    for _ in 0..copies {
        all.extend_from_slice(&once);
    }
    Ok(all)
}

/// The number of copies typed.
pub(crate) fn parse_copies(text: &str) -> Result<u32, String> {
    match text.trim().parse::<u32>() {
        Ok(n) if (1..=MAX_COPIES).contains(&n) => Ok(n),
        _ => Err(format!("Copies must be from 1 to {MAX_COPIES}")),
    }
}

/// Ctrl+P, Save's menu or the phone's More menu.
pub(crate) fn open(ui: &MainWindow) {
    let Some(state) = with_state_ret(|state| {
        (state.pdf_editable && !state.page_sizes.is_empty()).then(|| {
            (
                state.permits(ui, Need::Print),
                state.security.permissions.print_hq,
                state.doc_name(),
                state.page_sizes.len() as u32,
                state.running_tool.is_some(),
            )
        })
    })
    .flatten() else {
        return;
    };
    let (allowed, high_quality, name, count, busy) = state;
    if !allowed {
        return;
    }
    if busy {
        ui.set_status_text(SharedString::from("Another tool is still running"));
        return;
    }
    if printing::SYSTEM_DIALOG {
        // The system asks for the printer, the pages and the copies.
        start(ui, String::new(), 1, (0..count).collect(), name);
        return;
    }
    let printers = printing::printers();
    let chosen = MEMORY.with(|memory| {
        let memory = memory.borrow();
        memory
            .last
            .as_ref()
            .and_then(|last| printers.names.iter().position(|name| name == last))
            .unwrap_or(printers.default)
    });
    let names: Vec<SharedString> = printers.names.iter().map(SharedString::from).collect();
    ui.set_print_printers(ModelRc::from(Rc::new(VecModel::from(names))));
    ui.set_print_printer(SharedString::from(
        printers.names.get(chosen).map_or("", String::as_str),
    ));
    ui.set_print_index(chosen as i32);
    ui.set_print_has_printers(!printers.names.is_empty());
    ui.set_print_has_properties(printing::PRINTER_PROPERTIES);
    ui.set_print_pages(SharedString::new());
    ui.set_print_copies(SharedString::from("1"));
    ui.set_print_summary(SharedString::from(if printers.names.is_empty() {
        "No printer is set up on this computer.".to_owned()
    } else if high_quality {
        format!("{name} has {count} page{}.", if count == 1 { "" } else { "s" })
    } else {
        format!(
            "{name} has {count} page{}. Its permissions allow printing at low resolution only ({} dpi).",
            if count == 1 { "" } else { "s" },
            pdf_core::print::LOW_RES_DPI
        )
    }));
    ui.set_print_error(SharedString::new());
    MEMORY.with(|memory| memory.borrow_mut().printers = printers);
    ui.set_print_visible(true);
}

/// The dialog's Properties...: the driver's own dialog, whose settings this job then uses.
pub(crate) fn properties(ui: &MainWindow) {
    let printer = ui.get_print_printer().to_string();
    if printer.is_empty() {
        return;
    }
    MEMORY.with(|memory| {
        let mut memory = memory.borrow_mut();
        let mut settings = memory
            .settings
            .take()
            .filter(|(name, _)| *name == printer)
            .map(|(_, bytes)| bytes);
        printing::printer_properties(&printer, &mut settings);
        memory.settings = settings.map(|bytes| (printer, bytes));
    });
}

/// The dialog's Print.
pub(crate) fn run(ui: &MainWindow) {
    let printer = ui.get_print_printer().to_string();
    let Some((count, name)) =
        with_state_ret(|state| (state.page_sizes.len() as u32, state.doc_name()))
    else {
        return;
    };
    let chosen = parse_copies(&ui.get_print_copies()).and_then(|copies| {
        let per_copy = if printing::PRINTER_PROPERTIES {
            copies
        } else {
            // `lp` makes the copies.
            1
        };
        Ok((
            copies,
            pages_to_print(&ui.get_print_pages(), count, per_copy)?,
        ))
    });
    let (copies, pages) = match chosen {
        Ok(chosen) => chosen,
        Err(err) => {
            ui.set_print_error(SharedString::from(err));
            return;
        }
    };
    if start(ui, printer, copies, pages, name) {
        ui.set_print_visible(false);
    }
}

fn start(ui: &MainWindow, printer: String, copies: u32, pages: Vec<u32>, title: String) -> bool {
    MEMORY.with(|memory| {
        let mut memory = memory.borrow_mut();
        if !printer.is_empty() {
            memory.last = Some(printer.clone());
        }
    });
    let settings = MEMORY.with(|memory| {
        memory
            .borrow()
            .settings
            .as_ref()
            .filter(|(name, _)| *name == printer)
            .map(|(_, bytes)| bytes.clone())
    });
    let count = pages.len();
    with_state_ret(|state| {
        let target = printing::target(&printer, &title, settings.as_deref(), state.next_job_id + 1);
        let job = ToolJob::Print {
            pages,
            target,
            low_res: false,
        };
        let started = state.start_tool(ui, job, false);
        if started && let Some(tool) = &mut state.running_tool {
            tool.print = Some(Printing {
                printer,
                copies,
                title,
                pages: count,
            });
        }
        started
    })
    .unwrap_or(false)
}

/// A print job's pages are out of the engine: with a device, printed; with a file, ready to send.
pub(crate) fn finished(ui: &MainWindow, printing: Printing, files: Vec<PathBuf>) {
    let Printing {
        printer,
        copies,
        title,
        pages,
    } = printing;
    let sent_message = if printing::SYSTEM_DIALOG {
        "Sent to the system's print dialog".to_owned()
    } else {
        format!(
            "Sent {pages} page{} to {printer}",
            if pages == 1 { "" } else { "s" }
        )
    };
    let Some(file) = files.into_iter().next() else {
        ui.set_status_text(SharedString::from(sent_message));
        return;
    };
    let weak = ui.as_weak();
    printing::send_file(&printer, copies, &title, file, move |result| {
        let Some(ui) = weak.upgrade() else {
            return;
        };
        ui.set_status_text(SharedString::from(match result {
            Ok(()) => sent_message,
            Err(err) => format!("Printing failed: {err}"),
        }));
    });
}

/// Connects the dialog.
pub(crate) fn connect(ui: &MainWindow) {
    let weak = ui.as_weak();
    ui.on_print_open(move || {
        if let Some(ui) = weak.upgrade() {
            open(&ui);
        }
    });
    let weak = ui.as_weak();
    ui.on_print_run(move || {
        if let Some(ui) = weak.upgrade() {
            run(&ui);
        }
    });
    let weak = ui.as_weak();
    ui.on_print_properties(move || {
        if let Some(ui) = weak.upgrade() {
            properties(&ui);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_field_is_every_page() {
        assert_eq!(pages_to_print(" ", 3, 1).unwrap(), [0, 1, 2]);
    }

    #[test]
    fn pages_as_people_type_them() {
        assert_eq!(pages_to_print("2-3, 1", 3, 1).unwrap(), [1, 2, 0]);
        assert!(pages_to_print("4", 3, 1).is_err());
        assert!(pages_to_print("a", 3, 1).is_err());
    }

    #[test]
    fn copies_are_collated() {
        assert_eq!(pages_to_print("1-2", 3, 2).unwrap(), [0, 1, 0, 1]);
    }

    #[test]
    fn copies_are_a_number_in_range() {
        assert_eq!(parse_copies(" 2 "), Ok(2));
        assert!(parse_copies("0").is_err());
        assert!(parse_copies("1000").is_err());
        assert!(parse_copies("two").is_err());
    }
}
