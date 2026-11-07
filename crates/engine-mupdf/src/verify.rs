// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 FINE Association <su@fa.org.tr>

//! The cryptography of checking a signature (Plan 0009 Phase 3, §0 question 6): `ring` for the
//! arithmetic, RustCrypto's `cms` and `x509-cert` for the structures. MuPDF has no part in it: its
//! own verifier is built on OpenSSL, which this build does not have, and nothing here needs more
//! from it than the signature's `/ByteRange`, since the CMS blob is the hex string that range
//! leaves out of the file.
//!
//! What is checked: the range leaves out the signature and nothing else; the digest of the range
//! is the one the signer signed; the signature over it verifies with the signer's certificate;
//! that certificate leads to a root in [`Roots`], each link verified and valid when it was signed.
//! Offline by decision (§0 question 7): revocation is not checked. A certificate with a critical
//! extension this code does not act on (name constraints, say) is not trusted rather than half
//! read.

use cms::cert::{CertificateChoices, IssuerAndSerialNumber};
use cms::content_info::ContentInfo;
use cms::signed_data::{SignedData, SignerIdentifier, SignerInfo};
use der::asn1::{ObjectIdentifier as Oid, OctetString};
use der::oid::db::rfc5912::{
    ECDSA_WITH_SHA_256, ECDSA_WITH_SHA_384, ID_EC_PUBLIC_KEY, ID_RSASSA_PSS, ID_SHA_1, ID_SHA_256,
    ID_SHA_384, ID_SHA_512, RSA_ENCRYPTION, SECP_256_R_1, SECP_384_R_1, SHA_1_WITH_RSA_ENCRYPTION,
    SHA_256_WITH_RSA_ENCRYPTION, SHA_384_WITH_RSA_ENCRYPTION, SHA_512_WITH_RSA_ENCRYPTION,
};
use der::{Decode as _, Encode as _, Reader as _, SliceReader, Tag, TagNumber, Tagged as _};
use pdf_core::signature::{CertificateState, DigestState};
use ring::{digest, signature as ring_signature};
use std::fs::File;
use std::io::{Read as _, Seek as _, SeekFrom};
use std::path::Path;
use x509_cert::Certificate;
use x509_cert::ext::pkix::{BasicConstraints, KeyUsage, KeyUsages};
use x509_cert::spki::{AlgorithmIdentifierOwned, SubjectPublicKeyInfoOwned};

const ID_DATA_SIGNED: Oid = Oid::new_unwrap("1.2.840.113549.1.7.2");
const ID_MESSAGE_DIGEST: Oid = Oid::new_unwrap("1.2.840.113549.1.9.4");
const ID_COMMON_NAME: Oid = Oid::new_unwrap("2.5.4.3");
const ID_SUBJECT_KEY_ID: Oid = Oid::new_unwrap("2.5.29.14");
const ID_SUBJECT_ALT_NAME: Oid = Oid::new_unwrap("2.5.29.17");
const ID_BASIC_CONSTRAINTS: Oid = Oid::new_unwrap("2.5.29.19");
const ID_KEY_USAGE: Oid = Oid::new_unwrap("2.5.29.15");
const ID_CERT_POLICIES: Oid = Oid::new_unwrap("2.5.29.32");
const ID_EXT_KEY_USAGE: Oid = Oid::new_unwrap("2.5.29.37");

/// The most bytes of signature read from the file: a CMS blob with a long chain and a timestamp is
/// tens of kilobytes, and a range that leaves out more is not leaving out a signature.
const MAX_SIGNATURE_BYTES: u64 = 16 * 1024 * 1024;
/// The longest chain followed.
const MAX_CHAIN: usize = 10;

/// The certificates trusted as roots.
#[derive(Debug, Default)]
pub struct Roots(Vec<Certificate>);

impl Roots {
    /// The roots in PEM text: a bundle, or one file per root. What cannot be read is left out.
    pub fn from_pem(text: &[u8]) -> Self {
        let mut roots = Self::default();
        roots.add_pem(text);
        roots
    }

    fn add_pem(&mut self, text: &[u8]) {
        const BEGIN: &[u8] = b"-----BEGIN CERTIFICATE-----";
        const END: &[u8] = b"-----END CERTIFICATE-----";
        let mut rest = text;
        while let Some(start) = find(rest, BEGIN) {
            let body = &rest[start + BEGIN.len()..];
            let Some(end) = find(body, END) else {
                return;
            };
            if let Some(cert) = base64(&body[..end]).and_then(|d| Certificate::from_der(&d).ok()) {
                self.0.push(cert);
            }
            rest = &body[end + END.len()..];
        }
    }

    /// The roots this computer trusts.
    pub fn system() -> Self {
        let mut roots = Self::default();
        roots.add_system();
        roots
    }

    #[cfg(windows)]
    fn add_system(&mut self) {
        use windows_sys::Win32::Security::Cryptography::{
            CertCloseStore, CertEnumCertificatesInStore, CertOpenSystemStoreW,
        };
        let name: Vec<u16> = "ROOT".encode_utf16().chain(std::iter::once(0)).collect();
        // SAFETY: the name is terminated; a null provider is the default one.
        let store = unsafe { CertOpenSystemStoreW(0, name.as_ptr()) };
        if store.is_null() {
            return;
        }
        let mut context = std::ptr::null();
        loop {
            // SAFETY: `store` is open; `context` is null or the last one returned, which this call
            // frees.
            context = unsafe { CertEnumCertificatesInStore(store, context) };
            if context.is_null() {
                break;
            }
            // SAFETY: a returned context is valid until the next call, and holds its encoded
            // certificate of this length.
            let encoded = unsafe {
                std::slice::from_raw_parts(
                    (*context).pbCertEncoded,
                    (*context).cbCertEncoded as usize,
                )
            };
            if let Ok(cert) = Certificate::from_der(encoded) {
                self.0.push(cert);
            }
        }
        // SAFETY: opened above and closed once; the loop ran to its end, so no context is held.
        unsafe {
            CertCloseStore(store, 0);
        }
    }

    #[cfg(target_os = "android")]
    fn add_system(&mut self) {
        for folder in [
            "/apex/com.android.conscrypt/cacerts",
            "/system/etc/security/cacerts",
        ] {
            let Ok(files) = std::fs::read_dir(folder) else {
                continue;
            };
            for file in files.flatten() {
                if let Ok(text) = std::fs::read(file.path()) {
                    self.add_pem(&text);
                }
            }
            if !self.0.is_empty() {
                return;
            }
        }
    }

    #[cfg(all(unix, not(target_os = "android")))]
    fn add_system(&mut self) {
        // The first bundle there is: Debian and Arch, Fedora, openSUSE, macOS and the BSDs.
        for bundle in [
            "/etc/ssl/certs/ca-certificates.crt",
            "/etc/pki/tls/certs/ca-bundle.crt",
            "/etc/ssl/ca-bundle.pem",
            "/etc/ssl/cert.pem",
        ] {
            if let Ok(text) = std::fs::read(bundle) {
                self.add_pem(&text);
                if !self.0.is_empty() {
                    return;
                }
            }
        }
    }

    /// How many roots there are.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    fn holds(&self, cert: &Certificate) -> bool {
        self.0.iter().any(|root| {
            root.tbs_certificate.subject == cert.tbs_certificate.subject
                && root.tbs_certificate.subject_public_key_info
                    == cert.tbs_certificate.subject_public_key_info
        })
    }
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// PEM's base64, whitespace ignored.
fn base64(text: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(text.len() * 3 / 4);
    let (mut bits, mut count) = (0u32, 0u32);
    for &c in text {
        let value = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' | b' ' | b'\r' | b'\n' | b'\t' => continue,
            _ => return None,
        };
        bits = (bits << 6) | u32::from(value);
        count += 6;
        if count >= 8 {
            count -= 8;
            out.push((bits >> count) as u8);
            bits &= (1 << count) - 1;
        }
    }
    Some(out)
}

/// What one signature's check found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Checked {
    pub(crate) digest: DigestState,
    pub(crate) certificate: CertificateState,
    /// The certificate's common name.
    pub(crate) signer: Option<String>,
    /// Bytes of the file after the signed range, other than the line ends a writer leaves.
    pub(crate) saved_since: bool,
}

impl Checked {
    fn unread(why: impl Into<String>, saved_since: bool) -> Self {
        let why = why.into();
        Self {
            digest: DigestState::Unknown(why.clone()),
            certificate: CertificateState::Unknown(why),
            signer: None,
            saved_since,
        }
    }

    fn broken(why: impl Into<String>, saved_since: bool) -> Self {
        Self {
            digest: DigestState::Broken(why.into()),
            certificate: CertificateState::Unknown("the signature does not verify".to_owned()),
            signer: None,
            saved_since,
        }
    }
}

/// A signature with no byte range says nothing about any bytes.
pub(crate) fn unranged() -> Checked {
    Checked::broken("it has no byte range", false)
}

/// Checks one signature of the file at `path`. `sub_filter` is the signature's `/SubFilter` and
/// `at` the time it claims, in seconds since 1970, which the certificates must be valid at.
pub(crate) fn check(
    path: &Path,
    range: [u64; 4],
    sub_filter: &[u8],
    at: u64,
    roots: &Roots,
) -> Checked {
    let [start, first, gap_end, second] = range;
    let Ok(mut file) = File::open(path) else {
        return Checked::unread("the file could not be read", false);
    };
    let Ok(len) = file.metadata().map(|m| m.len()) else {
        return Checked::unread("the file could not be read", false);
    };
    // A range that does not start at the beginning, or runs past the end, or goes backwards, does
    // not cover the document, whatever its digest says.
    let end = gap_end.checked_add(second);
    let Some(end) = end.filter(|&end| start == 0 && first > 0 && gap_end > first && end <= len)
    else {
        return Checked::broken("its byte range does not fit the file", false);
    };
    let saved_since = match read_at(&mut file, end, (len - end).min(16)) {
        Ok(tail) if len - end <= 16 => tail.iter().any(|b| !b.is_ascii_whitespace()),
        _ => true,
    };
    if gap_end - first > MAX_SIGNATURE_BYTES {
        return Checked::broken("its byte range leaves out too much", saved_since);
    }
    let Ok(gap) = read_at(&mut file, first, gap_end - first) else {
        return Checked::unread("the file could not be read", saved_since);
    };
    // What the range leaves out must be the hex string and nothing else: the rest of the file is
    // what the signature speaks for.
    let Some(blob) = hex_string(&gap) else {
        return Checked::broken(
            "its byte range leaves out more than the signature",
            saved_since,
        );
    };
    if sub_filter != b"adbe.pkcs7.detached" && sub_filter != b"ETSI.CAdES.detached" {
        return Checked::unread(
            format!(
                "the format {} is not read",
                String::from_utf8_lossy(sub_filter)
            ),
            saved_since,
        );
    }
    let signed = match parse(&blob) {
        Ok(signed) => signed,
        Err(why) => return Checked::unread(why, saved_since),
    };
    let certs: Vec<&Certificate> = signed
        .certificates
        .iter()
        .flat_map(|set| set.0.iter())
        .filter_map(|choice| match choice {
            CertificateChoices::Certificate(cert) => Some(cert),
            CertificateChoices::Other(_) => None,
        })
        .collect();
    let Some(info) = signed.signer_infos.0.iter().next() else {
        return Checked::unread("the signature names no signer", saved_since);
    };
    let Some(cert) = certs.iter().copied().find(|cert| is_signer(cert, info)) else {
        return Checked::unread(
            "the signer's certificate is not in the signature",
            saved_since,
        );
    };
    let signer = common_name(cert);
    let digest = check_digest(&mut file, [first, gap_end, second], info, cert);
    let certificate = if matches!(digest, DigestState::Broken(_)) {
        CertificateState::Unknown("the signature does not verify".to_owned())
    } else {
        check_chain(cert, &certs, roots, at)
    };
    Checked {
        digest,
        certificate,
        signer,
        saved_since,
    }
}

fn read_at(file: &mut File, at: u64, len: u64) -> std::io::Result<Vec<u8>> {
    file.seek(SeekFrom::Start(at))?;
    let mut out = vec![0; len as usize];
    file.read_exact(&mut out)?;
    Ok(out)
}

/// The bytes of a `<hex>` string, with nothing around it.
fn hex_string(gap: &[u8]) -> Option<Vec<u8>> {
    let inner = gap.strip_prefix(b"<")?.strip_suffix(b">")?;
    let digit = |c: u8| (c as char).to_digit(16).map(|d| d as u8);
    let digits: Vec<u8> = inner
        .iter()
        .filter(|c| !c.is_ascii_whitespace())
        .map(|&c| digit(c))
        .collect::<Option<_>>()?;
    Some(
        digits
            .chunks(2)
            .map(|pair| (pair[0] << 4) | pair.get(1).copied().unwrap_or(0))
            .collect(),
    )
}

fn parse(blob: &[u8]) -> Result<SignedData, String> {
    const UNREAD: &str = "the signature is not in a form this app reads";
    // The string has room to spare, filled with zeros: only the first value is the signature.
    let mut reader = SliceReader::new(blob).map_err(|_| UNREAD)?;
    let exact = reader.tlv_bytes().map_err(|_| UNREAD)?;
    let info = ContentInfo::from_der(exact).map_err(|_| UNREAD)?;
    if info.content_type != ID_DATA_SIGNED {
        return Err(UNREAD.to_owned());
    }
    info.content
        .decode_as::<SignedData>()
        .map_err(|_| UNREAD.to_owned())
}

fn is_signer(cert: &Certificate, info: &SignerInfo) -> bool {
    match &info.sid {
        SignerIdentifier::IssuerAndSerialNumber(IssuerAndSerialNumber {
            issuer,
            serial_number,
        }) => {
            cert.tbs_certificate.issuer == *issuer
                && cert.tbs_certificate.serial_number == *serial_number
        }
        SignerIdentifier::SubjectKeyIdentifier(id) => {
            cert.tbs_certificate.extensions.iter().flatten().any(|ext| {
                ext.extn_id == ID_SUBJECT_KEY_ID
                    && OctetString::from_der(ext.extn_value.as_bytes())
                        .is_ok_and(|value| value == id.0)
            })
        }
    }
}

/// A subject's common name; the string types certificates use.
fn common_name(cert: &Certificate) -> Option<String> {
    cert.tbs_certificate
        .subject
        .0
        .iter()
        .flat_map(|rdn| rdn.0.iter())
        .find(|attr| attr.oid == ID_COMMON_NAME)
        .and_then(|attr| {
            let value = attr.value.value();
            match attr.value.tag() {
                Tag::Utf8String | Tag::PrintableString | Tag::Ia5String => {
                    std::str::from_utf8(value).ok().map(str::to_owned)
                }
                Tag::TeletexString => Some(value.iter().map(|&b| b as char).collect()),
                Tag::BmpString => {
                    let units: Vec<u16> = value
                        .chunks_exact(2)
                        .map(|pair| u16::from_be_bytes([pair[0], pair[1]]))
                        .collect();
                    Some(String::from_utf16_lossy(&units))
                }
                _ => None,
            }
        })
}

fn digest_algorithm(oid: Oid) -> Option<&'static digest::Algorithm> {
    match oid {
        ID_SHA_1 => Some(&digest::SHA1_FOR_LEGACY_USE_ONLY),
        ID_SHA_256 => Some(&digest::SHA256),
        ID_SHA_384 => Some(&digest::SHA384),
        ID_SHA_512 => Some(&digest::SHA512),
        _ => None,
    }
}

/// The digest of the two parts of the file around the signature.
fn digest_ranges(
    file: &mut File,
    [first, gap_end, second]: [u64; 3],
    algorithm: &'static digest::Algorithm,
) -> std::io::Result<digest::Digest> {
    let mut context = digest::Context::new(algorithm);
    let mut buffer = vec![0u8; 1024 * 1024];
    for (at, len) in [(0, first), (gap_end, second)] {
        file.seek(SeekFrom::Start(at))?;
        let mut left = len;
        while left > 0 {
            let chunk = left.min(buffer.len() as u64) as usize;
            file.read_exact(&mut buffer[..chunk])?;
            context.update(&buffer[..chunk]);
            left -= chunk as u64;
        }
    }
    Ok(context.finish())
}

fn check_digest(
    file: &mut File,
    range: [u64; 3],
    info: &SignerInfo,
    cert: &Certificate,
) -> DigestState {
    let Some(algorithm) = digest_algorithm(info.digest_alg.oid) else {
        return DigestState::Unknown(format!("the digest {} is not read", info.digest_alg.oid));
    };
    // Every signer in these formats signs attributes holding the digest; without them the
    // signature is over the content itself, which only old formats do.
    let Some(attrs) = &info.signed_attrs else {
        return DigestState::Unknown("the signature has no signed attributes".to_owned());
    };
    let claimed = attrs
        .iter()
        .find(|attr| attr.oid == ID_MESSAGE_DIGEST)
        .and_then(|attr| attr.values.iter().next())
        .and_then(|value| value.decode_as::<OctetString>().ok());
    let Some(claimed) = claimed else {
        return DigestState::Unknown("the signature names no digest".to_owned());
    };
    let Ok(actual) = digest_ranges(file, range, algorithm) else {
        return DigestState::Unknown("the file could not be read".to_owned());
    };
    if actual.as_ref() != claimed.as_bytes() {
        return DigestState::Broken("the digest of the signed bytes does not match".to_owned());
    }
    let Ok(message) = attrs.to_der() else {
        return DigestState::Unknown("the signed attributes could not be read".to_owned());
    };
    match verify(
        &info.signature_algorithm,
        Some(info.digest_alg.oid),
        &cert.tbs_certificate.subject_public_key_info,
        &message,
        info.signature.as_bytes(),
    ) {
        Ok(()) => DigestState::Intact,
        Err(Failure::Bad) => DigestState::Broken("the signature does not verify".to_owned()),
        Err(Failure::Unsupported(why)) => DigestState::Unknown(why),
    }
}

enum Failure {
    Bad,
    Unsupported(String),
}

/// The hash RSA-PSS's parameters name: `[0] EXPLICIT AlgorithmIdentifier`, SHA-1 when left out.
fn pss_hash(algorithm: &AlgorithmIdentifierOwned) -> Option<Oid> {
    let Some(params) = &algorithm.parameters else {
        return Some(ID_SHA_1);
    };
    let mut reader = SliceReader::new(params.value()).ok()?;
    let hash = der::asn1::ContextSpecific::<AlgorithmIdentifierOwned>::decode_explicit(
        &mut reader,
        TagNumber::N0,
    )
    .ok()?;
    Some(hash.map_or(ID_SHA_1, |h| h.value.oid))
}

/// Verifies a signature. `digest_hint` is the digest for the algorithm identifiers that name only
/// the key type.
fn verify(
    algorithm: &AlgorithmIdentifierOwned,
    digest_hint: Option<Oid>,
    key: &SubjectPublicKeyInfoOwned,
    message: &[u8],
    signature: &[u8],
) -> Result<(), Failure> {
    use ring_signature as s;
    let key_bytes = key.subject_public_key.raw_bytes();
    let unsupported =
        || Failure::Unsupported(format!("the algorithm {} is not read", algorithm.oid));
    let chosen: &'static dyn s::VerificationAlgorithm = match (key.algorithm.oid, algorithm.oid) {
        (RSA_ENCRYPTION | ID_RSASSA_PSS, oid) if oid != ID_RSASSA_PSS => {
            let hash = match oid {
                RSA_ENCRYPTION => digest_hint.ok_or_else(unsupported)?,
                SHA_1_WITH_RSA_ENCRYPTION => ID_SHA_1,
                SHA_256_WITH_RSA_ENCRYPTION => ID_SHA_256,
                SHA_384_WITH_RSA_ENCRYPTION => ID_SHA_384,
                SHA_512_WITH_RSA_ENCRYPTION => ID_SHA_512,
                _ => return Err(unsupported()),
            };
            match hash {
                ID_SHA_1 => &s::RSA_PKCS1_1024_8192_SHA1_FOR_LEGACY_USE_ONLY,
                ID_SHA_256 => &s::RSA_PKCS1_1024_8192_SHA256_FOR_LEGACY_USE_ONLY,
                ID_SHA_384 => &s::RSA_PKCS1_2048_8192_SHA384,
                ID_SHA_512 => &s::RSA_PKCS1_1024_8192_SHA512_FOR_LEGACY_USE_ONLY,
                _ => return Err(unsupported()),
            }
        }
        (RSA_ENCRYPTION | ID_RSASSA_PSS, _) => match pss_hash(algorithm) {
            Some(ID_SHA_256) => &s::RSA_PSS_2048_8192_SHA256,
            Some(ID_SHA_384) => &s::RSA_PSS_2048_8192_SHA384,
            Some(ID_SHA_512) => &s::RSA_PSS_2048_8192_SHA512,
            _ => return Err(unsupported()),
        },
        (ID_EC_PUBLIC_KEY, oid) => {
            let hash = match oid {
                ID_EC_PUBLIC_KEY => digest_hint.ok_or_else(unsupported)?,
                ECDSA_WITH_SHA_256 => ID_SHA_256,
                ECDSA_WITH_SHA_384 => ID_SHA_384,
                _ => return Err(unsupported()),
            };
            let curve = key
                .algorithm
                .parameters
                .as_ref()
                .and_then(|params| params.decode_as::<Oid>().ok());
            match (curve, hash) {
                (Some(SECP_256_R_1), ID_SHA_256) => &s::ECDSA_P256_SHA256_ASN1,
                (Some(SECP_256_R_1), ID_SHA_384) => &s::ECDSA_P256_SHA384_ASN1,
                (Some(SECP_384_R_1), ID_SHA_256) => &s::ECDSA_P384_SHA256_ASN1,
                (Some(SECP_384_R_1), ID_SHA_384) => &s::ECDSA_P384_SHA384_ASN1,
                _ => return Err(unsupported()),
            }
        }
        _ => return Err(unsupported()),
    };
    s::UnparsedPublicKey::new(chosen, key_bytes)
        .verify(message, signature)
        .map_err(|_| Failure::Bad)
}

/// Whether `issuer`'s key signed `cert`.
fn issued_by(cert: &Certificate, issuer: &Certificate) -> bool {
    let Ok(tbs) = cert.tbs_certificate.to_der() else {
        return false;
    };
    verify(
        &cert.signature_algorithm,
        None,
        &issuer.tbs_certificate.subject_public_key_info,
        &tbs,
        cert.signature.raw_bytes(),
    )
    .is_ok()
}

fn valid_at(cert: &Certificate, at: u64) -> bool {
    let validity = &cert.tbs_certificate.validity;
    validity.not_before.to_unix_duration().as_secs() <= at
        && at <= validity.not_after.to_unix_duration().as_secs()
}

/// Whether every critical extension is one this code acts on, or one that takes nothing from a
/// signature's trust here.
fn extensions_understood(cert: &Certificate) -> bool {
    cert.tbs_certificate
        .extensions
        .iter()
        .flatten()
        .filter(|ext| ext.critical)
        .all(|ext| {
            [
                ID_BASIC_CONSTRAINTS,
                ID_KEY_USAGE,
                ID_EXT_KEY_USAGE,
                ID_CERT_POLICIES,
                ID_SUBJECT_ALT_NAME,
            ]
            .contains(&ext.extn_id)
        })
}

fn extension<'c, T: der::Decode<'c>>(cert: &'c Certificate, id: Oid) -> Option<T> {
    cert.tbs_certificate
        .extensions
        .iter()
        .flatten()
        .find(|ext| ext.extn_id == id)
        .and_then(|ext| T::from_der(ext.extn_value.as_bytes()).ok())
}

fn is_ca(cert: &Certificate) -> bool {
    extension::<BasicConstraints>(cert, ID_BASIC_CONSTRAINTS).is_some_and(|bc| bc.ca)
}

fn may_sign_documents(cert: &Certificate) -> bool {
    extension::<KeyUsage>(cert, ID_KEY_USAGE).is_none_or(|usage| {
        usage.0.contains(KeyUsages::DigitalSignature) || usage.0.contains(KeyUsages::NonRepudiation)
    })
}

fn check_chain(
    signer: &Certificate,
    embedded: &[&Certificate],
    roots: &Roots,
    at: u64,
) -> CertificateState {
    let untrusted = |why: &str| CertificateState::Untrusted(why.to_owned());
    if !may_sign_documents(signer) {
        return untrusted("it is not for signing documents");
    }
    if !valid_at(signer, at) {
        return untrusted("it was not valid when the document was signed");
    }
    if !extensions_understood(signer) {
        return untrusted("it has a critical extension this app does not read");
    }
    if roots.holds(signer) {
        return CertificateState::Trusted;
    }
    if leads_to_root(signer, embedded, roots, at, 0) {
        return CertificateState::Trusted;
    }
    if signer.tbs_certificate.issuer == signer.tbs_certificate.subject {
        return untrusted("it is self-signed");
    }
    untrusted("it does not lead to a root this computer trusts")
}

/// Whether a verified path runs from `cert` to a root.
fn leads_to_root(
    cert: &Certificate,
    embedded: &[&Certificate],
    roots: &Roots,
    at: u64,
    depth: usize,
) -> bool {
    if depth >= MAX_CHAIN {
        return false;
    }
    let names_issuer =
        |candidate: &&Certificate| candidate.tbs_certificate.subject == cert.tbs_certificate.issuer;
    // A root is taken as the anchor it is: an old one has no extensions to look at, and the store
    // decides its validity.
    if roots
        .0
        .iter()
        .filter(names_issuer)
        .any(|root| issued_by(cert, root))
    {
        return true;
    }
    embedded.iter().copied().filter(names_issuer).any(|issuer| {
        !std::ptr::eq(issuer, cert)
            && issuer.tbs_certificate.subject != issuer.tbs_certificate.issuer
            && is_ca(issuer)
            && valid_at(issuer, at)
            && extensions_understood(issuer)
            && issued_by(cert, issuer)
            && leads_to_root(issuer, embedded, roots, at, depth + 1)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_of_pem_lines() {
        assert_eq!(base64(b"aGVs\r\nbG8=\n").as_deref(), Some(&b"hello"[..]));
        assert_eq!(base64(b"a*b"), None);
    }

    #[test]
    fn the_gap_is_the_hex_string_alone() {
        assert_eq!(hex_string(b"<0aF>"), Some(vec![0x0a, 0xf0]));
        assert_eq!(hex_string(b"<0a\n0b>"), Some(vec![0x0a, 0x0b]));
        assert_eq!(hex_string(b" <0a>"), None);
        assert_eq!(hex_string(b"<0a>/M"), None);
        assert_eq!(hex_string(b"<0g>"), None);
    }

    #[test]
    fn nothing_trusted_from_nonsense() {
        assert!(Roots::from_pem(b"no certificates here").0.is_empty());
        assert!(
            Roots::from_pem(b"-----BEGIN CERTIFICATE-----\nAAAA\n-----END CERTIFICATE-----")
                .0
                .is_empty()
        );
    }
}
