// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 FINE Association <su@fa.org.tr>

//! Reading a page's content stream as operations, for what a redaction leaves behind (Plan 0009
//! §3.6 and §4.1).
//!
//! MuPDF removes the glyphs, images and drawing under a mark, but it keeps a marked sequence's
//! inline `/ActualText`, `/Alt` and `/E` (the text a reader shows or reads aloud in place of the
//! glyphs) even when it removed every glyph of the sequence, and it keeps every resource the page
//! had, drawn or not. Here a content stream is split into operations, each marked sequence's
//! content is compared with the stream before the redaction, and the replacement text of a
//! sequence that changed is cut out ([`strip_replacement_text`]); [`resource_names`] says which
//! resources a stream still names.
//!
//! Pure byte work: the engine passes the streams and answers which object a name stands for.

use std::collections::{BTreeSet, HashMap};

/// Answers the object number a resource name stands for, given the operator naming it (`Do` or
/// `sh`) and the name.
pub type Resolve<'a> = dyn Fn(&[u8], &[u8]) -> Option<i64> + 'a;

/// The keys of a marked sequence's properties that hold text in place of its content (PDF 2.0
/// §14.9.3-14.9.5).
pub const REPLACEMENT_KEYS: [&[u8]; 3] = [b"ActualText", b"Alt", b"E"];

/// One operand, decoded.
#[derive(Debug, Clone, PartialEq)]
pub enum Operand {
    Number(f64),
    /// A name, without its slash and with `#xx` escapes decoded.
    Name(Vec<u8>),
    /// A literal or hexadecimal string, decoded.
    Str(Vec<u8>),
    Array(Vec<Operand>),
    Dict(Vec<DictEntry>),
    Bool(bool),
    Null,
    /// A keyword where an operand stands (inside an array or a dictionary).
    Keyword(Vec<u8>),
}

/// One entry of a dictionary operand, with the bytes it takes in the stream (key to value).
#[derive(Debug, Clone, PartialEq)]
pub struct DictEntry {
    pub key: Vec<u8>,
    pub value: Operand,
    pub start: usize,
    pub end: usize,
}

/// One operation: its operator, its operands, and the bytes they take (first operand to operator).
#[derive(Debug, Clone, PartialEq)]
pub struct Op {
    pub operator: Vec<u8>,
    pub operands: Vec<Operand>,
    /// The bytes of each operand.
    pub spans: Vec<(usize, usize)>,
    pub start: usize,
    pub end: usize,
    /// For an inline image (`BI`), the bytes of its data, between `ID` and `EI`.
    pub image: Option<(usize, usize)>,
}

fn is_white(b: u8) -> bool {
    matches!(b, 0 | b'\t' | b'\n' | 0x0c | b'\r' | b' ')
}

fn is_delimiter(b: u8) -> bool {
    matches!(
        b,
        b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%'
    )
}

fn is_regular(b: u8) -> bool {
    !is_white(b) && !is_delimiter(b)
}

fn hex(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// A lexical token and where it ends.
#[derive(Debug, Clone, PartialEq)]
enum Token {
    Operand(Operand),
    ArrayOpen,
    ArrayClose,
    DictOpen,
    DictClose,
    Keyword(Vec<u8>),
}

struct Lexer<'a> {
    src: &'a [u8],
    pos: usize,
}

impl<'a> Lexer<'a> {
    fn skip_space(&mut self) {
        while let Some(&b) = self.src.get(self.pos) {
            if is_white(b) {
                self.pos += 1;
            } else if b == b'%' {
                while self
                    .src
                    .get(self.pos)
                    .is_some_and(|&b| b != b'\n' && b != b'\r')
                {
                    self.pos += 1;
                }
            } else {
                break;
            }
        }
    }

    /// The next token and its start, or `None` at the end.
    fn next(&mut self) -> Option<(usize, Token)> {
        self.skip_space();
        let start = self.pos;
        let b = *self.src.get(self.pos)?;
        let token = match b {
            b'[' => {
                self.pos += 1;
                Token::ArrayOpen
            }
            b']' => {
                self.pos += 1;
                Token::ArrayClose
            }
            b'<' if self.src.get(self.pos + 1) == Some(&b'<') => {
                self.pos += 2;
                Token::DictOpen
            }
            b'>' if self.src.get(self.pos + 1) == Some(&b'>') => {
                self.pos += 2;
                Token::DictClose
            }
            b'<' => Token::Operand(Operand::Str(self.hex_string())),
            b'(' => Token::Operand(Operand::Str(self.literal_string())),
            b'/' => Token::Operand(Operand::Name(self.name())),
            // A lone `)`, `>`, `{` or `}`: taken as a one-byte keyword, so the lexer always moves.
            b')' | b'>' | b'{' | b'}' => {
                self.pos += 1;
                Token::Keyword(vec![b])
            }
            _ => {
                while self.src.get(self.pos).is_some_and(|&b| is_regular(b)) {
                    self.pos += 1;
                }
                let word = &self.src[start..self.pos];
                match word {
                    b"true" => Token::Operand(Operand::Bool(true)),
                    b"false" => Token::Operand(Operand::Bool(false)),
                    b"null" => Token::Operand(Operand::Null),
                    _ => match number(word) {
                        Some(n) => Token::Operand(Operand::Number(n)),
                        None => Token::Keyword(word.to_vec()),
                    },
                }
            }
        };
        Some((start, token))
    }

    fn name(&mut self) -> Vec<u8> {
        self.pos += 1;
        let mut out = Vec::new();
        while let Some(&b) = self.src.get(self.pos) {
            if !is_regular(b) {
                break;
            }
            if b == b'#'
                && let (Some(h), Some(l)) = (
                    self.src.get(self.pos + 1).copied().and_then(hex),
                    self.src.get(self.pos + 2).copied().and_then(hex),
                )
            {
                out.push(h << 4 | l);
                self.pos += 3;
                continue;
            }
            out.push(b);
            self.pos += 1;
        }
        out
    }

    fn hex_string(&mut self) -> Vec<u8> {
        self.pos += 1;
        let mut out = Vec::new();
        let mut high: Option<u8> = None;
        while let Some(&b) = self.src.get(self.pos) {
            self.pos += 1;
            if b == b'>' {
                break;
            }
            if let Some(v) = hex(b) {
                match high.take() {
                    Some(h) => out.push(h << 4 | v),
                    None => high = Some(v),
                }
            }
        }
        if let Some(h) = high {
            out.push(h << 4);
        }
        out
    }

    fn literal_string(&mut self) -> Vec<u8> {
        self.pos += 1;
        let mut out = Vec::new();
        let mut depth = 1;
        while let Some(&b) = self.src.get(self.pos) {
            self.pos += 1;
            match b {
                b'(' => {
                    depth += 1;
                    out.push(b);
                }
                b')' => {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                    out.push(b);
                }
                b'\\' => {
                    let Some(&e) = self.src.get(self.pos) else {
                        break;
                    };
                    self.pos += 1;
                    match e {
                        b'n' => out.push(b'\n'),
                        b'r' => out.push(b'\r'),
                        b't' => out.push(b'\t'),
                        b'b' => out.push(0x08),
                        b'f' => out.push(0x0c),
                        b'0'..=b'7' => {
                            let mut v = u32::from(e - b'0');
                            for _ in 0..2 {
                                match self.src.get(self.pos) {
                                    Some(&d @ b'0'..=b'7') => {
                                        v = v * 8 + u32::from(d - b'0');
                                        self.pos += 1;
                                    }
                                    _ => break,
                                }
                            }
                            out.push((v & 0xff) as u8);
                        }
                        // A backslash before a line end continues the string on the next line.
                        b'\r' => {
                            if self.src.get(self.pos) == Some(&b'\n') {
                                self.pos += 1;
                            }
                        }
                        b'\n' => {}
                        other => out.push(other),
                    }
                }
                _ => out.push(b),
            }
        }
        out
    }

    /// The data of an inline image, after its `ID`: from the byte after the one white-space byte to
    /// the white space before an `EI` that is followed by white space or the end.
    fn inline_image(&mut self) -> (usize, usize) {
        let start = (self.pos + 1).min(self.src.len());
        let mut i = start;
        while i + 1 < self.src.len() {
            if self.src[i] == b'E'
                && self.src[i + 1] == b'I'
                && i > start
                && is_white(self.src[i - 1])
                && self.src.get(i + 2).is_none_or(|&b| is_white(b))
            {
                self.pos = i + 2;
                return (start, i - 1);
            }
            i += 1;
        }
        self.pos = self.src.len();
        (start, self.src.len())
    }
}

fn number(word: &[u8]) -> Option<f64> {
    let digits = word.strip_prefix(b"+").or_else(|| word.strip_prefix(b"-"));
    let body = digits.unwrap_or(word);
    if body.is_empty() || !body.iter().all(|b| b.is_ascii_digit() || *b == b'.') {
        return None;
    }
    if body.iter().filter(|&&b| b == b'.').count() > 1 || body == b"." {
        return None;
    }
    let text = std::str::from_utf8(word).ok()?;
    // `5.` and `.5` are numbers in PDF; Rust parses both.
    text.parse::<f64>().ok()
}

/// Reads one operand that starts with `first`, nesting into arrays and dictionaries; returns it and
/// where it ends.
fn operand(lexer: &mut Lexer, first: Token) -> Operand {
    match first {
        Token::Operand(operand) => operand,
        Token::Keyword(word) => Operand::Keyword(word),
        Token::ArrayOpen => {
            let mut items = Vec::new();
            while let Some((_, token)) = lexer.next() {
                if token == Token::ArrayClose {
                    break;
                }
                items.push(operand(lexer, token));
            }
            Operand::Array(items)
        }
        Token::DictOpen => Operand::Dict(dict(lexer)),
        // A stray closer: nothing.
        Token::ArrayClose | Token::DictClose => Operand::Null,
    }
}

fn dict(lexer: &mut Lexer) -> Vec<DictEntry> {
    let mut entries = Vec::new();
    while let Some((start, token)) = lexer.next() {
        let key = match token {
            Token::DictClose => break,
            Token::Operand(Operand::Name(key)) => key,
            // A key that is not a name: read past its value and drop it.
            other => {
                let _ = operand(lexer, other);
                continue;
            }
        };
        let value = match lexer.next() {
            Some((_, Token::DictClose)) | None => break,
            Some((_, token)) => operand(lexer, token),
        };
        entries.push(DictEntry {
            key,
            value,
            start,
            end: lexer.pos,
        });
    }
    entries
}

/// Splits a content stream into operations. Never fails: bytes it cannot read are skipped as
/// keywords, as a PDF reader would.
pub fn parse(src: &[u8]) -> Vec<Op> {
    let mut lexer = Lexer { src, pos: 0 };
    let mut ops = Vec::new();
    let mut operands = Vec::new();
    let mut spans = Vec::new();
    while let Some((start, token)) = lexer.next() {
        match token {
            Token::Keyword(word) if word == b"BI" => {
                // An inline image: its parameters up to `ID`, then its data up to `EI`.
                let mut params = Vec::new();
                while let Some((at, token)) = lexer.next() {
                    match token {
                        Token::Keyword(word) if word == b"ID" => break,
                        Token::Operand(Operand::Name(key)) => {
                            let value = match lexer.next() {
                                Some((_, token)) => operand(&mut lexer, token),
                                None => Operand::Null,
                            };
                            params.push(DictEntry {
                                key,
                                value,
                                start: at,
                                end: lexer.pos,
                            });
                        }
                        other => {
                            let _ = operand(&mut lexer, other);
                        }
                    }
                }
                let image = lexer.inline_image();
                let begin = spans.first().map_or(start, |&(s, _)| s);
                ops.push(Op {
                    operator: b"BI".to_vec(),
                    operands: vec![Operand::Dict(params)],
                    spans: vec![(start, image.0)],
                    start: begin,
                    end: lexer.pos,
                    image: Some(image),
                });
                operands.clear();
                spans.clear();
            }
            Token::Keyword(word) => {
                let begin = spans.first().map_or(start, |&(s, _)| s);
                ops.push(Op {
                    operator: word,
                    operands: std::mem::take(&mut operands),
                    spans: std::mem::take(&mut spans),
                    start: begin,
                    end: lexer.pos,
                    image: None,
                });
            }
            token => {
                let value = operand(&mut lexer, token);
                operands.push(value);
                spans.push((start, lexer.pos));
            }
        }
    }
    ops
}

/// What an operation shows or draws, for telling whether a marked sequence changed.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Drawn {
    /// An XObject (`Do`) or shading (`sh`) by the object its name stands for, or by its name when
    /// the engine cannot say.
    Object(Vec<u8>, i64),
    Name(Vec<u8>, Vec<u8>),
    /// An inline image by its bytes.
    Inline(Vec<u8>),
}

/// The text a sequence shows (every string of its text operators, joined) and what it draws.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct Fingerprint {
    text: Vec<u8>,
    drawn: Vec<Drawn>,
}

fn strings(operand: &Operand, out: &mut Vec<u8>) {
    match operand {
        Operand::Str(s) => out.extend_from_slice(s),
        Operand::Array(items) => items.iter().for_each(|item| strings(item, out)),
        _ => {}
    }
}

/// A marked sequence's properties.
#[derive(Debug, Clone, PartialEq)]
enum Props {
    Inline(Vec<DictEntry>),
    Named(Vec<u8>),
}

/// One marked sequence with properties (`BDC`): the index of its `BDC`, its tag and properties,
/// and its content's fingerprint (nested sequences included).
#[derive(Debug, Clone)]
struct Sequence {
    begin: usize,
    tag: Vec<u8>,
    props: Props,
    fingerprint: Fingerprint,
}

impl Sequence {
    /// What the sequence is matched by between two versions of a stream: its tag and its
    /// properties' keys and values (not their bytes, which MuPDF rewrites).
    fn key(&self) -> String {
        match &self.props {
            Props::Inline(entries) => {
                let pairs: Vec<String> = entries
                    .iter()
                    .map(|e| format!("{:?}={:?}", e.key, e.value))
                    .collect();
                format!("{:?}|{}", self.tag, pairs.join(","))
            }
            Props::Named(name) => format!("{:?}|/{:?}", self.tag, name),
        }
    }
}

/// The marked sequences with properties of `ops`, in the order their `BDC`s come. `resolve` answers
/// the object number an operator's resource name stands for (`Do` and `sh`).
fn sequences(src: &[u8], ops: &[Op], resolve: &Resolve<'_>) -> Vec<Sequence> {
    let mut done = Vec::new();
    // The open sequences, innermost last; `None` for a `BMC`, which only nests.
    let mut open: Vec<Option<Sequence>> = Vec::new();
    for (index, op) in ops.iter().enumerate() {
        let shown = match op.operator.as_slice() {
            b"BDC" => {
                let tag = match op.operands.first() {
                    Some(Operand::Name(tag)) => tag.clone(),
                    _ => Vec::new(),
                };
                let props = match op.operands.get(1) {
                    Some(Operand::Dict(entries)) => Some(Props::Inline(entries.clone())),
                    Some(Operand::Name(name)) => Some(Props::Named(name.clone())),
                    _ => None,
                };
                open.push(props.map(|props| Sequence {
                    begin: index,
                    tag,
                    props,
                    fingerprint: Fingerprint::default(),
                }));
                continue;
            }
            b"BMC" => {
                open.push(None);
                continue;
            }
            b"EMC" => {
                if let Some(Some(sequence)) = open.pop() {
                    // Its content is part of the sequence around it too, which hands it on when it
                    // closes in turn.
                    if let Some(outer) = open.iter_mut().rev().flatten().next() {
                        outer
                            .fingerprint
                            .text
                            .extend_from_slice(&sequence.fingerprint.text);
                        outer
                            .fingerprint
                            .drawn
                            .extend(sequence.fingerprint.drawn.iter().cloned());
                    }
                    done.push(sequence);
                }
                continue;
            }
            b"Tj" | b"TJ" | b"'" | b"\"" => {
                let mut text = Vec::new();
                op.operands.iter().for_each(|o| strings(o, &mut text));
                Some((text, None))
            }
            b"Do" | b"sh" => {
                let drawn = match op.operands.first() {
                    Some(Operand::Name(name)) => match resolve(&op.operator, name) {
                        Some(object) => Drawn::Object(op.operator.clone(), object),
                        None => Drawn::Name(op.operator.clone(), name.clone()),
                    },
                    _ => continue,
                };
                Some((Vec::new(), Some(drawn)))
            }
            b"BI" => op
                .image
                .map(|(s, e)| (Vec::new(), Some(Drawn::Inline(src[s..e].to_vec())))),
            _ => None,
        };
        let Some((text, drawn)) = shown else {
            continue;
        };
        // The innermost `BDC` holds it (through any `BMC` inside it).
        if let Some(innermost) = open.iter_mut().rev().flatten().next() {
            innermost.fingerprint.text.extend_from_slice(&text);
            innermost.fingerprint.drawn.extend(drawn);
        }
    }
    // Sequences never closed end with the stream.
    done.extend(open.into_iter().flatten());
    done.sort_by_key(|s| s.begin);
    done
}

/// Removes the replacement text (`/ActualText`, `/Alt`, `/E`) of every marked sequence of
/// `filtered` whose content is not what it was in `original`: a sequence is matched to the
/// sequence of `original` with the same tag and properties that comes at the same place among
/// those, and it changed when the text it shows or what it draws differs, or when it has no match.
///
/// An inline property dictionary loses those keys; a sequence that names its properties in the
/// resources (`/Tag /Name BDC`) becomes `/Tag BMC` when `named_has_text` says that entry has them,
/// since the entry may be shared. `resolve_original` and `resolve_filtered` answer the object a
/// `Do` or `sh` name stands for in each version, so an image MuPDF replaced counts as a change.
///
/// Returns the new stream, or `None` when nothing had to go.
pub fn strip_replacement_text(
    original: &[u8],
    filtered: &[u8],
    resolve_original: &Resolve<'_>,
    resolve_filtered: &Resolve<'_>,
    named_has_text: &dyn Fn(&[u8]) -> bool,
) -> Option<Vec<u8>> {
    let before_ops = parse(original);
    let after_ops = parse(filtered);
    let before = sequences(original, &before_ops, resolve_original);
    let after = sequences(filtered, &after_ops, resolve_filtered);
    let mut by_key: HashMap<String, Vec<&Fingerprint>> = HashMap::new();
    for sequence in &before {
        by_key
            .entry(sequence.key())
            .or_default()
            .push(&sequence.fingerprint);
    }
    let mut seen: HashMap<String, usize> = HashMap::new();
    // `(start, end, replacement)`, in stream order.
    let mut edits: Vec<(usize, usize, Vec<u8>)> = Vec::new();
    for sequence in &after {
        let key = sequence.key();
        let nth = seen.entry(key.clone()).or_insert(0);
        let was = by_key.get(&key).and_then(|list| list.get(*nth));
        *nth += 1;
        if was.is_some_and(|was| **was == sequence.fingerprint) {
            continue;
        }
        let op = &after_ops[sequence.begin];
        match &sequence.props {
            Props::Inline(entries) => {
                if !entries
                    .iter()
                    .any(|e| REPLACEMENT_KEYS.contains(&e.key.as_slice()))
                {
                    continue;
                }
                let Some(&(start, end)) = op.spans.get(1) else {
                    continue;
                };
                let mut dict = b"<<".to_vec();
                for entry in entries
                    .iter()
                    .filter(|e| !REPLACEMENT_KEYS.contains(&e.key.as_slice()))
                {
                    dict.push(b' ');
                    dict.extend_from_slice(&filtered[entry.start..entry.end]);
                }
                dict.extend_from_slice(b" >>");
                edits.push((start, end, dict));
            }
            Props::Named(name) => {
                if !named_has_text(name) {
                    continue;
                }
                let Some(&(tag_start, tag_end)) = op.spans.first() else {
                    continue;
                };
                let mut marked = filtered[tag_start..tag_end].to_vec();
                marked.extend_from_slice(b" BMC");
                edits.push((op.start, op.end, marked));
            }
        }
    }
    if edits.is_empty() {
        return None;
    }
    let mut out = Vec::with_capacity(filtered.len());
    let mut at = 0;
    for (start, end, replacement) in edits {
        out.extend_from_slice(&filtered[at..start]);
        out.extend_from_slice(&replacement);
        at = end;
    }
    out.extend_from_slice(&filtered[at..]);
    Some(out)
}

/// The resource names a content stream uses, by the resource dictionary's category.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ResourceNames {
    pub font: BTreeSet<Vec<u8>>,
    pub x_object: BTreeSet<Vec<u8>>,
    pub ext_g_state: BTreeSet<Vec<u8>>,
    pub pattern: BTreeSet<Vec<u8>>,
    pub shading: BTreeSet<Vec<u8>>,
    pub color_space: BTreeSet<Vec<u8>>,
    pub properties: BTreeSet<Vec<u8>>,
}

impl ResourceNames {
    /// The names of one category of a resource dictionary (`Font`, `XObject`, ...); `None` for a
    /// category a content stream does not name things in (`ProcSet`).
    pub fn category(&self, key: &str) -> Option<&BTreeSet<Vec<u8>>> {
        Some(match key {
            "Font" => &self.font,
            "XObject" => &self.x_object,
            "ExtGState" => &self.ext_g_state,
            "Pattern" => &self.pattern,
            "Shading" => &self.shading,
            "ColorSpace" => &self.color_space,
            "Properties" => &self.properties,
            _ => return None,
        })
    }
}

/// The resource names `src` uses: fonts (`Tf`), XObjects (`Do`), graphics states (`gs`), patterns
/// (`scn`, `SCN`), shadings (`sh`), colour spaces (`cs`, `CS`, an inline image's `/CS`) and
/// properties (`BDC`, `DP`).
pub fn resource_names(src: &[u8]) -> ResourceNames {
    let mut names = ResourceNames::default();
    let name = |operand: Option<&Operand>| match operand {
        Some(Operand::Name(name)) => Some(name.clone()),
        _ => None,
    };
    for op in parse(src) {
        let (set, value) = match op.operator.as_slice() {
            b"Tf" => (&mut names.font, name(op.operands.first())),
            b"Do" => (&mut names.x_object, name(op.operands.first())),
            b"gs" => (&mut names.ext_g_state, name(op.operands.first())),
            b"scn" | b"SCN" => (&mut names.pattern, name(op.operands.last())),
            b"sh" => (&mut names.shading, name(op.operands.first())),
            b"cs" | b"CS" => (&mut names.color_space, name(op.operands.first())),
            b"BDC" | b"DP" => (&mut names.properties, name(op.operands.get(1))),
            b"BI" => {
                if let Some(Operand::Dict(params)) = op.operands.first() {
                    for entry in params {
                        if entry.key == b"CS" || entry.key == b"ColorSpace" {
                            if let Operand::Name(cs) = &entry.value {
                                names.color_space.insert(cs.clone());
                            }
                        }
                    }
                }
                continue;
            }
            _ => continue,
        };
        if let Some(value) = value {
            set.insert(value);
        }
    }
    names
}

/// Operators that put marks on the page: path painting, shadings, XObjects, inline images and
/// text.
fn paints(operator: &[u8]) -> bool {
    matches!(
        operator,
        b"f" | b"F"
            | b"f*"
            | b"B"
            | b"B*"
            | b"b"
            | b"b*"
            | b"S"
            | b"s"
            | b"sh"
            | b"Do"
            | b"BI"
            | b"Tj"
            | b"TJ"
            | b"'"
            | b"\""
    )
}

/// Removes the operations that set a graphics state (`gs`) or a colour (`cs`, `CS`, `scn`,
/// `SCN`) when nothing is painted after them before the state is restored (`Q`) or the stream
/// ends: they have no effect, and they name resources - a soft mask, a pattern - that would
/// otherwise stay in the file after a redaction removed what they applied to.
///
/// Returns the new stream, or `None` when there was nothing to remove.
pub fn drop_idle_state(src: &[u8]) -> Option<Vec<u8>> {
    let ops = parse(src);
    // Backwards: for each `q` level, whether anything is painted after this point inside it.
    let mut painted_after = vec![false];
    let mut idle = Vec::new();
    for (index, op) in ops.iter().enumerate().rev() {
        let operator = op.operator.as_slice();
        match operator {
            b"Q" => painted_after.push(false),
            b"q" => {
                if painted_after.len() > 1 {
                    let inner = painted_after.pop().unwrap_or(false);
                    if let Some(outer) = painted_after.last_mut() {
                        *outer |= inner;
                    }
                }
            }
            b"gs" | b"cs" | b"CS" | b"scn" | b"SCN" => {
                if !painted_after.last().copied().unwrap_or(true) {
                    idle.push(index);
                }
            }
            _ if paints(operator) => {
                if let Some(level) = painted_after.last_mut() {
                    *level = true;
                }
            }
            _ => {}
        }
    }
    if idle.is_empty() {
        return None;
    }
    idle.reverse();
    let mut out = Vec::with_capacity(src.len());
    let mut at = 0;
    for index in idle {
        let op = &ops[index];
        out.extend_from_slice(&src[at..op.start]);
        at = op.end;
    }
    out.extend_from_slice(&src[at..]);
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn none(_: &[u8], _: &[u8]) -> Option<i64> {
        None
    }

    #[test]
    fn parses_operands_and_operators() {
        let ops = parse(b"BT /F1 12 Tf 72 700 Td (a\\(b\\)c \\101) Tj [<4142> -20 (C)] TJ ET");
        let names: Vec<&[u8]> = ops.iter().map(|o| o.operator.as_slice()).collect();
        assert_eq!(names, [&b"BT"[..], b"Tf", b"Td", b"Tj", b"TJ", b"ET"]);
        assert_eq!(
            ops[1].operands,
            vec![Operand::Name(b"F1".to_vec()), Operand::Number(12.0)]
        );
        assert_eq!(ops[3].operands, vec![Operand::Str(b"a(b)c A".to_vec())]);
        assert_eq!(
            ops[4].operands,
            vec![Operand::Array(vec![
                Operand::Str(b"AB".to_vec()),
                Operand::Number(-20.0),
                Operand::Str(b"C".to_vec())
            ])]
        );
    }

    #[test]
    fn reads_names_hex_strings_comments_and_nesting() {
        let ops = parse(b"% comment\n/Sp#61n <</A (x(y)z) /B [1 .5 -3.] /C <41 4>>> BDC EMC");
        assert_eq!(ops.len(), 2);
        assert_eq!(ops[0].operands[0], Operand::Name(b"Span".to_vec()));
        let Operand::Dict(entries) = &ops[0].operands[1] else {
            panic!("dict");
        };
        assert_eq!(entries[0].value, Operand::Str(b"x(y)z".to_vec()));
        assert_eq!(
            entries[1].value,
            Operand::Array(vec![
                Operand::Number(1.0),
                Operand::Number(0.5),
                Operand::Number(-3.0)
            ])
        );
        assert_eq!(entries[2].value, Operand::Str(b"A@".to_vec()));
    }

    #[test]
    fn skips_inline_image_data() {
        // The data holds bytes that would read as operators and an unbalanced string.
        let src = b"q BI /W 2 /H 1 /CS /RGB /BPC 8 ID \x00(EI Tj\xff\x01 EI Q";
        let ops = parse(src);
        let names: Vec<&[u8]> = ops.iter().map(|o| o.operator.as_slice()).collect();
        assert_eq!(names, [&b"q"[..], b"BI", b"Q"]);
        let (s, e) = ops[1].image.expect("image");
        assert_eq!(&src[s..e], b"\x00(EI Tj\xff\x01");
        assert!(resource_names(src).color_space.contains(b"RGB".as_slice()));
    }

    #[test]
    fn strips_replacement_text_of_a_sequence_whose_glyphs_went() {
        let original = b"/Span <</Lang (en) /ActualText (SECRET)>> BDC BT (xx) Tj ET EMC \
                         /Span <</ActualText (kept)>> BDC BT (yy) Tj ET EMC";
        // MuPDF's output: the first sequence lost its glyphs, the second is as it was.
        let filtered = b"/Span<</Lang(en)/ActualText(SECRET)>>BDC EMC \
                         /Span<</ActualText(kept)>>BDC BT(yy)Tj ET EMC";
        let out =
            strip_replacement_text(original, filtered, &none, &none, &|_| false).expect("changed");
        let text = String::from_utf8_lossy(&out);
        assert!(!text.contains("SECRET"), "{text}");
        assert!(text.contains("/Lang(en)"), "{text}");
        assert!(text.contains("(kept)"), "{text}");
        // What is left still reads as the same operations.
        let names: Vec<Vec<u8>> = parse(&out).into_iter().map(|o| o.operator).collect();
        let before: Vec<Vec<u8>> = parse(filtered).into_iter().map(|o| o.operator).collect();
        assert_eq!(names, before);
    }

    #[test]
    fn a_partly_removed_sequence_counts_as_changed() {
        let original = b"/Span <</Alt (SECRET word)>> BDC BT (SECRET word) Tj ET EMC";
        let filtered = b"/Span <</Alt (SECRET word)>> BDC BT [( word)] TJ ET EMC";
        let out = strip_replacement_text(original, filtered, &none, &none, &|_| false);
        assert!(out.is_some_and(|out| !String::from_utf8_lossy(&out).contains("SECRET")));
    }

    #[test]
    fn unchanged_sequences_keep_their_text() {
        let original = b"/Span <</ActualText (fi)>> BDC BT (\\001) Tj ET EMC";
        // MuPDF rewrites the bytes but not the content.
        let filtered = b"/Span<</ActualText(fi)>>BDC BT<01>Tj ET EMC";
        assert_eq!(
            strip_replacement_text(original, filtered, &none, &none, &|_| true),
            None
        );
    }

    #[test]
    fn a_replaced_image_counts_as_changed() {
        let original = b"/Figure <</Alt (chart)>> BDC /Im1 Do EMC";
        let filtered = b"/Figure <</Alt (chart)>> BDC /Im1 Do EMC";
        let before = |_: &[u8], _: &[u8]| Some(7);
        let after = |_: &[u8], _: &[u8]| Some(9);
        let out = strip_replacement_text(original, filtered, &before, &after, &|_| false)
            .expect("changed");
        assert!(!String::from_utf8_lossy(&out).contains("chart"));
        assert_eq!(
            strip_replacement_text(original, filtered, &before, &before, &|_| false),
            None
        );
    }

    #[test]
    fn named_properties_become_a_plain_marked_sequence() {
        let original = b"/Span /MC0 BDC BT (SECRET) Tj ET EMC /OC /L1 BDC BT (x) Tj ET EMC";
        let filtered = b"/Span /MC0 BDC EMC /OC /L1 BDC EMC";
        let out = strip_replacement_text(original, filtered, &none, &none, &|name| name == b"MC0")
            .expect("changed");
        assert_eq!(out, b"/Span BMC EMC /OC /L1 BDC EMC");
    }

    #[test]
    fn nested_content_belongs_to_the_outer_sequence_too() {
        let original = b"/Div <</ActualText (outer)>> BDC /Span BMC BT (SECRET) Tj ET EMC EMC";
        let filtered = b"/Div <</ActualText (outer)>> BDC /Span BMC EMC EMC";
        let out =
            strip_replacement_text(original, filtered, &none, &none, &|_| false).expect("changed");
        assert!(!String::from_utf8_lossy(&out).contains("outer"));
    }

    #[test]
    fn lists_the_resource_names_a_stream_uses() {
        let names = resource_names(
            b"/GS0 gs /F1 12 Tf /Pattern cs /P1 scn 0 0 1 1 re f /Im1 Do /Sh0 sh \
              /OC /L1 BDC EMC 0.5 0 0 /P2 SCN",
        );
        assert!(names.font.contains(b"F1".as_slice()));
        assert!(names.ext_g_state.contains(b"GS0".as_slice()));
        assert_eq!(
            names.pattern,
            [b"P1".to_vec(), b"P2".to_vec()].into_iter().collect()
        );
        assert!(names.color_space.contains(b"Pattern".as_slice()));
        assert!(names.x_object.contains(b"Im1".as_slice()));
        assert!(names.shading.contains(b"Sh0".as_slice()));
        assert!(names.properties.contains(b"L1".as_slice()));
        assert!(names.category("ProcSet").is_none());
    }

    #[test]
    fn drops_state_nothing_is_painted_with() {
        // What MuPDF leaves when a redaction removed the fills: the soft mask and the pattern are
        // set, then the state is restored with nothing drawn.
        let src = b"q /GS0 gs BT (a) Tj ET Q q/GSm gs Q q /Pattern cs /P1 scn Q /GS1 gs";
        let out = drop_idle_state(src).expect("idle state");
        let names = resource_names(&out);
        assert_eq!(
            names.ext_g_state,
            [b"GS0".to_vec()].into_iter().collect(),
            "{}",
            String::from_utf8_lossy(&out)
        );
        assert!(names.pattern.is_empty());
        assert!(names.color_space.is_empty());
        // Painting inside a nested state counts for the state around it.
        let nested = b"/GSm gs q 0 0 1 1 re f Q";
        assert_eq!(drop_idle_state(nested), None);
        // A state set at the end of the stream with nothing after it goes too.
        assert_eq!(
            drop_idle_state(b"0 0 1 1 re f /GS9 gs"),
            Some(b"0 0 1 1 re f ".to_vec())
        );
    }

    #[test]
    fn never_stalls_on_bad_bytes() {
        for src in [
            &b")))>>}{"[..],
            b"(unterminated",
            b"<<",
            b"[ [ [",
            b"BI /W",
            b"/",
        ] {
            let _ = parse(src);
            let _ = resource_names(src);
        }
    }
}
