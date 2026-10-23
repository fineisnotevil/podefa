// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 FINE Association <su@fa.org.tr>

//! A generated AcroForm for the form tests and `forms_bench` (Plan 0006). Page 1 has every field
//! type: a text field, a multi-line one, a check box, a radio group of two, a combo box with
//! Turkish options, a multi-select list, two numbers and a read-only total with a calculation
//! script, an unsigned signature field and the first widget of "shared"; page 2 has the second
//! widget of "shared" (a field on two pages).

#![allow(dead_code)]

use std::fmt::Write as _;

/// A PDF text string as UTF-16BE hex, for names and options with Turkish letters.
pub fn utf16(s: &str) -> String {
    let mut out = String::from("<FEFF");
    for unit in s.encode_utf16() {
        let _ = write!(out, "{unit:04X}");
    }
    out.push('>');
    out
}

/// A PDF hex string of bytes, for scripts with characters a literal string would need escaped.
fn hex(s: &str) -> String {
    let mut out = String::from("<");
    for b in s.bytes() {
        let _ = write!(out, "{b:02X}");
    }
    out.push('>');
    out
}

pub fn form_pdf() -> Vec<u8> {
    let ap = |body: &str| {
        format!(
            "<< /Type /XObject /Subtype /Form /BBox [0 0 14 14] /Length {} >>\nstream\n{body}\nendstream",
            body.len()
        )
    };
    let tick = "0 g 2 7 m 6 3 l 12 12 l S";
    let dot = "0 g 7 7 4 0 360 re f";
    let objects: Vec<String> = vec![
        // 1 catalog, 2 pages, 3 form, 4 page.
        "<< /Type /Catalog /Pages 2 0 R /AcroForm 3 0 R >>".into(),
        "<< /Type /Pages /Kids [4 0 R 23 0 R] /Count 2 >>".into(),
        "<< /Fields [5 0 R 6 0 R 7 0 R 19 0 R 10 0 R 11 0 R 12 0 R 13 0 R 14 0 R 20 0 R 24 0 R] \
         /DR << /Font << /Helv 15 0 R >> >> /DA (/Helv 0 Tf 0 g) /CO [14 0 R] >>"
            .into(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 16 0 R \
         /Resources << /Font << /Helv 15 0 R >> >> \
         /Annots [5 0 R 6 0 R 7 0 R 8 0 R 9 0 R 10 0 R 11 0 R 12 0 R 13 0 R 14 0 R 20 0 R \
         25 0 R] >>"
            .into(),
        // 5 name, 6 notes (multi-line), 7 agree (check box).
        "<< /Type /Annot /Subtype /Widget /FT /Tx /T (name) /Rect [150 700 400 720] /P 4 0 R /F 4 \
         /DA (/Helv 11 Tf 0 g) >>"
            .into(),
        "<< /Type /Annot /Subtype /Widget /FT /Tx /T (notes) /Ff 4096 /Rect [150 600 400 690] \
         /P 4 0 R /F 4 /DA (/Helv 10 Tf 0 g) >>"
            .into(),
        "<< /Type /Annot /Subtype /Widget /FT /Btn /T (agree) /Rect [150 570 164 584] /P 4 0 R /F 4 \
         /V /Off /AS /Off /AP << /N << /Yes 17 0 R /Off 18 0 R >> >> /MK << /CA (4) >> \
         /DA (/ZaDb 0 Tf 0 g) >>"
            .into(),
        // 8, 9: the radio group's two buttons, children of 19.
        "<< /Type /Annot /Subtype /Widget /Parent 19 0 R /Rect [150 540 164 554] /P 4 0 R /F 4 \
         /AS /Off /AP << /N << /a 21 0 R /Off 18 0 R >> >> >>"
            .into(),
        "<< /Type /Annot /Subtype /Widget /Parent 19 0 R /Rect [200 540 214 554] /P 4 0 R /F 4 \
         /AS /Off /AP << /N << /b 22 0 R /Off 18 0 R >> >> >>"
            .into(),
        // 10 city (combo box, Turkish options), 11 langs (multi-select list).
        format!(
            "<< /Type /Annot /Subtype /Widget /FT /Ch /T (city) /Ff 131072 /Rect [150 500 400 520] \
             /P 4 0 R /F 4 /DA (/Helv 11 Tf 0 g) /Opt [{} {} {}] >>",
            utf16("İstanbul"),
            utf16("Ankara"),
            utf16("Muğla")
        ),
        "<< /Type /Annot /Subtype /Widget /FT /Ch /T (langs) /Ff 2097152 /Rect [150 420 400 480] \
         /P 4 0 R /F 4 /DA (/Helv 10 Tf 0 g) /Opt [(tr) (en) (de)] >>"
            .into(),
        // 12 a, 13 b, 14 total = a + b in JavaScript.
        "<< /Type /Annot /Subtype /Widget /FT /Tx /T (a) /Rect [150 380 250 400] /P 4 0 R /F 4 \
         /DA (/Helv 11 Tf 0 g) >>"
            .into(),
        "<< /Type /Annot /Subtype /Widget /FT /Tx /T (b) /Rect [260 380 360 400] /P 4 0 R /F 4 \
         /DA (/Helv 11 Tf 0 g) >>"
            .into(),
        format!(
            "<< /Type /Annot /Subtype /Widget /FT /Tx /T (total) /Ff 1 /Rect [370 380 470 400] \
             /P 4 0 R /F 4 /DA (/Helv 11 Tf 0 g) /AA << /C << /S /JavaScript /JS {} >> >> >>",
            hex(&std::env::var("FA_PDF_FORM_JS").unwrap_or_else(|_| {
                "AFSimple_Calculate(\"SUM\", new Array(\"a\", \"b\"));".to_owned()
            }))
        ),
        // 15 font, 16 page content.
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>".into(),
        {
            let c = "BT /Helv 11 Tf 54 705 Td (Name) Tj 0 -95 Td (Notes) Tj 0 -30 Td (Agree) Tj \
                     0 -30 Td (Choice) Tj 0 -40 Td (City) Tj 0 -60 Td (Languages) Tj \
                     0 -40 Td (a + b) Tj 0 -60 Td (Signature) Tj ET";
            format!("<< /Length {} >>\nstream\n{c}\nendstream", c.len())
        },
        // 17, 18: check box on and off; 19 the radio group; 20 a signature field.
        ap(tick),
        ap(""),
        "<< /FT /Btn /T (choice) /Ff 49152 /Kids [8 0 R 9 0 R] /V /Off >>".into(),
        "<< /Type /Annot /Subtype /Widget /FT /Sig /T (signature) /Rect [150 260 400 300] \
         /P 4 0 R /F 4 >>"
            .into(),
        // 21, 22: radio buttons on.
        ap(dot),
        ap(dot),
        // 23 a second page; 24 the field "shared" with a widget on each page (25, 26).
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Annots [26 0 R] >>".into(),
        "<< /FT /Tx /T (shared) /DA (/Helv 11 Tf 0 g) /TU (Your city) /MaxLen 20 \
         /Kids [25 0 R 26 0 R] >>"
            .into(),
        "<< /Type /Annot /Subtype /Widget /Parent 24 0 R /Rect [150 220 400 240] /P 4 0 R /F 4 >>"
            .into(),
        "<< /Type /Annot /Subtype /Widget /Parent 24 0 R /Rect [150 700 400 720] /P 23 0 R /F 4 >>"
            .into(),
    ];
    let mut out = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();
    for (i, body) in objects.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n{body}\nendobj\n", i + 1).as_bytes());
    }
    let xref = out.len();
    let mut table = format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1);
    for offset in offsets {
        let _ = writeln!(table, "{offset:010} 00000 n ");
    }
    let _ = write!(
        table,
        "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
        objects.len() + 1
    );
    out.extend_from_slice(table.as_bytes());
    out
}
