// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 FINE Association <su@fa.org.tr>

use engine_mupdf::EngineHandle;
use pdf_core::{EngineCmd, EngineEvent, RequestId, ZOOM_DEFAULT, ZOOM_STEP, clamp_zoom};
use slint::{ComponentHandle, Image, Rgba8Pixel, SharedPixelBuffer, SharedString};
use std::env;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;

slint::slint! {
    import { Button, ScrollView } from "std-widgets.slint";

    export component MainWindow inherits Window {
        title: "[PROJECT_NAME] - PDF Viewer";
        min-width: 600px;
        min-height: 500px;
        preferred-width: 900px;
        preferred-height: 700px;

        in-out property <image> page_image;
        in-out property <string> status_text: "No document loaded";
        in-out property <string> error_text: "";
        in-out property <bool> has_error: false;
        in-out property <int> current_page: 0;
        in-out property <int> total_pages: 0;
        in-out property <float> zoom_percent: 100.0;
        in-out property <bool> is_loading: false;

        callback prev_page();
        callback next_page();
        callback zoom_in();
        callback zoom_out();
        callback zoom_reset();

        VerticalLayout {
            padding: 8px;
            spacing: 8px;

            // Toolbar
            HorizontalLayout {
                spacing: 8px;
                height: 36px;

                Button {
                    text: "Previous";
                    enabled: root.current_page > 0 && !root.is_loading;
                    clicked => { root.prev_page(); }
                }

                Button {
                    text: "Next";
                    enabled: root.current_page + 1 < root.total_pages && !root.is_loading;
                    clicked => { root.next_page(); }
                }

                Text {
                    text: root.total_pages > 0 ? "Page " + (root.current_page + 1) + " of " + root.total_pages : "No pages";
                    vertical-alignment: center;
                    horizontal-alignment: center;
                    min-width: 120px;
                }

                Rectangle { horizontal-stretch: 1; }

                Button {
                    text: "- Zoom";
                    enabled: !root.is_loading && root.total_pages > 0;
                    clicked => { root.zoom_out(); }
                }

                Button {
                    text: "+ Zoom";
                    enabled: !root.is_loading && root.total_pages > 0;
                    clicked => { root.zoom_in(); }
                }

                Button {
                    text: "Reset (100%)";
                    enabled: !root.is_loading && root.total_pages > 0;
                    clicked => { root.zoom_reset(); }
                }

                Text {
                    text: Math.round(root.zoom_percent) + "%";
                    vertical-alignment: center;
                    min-width: 50px;
                }
            }

            // Error message banner
            if (root.has_error) : Rectangle {
                background: #fde8e8;
                border-color: #f8b4b4;
                border-width: 1px;
                border-radius: 4px;
                height: 40px;

                Text {
                    text: root.error_text;
                    color: #9b1c1c;
                    vertical-alignment: center;
                    horizontal-alignment: center;
                    overflow: elide;
                }
            }

            // Document viewport
            Rectangle {
                background: #e5e7eb;
                border-width: 1px;
                border-color: #d1d5db;
                vertical-stretch: 1;

                if (!root.has_error && root.total_pages > 0) : ScrollView {
                    content-width: root.page_image.width * 1px;
                    content-height: root.page_image.height * 1px;

                    Image {
                        source: root.page_image;
                        image-fit: contain;
                    }
                }

                if (root.total_pages == 0 && !root.has_error) : Text {
                    text: "Open a PDF by running: cargo run -p app -- <file.pdf>";
                    color: #6b7280;
                    vertical-alignment: center;
                    horizontal-alignment: center;
                }
            }

            // Status bar
            HorizontalLayout {
                height: 20px;
                Text {
                    text: root.status_text;
                    color: #4b5563;
                    font-size: 12px;
                    vertical-alignment: center;
                }
            }
        }
    }
}

fn main() -> Result<(), slint::PlatformError> {
    let main_window = MainWindow::new()?;

    // Spawn engine actor
    let (engine, event_rx) = EngineHandle::spawn();
    let engine_sender = engine.sender().clone();

    // Shared state for request IDs and rendering
    let next_request_id = Arc::new(AtomicU64::new(1));
    let current_request_id = Arc::new(AtomicU64::new(0));

    // Internal navigation state
    let state_page = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let state_total = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let state_zoom = Arc::new(std::sync::atomic::AtomicU32::new(ZOOM_DEFAULT.to_bits()));

    let weak_window = main_window.as_weak();

    // Event receiver thread forwarding to Slint UI thread via upgrade_in_event_loop
    let event_loop_weak = weak_window.clone();
    let current_req_for_events = Arc::clone(&current_request_id);
    let state_total_for_events = Arc::clone(&state_total);
    let state_page_for_events = Arc::clone(&state_page);

    thread::spawn(move || {
        while let Ok(event) = event_rx.recv() {
            let weak = event_loop_weak.clone();
            let current_req = Arc::clone(&current_req_for_events);
            let state_total = Arc::clone(&state_total_for_events);
            let state_page = Arc::clone(&state_page_for_events);

            let _ = weak.upgrade_in_event_loop(move |ui| match event {
                EngineEvent::Opened {
                    page_count,
                    page_sizes: _,
                } => {
                    ui.set_has_error(false);
                    ui.set_error_text(SharedString::from(""));
                    ui.set_total_pages(page_count as i32);
                    ui.set_current_page(0);
                    state_total.store(page_count, Ordering::Relaxed);
                    state_page.store(0, Ordering::Relaxed);
                    ui.set_status_text(SharedString::from(format!(
                        "Opened successfully ({page_count} pages)"
                    )));
                    ui.set_is_loading(false);
                }
                EngineEvent::PageRendered {
                    page,
                    scale,
                    request_id,
                    bitmap,
                } => {
                    // Check if this rendered frame was superseded by a newer request
                    if request_id != current_req.load(Ordering::Relaxed) {
                        return;
                    }

                    // Convert Bitmap raw RGBA8 into Slint SharedPixelBuffer
                    let width = bitmap.width;
                    let height = bitmap.height;
                    let mut pixel_buffer = SharedPixelBuffer::<Rgba8Pixel>::new(width, height);
                    let mut_slice = pixel_buffer.make_mut_bytes();

                    if mut_slice.len() == bitmap.data.len() {
                        mut_slice.copy_from_slice(&bitmap.data);
                    } else {
                        // Stride handling if row padding differs
                        let src_stride = bitmap.stride;
                        let dst_stride = (width as usize) * 4;
                        for y in 0..(height as usize) {
                            let src_start = y * src_stride;
                            let src_end = src_start + dst_stride.min(src_stride);
                            let dst_start = y * dst_stride;
                            let dst_end = dst_start + (src_end - src_start);
                            if src_end <= bitmap.data.len() && dst_end <= mut_slice.len() {
                                mut_slice[dst_start..dst_end]
                                    .copy_from_slice(&bitmap.data[src_start..src_end]);
                            }
                        }
                    }

                    let img = Image::from_rgba8(pixel_buffer);
                    ui.set_page_image(img);
                    ui.set_current_page(page as i32);
                    ui.set_zoom_percent(scale * 100.0);
                    ui.set_is_loading(false);
                    ui.set_status_text(SharedString::from(format!(
                        "Page {} rendered ({}x{})",
                        page + 1,
                        width,
                        height
                    )));
                }
                EngineEvent::Error { message } => {
                    ui.set_has_error(true);
                    ui.set_error_text(SharedString::from(message.clone()));
                    ui.set_status_text(SharedString::from(format!("Error: {message}")));
                    ui.set_is_loading(false);
                }
            });
        }
    });

    // Helper closure to trigger render request with cancellation tracking
    let trigger_render = {
        let engine_sender = engine_sender.clone();
        let next_request_id = Arc::clone(&next_request_id);
        let current_request_id = Arc::clone(&current_request_id);
        let weak_ui = weak_window.clone();

        Arc::new(move |page: usize, scale: f32| {
            let req_id = next_request_id.fetch_add(1, Ordering::Relaxed) as RequestId;
            current_request_id.store(req_id, Ordering::Relaxed);

            if let Some(ui) = weak_ui.upgrade() {
                ui.set_is_loading(true);
                ui.set_status_text(SharedString::from(format!(
                    "Rendering page {}...",
                    page + 1
                )));
            }

            let _ = engine_sender.send(EngineCmd::RenderPage {
                page,
                scale,
                request_id: req_id,
            });
        })
    };

    // UI Callbacks
    {
        let trigger = Arc::clone(&trigger_render);
        let state_page = Arc::clone(&state_page);
        let state_zoom = Arc::clone(&state_zoom);
        main_window.on_prev_page(move || {
            let cur = state_page.load(Ordering::Relaxed);
            if cur > 0 {
                let prev = cur - 1;
                state_page.store(prev, Ordering::Relaxed);
                let zoom = f32::from_bits(state_zoom.load(Ordering::Relaxed));
                trigger(prev, zoom);
            }
        });
    }

    {
        let trigger = Arc::clone(&trigger_render);
        let state_page = Arc::clone(&state_page);
        let state_total = Arc::clone(&state_total);
        let state_zoom = Arc::clone(&state_zoom);
        main_window.on_next_page(move || {
            let cur = state_page.load(Ordering::Relaxed);
            let total = state_total.load(Ordering::Relaxed);
            if cur + 1 < total {
                let next = cur + 1;
                state_page.store(next, Ordering::Relaxed);
                let zoom = f32::from_bits(state_zoom.load(Ordering::Relaxed));
                trigger(next, zoom);
            }
        });
    }

    {
        let trigger = Arc::clone(&trigger_render);
        let state_page = Arc::clone(&state_page);
        let state_zoom = Arc::clone(&state_zoom);
        main_window.on_zoom_in(move || {
            let cur_zoom = f32::from_bits(state_zoom.load(Ordering::Relaxed));
            let new_zoom = clamp_zoom(cur_zoom * ZOOM_STEP);
            state_zoom.store(new_zoom.to_bits(), Ordering::Relaxed);
            let page = state_page.load(Ordering::Relaxed);
            trigger(page, new_zoom);
        });
    }

    {
        let trigger = Arc::clone(&trigger_render);
        let state_page = Arc::clone(&state_page);
        let state_zoom = Arc::clone(&state_zoom);
        main_window.on_zoom_out(move || {
            let cur_zoom = f32::from_bits(state_zoom.load(Ordering::Relaxed));
            let new_zoom = clamp_zoom(cur_zoom / ZOOM_STEP);
            state_zoom.store(new_zoom.to_bits(), Ordering::Relaxed);
            let page = state_page.load(Ordering::Relaxed);
            trigger(page, new_zoom);
        });
    }

    {
        let trigger = Arc::clone(&trigger_render);
        let state_page = Arc::clone(&state_page);
        let state_zoom = Arc::clone(&state_zoom);
        main_window.on_zoom_reset(move || {
            let new_zoom = ZOOM_DEFAULT;
            state_zoom.store(new_zoom.to_bits(), Ordering::Relaxed);
            let page = state_page.load(Ordering::Relaxed);
            trigger(page, new_zoom);
        });
    }

    // CLI Argument Handling: Open file passed as first CLI argument
    let args: Vec<String> = env::args().collect();
    if let Some(path_arg) = args.get(1) {
        let pdf_path = PathBuf::from(path_arg);
        main_window.set_status_text(SharedString::from(format!(
            "Opening '{}'...",
            pdf_path.display()
        )));
        main_window.set_is_loading(true);

        let _ = engine_sender.send(EngineCmd::Open {
            path: pdf_path,
            password: None,
        });

        // Trigger rendering of page 0 at 100% zoom
        trigger_render(0, ZOOM_DEFAULT);
    }

    main_window.run()
}
