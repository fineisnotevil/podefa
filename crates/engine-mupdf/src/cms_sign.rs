// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 FINE Association <su@fa.org.tr>

//! The CMS a signature carries (Plan 0009 Phase 4, ADR 0015): a detached `SignedData` over the
//! digest MuPDF hands over, in the PAdES baseline B-B form (ETSI EN 319 142-1 §6.3, CAdES EN 319
//! 122-1 §5): the signed attributes are the content type, the message digest and the signing
//! certificate (v2, by its SHA-256 hash), and there is no signing time, since the PDF's `/M`
//! carries it. The structures are RustCrypto's (`cms`, `x509-cert`, `der`), the arithmetic
//! `ring`'s, as for verifying (ADR 0014).

use crate::p12::{Identity, SigningKey};
use crate::verify::subject_part;
use cms::cert::{CertificateChoices, IssuerAndSerialNumber};
use cms::content_info::{CmsVersion, ContentInfo};
use cms::signed_data::{
    CertificateSet, EncapsulatedContentInfo, SignedData, SignerIdentifier, SignerInfo, SignerInfos,
};
use der::asn1::{Any, ObjectIdentifier as Oid, OctetString, SetOfVec};
use der::oid::db::rfc5912::{
    ECDSA_WITH_SHA_256, ECDSA_WITH_SHA_384, ID_SHA_256, ID_SHA_384, RSA_ENCRYPTION,
};
use der::{Decode as _, Encode as _};
use mupdf::pdf::sign::{CmsSigner, SignatureDigest, SignerName};
use ring::digest;
use ring::rand::SystemRandom;
use ring::signature::RSA_PKCS1_SHA256;
use x509_cert::Certificate;
use x509_cert::attr::Attribute;
use x509_cert::spki::AlgorithmIdentifierOwned;

const ID_DATA: Oid = Oid::new_unwrap("1.2.840.113549.1.7.1");
const ID_SIGNED_DATA: Oid = Oid::new_unwrap("1.2.840.113549.1.7.2");
const ID_CONTENT_TYPE: Oid = Oid::new_unwrap("1.2.840.113549.1.9.3");
const ID_MESSAGE_DIGEST: Oid = Oid::new_unwrap("1.2.840.113549.1.9.4");
const ID_SIGNING_CERTIFICATE_V2: Oid = Oid::new_unwrap("1.2.840.113549.1.9.16.2.47");
const ID_COMMON_NAME: Oid = Oid::new_unwrap("2.5.4.3");
const ID_COUNTRY: Oid = Oid::new_unwrap("2.5.4.6");
const ID_ORGANIZATION: Oid = Oid::new_unwrap("2.5.4.10");
const ID_UNIT: Oid = Oid::new_unwrap("2.5.4.11");
const ID_EMAIL: Oid = Oid::new_unwrap("1.2.840.113549.1.9.1");

/// Room for everything in the CMS but the certificates and the signature: the structure, the
/// algorithms, the attributes (about 300 bytes in practice), with a wide margin.
const OVERHEAD: usize = 2048;

/// Signs digests with one identity.
pub struct Cms {
    key: SigningKey,
    /// The signer's certificate first, then the others from the key file (its chain).
    certificates: Vec<Certificate>,
}

impl Cms {
    pub fn new(identity: Identity) -> Self {
        let Identity {
            key,
            certificate,
            others,
        } = identity;
        let mut certificates = vec![certificate];
        for other in others {
            if !certificates.contains(&other) {
                certificates.push(other);
            }
        }
        Self { key, certificates }
    }

    pub fn certificate(&self) -> &Certificate {
        &self.certificates[0]
    }

    /// The hash of the signed bytes: SHA-384 for a P-384 key, SHA-256 otherwise.
    pub fn digest(&self) -> SignatureDigest {
        match self.key {
            SigningKey::P384(_) => SignatureDigest::Sha384,
            SigningKey::Rsa(_) | SigningKey::P256(_) => SignatureDigest::Sha256,
        }
    }

    /// The most bytes [`Self::build`] returns.
    pub fn max_size(&self) -> usize {
        let certificates: usize = self
            .certificates
            .iter()
            .map(|c| c.encoded_len().map_or(0, |len| u32::from(len) as usize))
            .sum();
        let signature = match &self.key {
            SigningKey::Rsa(key) => key.public().modulus_len(),
            // DER of two integers of up to 49 bytes each.
            SigningKey::P256(_) | SigningKey::P384(_) => 2 * 49 + 8,
        };
        // The certificate's hash and issuer go into the signing-certificate attribute again.
        let issuer = self
            .certificate()
            .tbs_certificate
            .issuer
            .encoded_len()
            .map_or(0, |len| u32::from(len) as usize);
        certificates + signature + issuer + OVERHEAD
    }

    /// The signer's name parts, for MuPDF's appearance.
    pub fn name(&self) -> SignerName {
        let cert = self.certificate();
        SignerName {
            common_name: subject_part(cert, ID_COMMON_NAME),
            organization: subject_part(cert, ID_ORGANIZATION),
            unit: subject_part(cert, ID_UNIT),
            email: subject_part(cert, ID_EMAIL),
            country: subject_part(cert, ID_COUNTRY),
        }
    }

    /// The DER of a `ContentInfo` holding the `SignedData` for `message_digest`, the hash of the
    /// signed bytes with [`Self::digest`].
    pub fn build(&self, message_digest: &[u8]) -> Result<Vec<u8>, String> {
        let fail = |what: &'static str| move |e: der::Error| format!("{what}: {e}");
        let (digest_oid, digest_len) = match self.digest() {
            SignatureDigest::Sha384 => (ID_SHA_384, 48),
            _ => (ID_SHA_256, 32),
        };
        if message_digest.len() != digest_len {
            return Err("the digest has the wrong length".to_owned());
        }
        let digest_alg = AlgorithmIdentifierOwned {
            oid: digest_oid,
            parameters: None,
        };
        let leaf = self.certificate();
        let attributes = vec![
            attribute(
                ID_CONTENT_TYPE,
                Any::encode_from(&ID_DATA).map_err(fail("content type"))?,
            )?,
            attribute(
                ID_MESSAGE_DIGEST,
                Any::encode_from(&OctetString::new(message_digest).map_err(fail("digest"))?)
                    .map_err(fail("digest"))?,
            )?,
            attribute(ID_SIGNING_CERTIFICATE_V2, signing_certificate_v2(leaf)?)?,
        ];
        let signed_attrs = SetOfVec::try_from(attributes).map_err(fail("attributes"))?;
        // The signature covers the attributes as a SET (RFC 5652 §5.4), not as the [0] they are
        // written in.
        let to_sign = signed_attrs.to_der().map_err(fail("attributes"))?;
        let (signature_algorithm, signature) = self.sign_bytes(&to_sign)?;
        let info = SignerInfo {
            version: CmsVersion::V1,
            sid: SignerIdentifier::IssuerAndSerialNumber(IssuerAndSerialNumber {
                issuer: leaf.tbs_certificate.issuer.clone(),
                serial_number: leaf.tbs_certificate.serial_number.clone(),
            }),
            digest_alg: digest_alg.clone(),
            signed_attrs: Some(signed_attrs),
            signature_algorithm,
            signature: OctetString::new(signature).map_err(fail("signature"))?,
            unsigned_attrs: None,
        };
        let certificates = SetOfVec::try_from(
            self.certificates
                .iter()
                .cloned()
                .map(CertificateChoices::Certificate)
                .collect::<Vec<_>>(),
        )
        .map_err(fail("certificates"))?;
        let signed = SignedData {
            version: CmsVersion::V1,
            digest_algorithms: SetOfVec::try_from(vec![digest_alg]).map_err(fail("algorithms"))?,
            encap_content_info: EncapsulatedContentInfo {
                econtent_type: ID_DATA,
                econtent: None,
            },
            certificates: Some(CertificateSet(certificates)),
            crls: None,
            signer_infos: SignerInfos(SetOfVec::try_from(vec![info]).map_err(fail("signer"))?),
        };
        ContentInfo {
            content_type: ID_SIGNED_DATA,
            content: Any::encode_from(&signed).map_err(fail("signed data"))?,
        }
        .to_der()
        .map_err(fail("content info"))
    }

    /// The signature over `message` and its algorithm identifier.
    fn sign_bytes(&self, message: &[u8]) -> Result<(AlgorithmIdentifierOwned, Vec<u8>), String> {
        let rng = SystemRandom::new();
        match &self.key {
            SigningKey::Rsa(key) => {
                let mut signature = vec![0; key.public().modulus_len()];
                key.sign(&RSA_PKCS1_SHA256, &rng, message, &mut signature)
                    .map_err(|_| "the RSA signature could not be made".to_owned())?;
                // rsaEncryption with NULL parameters: the form every reader takes (RFC 3370 §3.2).
                let algorithm = AlgorithmIdentifierOwned {
                    oid: RSA_ENCRYPTION,
                    parameters: Some(Any::null()),
                };
                Ok((algorithm, signature))
            }
            SigningKey::P256(key) | SigningKey::P384(key) => {
                let signature = key
                    .sign(&rng, message)
                    .map_err(|_| "the ECDSA signature could not be made".to_owned())?;
                let oid = if matches!(self.key, SigningKey::P384(_)) {
                    ECDSA_WITH_SHA_384
                } else {
                    ECDSA_WITH_SHA_256
                };
                let algorithm = AlgorithmIdentifierOwned {
                    oid,
                    parameters: None,
                };
                Ok((algorithm, signature.as_ref().to_vec()))
            }
        }
    }
}

impl CmsSigner for Cms {
    fn sign(&self, digest: &[u8]) -> Option<Vec<u8>> {
        self.build(digest).ok()
    }
}

fn attribute(oid: Oid, value: Any) -> Result<Attribute, String> {
    Ok(Attribute {
        oid,
        values: SetOfVec::try_from(vec![value]).map_err(|e| format!("attribute: {e}"))?,
    })
}

/// A DER element: `tag`, the length, `content`.
fn tlv(tag: u8, content: &[u8]) -> Vec<u8> {
    let mut out = vec![tag];
    let len = content.len();
    if len < 0x80 {
        out.push(len as u8);
    } else {
        let bytes = len.to_be_bytes();
        let skip = bytes.iter().take_while(|&&b| b == 0).count();
        out.push(0x80 | (bytes.len() - skip) as u8);
        out.extend_from_slice(&bytes[skip..]);
    }
    out.extend_from_slice(content);
    out
}

/// `SigningCertificateV2` (RFC 5035 §3) naming `cert` by its SHA-256 hash and its issuer and
/// serial number:
///
/// ```text
/// SigningCertificateV2 ::= SEQUENCE { certs SEQUENCE OF ESSCertIDv2 }
/// ESSCertIDv2 ::= SEQUENCE { hashAlgorithm DEFAULT sha256 (left out), certHash OCTET STRING,
///                            issuerSerial SEQUENCE { issuer GeneralNames, serialNumber INTEGER } }
/// ```
fn signing_certificate_v2(cert: &Certificate) -> Result<Any, String> {
    let fail = |e: der::Error| format!("signing certificate: {e}");
    let der = cert.to_der().map_err(fail)?;
    let hash = digest::digest(&digest::SHA256, &der);
    let issuer = cert.tbs_certificate.issuer.to_der().map_err(fail)?;
    let serial = cert.tbs_certificate.serial_number.to_der().map_err(fail)?;
    // GeneralNames ::= SEQUENCE OF GeneralName; directoryName is [4], explicit since Name is a
    // CHOICE (RFC 5280 §4.2.1.6).
    let general_names = tlv(0x30, &tlv(0xA4, &issuer));
    let issuer_serial = tlv(0x30, &[general_names, serial].concat());
    let cert_id = tlv(0x30, &[tlv(0x04, hash.as_ref()), issuer_serial].concat());
    let value = tlv(0x30, &tlv(0x30, &cert_id));
    Any::from_der(&value).map_err(fail)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tlv_writes_short_and_long_lengths() {
        assert_eq!(tlv(0x04, &[1, 2]), vec![0x04, 2, 1, 2]);
        let long = tlv(0x04, &[0; 300]);
        assert_eq!(&long[..4], &[0x04, 0x82, 0x01, 0x2C]);
        assert_eq!(long.len(), 304);
    }
}
