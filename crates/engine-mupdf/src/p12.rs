// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 FINE Association <su@fa.org.tr>

//! Reading a signing identity from a PKCS#12 file (`.p12`, `.pfx`; RFC 7292), Plan 0009 Phase 4,
//! ADR 0015.
//!
//! The structures come from RustCrypto's `pkcs12`, the modern encryption (PBES2: PBKDF2 with AES)
//! from `pkcs5`, and the legacy one (PKCS#12 PBE with 3DES or RC2, what older Windows and OpenSSL
//! 1.x export) from `des`, `rc2` and `cbc` with `pkcs12`'s key derivation. The file's MAC tells a
//! wrong password from a damaged file. The key is matched to its certificate by public key, so a
//! file with several certificates (the chain) gives the right one.

use cms::content_info::ContentInfo;
use cms::encrypted_data::EncryptedData;
use der::asn1::{ContextSpecific, ObjectIdentifier as Oid, OctetString};
use der::{Decode as _, Encode as _, Reader as _, SliceReader};
use pkcs12::kdf::{Pkcs12KeyType, derive_key, derive_key_utf8};
use pkcs12::pbe_params::{EncryptedPrivateKeyInfo, Pkcs12PbeParams};
use pkcs12::pfx::Pfx;
use pkcs12::safe_bag::{SafeBag, SafeContents};
use pkcs12::{
    PKCS_12_CERT_BAG_OID, PKCS_12_KEY_BAG_OID, PKCS_12_PBE_WITH_SHAAND3_KEY_TRIPLE_DES_CBC,
    PKCS_12_PBE_WITH_SHAAND128_BIT_RC2_CBC, PKCS_12_PBEWITH_SHAAND40_BIT_RC2_CBC,
    PKCS_12_PKCS8_KEY_BAG_OID, PKCS_12_X509_CERT_OID,
};
use ring::hmac;
use ring::rand::SystemRandom;
use ring::signature::{
    ECDSA_P256_SHA256_ASN1_SIGNING, ECDSA_P384_SHA384_ASN1_SIGNING, EcdsaKeyPair, RsaKeyPair,
};
use thiserror::Error;
use x509_cert::Certificate;
use x509_cert::spki::AlgorithmIdentifierOwned;

const ID_DATA: Oid = Oid::new_unwrap("1.2.840.113549.1.7.1");
const ID_ENCRYPTED_DATA: Oid = Oid::new_unwrap("1.2.840.113549.1.7.6");
const ID_PBES2: Oid = Oid::new_unwrap("1.2.840.113549.1.5.13");
const ID_SHA1: Oid = Oid::new_unwrap("1.3.14.3.2.26");
const ID_SHA256: Oid = Oid::new_unwrap("2.16.840.1.101.3.4.2.1");
const ID_SHA384: Oid = Oid::new_unwrap("2.16.840.1.101.3.4.2.2");
const ID_SHA512: Oid = Oid::new_unwrap("2.16.840.1.101.3.4.2.3");
const ID_RSA: Oid = Oid::new_unwrap("1.2.840.113549.1.1.1");
const ID_EC_PUBLIC_KEY: Oid = Oid::new_unwrap("1.2.840.10045.2.1");
const ID_P256: Oid = Oid::new_unwrap("1.2.840.10045.3.1.7");
const ID_P384: Oid = Oid::new_unwrap("1.3.132.0.34");

/// More is not a key store but something else: refused before any work.
const MAX_FILE_BYTES: usize = 1024 * 1024;
/// A KDF iteration count past this is an attack on the CPU, not a real file (OpenSSL writes 2048).
const MAX_ITERATIONS: i32 = 10_000_000;

/// Why a key file could not be used.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum P12Error {
    #[error("this is not a PKCS#12 key file")]
    NotPkcs12,
    #[error("the password is wrong")]
    WrongPassword,
    #[error("the key file holds no private key")]
    NoKey,
    #[error("the key file holds no certificate for its key")]
    NoCertificate,
    #[error("the key file uses something this app does not read: {0}")]
    Unsupported(String),
}

/// The private key, ready to sign.
pub enum SigningKey {
    Rsa(RsaKeyPair),
    P256(EcdsaKeyPair),
    P384(EcdsaKeyPair),
}

impl std::fmt::Debug for SigningKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Rsa(_) => "Rsa",
            Self::P256(_) => "P256",
            Self::P384(_) => "P384",
        })
    }
}

/// What a key file gives: the key, its certificate, and the other certificates in the file
/// (normally the chain up to the root).
#[derive(Debug)]
pub struct Identity {
    pub key: SigningKey,
    pub certificate: Certificate,
    pub others: Vec<Certificate>,
}

/// Reads `bytes` (a `.p12`/`.pfx` file) with `password`.
pub fn read(bytes: &[u8], password: &str) -> Result<Identity, P12Error> {
    if bytes.len() > MAX_FILE_BYTES {
        return Err(P12Error::NotPkcs12);
    }
    let pfx = Pfx::from_der(bytes).map_err(|_| P12Error::NotPkcs12)?;
    if pfx.auth_safe.content_type != ID_DATA {
        // Public-key integrity mode (id-signedData) is not used by any exporter we know.
        return Err(P12Error::Unsupported(
            "a key file signed instead of password-protected".into(),
        ));
    }
    let auth_safe = pfx
        .auth_safe
        .content
        .decode_as::<OctetString>()
        .map_err(|_| P12Error::NotPkcs12)?;
    let auth_safe = auth_safe.as_bytes();
    // OpenSSL's empty password is either no bytes or the BMP terminator alone; try both.
    let empty = password.is_empty();
    let mut legacy_password = Password::Utf8(password);
    if let Some(mac) = &pfx.mac_data {
        if !check_mac(mac, auth_safe, Password::Utf8(password))? {
            if empty && check_mac(mac, auth_safe, Password::None)? {
                legacy_password = Password::None;
            } else {
                return Err(P12Error::WrongPassword);
            }
        }
    }
    let contents = Vec::<ContentInfo>::from_der(auth_safe).map_err(|_| P12Error::NotPkcs12)?;
    let mut keys = Vec::new();
    let mut certificates = Vec::new();
    for content in contents {
        let bags = if content.content_type == ID_DATA {
            let data = content
                .content
                .decode_as::<OctetString>()
                .map_err(|_| P12Error::NotPkcs12)?;
            data.as_bytes().to_vec()
        } else if content.content_type == ID_ENCRYPTED_DATA {
            let data = content
                .content
                .decode_as::<EncryptedData>()
                .map_err(|_| P12Error::NotPkcs12)?;
            let info = data.enc_content_info;
            let Some(ciphertext) = info.encrypted_content else {
                continue;
            };
            decrypt(
                &info.content_enc_alg,
                ciphertext.as_bytes(),
                password,
                legacy_password,
            )?
        } else {
            // Enveloped (public-key privacy mode): not written by any exporter we know.
            continue;
        };
        let bags = SafeContents::from_der(&bags).map_err(|_| P12Error::WrongPassword)?;
        collect(
            bags,
            password,
            legacy_password,
            &mut keys,
            &mut certificates,
            0,
        )?;
    }
    if keys.is_empty() {
        return Err(P12Error::NoKey);
    }
    let mut unsupported = None;
    for key in &keys {
        for (index, certificate) in certificates.iter().enumerate() {
            match key_for(key, certificate) {
                Ok(Some(key)) => {
                    let certificate = certificates.remove(index);
                    return Ok(Identity {
                        key,
                        certificate,
                        others: certificates,
                    });
                }
                Ok(None) => {}
                Err(error) => unsupported = Some(error),
            }
        }
    }
    Err(unsupported.unwrap_or(P12Error::NoCertificate))
}

#[derive(Clone, Copy)]
enum Password<'p> {
    Utf8(&'p str),
    /// No bytes at all, not even the BMP terminator.
    None,
}

fn kdf_key(
    digest: Oid,
    password: Password<'_>,
    salt: &[u8],
    kind: Pkcs12KeyType,
    rounds: i32,
    len: usize,
) -> Result<Vec<u8>, P12Error> {
    if !(1..=MAX_ITERATIONS).contains(&rounds) {
        return Err(P12Error::Unsupported(format!("{rounds} iterations")));
    }
    let bad = |_| P12Error::Unsupported("a password that is not Unicode text".into());
    match password {
        Password::Utf8(text) => match digest {
            ID_SHA1 => derive_key_utf8::<sha1::Sha1>(text, salt, kind, rounds, len).map_err(bad),
            ID_SHA256 => {
                derive_key_utf8::<sha2::Sha256>(text, salt, kind, rounds, len).map_err(bad)
            }
            ID_SHA384 => {
                derive_key_utf8::<sha2::Sha384>(text, salt, kind, rounds, len).map_err(bad)
            }
            ID_SHA512 => {
                derive_key_utf8::<sha2::Sha512>(text, salt, kind, rounds, len).map_err(bad)
            }
            other => Err(P12Error::Unsupported(format!("the digest {other}"))),
        },
        Password::None => match digest {
            ID_SHA1 => Ok(derive_key::<sha1::Sha1>(&[], salt, kind, rounds, len)),
            ID_SHA256 => Ok(derive_key::<sha2::Sha256>(&[], salt, kind, rounds, len)),
            ID_SHA384 => Ok(derive_key::<sha2::Sha384>(&[], salt, kind, rounds, len)),
            ID_SHA512 => Ok(derive_key::<sha2::Sha512>(&[], salt, kind, rounds, len)),
            other => Err(P12Error::Unsupported(format!("the digest {other}"))),
        },
    }
}

/// Whether the file's MAC matches with `password`.
fn check_mac(
    mac: &pkcs12::mac_data::MacData,
    data: &[u8],
    password: Password<'_>,
) -> Result<bool, P12Error> {
    let (algorithm, len) = match mac.mac.algorithm.oid {
        ID_SHA1 => (hmac::HMAC_SHA1_FOR_LEGACY_USE_ONLY, 20),
        ID_SHA256 => (hmac::HMAC_SHA256, 32),
        ID_SHA384 => (hmac::HMAC_SHA384, 48),
        ID_SHA512 => (hmac::HMAC_SHA512, 64),
        other => return Err(P12Error::Unsupported(format!("the MAC {other}"))),
    };
    let key = kdf_key(
        mac.mac.algorithm.oid,
        password,
        mac.mac_salt.as_bytes(),
        Pkcs12KeyType::Mac,
        mac.iterations,
        len,
    )?;
    Ok(hmac::verify(
        &hmac::Key::new(algorithm, &key),
        data,
        mac.mac.digest.as_bytes(),
    )
    .is_ok())
}

/// Decrypts a bag or a safe; a padding error is a wrong password (the MAC, when there is one, has
/// already said so).
fn decrypt(
    algorithm: &AlgorithmIdentifierOwned,
    ciphertext: &[u8],
    password: &str,
    legacy_password: Password<'_>,
) -> Result<Vec<u8>, P12Error> {
    use cbc::cipher::block_padding::Pkcs7;
    use cbc::cipher::{BlockDecryptMut as _, InnerIvInit as _, KeyIvInit as _};
    if algorithm.oid == ID_PBES2 {
        let der = algorithm.to_der().map_err(|_| P12Error::NotPkcs12)?;
        let scheme = pkcs5::EncryptionScheme::try_from(der.as_slice())
            .map_err(|_| P12Error::Unsupported("this PBES2 encryption".into()))?;
        return scheme
            .decrypt(password.as_bytes(), ciphertext)
            .map_err(|_| P12Error::WrongPassword);
    }
    let params = algorithm
        .parameters
        .as_ref()
        .ok_or(P12Error::NotPkcs12)?
        .decode_as::<Pkcs12PbeParams>()
        .map_err(|_| P12Error::NotPkcs12)?;
    let salt = params.salt.as_bytes();
    let derive = |kind, len| kdf_key(ID_SHA1, legacy_password, salt, kind, params.iterations, len);
    let wrong = |_| P12Error::WrongPassword;
    match algorithm.oid {
        PKCS_12_PBE_WITH_SHAAND3_KEY_TRIPLE_DES_CBC => {
            let key = derive(Pkcs12KeyType::EncryptionKey, 24)?;
            let iv = derive(Pkcs12KeyType::Iv, 8)?;
            cbc::Decryptor::<des::TdesEde3>::new_from_slices(&key, &iv)
                .map_err(|_| P12Error::NotPkcs12)?
                .decrypt_padded_vec_mut::<Pkcs7>(ciphertext)
                .map_err(wrong)
        }
        PKCS_12_PBE_WITH_SHAAND128_BIT_RC2_CBC | PKCS_12_PBEWITH_SHAAND40_BIT_RC2_CBC => {
            let bits = if algorithm.oid == PKCS_12_PBE_WITH_SHAAND128_BIT_RC2_CBC {
                128
            } else {
                40
            };
            let key = derive(Pkcs12KeyType::EncryptionKey, bits / 8)?;
            let iv = derive(Pkcs12KeyType::Iv, 8)?;
            let cipher = rc2::Rc2::new_with_eff_key_len(&key, bits);
            cbc::Decryptor::<rc2::Rc2>::inner_iv_slice_init(cipher, &iv)
                .map_err(|_| P12Error::NotPkcs12)?
                .decrypt_padded_vec_mut::<Pkcs7>(ciphertext)
                .map_err(wrong)
        }
        other => Err(P12Error::Unsupported(format!("the encryption {other}"))),
    }
}

/// The private keys (PKCS#8 DER) and certificates in `bags`; nested safes are followed a few
/// levels down.
fn collect(
    bags: Vec<SafeBag>,
    password: &str,
    legacy_password: Password<'_>,
    keys: &mut Vec<Vec<u8>>,
    certificates: &mut Vec<Certificate>,
    depth: u32,
) -> Result<(), P12Error> {
    const SAFE_CONTENTS_BAG: Oid = Oid::new_unwrap("1.2.840.113549.1.12.10.1.6");
    if depth > 4 {
        return Ok(());
    }
    for bag in bags {
        match bag.bag_id {
            PKCS_12_KEY_BAG_OID => {
                let value: ContextSpecific<der::Any> =
                    ContextSpecific::from_der(&bag.bag_value).map_err(|_| P12Error::NotPkcs12)?;
                keys.push(value.value.to_der().map_err(|_| P12Error::NotPkcs12)?);
            }
            PKCS_12_PKCS8_KEY_BAG_OID => {
                let value: ContextSpecific<EncryptedPrivateKeyInfo> =
                    ContextSpecific::from_der(&bag.bag_value).map_err(|_| P12Error::NotPkcs12)?;
                let info = value.value;
                keys.push(decrypt(
                    &info.encryption_algorithm,
                    info.encrypted_data.as_bytes(),
                    password,
                    legacy_password,
                )?);
            }
            PKCS_12_CERT_BAG_OID => {
                let value: ContextSpecific<pkcs12::cert_type::CertBag> =
                    ContextSpecific::from_der(&bag.bag_value).map_err(|_| P12Error::NotPkcs12)?;
                if value.value.cert_id == PKCS_12_X509_CERT_OID {
                    if let Ok(certificate) =
                        Certificate::from_der(value.value.cert_value.as_bytes())
                    {
                        certificates.push(certificate);
                    }
                }
            }
            SAFE_CONTENTS_BAG => {
                let value: ContextSpecific<SafeContents> =
                    ContextSpecific::from_der(&bag.bag_value).map_err(|_| P12Error::NotPkcs12)?;
                collect(
                    value.value,
                    password,
                    legacy_password,
                    keys,
                    certificates,
                    depth + 1,
                )?;
            }
            _ => {}
        }
    }
    Ok(())
}

/// A PKCS#8 `PrivateKeyInfo` (v1) or `OneAsymmetricKey` (v2): the algorithm and the key.
fn private_key_info(der: &[u8]) -> Result<(AlgorithmIdentifierOwned, Vec<u8>), P12Error> {
    let mut reader = SliceReader::new(der).map_err(|_| P12Error::NoKey)?;
    reader
        .sequence(|r| {
            let _version: u8 = r.decode()?;
            let algorithm: AlgorithmIdentifierOwned = r.decode()?;
            let key: OctetString = r.decode()?;
            while !r.is_finished() {
                let _: der::Any = r.decode()?;
            }
            Ok((algorithm, key.as_bytes().to_vec()))
        })
        .map_err(|_| P12Error::NoKey)
}

/// The key in `pkcs8` as a signer, when `certificate` holds its public key.
fn key_for(pkcs8: &[u8], certificate: &Certificate) -> Result<Option<SigningKey>, P12Error> {
    let (algorithm, private_key) = private_key_info(pkcs8)?;
    let spki = &certificate.tbs_certificate.subject_public_key_info;
    if spki.algorithm.oid != algorithm.oid {
        return Ok(None);
    }
    let public = spki.subject_public_key.raw_bytes();
    match algorithm.oid {
        ID_RSA => {
            let key = RsaKeyPair::from_pkcs8(pkcs8)
                .map_err(|e| P12Error::Unsupported(format!("this RSA key ({e})")))?;
            // The certificate's key is an RSAPublicKey; ring's is the same DER.
            Ok((key.public().as_ref() == public).then_some(SigningKey::Rsa(key)))
        }
        ID_EC_PUBLIC_KEY => {
            let curve = algorithm
                .parameters
                .as_ref()
                .and_then(|p| p.decode_as::<Oid>().ok())
                .ok_or_else(|| P12Error::Unsupported("an EC key without a named curve".into()))?;
            let (signing, size) = match curve {
                ID_P256 => (&ECDSA_P256_SHA256_ASN1_SIGNING, 32),
                ID_P384 => (&ECDSA_P384_SHA384_ASN1_SIGNING, 48),
                other => return Err(P12Error::Unsupported(format!("the curve {other}"))),
            };
            // ring wants the public key inside the PKCS#8 key, which many files leave out: the
            // scalar comes from the key and the point from the certificate, and ring checks
            // they belong together.
            let scalar = ec_scalar(&private_key, size)?;
            let rng = SystemRandom::new();
            let Ok(key) =
                EcdsaKeyPair::from_private_key_and_public_key(signing, &scalar, public, &rng)
            else {
                return Ok(None);
            };
            Ok(Some(if size == 32 {
                SigningKey::P256(key)
            } else {
                SigningKey::P384(key)
            }))
        }
        other => Err(P12Error::Unsupported(format!("the key type {other}"))),
    }
}

/// The private scalar of an `ECPrivateKey` (RFC 5915), left-padded to `size` bytes.
fn ec_scalar(der: &[u8], size: usize) -> Result<Vec<u8>, P12Error> {
    let bad = |_| P12Error::Unsupported("this EC key".into());
    let mut reader = SliceReader::new(der).map_err(bad)?;
    let raw = reader
        .sequence(|r| {
            let _version: u8 = r.decode()?;
            let key: OctetString = r.decode()?;
            while !r.is_finished() {
                let _: der::Any = r.decode()?;
            }
            Ok(key.as_bytes().to_vec())
        })
        .map_err(bad)?;
    if raw.len() > size {
        return Err(P12Error::Unsupported("this EC key".into()));
    }
    let mut scalar = vec![0; size - raw.len()];
    scalar.extend_from_slice(&raw);
    Ok(scalar)
}
