// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 FINE Association <su@fa.org.tr>

//! Form fields (Plan 0006): what the engine reports of a page's AcroForm widgets, and the pure
//! logic of filling them - which widget a click lands on, what a click on a check box or a radio
//! button sets, which value a choice field shows.
//!
//! A field is known by its fully qualified name (`parent.child`); one field can have several
//! widgets, on one page (a radio group) or on several (the same field printed twice). Each
//! [`FieldInfo`] describes one widget, with the field's value and properties.

use crate::Rect;

/// What kind of field a widget belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldKind {
    Text,
    CheckBox,
    Radio,
    /// A drop-down list; `editable` choices also take typed text.
    ComboBox,
    ListBox,
    PushButton,
    Signature,
    Unknown,
}

/// One entry of a choice field: the value stored in the file and the text shown for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChoiceOption {
    pub export: String,
    pub label: String,
}

/// One widget of a field, as the engine reports it for a page.
#[derive(Debug, Clone, PartialEq)]
pub struct FieldInfo {
    /// The field's fully qualified name; the same for every widget of the field.
    pub name: String,
    pub kind: FieldKind,
    /// The field's value: text, a choice's export value, or a button's state (`Off` when off).
    /// For a list with several choices, the first of them.
    pub value: String,
    /// A choice field's chosen export values, in the file's order: one for a combo box or a
    /// single-choice list, any number for a multi-select list (Plan 0006 Phase 2).
    pub values: Vec<String>,
    /// The widget's rectangle in page space (points from the page's top left).
    pub rect: Rect,
    /// A check box's or radio button's "on" state for this widget (`Yes`, `1`, a button's own
    /// name...); `None` for other kinds.
    pub on_state: Option<String>,
    /// A choice field's entries, in order.
    pub options: Vec<ChoiceOption>,
    pub read_only: bool,
    pub required: bool,
    pub multiline: bool,
    pub password: bool,
    /// A combo box that also takes typed text.
    pub editable: bool,
    pub multi_select: bool,
    /// The most characters a text field takes.
    pub max_len: Option<u32>,
    /// A text field drawn as a row of boxes, one character in each (`Comb`, with `max_len` boxes).
    pub comb: bool,
    /// The text size in points the field's appearance uses; 0 means "fit the field".
    pub font_size: f32,
    /// The field's description for the user (`/TU`).
    pub tooltip: Option<String>,
    /// The widget's place in its page's tab order (`/Tabs`), from 0.
    pub tab: u32,
}

impl FieldInfo {
    /// Whether a click can change the field.
    pub fn fillable(&self) -> bool {
        !self.read_only
            && matches!(
                self.kind,
                FieldKind::Text
                    | FieldKind::CheckBox
                    | FieldKind::Radio
                    | FieldKind::ComboBox
                    | FieldKind::ListBox
            )
    }

    /// Whether this check box or radio button is on.
    pub fn is_on(&self) -> bool {
        self.on_state.as_deref().is_some_and(|on| on == self.value)
    }

    /// The value a click on this widget sets, for a check box or a radio button: a check box
    /// toggles; a radio button turns on (a click on the one that is on leaves it on, as readers
    /// do with radio groups that may not be all off).
    pub fn clicked_value(&self) -> Option<String> {
        let on = self.on_state.clone()?;
        match self.kind {
            FieldKind::CheckBox if self.is_on() => Some(OFF.to_owned()),
            FieldKind::CheckBox | FieldKind::Radio => Some(on),
            _ => None,
        }
    }

    /// The number of boxes of a comb field: one character in each, the field's width split evenly.
    /// `None` for any other field; PDF ignores the flag on multi-line and password fields and
    /// without a maximum length (ISO 32000-2, 12.7.5.3).
    pub fn comb_cells(&self) -> Option<u32> {
        self.max_len.filter(|&n| {
            n > 0 && self.comb && self.kind == FieldKind::Text && !self.multiline && !self.password
        })
    }

    /// The text a choice field shows for its value: the option's label, or the value itself.
    pub fn shown_value(&self) -> &str {
        self.options
            .iter()
            .find(|o| o.export == self.value)
            .map_or(self.value.as_str(), |o| o.label.as_str())
    }
}

/// What the document's form has, reported when it opens (Plan 0006, §0 question 2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FormSummary {
    /// The document has an AcroForm with fields.
    pub fields: bool,
    /// Its fields or the document carry JavaScript, which v0.4 does not run.
    pub scripts: bool,
    /// The form is (also) an XFA form, which MuPDF does not read.
    pub xfa: bool,
}

/// The state of a check box or radio button that is off.
pub const OFF: &str = "Off";

/// The order Tab visits a page's widgets in, from the page's `/Tabs` (ISO 32000-2, 12.5.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TabOrder {
    /// The order of the page's annotations (no `/Tabs`, or `/S`: structure order, which falls back
    /// to it here as a tagged document's structure is not read).
    #[default]
    Annotations,
    /// Row by row, top to bottom; left to right in a row (`/R`).
    Rows,
    /// Column by column, left to right; top to bottom in a column (`/C`).
    Columns,
}

/// Each widget's place in the tab order, given the widgets' rectangles in annotation order.
/// Widgets whose rectangles overlap by at least half the shorter one's height are on one row (for
/// columns: width), so fields drawn a fraction of a point apart stay in reading order.
pub fn tab_places(rects: &[Rect], order: TabOrder) -> Vec<u32> {
    let mut seq: Vec<usize> = (0..rects.len()).collect();
    // (across, along): rows are found down the page and read across it; columns the other way.
    let key = |r: &Rect| match order {
        TabOrder::Rows => (r.y, r.height, r.x),
        TabOrder::Columns => (r.x, r.width, r.y),
        TabOrder::Annotations => (0.0, 0.0, 0.0),
    };
    if order != TabOrder::Annotations {
        seq.sort_by(|&a, &b| key(&rects[a]).0.total_cmp(&key(&rects[b]).0));
        // Group into bands, then read each band along.
        let mut bands: Vec<Vec<usize>> = Vec::new();
        let mut band_end = f32::NEG_INFINITY;
        for i in seq {
            let (start, size, _) = key(&rects[i]);
            match bands.last_mut() {
                Some(band) if band_end - start >= size / 2.0 => {
                    band.push(i);
                    band_end = band_end.max(start + size);
                }
                _ => {
                    bands.push(vec![i]);
                    band_end = start + size;
                }
            }
        }
        seq = bands
            .into_iter()
            .flat_map(|mut band| {
                band.sort_by(|&a, &b| key(&rects[a]).2.total_cmp(&key(&rects[b]).2));
                band
            })
            .collect();
    }
    let mut places = vec![0; rects.len()];
    for (place, i) in seq.into_iter().enumerate() {
        places[i] = place as u32;
    }
    places
}

/// The fillable widgets of a page in tab order, as indices into `fields`.
pub fn tab_sequence(fields: &[FieldInfo]) -> Vec<usize> {
    let mut seq: Vec<usize> = (0..fields.len())
        .filter(|&i| fields[i].fillable())
        .collect();
    seq.sort_by_key(|&i| fields[i].tab);
    seq
}

/// The widget under a page point, the topmost (last drawn) first.
pub fn hit(fields: &[FieldInfo], x: f32, y: f32) -> Option<usize> {
    fields.iter().rposition(|f| {
        x >= f.rect.x
            && x <= f.rect.x + f.rect.width
            && y >= f.rect.y
            && y <= f.rect.y + f.rect.height
    })
}

/// The text size the editor over a text field uses: the field's own, or for "fit the field" (0) a
/// size from its height, one line of it for a single-line field.
pub fn editor_font_size(field: &FieldInfo) -> f32 {
    if field.font_size > 0.0 {
        return field.font_size;
    }
    if field.multiline {
        12.0
    } else {
        (field.rect.height * 0.7).clamp(6.0, 24.0)
    }
}

/// A text value cut to the field's maximum length, counted in characters.
pub fn clip_to_max_len(value: &str, max_len: Option<u32>) -> String {
    match max_len {
        Some(n) if n > 0 => value.chars().take(n as usize).collect(),
        _ => value.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn field(kind: FieldKind, value: &str, on: Option<&str>) -> FieldInfo {
        FieldInfo {
            name: "f".into(),
            kind,
            value: value.into(),
            rect: Rect::new(10.0, 20.0, 100.0, 20.0),
            on_state: on.map(str::to_owned),
            options: Vec::new(),
            read_only: false,
            required: false,
            multiline: false,
            password: false,
            editable: false,
            multi_select: false,
            max_len: None,
            comb: false,
            font_size: 0.0,
            tooltip: None,
            values: Vec::new(),
            tab: 0,
        }
    }

    #[test]
    fn rows_read_across_and_tolerate_a_fraction_of_a_point() {
        // Annotation order: lower right, upper right, upper left (0.2 pt lower), lower left.
        let rects = [
            Rect::new(300.0, 100.0, 50.0, 16.0),
            Rect::new(300.0, 20.0, 50.0, 16.0),
            Rect::new(10.0, 20.2, 50.0, 16.0),
            Rect::new(10.0, 100.0, 50.0, 16.0),
        ];
        assert_eq!(tab_places(&rects, TabOrder::Annotations), [0, 1, 2, 3]);
        // Rows: upper left, upper right, lower left, lower right.
        assert_eq!(tab_places(&rects, TabOrder::Rows), [3, 1, 0, 2]);
        // Columns: upper left, lower left, upper right, lower right.
        assert_eq!(tab_places(&rects, TabOrder::Columns), [3, 2, 0, 1]);
    }

    #[test]
    fn the_tab_sequence_skips_what_cannot_be_filled() {
        let mut fields: Vec<FieldInfo> = (0..4)
            .map(|i| {
                let mut f = field(FieldKind::Text, "", None);
                f.tab = 3 - i;
                f
            })
            .collect();
        fields[1].read_only = true;
        fields[2].kind = FieldKind::Signature;
        assert_eq!(tab_sequence(&fields), [3, 0]);
    }

    #[test]
    fn a_comb_field_has_one_box_per_character_it_takes() {
        let mut vin = field(FieldKind::Text, "", None);
        vin.comb = true;
        assert_eq!(vin.comb_cells(), None, "no maximum length, no boxes");
        vin.max_len = Some(17);
        assert_eq!(vin.comb_cells(), Some(17));
        vin.multiline = true;
        assert_eq!(vin.comb_cells(), None);
        vin.multiline = false;
        vin.comb = false;
        assert_eq!(vin.comb_cells(), None);
    }

    #[test]
    fn a_check_box_toggles_and_a_radio_button_turns_on() {
        let off = field(FieldKind::CheckBox, "Off", Some("Yes"));
        assert_eq!(off.clicked_value().as_deref(), Some("Yes"));
        let on = field(FieldKind::CheckBox, "Yes", Some("Yes"));
        assert!(on.is_on());
        assert_eq!(on.clicked_value().as_deref(), Some("Off"));
        let radio = field(FieldKind::Radio, "a", Some("b"));
        assert_eq!(radio.clicked_value().as_deref(), Some("b"));
        let chosen = field(FieldKind::Radio, "b", Some("b"));
        assert_eq!(chosen.clicked_value().as_deref(), Some("b"));
        assert_eq!(field(FieldKind::Text, "", None).clicked_value(), None);
    }

    #[test]
    fn the_topmost_widget_under_the_point_is_hit() {
        let mut a = field(FieldKind::Text, "", None);
        a.name = "a".into();
        let mut b = field(FieldKind::Text, "", None);
        b.name = "b".into();
        b.rect = Rect::new(50.0, 25.0, 100.0, 20.0);
        let fields = [a, b];
        assert_eq!(hit(&fields, 60.0, 30.0), Some(1));
        assert_eq!(hit(&fields, 20.0, 30.0), Some(0));
        assert_eq!(hit(&fields, 5.0, 30.0), None);
    }

    #[test]
    fn values_and_sizes_for_the_editor() {
        let mut combo = field(FieldKind::ComboBox, "MU", None);
        combo.options = vec![
            ChoiceOption {
                export: "IST".into(),
                label: "İstanbul".into(),
            },
            ChoiceOption {
                export: "MU".into(),
                label: "Muğla".into(),
            },
        ];
        assert_eq!(combo.shown_value(), "Muğla");
        combo.value = "other".into();
        assert_eq!(combo.shown_value(), "other");
        assert_eq!(clip_to_max_len("Öğüt Su", Some(4)), "Öğüt");
        assert_eq!(clip_to_max_len("Öğüt", None), "Öğüt");
        let text = field(FieldKind::Text, "", None);
        assert_eq!(editor_font_size(&text), 14.0);
        let mut ro = field(FieldKind::Text, "", None);
        ro.read_only = true;
        assert!(!ro.fillable() && text.fillable());
        assert!(!field(FieldKind::Signature, "", None).fillable());
    }
}
