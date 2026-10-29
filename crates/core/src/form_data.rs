// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 FINE Association <su@fa.org.tr>

//! Form data files (Plan 0006 Phase 3, §0 question 3): a form's values written to a file and read
//! back, as XFDF (the XML form of FDF, which Acrobat and most readers import) or as JSON (a plain
//! object of field names to values).
//!
//! A field's value is a list: one entry for a text field, a check box or a radio group (`Off`
//! when off), any number for a list. JSON writes one value as a string and others as an array;
//! XFDF writes one `<value>` per entry and nests dotted names (`a.b`) as fields inside fields, as
//! the format does.

use std::path::Path;

/// One field's value in a data file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldValue {
    /// The field's fully qualified name (`parent.child`).
    pub name: String,
    pub values: Vec<String>,
}

impl FieldValue {
    pub fn new(name: impl Into<String>, values: Vec<String>) -> Self {
        Self {
            name: name.into(),
            values,
        }
    }
}

/// The file formats.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DataFormat {
    Xfdf,
    Json,
}

impl DataFormat {
    /// The format a file name says: `.xfdf` (or `.xml`) or `.json`, in any case.
    pub fn from_path(path: &Path) -> Option<Self> {
        let ext = path.extension()?.to_str()?.to_ascii_lowercase();
        match ext.as_str() {
            "xfdf" | "xml" => Some(Self::Xfdf),
            "json" => Some(Self::Json),
            _ => None,
        }
    }
}

/// Why a data file could not be read.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum DataError {
    #[error("not well-formed XML: {0}")]
    Xml(String),
    #[error("not an XFDF file (no <xfdf> element)")]
    NotXfdf,
    #[error("not valid JSON at byte {at}: {what}")]
    Json { at: usize, what: &'static str },
    #[error("the JSON file is not an object of field names and values")]
    NotFormJson,
}

/// Writes the values in `format`; `pdf` is the form's file name, which XFDF records.
pub fn write(values: &[FieldValue], format: DataFormat, pdf: Option<&str>) -> String {
    match format {
        DataFormat::Xfdf => to_xfdf(values, pdf),
        DataFormat::Json => to_json(values),
    }
}

/// Reads values written in `format`.
pub fn read(text: &str, format: DataFormat) -> Result<Vec<FieldValue>, DataError> {
    match format {
        DataFormat::Xfdf => from_xfdf(text),
        DataFormat::Json => from_json(text),
    }
}

// --- XFDF -----------------------------------------------------------------------------------

fn xml_escape(s: &str, out: &mut String) {
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\r' => out.push_str("&#13;"),
            c => out.push(c),
        }
    }
}

/// A field in the XFDF tree: its own name part, its value when it is a field with one, and its
/// kids, in the order they first appear.
struct Node<'a> {
    part: &'a str,
    values: Option<&'a [String]>,
    kids: Vec<Node<'a>>,
}

fn insert<'a>(nodes: &mut Vec<Node<'a>>, parts: &[&'a str], values: &'a [String]) {
    let Some((first, rest)) = parts.split_first() else {
        return;
    };
    let i = match nodes.iter().position(|n| n.part == *first) {
        Some(i) => i,
        None => {
            nodes.push(Node {
                part: first,
                values: None,
                kids: Vec::new(),
            });
            nodes.len() - 1
        }
    };
    if rest.is_empty() {
        nodes[i].values = Some(values);
    } else {
        insert(&mut nodes[i].kids, rest, values);
    }
}

fn write_nodes(nodes: &[Node], depth: usize, out: &mut String) {
    for node in nodes {
        let pad = "  ".repeat(depth);
        out.push_str(&pad);
        out.push_str("<field name=\"");
        xml_escape(node.part, out);
        out.push_str("\">\n");
        for value in node.values.unwrap_or_default() {
            out.push_str(&pad);
            out.push_str("  <value>");
            xml_escape(value, out);
            out.push_str("</value>\n");
        }
        write_nodes(&node.kids, depth + 1, out);
        out.push_str(&pad);
        out.push_str("</field>\n");
    }
}

fn to_xfdf(values: &[FieldValue], pdf: Option<&str>) -> String {
    let mut roots = Vec::new();
    for v in values {
        let parts: Vec<&str> = v.name.split('.').collect();
        insert(&mut roots, &parts, &v.values);
    }
    let mut out = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <xfdf xmlns=\"http://ns.adobe.com/xfdf/\" xml:space=\"preserve\">\n",
    );
    if let Some(pdf) = pdf {
        out.push_str("  <f href=\"");
        xml_escape(pdf, &mut out);
        out.push_str("\"/>\n");
    }
    out.push_str("  <fields>\n");
    write_nodes(&roots, 2, &mut out);
    out.push_str("  </fields>\n</xfdf>\n");
    out
}

fn from_xfdf(text: &str) -> Result<Vec<FieldValue>, DataError> {
    let doc = roxmltree::Document::parse(text).map_err(|e| DataError::Xml(e.to_string()))?;
    let root = doc.root_element();
    if root.tag_name().name() != "xfdf" {
        return Err(DataError::NotXfdf);
    }
    let mut out = Vec::new();
    for fields in root.children().filter(|n| n.has_tag_name("fields")) {
        read_fields(fields, "", &mut out);
    }
    Ok(out)
}

fn read_fields(parent: roxmltree::Node, prefix: &str, out: &mut Vec<FieldValue>) {
    for field in parent.children().filter(|n| n.tag_name().name() == "field") {
        let Some(part) = field.attribute("name") else {
            continue;
        };
        let name = if prefix.is_empty() {
            part.to_owned()
        } else {
            format!("{prefix}.{part}")
        };
        let values: Vec<String> = field
            .children()
            .filter(|n| n.tag_name().name() == "value")
            .map(|v| v.text().unwrap_or_default().to_owned())
            .collect();
        if !values.is_empty() {
            out.push(FieldValue::new(name.clone(), values));
        }
        read_fields(field, &name, out);
    }
}

// --- JSON -----------------------------------------------------------------------------------

fn json_string(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

fn to_json(values: &[FieldValue]) -> String {
    let mut out = String::from("{\n");
    for (i, v) in values.iter().enumerate() {
        out.push_str("  ");
        json_string(&v.name, &mut out);
        out.push_str(": ");
        if let [one] = v.values.as_slice() {
            json_string(one, &mut out);
        } else {
            out.push('[');
            for (j, value) in v.values.iter().enumerate() {
                if j > 0 {
                    out.push_str(", ");
                }
                json_string(value, &mut out);
            }
            out.push(']');
        }
        out.push_str(if i + 1 < values.len() { ",\n" } else { "\n" });
    }
    out.push_str("}\n");
    out
}

/// A JSON value, as far as a form's data needs one.
enum Json {
    Text(String),
    List(Vec<Json>),
    Object(Vec<(String, Json)>),
    Null,
}

struct Parser<'a> {
    s: &'a [u8],
    at: usize,
}

impl Parser<'_> {
    fn fail<T>(&self, what: &'static str) -> Result<T, DataError> {
        Err(DataError::Json { at: self.at, what })
    }

    fn space(&mut self) {
        while self.at < self.s.len() && matches!(self.s[self.at], b' ' | b'\t' | b'\n' | b'\r') {
            self.at += 1;
        }
    }

    fn eat(&mut self, b: u8) -> bool {
        self.space();
        if self.s.get(self.at) == Some(&b) {
            self.at += 1;
            true
        } else {
            false
        }
    }

    fn value(&mut self, depth: usize) -> Result<Json, DataError> {
        if depth > 64 {
            return self.fail("nested too deeply");
        }
        self.space();
        match self.s.get(self.at) {
            Some(b'"') => Ok(Json::Text(self.string()?)),
            Some(b'[') => {
                self.at += 1;
                let mut items = Vec::new();
                if self.eat(b']') {
                    return Ok(Json::List(items));
                }
                loop {
                    items.push(self.value(depth + 1)?);
                    if self.eat(b']') {
                        return Ok(Json::List(items));
                    }
                    if !self.eat(b',') {
                        return self.fail("expected , or ]");
                    }
                }
            }
            Some(b'{') => {
                self.at += 1;
                let mut members = Vec::new();
                if self.eat(b'}') {
                    return Ok(Json::Object(members));
                }
                loop {
                    self.space();
                    if self.s.get(self.at) != Some(&b'"') {
                        return self.fail("expected a name");
                    }
                    let key = self.string()?;
                    if !self.eat(b':') {
                        return self.fail("expected :");
                    }
                    members.push((key, self.value(depth + 1)?));
                    if self.eat(b'}') {
                        return Ok(Json::Object(members));
                    }
                    if !self.eat(b',') {
                        return self.fail("expected , or }");
                    }
                }
            }
            Some(b't') => self.word("true", Json::Text("true".into())),
            Some(b'f') => self.word("false", Json::Text("false".into())),
            Some(b'n') => self.word("null", Json::Null),
            Some(b'-' | b'0'..=b'9') => {
                let start = self.at;
                while self.at < self.s.len()
                    && matches!(
                        self.s[self.at],
                        b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9'
                    )
                {
                    self.at += 1;
                }
                let text = std::str::from_utf8(&self.s[start..self.at]).unwrap_or_default();
                if text.parse::<f64>().is_err() {
                    self.at = start;
                    return self.fail("not a number");
                }
                Ok(Json::Text(text.to_owned()))
            }
            _ => self.fail("expected a value"),
        }
    }

    fn word(&mut self, word: &str, value: Json) -> Result<Json, DataError> {
        if self.s[self.at..].starts_with(word.as_bytes()) {
            self.at += word.len();
            Ok(value)
        } else {
            self.fail("expected a value")
        }
    }

    fn hex4(&mut self) -> Result<u32, DataError> {
        let digits = self.s.get(self.at..self.at + 4).unwrap_or_default();
        let text = std::str::from_utf8(digits).unwrap_or_default();
        match (text.len(), u32::from_str_radix(text, 16)) {
            (4, Ok(n)) => {
                self.at += 4;
                Ok(n)
            }
            _ => self.fail("bad \\u escape"),
        }
    }

    fn string(&mut self) -> Result<String, DataError> {
        self.at += 1; // the opening quote
        let mut out = String::new();
        loop {
            let start = self.at;
            while self.at < self.s.len() && !matches!(self.s[self.at], b'"' | b'\\') {
                if self.s[self.at] < 0x20 {
                    return self.fail("control character in a string");
                }
                self.at += 1;
            }
            match std::str::from_utf8(&self.s[start..self.at]) {
                Ok(text) => out.push_str(text),
                Err(_) => return self.fail("not UTF-8"),
            }
            match self.s.get(self.at) {
                Some(b'"') => {
                    self.at += 1;
                    return Ok(out);
                }
                Some(b'\\') => {
                    self.at += 1;
                    let Some(&c) = self.s.get(self.at) else {
                        return self.fail("unfinished string");
                    };
                    self.at += 1;
                    match c {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'b' => out.push('\u{8}'),
                        b'f' => out.push('\u{c}'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => {
                            let mut code = self.hex4()?;
                            // A surrogate pair: a high half, then `\u` and the low half.
                            if (0xD800..0xDC00).contains(&code) {
                                if !self.s[self.at..].starts_with(b"\\u") {
                                    return self.fail("lone surrogate");
                                }
                                self.at += 2;
                                let low = self.hex4()?;
                                if !(0xDC00..0xE000).contains(&low) {
                                    return self.fail("lone surrogate");
                                }
                                code = 0x10000 + ((code - 0xD800) << 10) + (low - 0xDC00);
                            }
                            match char::from_u32(code) {
                                Some(ch) => out.push(ch),
                                None => return self.fail("lone surrogate"),
                            }
                        }
                        _ => return self.fail("bad escape"),
                    }
                }
                _ => return self.fail("unfinished string"),
            }
        }
    }
}

fn from_json(text: &str) -> Result<Vec<FieldValue>, DataError> {
    let mut parser = Parser {
        s: text.strip_prefix('\u{feff}').unwrap_or(text).as_bytes(),
        at: 0,
    };
    let value = parser.value(0)?;
    parser.space();
    if parser.at != parser.s.len() {
        return parser.fail("text after the object");
    }
    let Json::Object(members) = value else {
        return Err(DataError::NotFormJson);
    };
    let mut out = Vec::new();
    for (name, value) in members {
        let values = match value {
            Json::Text(text) => vec![text],
            Json::List(items) => items
                .into_iter()
                .map(|item| match item {
                    Json::Text(text) => Ok(text),
                    _ => Err(DataError::NotFormJson),
                })
                .collect::<Result<_, _>>()?,
            Json::Null => continue,
            Json::Object(_) => return Err(DataError::NotFormJson),
        };
        out.push(FieldValue::new(name, values));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Vec<FieldValue> {
        vec![
            FieldValue::new("name", vec!["Öğüt \"Şükrü\" <İnce> & co".into()]),
            FieldValue::new("notes", vec!["Birinci satır\nİkinci satır".into()]),
            FieldValue::new("agree", vec!["Off".into()]),
            FieldValue::new("langs", vec!["tr".into(), "de".into()]),
            FieldValue::new("empty", vec![]),
            FieldValue::new("address.city", vec!["Muğla".into()]),
            FieldValue::new("address.zip", vec!["48000".into()]),
            FieldValue::new("blank", vec![String::new()]),
        ]
    }

    #[test]
    fn xfdf_round_trips_and_nests_dotted_names() {
        let text = write(&sample(), DataFormat::Xfdf, Some("form & co.pdf"));
        assert!(text.contains("<f href=\"form &amp; co.pdf\"/>"), "{text}");
        assert!(
            text.contains("<field name=\"address\">\n      <field name=\"city\">"),
            "{text}"
        );
        // A field with no value is not written back as one (it has nothing to set).
        let mut expected = sample();
        expected.retain(|v| v.name != "empty");
        assert_eq!(read(&text, DataFormat::Xfdf), Ok(expected));
    }

    #[test]
    fn json_round_trips() {
        let text = write(&sample(), DataFormat::Json, None);
        assert!(text.contains("\"langs\": [\"tr\", \"de\"]"), "{text}");
        assert!(text.contains("\"agree\": \"Off\""), "{text}");
        assert_eq!(read(&text, DataFormat::Json), Ok(sample()));
    }

    #[test]
    fn json_from_elsewhere_is_read_and_bad_json_is_refused() {
        let text = "\u{feff} { \"a\" : 12.5, \"b\": true, \"c\": null, \"d\": \"\\u011f\\ud83d\\ude00\\/\" }";
        assert_eq!(
            read(text, DataFormat::Json),
            Ok(vec![
                FieldValue::new("a", vec!["12.5".into()]),
                FieldValue::new("b", vec!["true".into()]),
                FieldValue::new("d", vec!["ğ😀/".into()]),
            ])
        );
        assert!(matches!(
            read("{\"a\": \"x\"", DataFormat::Json),
            Err(DataError::Json { .. })
        ));
        assert!(matches!(
            read("{\"a\": \"\\ud800\"}", DataFormat::Json),
            Err(DataError::Json { .. })
        ));
        assert_eq!(read("[1]", DataFormat::Json), Err(DataError::NotFormJson));
        assert_eq!(
            read("{\"a\": {\"b\": \"c\"}}", DataFormat::Json),
            Err(DataError::NotFormJson)
        );
    }

    #[test]
    fn xfdf_from_acrobat_is_read() {
        // Acrobat's shape: a namespace, an empty value, a nested field, rich text left out.
        let text = r#"<?xml version="1.0" encoding="UTF-8"?>
<xfdf xmlns="http://ns.adobe.com/xfdf/" xml:space="preserve">
<f href="form.pdf"/>
<fields>
<field name="Company Name 1"><value>FINE</value></field>
<field name="2a. VIN"><value/></field>
<field name="p"><field name="q"><value>1</value></field></field>
</fields>
<ids original="ABC" modified="DEF"/>
</xfdf>"#;
        assert_eq!(
            read(text, DataFormat::Xfdf),
            Ok(vec![
                FieldValue::new("Company Name 1", vec!["FINE".into()]),
                FieldValue::new("2a. VIN", vec![String::new()]),
                FieldValue::new("p.q", vec!["1".into()]),
            ])
        );
        assert_eq!(read("<html/>", DataFormat::Xfdf), Err(DataError::NotXfdf));
        assert!(matches!(
            read("<xfdf>", DataFormat::Xfdf),
            Err(DataError::Xml(_))
        ));
    }

    #[test]
    fn the_format_follows_the_extension() {
        assert_eq!(
            DataFormat::from_path(Path::new("a.XFDF")),
            Some(DataFormat::Xfdf)
        );
        assert_eq!(
            DataFormat::from_path(Path::new("a.json")),
            Some(DataFormat::Json)
        );
        assert_eq!(DataFormat::from_path(Path::new("a.pdf")), None);
    }
}
