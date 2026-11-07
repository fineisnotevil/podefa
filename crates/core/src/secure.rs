// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 FINE Association <su@fa.org.tr>

//! Encryption and permissions (Plan 0009 Phase 2, §4.1): what a protected document allows, what
//! each edit and tool needs, and the protection a save writes.
//!
//! A document's permissions are advice MuPDF leaves to the application (§3.3); by §0 question 4
//! this app follows them, and the permissions password unlocks them.

use crate::annot::EditOp;
use crate::pages::PageOp;
use crate::tools::{MergeInput, ToolJob};

/// The longest password MuPDF writes, in UTF-8 bytes: its write options keep 128 bytes with the
/// closing zero (`pdf_write_options`, and mupdf-rs `assert!`s the length, §3.3 flaw 2).
pub const MAX_PASSWORD_BYTES: usize = 127;

/// The `/P` bits of the PDF standard security handler (ISO 32000-1, table 22).
mod bits {
    pub const PRINT: u32 = 1 << 2;
    pub const MODIFY: u32 = 1 << 3;
    pub const COPY: u32 = 1 << 4;
    pub const ANNOTATE: u32 = 1 << 5;
    pub const FILL: u32 = 1 << 8;
    pub const ACCESSIBILITY: u32 = 1 << 9;
    pub const ASSEMBLE: u32 = 1 << 10;
    pub const PRINT_HQ: u32 = 1 << 11;
}

/// What a document allows, as its `/P` entry says (every permission for a document without
/// encryption, or opened with its permissions password).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Permissions {
    pub print: bool,
    /// Printing at full quality; without it only a low-resolution print.
    pub print_hq: bool,
    /// Copying or extracting text and images.
    pub copy: bool,
    /// Changing the content in ways the other permissions do not name.
    pub modify: bool,
    /// Adding and changing annotations, and filling forms.
    pub annotate: bool,
    /// Filling forms (and signing), even without `annotate`.
    pub fill: bool,
    /// Inserting, rotating and deleting pages and changing bookmarks, even without `modify`.
    pub assemble: bool,
    /// Extracting text for accessibility.
    pub accessibility: bool,
}

impl Permissions {
    pub const ALL: Self = Self {
        print: true,
        print_hq: true,
        copy: true,
        modify: true,
        annotate: true,
        fill: true,
        assemble: true,
        accessibility: true,
    };

    /// Reads `/P`; the reserved bits are ignored.
    pub fn from_bits(p: u32) -> Self {
        let has = |bit: u32| p & bit != 0;
        Self {
            print: has(bits::PRINT),
            print_hq: has(bits::PRINT_HQ),
            copy: has(bits::COPY),
            modify: has(bits::MODIFY),
            annotate: has(bits::ANNOTATE),
            fill: has(bits::FILL),
            assemble: has(bits::ASSEMBLE),
            accessibility: has(bits::ACCESSIBILITY),
        }
    }

    /// The permission bits of `/P` (MuPDF sets the reserved bits itself when it writes them).
    pub fn to_bits(self) -> u32 {
        let bit = |on: bool, bit: u32| if on { bit } else { 0 };
        bit(self.print, bits::PRINT)
            | bit(self.print_hq, bits::PRINT_HQ)
            | bit(self.copy, bits::COPY)
            | bit(self.modify, bits::MODIFY)
            | bit(self.annotate, bits::ANNOTATE)
            | bit(self.fill, bits::FILL)
            | bit(self.assemble, bits::ASSEMBLE)
            | bit(self.accessibility, bits::ACCESSIBILITY)
    }

    pub fn is_all(self) -> bool {
        self == Self::ALL
    }

    /// Whether the document allows what `need` names; a permission includes the narrower one the
    /// standard puts under it (annotating includes filling, modifying includes assembling).
    pub fn allows(self, need: Need) -> bool {
        match need {
            Need::Print => self.print,
            Need::Copy => self.copy,
            Need::Annotate => self.annotate,
            Need::Fill => self.fill || self.annotate,
            Need::Assemble => self.assemble || self.modify,
            Need::Modify => self.modify,
        }
    }

    /// Whether any edit at all is allowed.
    pub fn allows_any_edit(self) -> bool {
        [Need::Annotate, Need::Fill, Need::Assemble, Need::Modify]
            .into_iter()
            .any(|need| self.allows(need))
    }

    /// The permissions withheld, in words, for the status bar's lock.
    pub fn withheld(self) -> Vec<&'static str> {
        let mut out = Vec::new();
        if !self.print {
            out.push("printing");
        } else if !self.print_hq {
            out.push("printing at full quality");
        }
        if !self.copy {
            out.push("copying text and images");
        }
        if !self.modify {
            out.push("editing the content");
        }
        if !self.annotate {
            out.push("adding comments");
        }
        if !self.allows(Need::Fill) {
            out.push("filling forms");
        }
        if !self.allows(Need::Assemble) {
            out.push("changing pages");
        }
        out
    }
}

/// What an action needs the document to allow.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Need {
    /// Printing, at low resolution without `print_hq`.
    Print,
    /// Copying text, or writing the document's pages into another file.
    Copy,
    /// Annotations: markup, notes, ink, and deleting or changing them.
    Annotate,
    /// Filling form fields.
    Fill,
    /// Page structure and the outline.
    Assemble,
    /// Everything else that changes the document: stamps, crop, redaction, properties, flattening.
    Modify,
}

impl Need {
    /// The action in words, for a refusal: "The document's permissions do not allow {words}".
    pub fn words(self) -> &'static str {
        match self {
            Self::Print => "printing it",
            Self::Copy => "copying its content",
            Self::Annotate => "comments",
            Self::Fill => "filling its form",
            Self::Assemble => "changing its pages",
            Self::Modify => "editing it",
        }
    }
}

impl EditOp {
    /// What the edit needs the document to allow.
    pub fn need(&self) -> Need {
        match self {
            Self::AddMarkup { .. }
            | Self::Delete { .. }
            | Self::SetColour { .. }
            | Self::AddNote { .. }
            | Self::SetContents { .. }
            | Self::MoveNote { .. }
            | Self::AddInk { .. } => Need::Annotate,
            Self::SetField { .. }
            | Self::SetChoices { .. }
            | Self::ResetField { .. }
            | Self::ResetForm
            | Self::ImportForm { .. } => Need::Fill,
            Self::Pages(op) => match op {
                PageOp::Rotate { .. }
                | PageOp::Delete { .. }
                | PageOp::Duplicate { .. }
                | PageOp::Move { .. }
                | PageOp::InsertBlank { .. }
                | PageOp::InsertFrom { .. } => Need::Assemble,
                PageOp::Crop { .. } | PageOp::Stamp { .. } => Need::Modify,
            },
            Self::RemoveOutline { .. } => Need::Assemble,
            Self::SetMetadata(_)
            | Self::FlattenForm
            | Self::MarkRedactions { .. }
            | Self::ApplyRedactions { .. } => Need::Modify,
        }
    }
}

impl ToolJob {
    /// What the job needs the open document to allow: a job that writes its pages or their content
    /// into another, unprotected file needs [`Need::Copy`]. Compress keeps the encryption, and a
    /// job that does not read the open document needs nothing.
    pub fn need(&self) -> Option<Need> {
        match self {
            Self::Merge { inputs, .. } => {
                inputs.contains(&MergeInput::Current).then_some(Need::Copy)
            }
            Self::Extract { .. } | Self::ExportImages { .. } | Self::ExportWord { .. } => {
                Some(Need::Copy)
            }
            Self::Print { .. } => Some(Need::Print),
            Self::ImagesToPdf { .. } | Self::Compress { .. } => None,
        }
    }
}

/// How a document is protected, as its `Opened` and each change of protection report it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Security {
    /// The document is encrypted.
    pub encrypted: bool,
    /// The encryption in words ("AES-256"), empty without one.
    pub method: String,
    /// What it allows as opened now.
    pub permissions: Permissions,
    /// How many of its signature fields are signed (Plan 0009 §3.5: a full rewrite breaks them).
    pub signed: u32,
    /// The DocMDP level of a certifying signature: 1 no changes, 2 filling and signing, 3 also
    /// annotations.
    pub certified: Option<u8>,
}

impl Default for Security {
    fn default() -> Self {
        Self {
            encrypted: false,
            method: String::new(),
            permissions: Permissions::ALL,
            signed: 0,
            certified: None,
        }
    }
}

impl Security {
    /// Whether the certifying signature's level allows an edit: one that it does not shows as a
    /// disallowed change in every reader (§3.5).
    pub fn certification_allows(&self, need: Need) -> bool {
        match self.certified {
            None => true,
            Some(1) => false,
            Some(2) => need == Need::Fill,
            Some(_) => matches!(need, Need::Fill | Need::Annotate),
        }
    }
}

/// The protection a save writes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Protection {
    /// The document's encryption as it is.
    Keep,
    /// No encryption: the document opens without a password and allows everything.
    Remove,
    /// AES-256 with these passwords and permissions.
    Encrypt(Encryption),
}

/// AES-256 encryption (the only method this app writes, §3.3).
#[derive(Clone, PartialEq, Eq)]
pub struct Encryption {
    /// The password to open; empty opens without one (then only the permissions protect it).
    pub user: String,
    /// The permissions password; it opens the document too, with every permission.
    pub owner: String,
    pub permissions: Permissions,
}

impl std::fmt::Debug for Encryption {
    /// Never the passwords.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Encryption")
            .field("user", &(!self.user.is_empty()))
            .field("owner", &(!self.owner.is_empty()))
            .field("permissions", &self.permissions)
            .finish()
    }
}

/// Why a protection cannot be written.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProtectionError {
    #[error("give a password to open or restrict a permission")]
    Nothing,
    #[error("a password is longer than {MAX_PASSWORD_BYTES} bytes")]
    TooLong,
    #[error("a password cannot hold a zero character")]
    Zero,
    #[error("restricted permissions need a permissions password")]
    NoOwner,
    #[error("the permissions password must differ from the password to open")]
    SameAsUser,
}

impl Encryption {
    /// The encryption as written: checked, and without a permissions password the password to
    /// open stands for it (all permissions are given then, so it unlocks nothing).
    pub fn checked(mut self) -> Result<Self, ProtectionError> {
        for password in [&self.user, &self.owner] {
            if password.len() > MAX_PASSWORD_BYTES {
                return Err(ProtectionError::TooLong);
            }
            if password.contains('\0') {
                return Err(ProtectionError::Zero);
            }
        }
        let restricted = !self.permissions.is_all();
        if self.user.is_empty() && self.owner.is_empty() && !restricted {
            return Err(ProtectionError::Nothing);
        }
        if restricted {
            if self.owner.is_empty() {
                return Err(ProtectionError::NoOwner);
            }
            // MuPDF opens a document whose two passwords are equal with every permission.
            if self.owner == self.user {
                return Err(ProtectionError::SameAsUser);
            }
        }
        if self.owner.is_empty() {
            self.owner = self.user.clone();
        }
        // Accessibility stays allowed: screen readers need the text (ISO 32000-1, table 22).
        self.permissions.accessibility = true;
        Ok(self)
    }

    /// The password the document is opened with after the save: the permissions password, so the
    /// user who protected it keeps every permission.
    pub fn reopen_password(&self) -> Option<&str> {
        [&self.owner, &self.user]
            .into_iter()
            .find(|p| !p.is_empty())
            .map(String::as_str)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encryption(user: &str, owner: &str, permissions: Permissions) -> Encryption {
        Encryption {
            user: user.to_owned(),
            owner: owner.to_owned(),
            permissions,
        }
    }

    #[test]
    fn bits_round_trip_and_reserved_bits_are_ignored() {
        // A print-only `/P` as a writer stores it: the reserved high bits set (two's complement).
        let p = 0xFFFF_F0C4_u32;
        let read = Permissions::from_bits(p);
        assert!(read.print);
        assert!(!read.copy && !read.modify && !read.annotate && !read.fill && !read.assemble);
        assert_eq!(Permissions::from_bits(read.to_bits()), read);
        assert!(Permissions::from_bits(0xFFFF_FFFC).is_all());
    }

    #[test]
    fn wider_permissions_include_narrower_ones() {
        let annotate = Permissions {
            annotate: true,
            ..Permissions::from_bits(0)
        };
        assert!(annotate.allows(Need::Fill));
        assert!(!annotate.allows(Need::Assemble));
        let modify = Permissions {
            modify: true,
            ..Permissions::from_bits(0)
        };
        assert!(modify.allows(Need::Assemble));
        assert!(!modify.allows(Need::Annotate));
        assert!(!Permissions::from_bits(0).allows_any_edit());
    }

    #[test]
    fn each_edit_needs_its_permission() {
        assert_eq!(EditOp::ResetForm.need(), Need::Fill);
        assert_eq!(EditOp::FlattenForm.need(), Need::Modify);
        assert_eq!(EditOp::Delete { page: 0, id: 1 }.need(), Need::Annotate);
        assert_eq!(
            EditOp::Pages(PageOp::Delete { pages: vec![0] }).need(),
            Need::Assemble
        );
        assert_eq!(ToolJob::Extract { parts: vec![] }.need(), Some(Need::Copy));
        assert_eq!(
            ToolJob::Merge {
                inputs: vec![MergeInput::File("a.pdf".into())],
                output: "b.pdf".into()
            }
            .need(),
            None
        );
    }

    #[test]
    fn certification_levels() {
        let level = |l| Security {
            certified: Some(l),
            ..Security::default()
        };
        assert!(!level(1).certification_allows(Need::Fill));
        assert!(level(2).certification_allows(Need::Fill));
        assert!(!level(2).certification_allows(Need::Annotate));
        assert!(level(3).certification_allows(Need::Annotate));
        assert!(!level(3).certification_allows(Need::Assemble));
        assert!(Security::default().certification_allows(Need::Modify));
    }

    #[test]
    fn protection_is_checked() {
        let print_only = Permissions::from_bits(bits::PRINT);
        assert_eq!(
            encryption("", "", Permissions::ALL).checked(),
            Err(ProtectionError::Nothing)
        );
        assert_eq!(
            encryption("açık", "", print_only).checked(),
            Err(ProtectionError::NoOwner)
        );
        assert_eq!(
            encryption("şifre", "şifre", print_only).checked(),
            Err(ProtectionError::SameAsUser)
        );
        assert_eq!(
            encryption(&"ğ".repeat(64), "", Permissions::ALL).checked(),
            Err(ProtectionError::TooLong)
        );
        let open_only = encryption("şifre", "", Permissions::ALL)
            .checked()
            .expect("valid");
        assert_eq!(open_only.owner, "şifre");
        let restricted = encryption("", "sahip", print_only)
            .checked()
            .expect("valid");
        assert!(restricted.permissions.accessibility);
        assert_eq!(restricted.reopen_password(), Some("sahip"));
        assert!(!format!("{restricted:?}").contains("sahip"));
    }
}
