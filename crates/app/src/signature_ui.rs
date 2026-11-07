// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 FINE Association <su@fa.org.tr>

//! A document's signatures (Plan 0009 Phase 3, §4.2): the mark in the status island, the panel that
//! lists them, and what the pointer shows over a signature on the page. What they say comes from
//! the engine, about the file on disk, after it is opened and after each save.

use crate::{AppState, MainWindow, SigRow};
use pdf_core::select::PagePoint;
use pdf_core::signature::{SignatureInfo, Verdict, overall};
use slint::{ModelRc, SharedString, VecModel};
use std::path::Path;

fn verdict_index(verdict: Verdict) -> i32 {
    match verdict {
        Verdict::Valid => 0,
        Verdict::Unsure => 1,
        Verdict::Invalid => 2,
    }
}

impl AppState {
    /// What the engine found for the file at `path`; a late answer about another file is dropped.
    pub(crate) fn set_signatures(
        &mut self,
        ui: &MainWindow,
        path: &Path,
        signatures: Vec<SignatureInfo>,
    ) {
        if self.doc_path.as_deref() != Some(path) {
            return;
        }
        self.signatures = signatures;
        self.show_signatures(ui);
        // A signature that cannot be relied on is said at once, not only by the colour of a mark.
        match overall(&self.signatures) {
            Some(Verdict::Invalid) => ui.set_status_text(SharedString::from(
                "A signature is not valid: the shield says why",
            )),
            Some(Verdict::Unsure) => ui.set_status_text(SharedString::from(
                "A signature is not verified: the shield says why",
            )),
            _ => {}
        }
    }

    /// The document changed: what was said about the last one is gone.
    pub(crate) fn clear_signatures(&mut self, ui: &MainWindow) {
        self.signatures.clear();
        self.show_signatures(ui);
    }

    /// The mark and the panel, for this tab.
    pub(crate) fn show_signatures(&self, ui: &MainWindow) {
        let rows: Vec<SigRow> = self
            .signatures
            .iter()
            .map(|signature| SigRow {
                headline: SharedString::from(signature.headline()),
                byline: SharedString::from(signature.byline()),
                details: SharedString::from(signature.details().join("\n")),
                verdict: verdict_index(signature.verdict()),
                page: signature.page.map_or(-1, |page| page as i32),
            })
            .collect();
        ui.set_signature_rows(ModelRc::new(VecModel::from(rows)));
        ui.set_signature_verdict(overall(&self.signatures).map_or(-1, verdict_index));
        // The panel has nothing to show for a document without signatures.
        if self.signatures.is_empty() && ui.get_sidebar_tab() == 2 {
            ui.set_sidebar_tab(0);
            ui.invoke_sidebar_tab_changed();
        }
    }

    /// A row of the panel was clicked: its signature's page, when it has a place on one.
    pub(crate) fn signature_clicked(&mut self, ui: &MainWindow, row: usize) {
        let Some(signature) = self.signatures.get(row) else {
            return;
        };
        let Some(page) = signature.page else {
            return;
        };
        let top = signature.rect.map(|rect| rect.y);
        self.go_to_dest(ui, page as usize, top);
    }

    /// What to show while the pointer rests on a signature on the page.
    pub(crate) fn signature_hover(&self, point: PagePoint) -> Option<String> {
        let signature = self.signatures.iter().find(|signature| {
            signature.page == Some(point.page)
                && signature.rect.is_some_and(|rect| {
                    point.x >= rect.x
                        && point.x <= rect.x + rect.width
                        && point.y >= rect.y
                        && point.y <= rect.y + rect.height
                })
        })?;
        Some(format!("{}\n{}", signature.headline(), signature.byline()))
    }
}
