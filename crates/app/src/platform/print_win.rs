// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 FINE Association <su@fa.org.tr>

//! Printing on Windows (ADR 0013): the printers `winspool` knows, and a [`PrintSink`] over a
//! printer's device context. Windows prints through GDI, not from PDF files, so the engine draws
//! the pages at the printer's resolution and the bands go in with `StretchDIBits`.

use pdf_core::print::{PrintBand, PrintSink, Sheet};
use std::mem::size_of;
use std::ptr::{null, null_mut};
use windows_sys::Win32::Foundation::{ERROR_INSUFFICIENT_BUFFER, GetLastError, HWND};
use windows_sys::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateDCW, DEVMODEW, DIB_RGB_COLORS, DM_IN_BUFFER,
    DM_IN_PROMPT, DM_OUT_BUFFER, DeleteDC, GetDeviceCaps, HALFTONE, HDC, HORZRES, LOGPIXELSX,
    LOGPIXELSY, PHYSICALHEIGHT, PHYSICALOFFSETX, PHYSICALOFFSETY, PHYSICALWIDTH, SRCCOPY,
    SetStretchBltMode, StretchDIBits, VERTRES,
};
use windows_sys::Win32::Graphics::Printing::{
    ClosePrinter, DocumentPropertiesW, EnumPrintersW, GetDefaultPrinterW, OpenPrinterW,
    PRINTER_ENUM_CONNECTIONS, PRINTER_ENUM_LOCAL, PRINTER_HANDLE, PRINTER_INFO_4W,
};
use windows_sys::Win32::Storage::Xps::{AbortDoc, DOCINFOW, EndDoc, EndPage, StartDocW, StartPage};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::GetActiveWindow;

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

/// # Safety
/// `text` is null or points at a zero-terminated UTF-16 string.
unsafe fn from_wide(text: *const u16) -> String {
    if text.is_null() {
        return String::new();
    }
    let mut len = 0;
    // SAFETY: the caller's promise: reads up to and including the terminator.
    while unsafe { *text.add(len) } != 0 {
        len += 1;
    }
    // SAFETY: `len` elements were just read.
    String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(text, len) })
}

/// The printers on this machine and its connections, by name.
pub fn printers() -> Vec<String> {
    let flags = PRINTER_ENUM_LOCAL | PRINTER_ENUM_CONNECTIONS;
    let (mut needed, mut count) = (0u32, 0u32);
    // SAFETY: a null buffer of size 0 only asks for the size.
    unsafe {
        EnumPrintersW(
            flags,
            null(),
            4,
            null_mut(),
            0,
            &raw mut needed,
            &raw mut count,
        );
    }
    if needed == 0 {
        return Vec::new();
    }
    // u64s, so the array of pointer-holding structures is aligned.
    let mut buffer = vec![0u64; (needed as usize).div_ceil(8)];
    // SAFETY: the buffer holds `needed` bytes; on success it starts with `count` PRINTER_INFO_4W.
    let done = unsafe {
        EnumPrintersW(
            flags,
            null(),
            4,
            buffer.as_mut_ptr().cast(),
            needed,
            &raw mut needed,
            &raw mut count,
        )
    };
    if done == 0 {
        return Vec::new();
    }
    // SAFETY: as above; the names point into the same buffer, which outlives this.
    unsafe {
        std::slice::from_raw_parts(buffer.as_ptr().cast::<PRINTER_INFO_4W>(), count as usize)
            .iter()
            .map(|info| from_wide(info.pPrinterName))
            .collect()
    }
}

/// The default printer's name.
pub fn default_printer() -> Option<String> {
    let mut len = 0u32;
    // SAFETY: a null buffer only asks for the length.
    let asked = unsafe { GetDefaultPrinterW(null_mut(), &raw mut len) };
    // SAFETY: reads this thread's last error.
    if asked != 0 || unsafe { GetLastError() } != ERROR_INSUFFICIENT_BUFFER || len == 0 {
        return None;
    }
    let mut name = vec![0u16; len as usize];
    // SAFETY: the buffer holds `len` elements.
    let done = unsafe { GetDefaultPrinterW(name.as_mut_ptr(), &raw mut len) };
    // SAFETY: on success the buffer holds a terminated string.
    (done != 0).then(|| unsafe { from_wide(name.as_ptr()) })
}

/// A printer opened for its settings, closed when dropped.
struct OpenPrinter(PRINTER_HANDLE);

impl OpenPrinter {
    fn new(name: &[u16]) -> Option<Self> {
        let mut handle = PRINTER_HANDLE { Value: null_mut() };
        // SAFETY: `name` is terminated; the defaults are optional.
        (unsafe { OpenPrinterW(name.as_ptr(), &raw mut handle, null()) } != 0)
            .then_some(Self(handle))
    }
}

impl Drop for OpenPrinter {
    fn drop(&mut self) {
        // SAFETY: opened by `new` and closed once.
        unsafe {
            ClosePrinter(self.0);
        }
    }
}

/// The driver's own settings dialog for a printer, modal to the active window. `settings` is what
/// it starts from and what it leaves: a `DEVMODEW` with the driver's data after it, for
/// [`GdiSink`]. `false` when cancelled or when the driver has no such dialog.
pub fn printer_properties(printer: &str, settings: &mut Option<Vec<u8>>) -> bool {
    let name = wide(printer);
    let Some(open) = OpenPrinter::new(&name) else {
        return false;
    };
    // SAFETY: mode 0 asks for the size the settings take.
    let size =
        unsafe { DocumentPropertiesW(null_mut(), open.0, name.as_ptr(), null_mut(), null(), 0) };
    if size <= 0 {
        return false;
    }
    let mut out = vec![0u64; (size as usize).div_ceil(8)];
    // A setting for another printer is not this one's.
    let start = settings
        .as_ref()
        .filter(|old| old.len() <= size as usize)
        .map(|old| aligned(old));
    let mut mode = DM_OUT_BUFFER | DM_IN_PROMPT;
    if start.is_some() {
        mode |= DM_IN_BUFFER;
    }
    // SAFETY: reads this thread's window state.
    let window: HWND = unsafe { GetActiveWindow() };
    // SAFETY: `out` holds the size the driver asked for and `start`, when given, was the driver's.
    let answer = unsafe {
        DocumentPropertiesW(
            window,
            open.0,
            name.as_ptr(),
            out.as_mut_ptr().cast(),
            start
                .as_ref()
                .map_or(null(), |bytes| bytes.as_ptr().cast::<DEVMODEW>()),
            mode,
        )
    };
    // IDOK.
    if answer != 1 {
        return false;
    }
    // SAFETY: `out` is `size` initialised bytes.
    let bytes = unsafe { std::slice::from_raw_parts(out.as_ptr().cast::<u8>(), size as usize) };
    *settings = Some(bytes.to_vec());
    true
}

/// `bytes` in a buffer aligned for `DEVMODEW`.
fn aligned(bytes: &[u8]) -> Vec<u64> {
    let mut out = vec![0u64; bytes.len().div_ceil(8)];
    // SAFETY: `out` has room for every byte, and the two do not overlap.
    unsafe {
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), out.as_mut_ptr().cast::<u8>(), bytes.len());
    }
    out
}

/// A print job on a printer's device context, created and used on the tool thread.
#[derive(Debug)]
pub struct GdiSink {
    printer: Vec<u16>,
    title: Vec<u16>,
    /// A file for the printer to write instead of its port, for "Print to PDF" without its dialog.
    output: Option<Vec<u16>>,
    settings: Option<Vec<u64>>,
    dc: HDC,
    started: bool,
    /// One band's rows, bottom padding included, in the order GDI takes them.
    bgr: Vec<u8>,
}

// SAFETY: the device context is created in `start`, on the thread that runs the job, and used and
// deleted there; before `start` the sink holds no handle.
unsafe impl Send for GdiSink {}

impl GdiSink {
    pub fn new(printer: &str, title: &str, settings: Option<&[u8]>) -> Self {
        Self {
            printer: wide(printer),
            title: wide(title),
            output: None,
            settings: settings.map(aligned),
            dc: null_mut(),
            started: false,
            bgr: Vec::new(),
        }
    }

    /// The job goes to this file instead of the printer's port.
    pub fn to_file(mut self, path: &str) -> Self {
        self.output = Some(wide(path));
        self
    }

    fn close(&mut self) {
        if !self.dc.is_null() {
            // SAFETY: created by `start` and deleted once.
            unsafe {
                DeleteDC(self.dc);
            }
            self.dc = null_mut();
        }
    }

    fn cap(&self, index: u32) -> u32 {
        // SAFETY: `dc` is a live device context while the job runs.
        unsafe { GetDeviceCaps(self.dc, index as i32) }.max(0) as u32
    }

    fn failed(&self, what: &str) -> String {
        // SAFETY: reads this thread's last error.
        let code = unsafe { GetLastError() };
        format!("{what}: Windows error {code}")
    }
}

impl PrintSink for GdiSink {
    fn start(&mut self) -> Result<Sheet, String> {
        let driver = wide("WINSPOOL");
        let settings = self
            .settings
            .as_ref()
            .map_or(null(), |bytes| bytes.as_ptr().cast::<DEVMODEW>());
        // SAFETY: the strings are terminated and `settings` is null or the driver's own, aligned.
        self.dc = unsafe { CreateDCW(driver.as_ptr(), self.printer.as_ptr(), null(), settings) };
        if self.dc.is_null() {
            return Err(self.failed("could not open the printer"));
        }
        // SAFETY: `dc` was just made; the strings live as long as `self`, past the call.
        let job = unsafe {
            let info = DOCINFOW {
                cbSize: size_of::<DOCINFOW>() as i32,
                lpszDocName: self.title.as_ptr(),
                lpszOutput: self.output.as_ref().map_or(null(), |path| path.as_ptr()),
                lpszDatatype: null(),
                fwType: 0,
            };
            StartDocW(self.dc, &raw const info)
        };
        if job <= 0 {
            let error = self.failed("the printer took no job");
            self.close();
            return Err(error);
        }
        self.started = true;
        // SAFETY: `dc` is live.
        unsafe {
            SetStretchBltMode(self.dc, HALFTONE);
        }
        Ok(Sheet {
            dpi: (self.cap(LOGPIXELSX), self.cap(LOGPIXELSY)),
            paper: (self.cap(PHYSICALWIDTH), self.cap(PHYSICALHEIGHT)),
            printable: (self.cap(HORZRES), self.cap(VERTRES)),
            printable_at: (self.cap(PHYSICALOFFSETX), self.cap(PHYSICALOFFSETY)),
        })
    }

    fn begin_page(&mut self) -> Result<(), String> {
        // SAFETY: `dc` is live, in a started document.
        if unsafe { StartPage(self.dc) } <= 0 {
            return Err(self.failed("the printer took no page"));
        }
        // StartPage resets a device context's mode on older drivers.
        // SAFETY: as above.
        unsafe {
            SetStretchBltMode(self.dc, HALFTONE);
        }
        Ok(())
    }

    fn band(&mut self, band: PrintBand<'_>) -> Result<(), String> {
        let (width, height) = (band.width as usize, band.height as usize);
        if band.rgb.len() < width * height * 3 {
            return Err("a band has too few pixels".to_owned());
        }
        // Rows of 4-byte multiples, blue first.
        let stride = (width * 3 + 3) & !3;
        self.bgr.clear();
        self.bgr.resize(stride * height, 0);
        for (to, from) in self
            .bgr
            .chunks_exact_mut(stride)
            .zip(band.rgb.chunks_exact(width * 3))
        {
            for (to, from) in to.chunks_exact_mut(3).zip(from.chunks_exact(3)) {
                to[0] = from[2];
                to[1] = from[1];
                to[2] = from[0];
            }
        }
        // SAFETY: plain data, every field that matters set below.
        let mut info: BITMAPINFO = unsafe { std::mem::zeroed() };
        info.bmiHeader = BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: band.width as i32,
            // Negative: the first row is the top one.
            biHeight: -(band.height as i32),
            biPlanes: 1,
            biBitCount: 24,
            biCompression: BI_RGB,
            biSizeImage: self.bgr.len() as u32,
            biXPelsPerMeter: 0,
            biYPelsPerMeter: 0,
            biClrUsed: 0,
            biClrImportant: 0,
        };
        let (x, y, w, h) = band.dest;
        // SAFETY: `bgr` holds `height` rows of `stride` bytes, as `info` says.
        let lines = unsafe {
            StretchDIBits(
                self.dc,
                x,
                y,
                w as i32,
                h as i32,
                0,
                0,
                band.width as i32,
                band.height as i32,
                self.bgr.as_ptr().cast(),
                &raw const info,
                DIB_RGB_COLORS,
                SRCCOPY,
            )
        };
        // Zero rows of the sheet need no pixels; otherwise 0 is GDI's failure.
        if lines == 0 && h > 0 {
            return Err(self.failed("the printer took no pixels"));
        }
        Ok(())
    }

    fn end_page(&mut self) -> Result<(), String> {
        // SAFETY: `dc` is live, on a started page.
        if unsafe { EndPage(self.dc) } <= 0 {
            return Err(self.failed("the printer did not finish a page"));
        }
        Ok(())
    }

    fn finish(&mut self) -> Result<(), String> {
        // SAFETY: `dc` is live, in a started document.
        let done = unsafe { EndDoc(self.dc) };
        let error = (done <= 0).then(|| self.failed("the printer did not finish the job"));
        self.started = false;
        self.close();
        error.map_or(Ok(()), Err)
    }

    fn abort(&mut self) {
        if self.started {
            // SAFETY: `dc` is live, in a started document.
            unsafe {
                AbortDoc(self.dc);
            }
            self.started = false;
        }
        self.close();
    }
}

impl Drop for GdiSink {
    fn drop(&mut self) {
        self.abort();
    }
}
