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
    pub value: String,
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
    /// The text size in points the field's appearance uses; 0 means "fit the field".
    pub font_size: f32,
    /// The field's description for the user (`/TU`).
    pub tooltip: Option<String>,
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
            font_size: 0.0,
            tooltip: None,
        }
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
