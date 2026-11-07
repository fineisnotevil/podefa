// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 FINE Association <su@fa.org.tr>

//! What a document's signatures say (Plan 0009 Phase 3, §4.1): what the engine found for each, and
//! the words and the verdict the app shows.

use crate::Rect;

/// Whether the signed bytes are what the signer signed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DigestState {
    /// The digest matches and the signature over it verifies with the signer's key.
    Intact,
    /// Why not: the file changed in the signed range, or the signature does not verify.
    Broken(String),
    /// It could not be checked (a format or an algorithm this app does not read), and why.
    Unknown(String),
}

/// Whether the signer's certificate leads to a root this computer trusts. Offline: revocation is
/// never checked (§0 question 7), so "trusted" says nothing about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CertificateState {
    Trusted,
    Untrusted(String),
    Unknown(String),
}

/// What happened to the document after this signature.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangedAfter {
    /// The signature covers the file as it is.
    None,
    /// Saved again since, in ways the signatures allow.
    Allowed,
    /// Saved again since, in a way a signature's permissions forbid.
    Disallowed,
}

/// One signed signature field.
#[derive(Debug, Clone, PartialEq)]
pub struct SignatureInfo {
    /// The field's full name.
    pub field: String,
    /// Where its widget is, when it has one.
    pub page: Option<u32>,
    pub rect: Option<Rect>,
    /// The certificate's common name, or the name in the signature when there is no certificate.
    pub signer: String,
    /// When it was signed, as `2026-11-07 12:00 +03:00`, empty when it does not say.
    pub time: String,
    pub reason: String,
    pub location: String,
    pub digest: DigestState,
    pub certificate: CertificateState,
    pub changed_after: ChangedAfter,
}

/// How a signature comes out, in one word: what the mark and the row show.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Verdict {
    /// Intact, from a trusted certificate, no change it forbids.
    Valid,
    /// Not shown wrong, not shown right: an unknown signer or something unread.
    Unsure,
    /// The signed bytes changed, or the document changed in a forbidden way.
    Invalid,
}

impl SignatureInfo {
    pub fn verdict(&self) -> Verdict {
        if matches!(self.digest, DigestState::Broken(_))
            || self.changed_after == ChangedAfter::Disallowed
        {
            return Verdict::Invalid;
        }
        if self.digest == DigestState::Intact && self.certificate == CertificateState::Trusted {
            return Verdict::Valid;
        }
        Verdict::Unsure
    }

    /// The first line of the state: what it means for the reader.
    pub fn headline(&self) -> &'static str {
        match self.verdict() {
            Verdict::Valid => "Valid signature",
            Verdict::Unsure => "Signature not verified",
            Verdict::Invalid => "Invalid signature",
        }
    }

    /// The findings, one to a line. Revocation is always said to be unchecked, so "valid" is not
    /// read as more than it is.
    pub fn details(&self) -> Vec<String> {
        let mut lines = Vec::new();
        lines.push(match &self.digest {
            DigestState::Intact => "The signed content has not changed.".to_owned(),
            DigestState::Broken(why) => format!("The signed content has changed: {why}."),
            DigestState::Unknown(why) => format!("The signed content could not be checked: {why}."),
        });
        lines.push(match self.changed_after {
            ChangedAfter::None => "The document has not been saved since it was signed.".to_owned(),
            ChangedAfter::Allowed => {
                "The document was saved again after signing, in ways the signature allows."
                    .to_owned()
            }
            ChangedAfter::Disallowed => {
                "The document was changed after signing in a way the signature forbids.".to_owned()
            }
        });
        lines.push(match &self.certificate {
            CertificateState::Trusted => {
                "The certificate leads to a root this computer trusts. Revocation was not checked."
                    .to_owned()
            }
            CertificateState::Untrusted(why) => {
                format!("The certificate is not trusted: {why}. Revocation was not checked.")
            }
            CertificateState::Unknown(why) => {
                format!("The certificate could not be checked: {why}.")
            }
        });
        lines
    }

    /// Who, when and why, in a line.
    pub fn byline(&self) -> String {
        let mut line = self.signer.clone();
        for part in [&self.time, &self.reason, &self.location] {
            if !part.is_empty() {
                if !line.is_empty() {
                    line.push_str(", ");
                }
                line.push_str(part);
            }
        }
        line
    }
}

/// The worst verdict of a document's signatures: what the status bar's mark shows.
pub fn overall(signatures: &[SignatureInfo]) -> Option<Verdict> {
    signatures.iter().map(SignatureInfo::verdict).max()
}

/// A PDF date (`D:YYYYMMDDHHmmSS+hh'mm'`, every part after the year optional) as
/// `2026-11-07 12:00 +03:00`; text that is not one comes back as it is.
pub fn pdf_date(text: &str) -> String {
    let body = text.strip_prefix("D:").unwrap_or(text);
    let digits = body.chars().take_while(char::is_ascii_digit).count();
    if digits < 4 || digits % 2 != 0 {
        return text.to_owned();
    }
    let part = |from: usize, default: &'static str| body.get(from..from + 2).unwrap_or(default);
    let (year, rest) = body.split_at(4);
    let (month, day) = (
        if digits >= 6 { part(4, "01") } else { "01" },
        if digits >= 8 { part(6, "01") } else { "01" },
    );
    let (hour, minute) = (
        if digits >= 10 { part(8, "00") } else { "00" },
        if digits >= 12 { part(10, "00") } else { "00" },
    );
    let mut out = format!("{year}-{month}-{day} {hour}:{minute}");
    let zone = rest.get(digits - 4..).unwrap_or("");
    let numbers: String = zone.chars().filter(char::is_ascii_digit).collect();
    match zone.chars().next() {
        Some('Z') => out.push_str(" UTC"),
        Some(sign @ ('+' | '-')) if numbers.len() >= 2 => {
            let (h, m) = numbers.split_at(2);
            let m = if m.len() >= 2 { &m[..2] } else { "00" };
            out.push_str(&format!(" {sign}{h}:{m}"));
        }
        _ => {}
    }
    out
}

/// A PDF date in seconds since 1970, for the certificates' validity; `None` when it is not one.
pub fn pdf_date_seconds(text: &str) -> Option<u64> {
    let body = text.strip_prefix("D:").unwrap_or(text);
    let digits = body.chars().take_while(char::is_ascii_digit).count();
    if digits < 4 || digits % 2 != 0 {
        return None;
    }
    let number = |from: usize, default: i64| -> Option<i64> {
        if from + 2 > digits {
            return Some(default);
        }
        body.get(from..from + 2)?.parse().ok()
    };
    let year: i64 = body.get(..4)?.parse().ok()?;
    let (month, day) = (number(4, 1)?, number(6, 1)?);
    let (hour, minute, second) = (number(8, 0)?, number(10, 0)?, number(12, 0)?);
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    // Days from civil dates, for the proleptic Gregorian calendar.
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    let zone = body.get(digits..).unwrap_or("");
    let numbers: String = zone.chars().filter(char::is_ascii_digit).collect();
    let offset = match zone.chars().next() {
        Some(sign @ ('+' | '-')) if numbers.len() >= 2 => {
            let h: i64 = numbers.get(..2)?.parse().ok()?;
            let m: i64 = numbers.get(2..4).and_then(|m| m.parse().ok()).unwrap_or(0);
            let seconds = h * 3600 + m * 60;
            if sign == '-' { -seconds } else { seconds }
        }
        _ => 0,
    };
    u64::try_from(days * 86_400 + hour * 3600 + minute * 60 + second - offset).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pdf_dates_in_seconds() {
        // `date -u -d 2026-11-07T09:00:00 +%s`.
        assert_eq!(
            pdf_date_seconds("D:20261107120000+03'00'"),
            Some(1_794_042_000)
        );
        assert_eq!(pdf_date_seconds("D:20261107090000Z"), Some(1_794_042_000));
        assert_eq!(pdf_date_seconds("D:1970"), Some(0));
        assert_eq!(
            pdf_date_seconds("D:20000301000000-01'30'"),
            Some(951_874_200)
        );
        assert_eq!(pdf_date_seconds("yesterday"), None);
        assert_eq!(pdf_date_seconds("D:20261307"), None);
    }

    fn info() -> SignatureInfo {
        SignatureInfo {
            field: "Signature1".to_owned(),
            page: Some(0),
            rect: None,
            signer: "Şule".to_owned(),
            time: "2026-11-07 12:00 +03:00".to_owned(),
            reason: "Approved".to_owned(),
            location: String::new(),
            digest: DigestState::Intact,
            certificate: CertificateState::Trusted,
            changed_after: ChangedAfter::None,
        }
    }

    #[test]
    fn the_verdict_follows_the_worst_finding() {
        assert_eq!(info().verdict(), Verdict::Valid);
        let allowed = SignatureInfo {
            changed_after: ChangedAfter::Allowed,
            ..info()
        };
        assert_eq!(allowed.verdict(), Verdict::Valid);
        let stranger = SignatureInfo {
            certificate: CertificateState::Untrusted("self-signed".to_owned()),
            ..info()
        };
        assert_eq!(stranger.verdict(), Verdict::Unsure);
        let unread = SignatureInfo {
            digest: DigestState::Unknown("format".to_owned()),
            ..info()
        };
        assert_eq!(unread.verdict(), Verdict::Unsure);
        let broken = SignatureInfo {
            digest: DigestState::Broken("digest".to_owned()),
            ..info()
        };
        assert_eq!(broken.verdict(), Verdict::Invalid);
        let forbidden = SignatureInfo {
            changed_after: ChangedAfter::Disallowed,
            ..info()
        };
        assert_eq!(forbidden.verdict(), Verdict::Invalid);
        assert_eq!(overall(&[]), None);
        assert_eq!(overall(&[info(), stranger.clone()]), Some(Verdict::Unsure));
        assert_eq!(overall(&[broken, stranger, info()]), Some(Verdict::Invalid));
    }

    #[test]
    fn revocation_is_always_said_to_be_unchecked() {
        assert!(
            info()
                .details()
                .iter()
                .any(|l| l.contains("Revocation was not checked"))
        );
    }

    #[test]
    fn the_byline_leaves_out_what_is_empty() {
        assert_eq!(info().byline(), "Şule, 2026-11-07 12:00 +03:00, Approved");
    }

    #[test]
    fn pdf_dates() {
        assert_eq!(
            pdf_date("D:20261107120000+03'00'"),
            "2026-11-07 12:00 +03:00"
        );
        assert_eq!(
            pdf_date("D:20261107120000-05'30"),
            "2026-11-07 12:00 -05:30"
        );
        assert_eq!(pdf_date("D:20261107120000Z"), "2026-11-07 12:00 UTC");
        assert_eq!(pdf_date("D:2026"), "2026-01-01 00:00");
        assert_eq!(pdf_date("D:202611"), "2026-11-01 00:00");
        assert_eq!(pdf_date("yesterday"), "yesterday");
        assert_eq!(pdf_date("D:202"), "D:202");
    }
}
