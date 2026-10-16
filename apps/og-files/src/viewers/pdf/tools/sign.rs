//! Certificate signatures: PAdES B-B signing and signature validation, in
//! pure Rust (RustCrypto: `rsa`, `p256`, `p384`, `sha1`/`sha2`, `der`,
//! `x509-cert`, `p12-keystore`). Replaces Stirling's cert-sign and
//! validate-signature endpoints.
//!
//! **Signing** ([`sign`]):
//!   * credentials: PKCS#12 (`.p12`/`.pfx`, legacy 3DES/RC2 or AES-256
//!     PBES2) with its password, or PEM (certificate + `PRIVATE KEY`,
//!     `ENCRYPTED PRIVATE KEY`, `RSA PRIVATE KEY` or `EC PRIVATE KEY`, in
//!     one file or a separate key file). Keys: RSA (PKCS#1 v1.5 with
//!     SHA-256), ECDSA P-256 (SHA-256) and P-384 (SHA-384).
//!   * written as an **incremental update** (the original bytes stay intact,
//!     so earlier signatures remain valid): a `/FT /Sig` field merged with
//!     its widget (invisible, or a visible box with signer / date / reason
//!     / location text), `/SigFlags 3`, a signature dictionary with
//!     `/SubFilter /ETSI.CAdES.detached`, `/ByteRange` and a fixed-width
//!     `/Contents` reserve that is filled after hashing.
//!   * CMS SignedData (detached) with signed attributes content-type,
//!     message-digest and signing-certificate-v2 (ESS, SHA-256 cert hash +
//!     issuer/serial); the signer certificate and any chain from the
//!     credential file are embedded. Following PAdES (ETSI EN 319 142-1)
//!     the claimed signing time goes in the signature dictionary's `/M`,
//!     not in a CMS signing-time attribute.
//!   * no timestamp (B-T) — that needs a TSA on the network.
//!
//! **Validation** ([`validate`]): every signature dictionary reachable from
//! the form fields (or lying loose in the file) is checked: byte range
//! sanity, digest of the signed ranges vs. the message-digest attribute,
//! the CMS signature with the signer's public key (RSA PKCS#1 v1.5 / PSS,
//! ECDSA P-256 / P-384; SHA-1/256/384/512), content-type attribute,
//! whether the signature covers the whole file, signer / issuer / validity,
//! self-signed, embedded chain (each link's signature checked), signing
//! time and an RFC 3161 timestamp token if present (its own CMS signature
//! and message imprint are checked too). Trust is **not** evaluated: there
//! is no certificate store, so a valid signature only proves the file is
//! unchanged since the holder of that certificate signed it.

use base64::Engine as _;
use lopdf::{dictionary, Dictionary, Document, IncrementalDocument, Object, ObjectId, Stream};
use memchr::memmem;
use sha2::Digest;

// ------------------------------------------------------------------ DER

/// One DER/BER element (single-byte tags only, which covers CMS/X.509).
#[derive(Clone, Copy)]
struct Tlv<'a> {
    tag: u8,
    content: &'a [u8],
    /// the whole element, header included
    raw: &'a [u8],
}

fn read_tlv(buf: &[u8]) -> Option<(Tlv<'_>, &[u8])> {
    let tag = *buf.first()?;
    if tag & 0x1f == 0x1f {
        return None; // multi-byte tags aren't used here
    }
    let l0 = *buf.get(1)?;
    let (len, hl) = if l0 < 0x80 {
        (l0 as usize, 2)
    } else if l0 == 0x80 {
        // BER indefinite length (constructed only): walk to end-of-contents
        if tag & 0x20 == 0 {
            return None;
        }
        let mut rest = &buf[2..];
        let mut used = 0;
        loop {
            if rest.len() >= 2 && rest[0] == 0 && rest[1] == 0 {
                let content = &buf[2..2 + used];
                let total = 2 + used + 2;
                return Some((
                    Tlv {
                        tag,
                        content,
                        raw: &buf[..total],
                    },
                    &buf[total..],
                ));
            }
            let (t, r) = read_tlv(rest)?;
            used += t.raw.len();
            rest = r;
        }
    } else {
        let n = (l0 & 0x7f) as usize;
        if n > 4 || buf.len() < 2 + n {
            return None;
        }
        let mut len = 0usize;
        for &b in &buf[2..2 + n] {
            len = (len << 8) | b as usize;
        }
        (len, 2 + n)
    };
    let end = hl.checked_add(len)?;
    if buf.len() < end {
        return None;
    }
    Some((
        Tlv {
            tag,
            content: &buf[hl..end],
            raw: &buf[..end],
        },
        &buf[end..],
    ))
}

fn children(content: &[u8]) -> Vec<Tlv<'_>> {
    let mut out = Vec::new();
    let mut rest = content;
    while !rest.is_empty() {
        match read_tlv(rest) {
            Some((t, r)) => {
                out.push(t);
                rest = r;
            }
            None => break,
        }
    }
    out
}

/// OCTET STRING content, joining BER constructed segments.
fn octets(t: &Tlv) -> Vec<u8> {
    if t.tag == 0x24 {
        children(t.content).iter().flat_map(octets).collect()
    } else {
        t.content.to_vec()
    }
}

fn der_len(n: usize) -> Vec<u8> {
    if n < 0x80 {
        vec![n as u8]
    } else {
        let b = n.to_be_bytes();
        let skip = b.iter().take_while(|&&x| x == 0).count();
        let mut v = vec![0x80 | (b.len() - skip) as u8];
        v.extend_from_slice(&b[skip..]);
        v
    }
}

fn tlv(tag: u8, content: &[u8]) -> Vec<u8> {
    let mut v = vec![tag];
    v.extend(der_len(content.len()));
    v.extend_from_slice(content);
    v
}

fn seq(parts: &[&[u8]]) -> Vec<u8> {
    tlv(0x30, &parts.concat())
}

/// DER SET OF: elements sorted by their encodings.
fn set_of(mut parts: Vec<Vec<u8>>) -> Vec<u8> {
    parts.sort();
    tlv(0x31, &parts.concat())
}

fn oid(s: &str) -> Vec<u8> {
    let o = der::asn1::ObjectIdentifier::new_unwrap(s);
    tlv(0x06, o.as_bytes())
}

fn oid_str(t: &Tlv) -> String {
    if t.tag != 0x06 {
        return String::new();
    }
    der::asn1::ObjectIdentifier::from_bytes(t.content)
        .map(|o| o.to_string())
        .unwrap_or_default()
}

const OID_DATA: &str = "1.2.840.113549.1.7.1";
const OID_SIGNED_DATA: &str = "1.2.840.113549.1.7.2";
const OID_CONTENT_TYPE: &str = "1.2.840.113549.1.9.3";
const OID_MESSAGE_DIGEST: &str = "1.2.840.113549.1.9.4";
const OID_SIGNING_TIME: &str = "1.2.840.113549.1.9.5";
const OID_SIGNING_CERT_V2: &str = "1.2.840.113549.1.9.16.2.47";
const OID_SIGNING_CERT: &str = "1.2.840.113549.1.9.16.2.12";
const OID_TIMESTAMP_TOKEN: &str = "1.2.840.113549.1.9.16.2.14";
const OID_TST_INFO: &str = "1.2.840.113549.1.9.16.1.4";
const OID_SHA1: &str = "1.3.14.3.2.26";
const OID_SHA256: &str = "2.16.840.1.101.3.4.2.1";
const OID_SHA384: &str = "2.16.840.1.101.3.4.2.2";
const OID_SHA512: &str = "2.16.840.1.101.3.4.2.3";
const OID_RSA: &str = "1.2.840.113549.1.1.1";
const OID_RSA_SHA1: &str = "1.2.840.113549.1.1.5";
const OID_RSA_SHA256: &str = "1.2.840.113549.1.1.11";
const OID_RSA_SHA384: &str = "1.2.840.113549.1.1.12";
const OID_RSA_SHA512: &str = "1.2.840.113549.1.1.13";
const OID_RSA_PSS: &str = "1.2.840.113549.1.1.10";
const OID_EC_PUBLIC_KEY: &str = "1.2.840.10045.2.1";
const OID_ECDSA_SHA1: &str = "1.2.840.10045.4.1";
const OID_ECDSA_SHA256: &str = "1.2.840.10045.4.3.2";
const OID_ECDSA_SHA384: &str = "1.2.840.10045.4.3.3";
const OID_ECDSA_SHA512: &str = "1.2.840.10045.4.3.4";
const OID_P256: &str = "1.2.840.10045.3.1.7";
const OID_P384: &str = "1.3.132.0.34";

#[derive(Clone, Copy, PartialEq, Debug)]
enum Hash {
    Sha1,
    Sha256,
    Sha384,
    Sha512,
}

impl Hash {
    fn from_oid(o: &str) -> Option<Hash> {
        Some(match o {
            OID_SHA1 => Hash::Sha1,
            OID_SHA256 => Hash::Sha256,
            OID_SHA384 => Hash::Sha384,
            OID_SHA512 => Hash::Sha512,
            _ => return None,
        })
    }
    fn oid(self) -> &'static str {
        match self {
            Hash::Sha1 => OID_SHA1,
            Hash::Sha256 => OID_SHA256,
            Hash::Sha384 => OID_SHA384,
            Hash::Sha512 => OID_SHA512,
        }
    }
    fn name(self) -> &'static str {
        match self {
            Hash::Sha1 => "SHA-1",
            Hash::Sha256 => "SHA-256",
            Hash::Sha384 => "SHA-384",
            Hash::Sha512 => "SHA-512",
        }
    }
    fn digest(self, parts: &[&[u8]]) -> Vec<u8> {
        fn run<D: Digest>(parts: &[&[u8]]) -> Vec<u8> {
            let mut d = D::new();
            for p in parts {
                d.update(p);
            }
            d.finalize().to_vec()
        }
        match self {
            Hash::Sha1 => run::<sha1::Sha1>(parts),
            Hash::Sha256 => run::<sha2::Sha256>(parts),
            Hash::Sha384 => run::<sha2::Sha384>(parts),
            Hash::Sha512 => run::<sha2::Sha512>(parts),
        }
    }
    fn alg_id(self) -> Vec<u8> {
        seq(&[&oid(self.oid())])
    }
}

// ------------------------------------------------------------------ X.509

/// Raw pieces of a certificate.
struct CertParts<'a> {
    tbs: &'a [u8],
    serial: Tlv<'a>,
    issuer: Tlv<'a>,
    subject: Tlv<'a>,
    spki: Tlv<'a>,
    sig_alg: Tlv<'a>,
    sig: &'a [u8],
}

fn cert_parts(der: &[u8]) -> Option<CertParts<'_>> {
    let (cert, _) = read_tlv(der)?;
    let top = children(cert.content);
    let tbs = top.first()?;
    let sig_alg = *top.get(1)?;
    let sig_bits = top.get(2)?;
    let f = children(tbs.content);
    let mut i = 0;
    if f.first()?.tag == 0xa0 {
        i = 1; // [0] version
    }
    Some(CertParts {
        tbs: tbs.raw,
        serial: *f.get(i)?,
        issuer: *f.get(i + 2)?,
        subject: *f.get(i + 4)?,
        spki: *f.get(i + 5)?,
        sig_alg,
        sig: sig_bits.content.get(1..)?,
    })
}

fn name_string(raw: &[u8]) -> String {
    use der::Decode;
    match x509_cert::name::Name::from_der(raw) {
        Ok(n) => {
            let s = n.to_string();
            if s.is_empty() {
                "(empty name)".into()
            } else {
                s
            }
        }
        Err(_) => "(unreadable name)".into(),
    }
}

/// Common name, else the whole name.
fn common_name(raw: &[u8]) -> String {
    let full = name_string(raw);
    full.split(',')
        .find_map(|p| p.trim().strip_prefix("CN="))
        .map(str::to_string)
        .unwrap_or(full)
}

struct Validity {
    not_before: String,
    not_after: String,
    /// seconds since the epoch, for comparisons
    nb: i64,
    na: i64,
}

fn validity(der_cert: &[u8]) -> Option<Validity> {
    use der::Decode;
    let c = x509_cert::Certificate::from_der(der_cert).ok()?;
    let v = &c.tbs_certificate.validity;
    let nb = v.not_before.to_unix_duration().as_secs() as i64;
    let na = v.not_after.to_unix_duration().as_secs() as i64;
    Some(Validity {
        not_before: fmt_time(nb),
        not_after: fmt_time(na),
        nb,
        na,
    })
}

fn fmt_time(secs: i64) -> String {
    match der::DateTime::from_unix_duration(std::time::Duration::from_secs(secs.max(0) as u64)) {
        Ok(d) => format!(
            "{:04}-{:02}-{:02} {:02}:{:02}:{:02} UTC",
            d.year(),
            d.month(),
            d.day(),
            d.hour(),
            d.minutes(),
            d.seconds()
        ),
        Err(_) => "?".into(),
    }
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// UTCTime / GeneralizedTime → epoch seconds.
fn parse_asn1_time(t: &Tlv) -> Option<i64> {
    use der::Decode;
    match t.tag {
        0x17 => der::asn1::UtcTime::from_der(t.raw)
            .ok()
            .map(|u| u.to_unix_duration().as_secs() as i64),
        0x18 => {
            // GeneralizedTime may carry fractions; keep YYYYMMDDHHMMSS
            let s = std::str::from_utf8(t.content).ok()?;
            let digits: String = s.chars().take(14).collect();
            let g = format!("{digits}Z");
            der::asn1::GeneralizedTime::from_der(&tlv(0x18, g.as_bytes()))
                .ok()
                .map(|u| u.to_unix_duration().as_secs() as i64)
        }
        _ => None,
    }
}

// ------------------------------------------------------------------ keys

enum Key {
    Rsa(Box<rsa::RsaPrivateKey>),
    P256(p256::SecretKey),
    P384(p384::SecretKey),
}

impl Key {
    fn from_pkcs8(der: &[u8]) -> Result<Key, String> {
        use pkcs8::DecodePrivateKey;
        let info = pkcs8::PrivateKeyInfo::try_from(der)
            .map_err(|e| format!("unreadable private key ({e})"))?;
        let alg = info.algorithm.oid.to_string();
        if alg == OID_RSA {
            return rsa::RsaPrivateKey::from_pkcs8_der(der)
                .map(|k| Key::Rsa(Box::new(k)))
                .map_err(|e| format!("unreadable RSA key ({e})"));
        }
        if alg == OID_EC_PUBLIC_KEY {
            let curve = info
                .algorithm
                .parameters_oid()
                .map(|o| o.to_string())
                .unwrap_or_default();
            return match curve.as_str() {
                OID_P256 => p256::SecretKey::from_pkcs8_der(der)
                    .map(Key::P256)
                    .map_err(|e| format!("unreadable P-256 key ({e})")),
                OID_P384 => p384::SecretKey::from_pkcs8_der(der)
                    .map(Key::P384)
                    .map_err(|e| format!("unreadable P-384 key ({e})")),
                _ => Err("unsupported elliptic curve (use P-256 or P-384)".into()),
            };
        }
        Err(format!(
            "unsupported key type {alg} (use RSA or ECDSA P-256/P-384)"
        ))
    }

    fn public_spki(&self) -> Option<Vec<u8>> {
        use spki::EncodePublicKey;
        match self {
            Key::Rsa(k) => k.to_public_key().to_public_key_der().ok(),
            Key::P256(k) => k.public_key().to_public_key_der().ok(),
            Key::P384(k) => k.public_key().to_public_key_der().ok(),
        }
        .map(|d| d.as_bytes().to_vec())
    }

    fn hash(&self) -> Hash {
        match self {
            Key::P384(_) => Hash::Sha384,
            _ => Hash::Sha256,
        }
    }

    fn sig_alg_id(&self) -> Vec<u8> {
        match self {
            Key::Rsa(_) => seq(&[&oid(OID_RSA_SHA256), &[0x05, 0x00]]),
            Key::P256(_) => seq(&[&oid(OID_ECDSA_SHA256)]),
            Key::P384(_) => seq(&[&oid(OID_ECDSA_SHA384)]),
        }
    }

    /// Sign the hash of the signed attributes.
    fn sign_prehash(&self, hash: &[u8]) -> Result<Vec<u8>, String> {
        use p256::ecdsa::signature::hazmat::PrehashSigner;
        match self {
            Key::Rsa(k) => k
                .sign_with_rng(
                    &mut rand_core::OsRng,
                    rsa::Pkcs1v15Sign::new::<sha2::Sha256>(),
                    hash,
                )
                .map_err(|e| format!("RSA signing failed ({e})")),
            Key::P256(k) => {
                let sk = p256::ecdsa::SigningKey::from(k);
                let sig: p256::ecdsa::Signature = sk
                    .sign_prehash(hash)
                    .map_err(|e| format!("ECDSA signing failed ({e})"))?;
                Ok(sig.to_der().as_bytes().to_vec())
            }
            Key::P384(k) => {
                let sk = p384::ecdsa::SigningKey::from(k);
                let sig: p384::ecdsa::Signature = sk
                    .sign_prehash(hash)
                    .map_err(|e| format!("ECDSA signing failed ({e})"))?;
                Ok(sig.to_der().as_bytes().to_vec())
            }
        }
    }
}

/// Signing key, its certificate and any further chain certificates.
struct Credentials {
    key: Key,
    cert: Vec<u8>,
    chain: Vec<Vec<u8>>,
}

fn pem_blocks(text: &str) -> Vec<(String, Vec<u8>, bool)> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(b) = rest.find("-----BEGIN ") {
        let after = &rest[b + 11..];
        let Some(le) = after.find("-----") else { break };
        let label = after[..le].to_string();
        let body_start = &after[le + 5..];
        let end_marker = format!("-----END {label}-----");
        let Some(e) = body_start.find(&end_marker) else {
            break;
        };
        let body = &body_start[..e];
        let legacy_encrypted = body.contains("Proc-Type:") || body.contains("DEK-Info:");
        let b64: String = body
            .lines()
            .filter(|l| !l.contains(':'))
            .flat_map(|l| l.chars())
            .filter(|c| !c.is_whitespace())
            .collect();
        if let Ok(der) = base64::engine::general_purpose::STANDARD.decode(b64) {
            out.push((label, der, legacy_encrypted));
        }
        rest = &body_start[e + end_marker.len()..];
    }
    out
}

fn credentials(
    cert_file: &[u8],
    key_file: Option<&[u8]>,
    password: &str,
) -> Result<Credentials, String> {
    let is_pem = |b: &[u8]| memmem::find(b, b"-----BEGIN ").is_some();
    let mut certs: Vec<Vec<u8>> = Vec::new();
    let mut key: Option<Key> = None;
    let mut sources: Vec<&[u8]> = vec![cert_file];
    if let Some(k) = key_file.filter(|k| !k.is_empty()) {
        sources.push(k);
    }
    for src in sources {
        if is_pem(src) {
            let text = String::from_utf8_lossy(src);
            for (label, der, legacy) in pem_blocks(&text) {
                match label.as_str() {
                    "CERTIFICATE" | "X509 CERTIFICATE" | "TRUSTED CERTIFICATE" => certs.push(der),
                    _ if legacy => {
                        return Err("legacy encrypted PEM keys aren't supported; convert with \
                                    `openssl pkcs8 -topk8` or use a .p12"
                            .into())
                    }
                    "PRIVATE KEY" => key = Some(Key::from_pkcs8(&der)?),
                    "ENCRYPTED PRIVATE KEY" => {
                        let enc = pkcs8::EncryptedPrivateKeyInfo::try_from(der.as_slice())
                            .map_err(|e| format!("unreadable encrypted key ({e})"))?;
                        let doc = enc
                            .decrypt(password.as_bytes())
                            .map_err(|_| "wrong password for the private key".to_string())?;
                        key = Some(Key::from_pkcs8(doc.as_bytes())?);
                    }
                    "RSA PRIVATE KEY" => {
                        use rsa::pkcs1::DecodeRsaPrivateKey;
                        key = Some(Key::Rsa(Box::new(
                            rsa::RsaPrivateKey::from_pkcs1_der(&der)
                                .map_err(|e| format!("unreadable RSA key ({e})"))?,
                        )));
                    }
                    "EC PRIVATE KEY" => {
                        key = Some(match p256::SecretKey::from_sec1_der(&der) {
                            Ok(k) => Key::P256(k),
                            Err(_) => Key::P384(
                                p384::SecretKey::from_sec1_der(&der)
                                    .map_err(|_| "unsupported EC key (use P-256 or P-384)")?,
                            ),
                        });
                    }
                    _ => {}
                }
            }
        } else if cert_parts(src).is_some() && key_file.is_some() && certs.is_empty() {
            // a bare DER certificate
            certs.push(src.to_vec());
        } else {
            let ks = p12_keystore::KeyStore::from_pkcs12(src, password).map_err(|e| {
                let m = e.to_string();
                if m.to_lowercase().contains("mac") || m.to_lowercase().contains("password") {
                    "wrong certificate password".to_string()
                } else {
                    format!("not a readable .p12/.pfx or PEM file ({m})")
                }
            })?;
            let (_, chain) = ks
                .private_key_chain()
                .ok_or("the certificate file contains no private key")?;
            key = Some(Key::from_pkcs8(chain.key())?);
            certs.extend(chain.chain().iter().map(|c| c.as_der().to_vec()));
        }
    }
    let key = key.ok_or("no private key found (PEM needs a PRIVATE KEY block or a key file)")?;
    if certs.is_empty() {
        return Err("no certificate found".into());
    }
    let spki = key.public_spki();
    let idx = certs
        .iter()
        .position(|c| {
            cert_parts(c)
                .map(|p| Some(p.spki.raw.to_vec()) == spki)
                .unwrap_or(false)
        })
        .ok_or("the private key doesn't match any certificate in the file")?;
    let cert = certs.remove(idx);
    Ok(Credentials {
        key,
        cert,
        chain: certs,
    })
}

// ------------------------------------------------------------------ CMS

fn attribute(oid_s: &str, value: &[u8]) -> Vec<u8> {
    seq(&[&oid(oid_s), &tlv(0x31, value)])
}

/// Detached CMS SignedData over `digest` (already computed with the key's
/// hash).
fn build_cms(cred: &Credentials, digest: &[u8]) -> Result<Vec<u8>, String> {
    let hash = cred.key.hash();
    let cp = cert_parts(&cred.cert).ok_or("unreadable signer certificate")?;
    // signing-certificate-v2: ESSCertIDv2 { [hashAlgorithm], certHash, issuerSerial }
    let cert_hash = Hash::Sha256.digest(&[&cred.cert]);
    let issuer_serial = seq(&[&seq(&[&tlv(0xa4, cp.issuer.raw)]), cp.serial.raw]);
    let ess = seq(&[&tlv(0x04, &cert_hash), &issuer_serial]);
    let signing_cert = seq(&[&seq(&[&ess])]);
    let attrs = vec![
        attribute(OID_CONTENT_TYPE, &oid(OID_DATA)),
        attribute(OID_MESSAGE_DIGEST, &tlv(0x04, digest)),
        attribute(OID_SIGNING_CERT_V2, &signing_cert),
    ];
    let signed_attrs = set_of(attrs); // tag 0x31, as signed
    let to_sign = hash.digest(&[&signed_attrs]);
    let signature = cred.key.sign_prehash(&to_sign)?;
    let mut implicit_attrs = signed_attrs.clone();
    implicit_attrs[0] = 0xa0;
    let sid = seq(&[cp.issuer.raw, cp.serial.raw]);
    let signer_info = seq(&[
        &[0x02, 0x01, 0x01],
        &sid,
        &hash.alg_id(),
        &implicit_attrs,
        &cred.key.sig_alg_id(),
        &tlv(0x04, &signature),
    ]);
    let mut certs = cred.cert.clone();
    for c in &cred.chain {
        certs.extend_from_slice(c);
    }
    let signed_data = seq(&[
        &[0x02, 0x01, 0x01],
        &tlv(0x31, &hash.alg_id()),
        &seq(&[&oid(OID_DATA)]),
        &tlv(0xa0, &certs),
        &tlv(0x31, &signer_info),
    ]);
    Ok(seq(&[&oid(OID_SIGNED_DATA), &tlv(0xa0, &signed_data)]))
}

// ------------------------------------------------------------------ signing

/// Where a visible signature goes.
#[derive(Debug, Clone, Copy)]
pub struct Visible {
    /// 0-based page index
    pub page: usize,
    pub anchor: super::Anchor,
}

#[derive(Debug, Clone, Default)]
pub struct Request {
    pub reason: String,
    pub location: String,
    /// contact info (optional)
    pub contact: String,
    pub visible: Option<Visible>,
}

const SIG_W: f32 = 210.0;
const SIG_H: f32 = 64.0;

pub fn sign(
    pdf: &[u8],
    cert_file: &[u8],
    key_file: Option<&[u8]>,
    password: &str,
    req: &Request,
) -> Result<Vec<u8>, String> {
    if cert_file.is_empty() {
        return Err("no certificate file".into());
    }
    let cred = credentials(cert_file, key_file, password)?;
    let signer = cert_parts(&cred.cert)
        .map(|p| common_name(p.subject.raw))
        .unwrap_or_default();

    let mut base = pdf.to_vec();
    let mut doc = Document::load_mem(&base).map_err(|e| format!("not a readable PDF ({e})"))?;
    if doc.is_encrypted() || doc.trailer.has(b"Encrypt") {
        return Err("the document is password-protected; remove the password first".into());
    }
    // a file without a usable xref chain can't take an incremental update:
    // rewrite it once, cleanly, then sign that
    if doc.xref_start == 0 || doc.get_pages().is_empty() {
        let mut d = doc;
        let mut buf = Vec::new();
        d.save_to(&mut buf)
            .map_err(|e| format!("could not write PDF: {e}"))?;
        base = buf;
        doc = Document::load_mem(&base).map_err(|e| format!("not a readable PDF ({e})"))?;
    }
    let pages = doc.get_pages();
    let page_ids: Vec<ObjectId> = pages.values().copied().collect();
    let page_idx = req.visible.map(|v| v.page).unwrap_or(0);
    let page_id = *page_ids
        .get(page_idx)
        .ok_or_else(|| format!("page {} doesn't exist", page_idx + 1))?;

    let catalog_id = doc
        .trailer
        .get(b"Root")
        .and_then(Object::as_reference)
        .map_err(|_| "the PDF has no catalog")?;
    let mut inc = IncrementalDocument::create_from(base.clone(), doc);
    let prev = inc.get_prev_documents().clone();
    let nd = &mut inc.new_document;
    for k in [
        &b"Filter"[..],
        b"DecodeParms",
        b"XRefStm",
        b"Length",
        b"Index",
        b"W",
    ] {
        nd.trailer.remove(k);
    }

    // signature value dictionary with placeholders
    let reserve = 8192 + cred.cert.len() * 2 + cred.chain.iter().map(Vec::len).sum::<usize>() * 2;
    let now = now_secs();
    let mut v = dictionary! {
        "Type" => "Sig",
        "Filter" => "Adobe.PPKLite",
        "SubFilter" => "ETSI.CAdES.detached",
        "ByteRange" => vec![0.into(), 9_999_999_999i64.into(), 9_999_999_999i64.into(), 9_999_999_999i64.into()],
        "Contents" => Object::String(vec![0u8; reserve], lopdf::StringFormat::Hexadecimal),
        "M" => Object::string_literal(pdf_date(now)),
        "Name" => text_obj(&signer),
    };
    if !req.reason.trim().is_empty() {
        v.set("Reason", text_obj(req.reason.trim()));
    }
    if !req.location.trim().is_empty() {
        v.set("Location", text_obj(req.location.trim()));
    }
    if !req.contact.trim().is_empty() {
        v.set("ContactInfo", text_obj(req.contact.trim()));
    }
    let v_id = nd.add_object(v);

    // appearance
    let media = page_box(&prev, page_id);
    let (rect, ap) = match req.visible {
        None => ([0.0; 4], appearance(0.0, 0.0, &[])),
        Some(vis) => {
            let r = place(media, vis.anchor);
            let mut lines = vec![
                format!("Digitally signed by {signer}"),
                format!("Date: {}", fmt_time(now)),
            ];
            if !req.reason.trim().is_empty() {
                lines.push(format!("Reason: {}", req.reason.trim()));
            }
            if !req.location.trim().is_empty() {
                lines.push(format!("Location: {}", req.location.trim()));
            }
            (r, appearance(r[2] - r[0], r[3] - r[1], &lines))
        }
    };
    let ap_id = nd.add_object(ap);

    // field name unique among existing fields
    let existing = field_names(&prev);
    let name = (1..)
        .map(|i| format!("Signature{i}"))
        .find(|n| !existing.contains(n))
        .unwrap_or_else(|| "Signature".into());
    let field = dictionary! {
        "Type" => "Annot",
        "Subtype" => "Widget",
        "FT" => "Sig",
        "T" => Object::string_literal(name),
        "V" => v_id,
        "F" => 132,
        "P" => page_id,
        "Rect" => rect.iter().map(|&x| Object::Real(x)).collect::<Vec<_>>(),
        "AP" => dictionary! { "N" => ap_id },
    };
    let field_id = nd.add_object(field);

    // page /Annots
    let mut page = prev
        .get_dictionary(page_id)
        .map_err(|_| "unreadable page")?
        .clone();
    match page.get(b"Annots") {
        Ok(Object::Reference(aid)) => {
            let aid = *aid;
            let mut arr = prev
                .get_object(aid)
                .and_then(Object::as_array)
                .cloned()
                .unwrap_or_default();
            arr.push(field_id.into());
            nd.set_object(aid, Object::Array(arr));
        }
        Ok(Object::Array(a)) => {
            let mut a = a.clone();
            a.push(field_id.into());
            page.set("Annots", a);
        }
        _ => page.set("Annots", vec![Object::Reference(field_id)]),
    }
    nd.set_object(page_id, Object::Dictionary(page));

    // catalog /AcroForm
    let mut catalog = prev
        .get_dictionary(catalog_id)
        .map_err(|_| "unreadable catalog")?
        .clone();
    let add_field = |form: &mut Dictionary, prev: &Document| {
        let mut fields = match form.get(b"Fields") {
            Ok(Object::Reference(r)) => prev
                .get_object(*r)
                .and_then(Object::as_array)
                .cloned()
                .unwrap_or_default(),
            Ok(Object::Array(a)) => a.clone(),
            _ => vec![],
        };
        fields.push(field_id.into());
        form.set("Fields", fields);
        form.set("SigFlags", 3);
    };
    match catalog.get(b"AcroForm") {
        Ok(Object::Reference(fid)) => {
            let fid = *fid;
            let mut form = prev.get_dictionary(fid).cloned().unwrap_or_default();
            add_field(&mut form, &prev);
            nd.set_object(fid, Object::Dictionary(form));
        }
        Ok(Object::Dictionary(d)) => {
            let mut form = d.clone();
            add_field(&mut form, &prev);
            catalog.set("AcroForm", form);
        }
        _ => {
            let mut form = Dictionary::new();
            add_field(&mut form, &prev);
            catalog.set("AcroForm", form);
        }
    }
    nd.set_object(catalog_id, Object::Dictionary(catalog));

    let mut out = Vec::new();
    inc.save_to(&mut out)
        .map_err(|e| format!("could not write PDF: {e}"))?;

    // locate the placeholders in the appended section
    let tail = base.len();
    let zeros = vec![b'0'; reserve * 2];
    let c_at = memmem::find(&out[tail..], &zeros)
        .map(|o| tail + o)
        .ok_or("internal: signature placeholder not found")?;
    let lt = c_at - 1;
    if out.get(lt) != Some(&b'<') || out.get(c_at + reserve * 2) != Some(&b'>') {
        return Err("internal: unexpected signature placeholder layout".into());
    }
    let gt = c_at + reserve * 2 + 1;
    let br_pat = b"[0 9999999999 9999999999 9999999999]";
    let br_at = memmem::find(&out[tail..], br_pat)
        .map(|o| tail + o)
        .ok_or("internal: byte range placeholder not found")?;
    let br = [0usize, lt, gt, out.len() - gt];
    let mut text = format!("[{} {} {} {}]", br[0], br[1], br[2], br[3]).into_bytes();
    if text.len() > br_pat.len() {
        return Err("internal: byte range too long".into());
    }
    text.pop();
    while text.len() < br_pat.len() - 1 {
        text.push(b' ');
    }
    text.push(b']');
    out[br_at..br_at + br_pat.len()].copy_from_slice(&text);

    let digest = cred.key.hash().digest(&[&out[..lt], &out[gt..]]);
    let cms = build_cms(&cred, &digest)?;
    if cms.len() > reserve {
        return Err("internal: signature larger than its reserved space".into());
    }
    let hex: Vec<u8> = cms
        .iter()
        .flat_map(|b| {
            let h = format!("{b:02x}");
            h.into_bytes()
        })
        .collect();
    out[c_at..c_at + hex.len()].copy_from_slice(&hex);
    Ok(out)
}

/// PDF text string: PDFDocEncoding-compatible ASCII as a literal, else
/// UTF-16BE.
fn text_obj(s: &str) -> Object {
    if s.is_ascii() {
        Object::string_literal(s)
    } else {
        let mut b = vec![0xfe, 0xff];
        for u in s.encode_utf16() {
            b.extend_from_slice(&u.to_be_bytes());
        }
        Object::String(b, lopdf::StringFormat::Hexadecimal)
    }
}

fn pdf_date(secs: i64) -> String {
    match der::DateTime::from_unix_duration(std::time::Duration::from_secs(secs.max(0) as u64)) {
        Ok(d) => format!(
            "D:{:04}{:02}{:02}{:02}{:02}{:02}+00'00'",
            d.year(),
            d.month(),
            d.day(),
            d.hour(),
            d.minutes(),
            d.seconds()
        ),
        Err(_) => "D:19700101000000Z".into(),
    }
}

fn page_box(doc: &Document, page: ObjectId) -> [f32; 4] {
    let mut id = page;
    for _ in 0..32 {
        let Ok(d) = doc.get_dictionary(id) else { break };
        for key in [&b"CropBox"[..], b"MediaBox"] {
            if let Ok(o) = d.get(key) {
                let o = match o {
                    Object::Reference(r) => doc.get_object(*r).unwrap_or(o),
                    _ => o,
                };
                if let Ok(a) = o.as_array() {
                    let n: Vec<f32> = a.iter().filter_map(|x| x.as_float().ok()).collect();
                    if n.len() == 4 {
                        return [
                            n[0].min(n[2]),
                            n[1].min(n[3]),
                            n[0].max(n[2]),
                            n[1].max(n[3]),
                        ];
                    }
                }
            }
        }
        match d.get(b"Parent").and_then(Object::as_reference) {
            Ok(p) => id = p,
            Err(_) => break,
        }
    }
    [0.0, 0.0, 612.0, 792.0]
}

fn place(b: [f32; 4], anchor: super::Anchor) -> [f32; 4] {
    use super::Anchor::*;
    let m = 28.0;
    let (w, h) = (
        SIG_W.min(b[2] - b[0] - 2.0 * m),
        SIG_H.min(b[3] - b[1] - 2.0 * m),
    );
    let x = match anchor {
        TopLeft | Left | BottomLeft => b[0] + m,
        Top | Center | Bottom => (b[0] + b[2] - w) / 2.0,
        _ => b[2] - m - w,
    };
    let y = match anchor {
        TopLeft | Top | TopRight => b[3] - m - h,
        Left | Center | Right => (b[1] + b[3] - h) / 2.0,
        _ => b[1] + m,
    };
    [x, y, x + w, y + h]
}

/// Latin-1 bytes for a WinAnsi Helvetica string (others → '?').
fn latin1(s: &str) -> Vec<u8> {
    s.chars()
        .map(|c| if (c as u32) < 256 { c as u8 } else { b'?' })
        .collect()
}

fn appearance(w: f32, h: f32, lines: &[String]) -> Stream {
    let mut c: Vec<u8> = Vec::new();
    if !lines.is_empty() {
        c.extend_from_slice(
            format!(
                "q 0.97 0.97 1 rg 0.2 0.3 0.6 RG 0.8 w 0.4 0.4 {:.2} {:.2} re B Q\n",
                w - 0.8,
                h - 0.8
            )
            .as_bytes(),
        );
        let longest = lines.iter().map(|l| l.chars().count()).max().unwrap_or(1) as f32;
        let size = (h / (lines.len() as f32 * 1.3 + 0.6))
            .min((w - 8.0) / (longest * 0.52))
            .clamp(3.0, 11.0);
        let mut y = h - 4.0 - size;
        c.extend_from_slice(b"BT 0.1 0.15 0.35 rg\n");
        for (i, l) in lines.iter().enumerate() {
            let font = if i == 0 { "F2" } else { "F1" };
            c.extend_from_slice(format!("/{font} {size:.2} Tf 1 0 0 1 5 {y:.2} Tm (").as_bytes());
            for b in latin1(l) {
                if matches!(b, b'(' | b')' | b'\\') {
                    c.push(b'\\');
                }
                c.push(b);
            }
            c.extend_from_slice(b") Tj\n");
            y -= size * 1.3;
        }
        c.extend_from_slice(b"ET\n");
    }
    let res = dictionary! {
        "Font" => dictionary! {
            "F1" => dictionary! { "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Helvetica", "Encoding" => "WinAnsiEncoding" },
            "F2" => dictionary! { "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Helvetica-Bold", "Encoding" => "WinAnsiEncoding" },
        },
    };
    Stream::new(
        dictionary! {
            "Type" => "XObject",
            "Subtype" => "Form",
            "BBox" => vec![0.into(), 0.into(), Object::Real(w), Object::Real(h)],
            "Resources" => res,
        },
        c,
    )
}

fn field_names(doc: &Document) -> std::collections::HashSet<String> {
    let mut out = std::collections::HashSet::new();
    for (_, o) in &doc.objects {
        if let Ok(d) = o.as_dict() {
            if let Ok(t) = d.get(b"T").and_then(Object::as_str) {
                out.insert(String::from_utf8_lossy(t).into_owned());
            }
        }
    }
    out
}

// ------------------------------------------------------------------ verify

/// Verify `sig` over `msg` with the key in `spki` using the signature
/// algorithm `alg` (an AlgorithmIdentifier); `digest` is the hash to use
/// when the algorithm doesn't name one. Returns a description.
fn verify_raw(
    spki: &[u8],
    alg: &Tlv,
    digest: Option<Hash>,
    msg: &[u8],
    sig: &[u8],
) -> Result<String, String> {
    use p256::ecdsa::signature::hazmat::PrehashVerifier;
    use rsa::traits::PublicKeyParts;
    use spki::DecodePublicKey;
    let parts = children(alg.content);
    let alg_oid = parts.first().map(oid_str).unwrap_or_default();
    let need = |h: Option<Hash>| h.ok_or_else(|| "unknown digest algorithm".to_string());
    let rsa_key = || {
        rsa::RsaPublicKey::from_public_key_der(spki)
            .map_err(|e| format!("bad RSA public key ({e})"))
    };
    let pkcs1 = |h: Hash| -> Result<String, String> {
        let k = rsa_key()?;
        let hashed = h.digest(&[msg]);
        let scheme = match h {
            Hash::Sha1 => rsa::Pkcs1v15Sign::new::<sha1::Sha1>(),
            Hash::Sha256 => rsa::Pkcs1v15Sign::new::<sha2::Sha256>(),
            Hash::Sha384 => rsa::Pkcs1v15Sign::new::<sha2::Sha384>(),
            Hash::Sha512 => rsa::Pkcs1v15Sign::new::<sha2::Sha512>(),
        };
        k.verify(scheme, &hashed, sig)
            .map(|_| format!("RSA {} PKCS#1 v1.5 with {}", k.size() * 8, h.name()))
            .map_err(|_| "the signature doesn't match".to_string())
    };
    let ecdsa = |h: Hash| -> Result<String, String> {
        let hashed = h.digest(&[msg]);
        if let Ok(vk) = p256::ecdsa::VerifyingKey::from_public_key_der(spki) {
            let s =
                p256::ecdsa::Signature::from_der(sig).map_err(|_| "malformed ECDSA signature")?;
            return vk
                .verify_prehash(&hashed, &s)
                .map(|_| format!("ECDSA P-256 with {}", h.name()))
                .map_err(|_| "the signature doesn't match".to_string());
        }
        if let Ok(vk) = p384::ecdsa::VerifyingKey::from_public_key_der(spki) {
            let s =
                p384::ecdsa::Signature::from_der(sig).map_err(|_| "malformed ECDSA signature")?;
            return vk
                .verify_prehash(&hashed, &s)
                .map(|_| format!("ECDSA P-384 with {}", h.name()))
                .map_err(|_| "the signature doesn't match".to_string());
        }
        Err("unsupported elliptic curve (only P-256 and P-384 are checked)".into())
    };
    match alg_oid.as_str() {
        OID_RSA => pkcs1(need(digest)?),
        OID_RSA_SHA1 => pkcs1(Hash::Sha1),
        OID_RSA_SHA256 => pkcs1(Hash::Sha256),
        OID_RSA_SHA384 => pkcs1(Hash::Sha384),
        OID_RSA_SHA512 => pkcs1(Hash::Sha512),
        OID_RSA_PSS => {
            // RSASSA-PSS-params { [0] hashAlgorithm, [1] mgf, [2] saltLength }
            let mut h = Hash::Sha1;
            let mut salt = 20usize;
            if let Some(p) = parts.get(1).filter(|p| p.tag == 0x30) {
                for f in children(p.content) {
                    match f.tag {
                        0xa0 => {
                            if let Some(a) = children(f.content).first() {
                                if let Some(o) = children(a.content).first() {
                                    h = Hash::from_oid(&oid_str(o)).ok_or("unknown PSS digest")?;
                                }
                            }
                        }
                        0xa2 => {
                            if let Some(i) = children(f.content).first() {
                                salt = i.content.iter().fold(0usize, |a, &b| (a << 8) | b as usize);
                            }
                        }
                        _ => {}
                    }
                }
            }
            let k = rsa_key()?;
            let hashed = h.digest(&[msg]);
            let scheme = match h {
                Hash::Sha1 => rsa::Pss::new_with_salt::<sha1::Sha1>(salt),
                Hash::Sha256 => rsa::Pss::new_with_salt::<sha2::Sha256>(salt),
                Hash::Sha384 => rsa::Pss::new_with_salt::<sha2::Sha384>(salt),
                Hash::Sha512 => rsa::Pss::new_with_salt::<sha2::Sha512>(salt),
            };
            k.verify(scheme, &hashed, sig)
                .map(|_| format!("RSA {} PSS with {}", k.size() * 8, h.name()))
                .map_err(|_| "the signature doesn't match".to_string())
        }
        OID_EC_PUBLIC_KEY => ecdsa(need(digest)?),
        OID_ECDSA_SHA1 => ecdsa(Hash::Sha1),
        OID_ECDSA_SHA256 => ecdsa(Hash::Sha256),
        OID_ECDSA_SHA384 => ecdsa(Hash::Sha384),
        OID_ECDSA_SHA512 => ecdsa(Hash::Sha512),
        other => Err(format!("unsupported signature algorithm {other}")),
    }
}

/// Outcome of checking one CMS SignedData.
struct CmsCheck {
    ok: bool,
    problems: Vec<String>,
    algorithm: String,
    signer_cert: Option<Vec<u8>>,
    certs: Vec<Vec<u8>>,
    signing_time: Option<i64>,
    has_signing_cert_attr: bool,
    timestamp: Option<String>,
    econtent: Option<Vec<u8>>,
}

/// Check a CMS SignedData: `detached` is the signed content when the
/// content isn't encapsulated; for `adbe.pkcs7.sha1` the encapsulated
/// content (a SHA-1 digest) is returned for the caller to compare.
fn check_cms(cms: &[u8], detached: Option<&[&[u8]]>) -> CmsCheck {
    let mut r = CmsCheck {
        ok: false,
        problems: vec![],
        algorithm: String::new(),
        signer_cert: None,
        certs: vec![],
        signing_time: None,
        has_signing_cert_attr: false,
        timestamp: None,
        econtent: None,
    };
    let fail = |mut r: CmsCheck, m: &str| {
        r.problems.push(m.to_string());
        r
    };
    let Some((ci, _)) = read_tlv(cms) else {
        return fail(r, "the signature container isn't valid DER");
    };
    let ci = children(ci.content);
    if ci.first().map(oid_str).as_deref() != Some(OID_SIGNED_DATA) {
        return fail(r, "the signature isn't a CMS SignedData");
    }
    let Some(sd) = ci.get(1).and_then(|t| children(t.content).first().copied()) else {
        return fail(r, "empty SignedData");
    };
    let f = children(sd.content);
    // version, digestAlgorithms, encapContentInfo, [0] certs?, [1] crls?, signerInfos
    let Some(encap) = f.get(2) else {
        return fail(r, "truncated SignedData");
    };
    let encap_f = children(encap.content);
    let e_type = encap_f.first().map(oid_str).unwrap_or_default();
    let econtent: Option<Vec<u8>> = encap_f
        .get(1)
        .filter(|t| t.tag == 0xa0)
        .and_then(|t| children(t.content).first().map(octets));
    r.econtent = econtent.clone();
    let mut idx = 3;
    if let Some(c) = f.get(idx).filter(|t| t.tag == 0xa0) {
        r.certs = children(c.content)
            .iter()
            .filter(|t| t.tag == 0x30)
            .map(|t| t.raw.to_vec())
            .collect();
        idx += 1;
    }
    if f.get(idx).map(|t| t.tag == 0xa1).unwrap_or(false) {
        idx += 1;
    }
    let Some(sis) = f.get(idx).filter(|t| t.tag == 0x31) else {
        return fail(r, "no signer information");
    };
    let sis = children(sis.content);
    let Some(si) = sis.first() else {
        return fail(r, "no signer information");
    };
    if sis.len() > 1 {
        r.problems.push(format!(
            "{} signers in one container; only the first is checked",
            sis.len()
        ));
    }
    let s = children(si.content);
    // version, sid, digestAlgorithm, [0] signedAttrs?, signatureAlgorithm, signature, [1] unsigned?
    let (Some(sid), Some(dig_alg)) = (s.get(1), s.get(2)) else {
        return fail(r, "truncated signer information");
    };
    let hash = children(dig_alg.content)
        .first()
        .and_then(|o| Hash::from_oid(&oid_str(o)));
    let Some(hash) = hash else {
        return fail(r, "unsupported digest algorithm");
    };
    let mut j = 3;
    let signed_attrs = s.get(j).filter(|t| t.tag == 0xa0).copied();
    if signed_attrs.is_some() {
        j += 1;
    }
    let (Some(sig_alg), Some(sig_val)) = (s.get(j), s.get(j + 1)) else {
        return fail(r, "truncated signer information");
    };
    let unsigned = s.get(j + 2).filter(|t| t.tag == 0xa1).copied();

    // signer certificate by issuer+serial or subject key identifier
    let signer = r.certs.iter().find(|c| {
        let Some(p) = cert_parts(c) else { return false };
        match sid.tag {
            0x30 => {
                let k = children(sid.content);
                k.len() == 2 && k[0].raw == p.issuer.raw && k[1].raw == p.serial.raw
            }
            0x80 => ski_of(c).map(|k| k == sid.content).unwrap_or(false),
            _ => false,
        }
    });
    let Some(signer) = signer.cloned() else {
        return fail(
            r,
            "the signer's certificate isn't embedded in the signature",
        );
    };
    r.signer_cert = Some(signer.clone());
    let cp = cert_parts(&signer).expect("checked above");

    // the content: detached ranges or encapsulated
    let content_digest = match (detached, &econtent) {
        (Some(parts), None) => hash.digest(parts),
        (_, Some(ec)) => hash.digest(&[ec]),
        (None, None) => return fail(r, "the signature carries no content"),
    };

    let msg: Vec<u8>;
    if let Some(sa) = signed_attrs {
        let mut ok_digest = false;
        let mut ok_type = false;
        for a in children(sa.content) {
            let af = children(a.content);
            let Some(t) = af.first() else { continue };
            let val = af.get(1).and_then(|v| children(v.content).first().copied());
            match oid_str(t).as_str() {
                OID_MESSAGE_DIGEST => {
                    ok_digest = val
                        .map(|v| v.content == content_digest.as_slice())
                        .unwrap_or(false);
                }
                OID_CONTENT_TYPE => {
                    ok_type = val.map(|v| oid_str(&v) == e_type).unwrap_or(false);
                }
                OID_SIGNING_TIME => {
                    r.signing_time = val.and_then(|v| parse_asn1_time(&v));
                }
                OID_SIGNING_CERT_V2 | OID_SIGNING_CERT => r.has_signing_cert_attr = true,
                _ => {}
            }
        }
        if !ok_digest {
            r.problems.push(
                "the document digest doesn't match the signed message digest — the signed bytes were changed"
                    .into(),
            );
        }
        if !ok_type {
            r.problems
                .push("content-type attribute missing or wrong".into());
        }
        let mut m = sa.raw.to_vec();
        m[0] = 0x31;
        msg = m;
    } else {
        // no attributes: signature directly over the content
        msg = match (detached, &econtent) {
            (_, Some(ec)) => ec.clone(),
            (Some(parts), None) => parts.concat(),
            _ => vec![],
        };
    }
    match verify_raw(cp.spki.raw, sig_alg, Some(hash), &msg, sig_val.content) {
        Ok(desc) => {
            r.algorithm = desc;
            r.ok = r.problems.iter().all(|p| p.contains("only the first"));
        }
        Err(e) => r.problems.push(format!("cryptographic check failed: {e}")),
    }

    // RFC 3161 signature timestamp
    if let Some(u) = unsigned {
        for a in children(u.content) {
            let af = children(a.content);
            if af.first().map(oid_str).as_deref() != Some(OID_TIMESTAMP_TOKEN) {
                continue;
            }
            let Some(tok) = af.get(1).and_then(|v| children(v.content).first().copied()) else {
                continue;
            };
            r.timestamp = Some(check_timestamp(tok.raw, sig_val.content));
        }
    }
    r
}

fn check_timestamp(token: &[u8], signature: &[u8]) -> String {
    let c = check_cms(token, None);
    let Some(tst) = c.econtent.as_deref() else {
        return "present but unreadable".into();
    };
    // TSTInfo { version, policy, messageImprint { alg, hash }, serial, genTime, … }
    let Some((t, _)) = read_tlv(tst) else {
        return "present but unreadable".into();
    };
    let f = children(t.content);
    let imprint_ok = f
        .get(2)
        .map(|mi| {
            let m = children(mi.content);
            let h = m
                .first()
                .and_then(|a| children(a.content).first().map(oid_str))
                .and_then(|o| Hash::from_oid(&o));
            match (h, m.get(1)) {
                (Some(h), Some(v)) => h.digest(&[signature]) == v.content,
                _ => false,
            }
        })
        .unwrap_or(false);
    let time = f
        .get(4)
        .and_then(parse_asn1_time)
        .map(fmt_time)
        .unwrap_or("?".into());
    let tsa = c
        .signer_cert
        .as_deref()
        .and_then(cert_parts)
        .map(|p| common_name(p.subject.raw))
        .unwrap_or_else(|| "unknown TSA".into());
    let state = match (c.ok, imprint_ok) {
        (true, true) => "valid",
        (true, false) => "INVALID (it covers a different signature)",
        _ => "INVALID (its own signature doesn't verify)",
    };
    let _ = OID_TST_INFO;
    format!("{time} by {tsa} — {state}")
}

/// subjectKeyIdentifier extension value.
fn ski_of(cert: &[u8]) -> Option<Vec<u8>> {
    use der::Decode;
    let c = x509_cert::Certificate::from_der(cert).ok()?;
    let exts = c.tbs_certificate.extensions?;
    let e = exts.iter().find(|e| e.extn_id.to_string() == "2.5.29.14")?;
    let (t, _) = read_tlv(e.extn_value.as_bytes())?;
    Some(t.content.to_vec())
}

/// Walk issuer links within `certs` from `leaf`; returns a description.
fn chain_report(leaf: &[u8], certs: &[Vec<u8>]) -> (bool, String) {
    let mut cur = leaf.to_vec();
    let mut lines = Vec::new();
    let mut self_signed = false;
    for depth in 0..8 {
        let Some(p) = cert_parts(&cur) else { break };
        if p.issuer.raw == p.subject.raw {
            let ok = verify_raw(p.spki.raw, &p.sig_alg, None, p.tbs, p.sig).is_ok();
            if depth == 0 {
                self_signed = true;
            }
            lines.push(format!(
                "root {} ({})",
                common_name(p.subject.raw),
                if ok {
                    "self-signature valid"
                } else {
                    "self-signature INVALID"
                }
            ));
            break;
        }
        let issuer = certs.iter().find(|c| {
            cert_parts(c)
                .map(|q| q.subject.raw == p.issuer.raw)
                .unwrap_or(false)
        });
        match issuer {
            Some(i) => {
                let q = cert_parts(i).expect("parsed above");
                let ok = verify_raw(q.spki.raw, &p.sig_alg, None, p.tbs, p.sig).is_ok();
                lines.push(format!(
                    "{} ← issued by {} ({})",
                    common_name(p.subject.raw),
                    common_name(q.subject.raw),
                    if ok {
                        "signature valid"
                    } else {
                        "signature INVALID"
                    }
                ));
                if i.as_slice() == cur.as_slice() {
                    break;
                }
                cur = i.clone();
            }
            None => {
                lines.push(format!(
                    "issuer {} not embedded (chain incomplete)",
                    common_name(p.issuer.raw)
                ));
                break;
            }
        }
    }
    (self_signed, lines.join("; "))
}

fn str_of(doc: &Document, d: &Dictionary, k: &[u8]) -> Option<String> {
    let o = d.get(k).ok()?;
    let o = match o {
        Object::Reference(r) => doc.get_object(*r).ok()?,
        o => o,
    };
    let b = o.as_str().ok()?;
    Some(if b.starts_with(&[0xfe, 0xff]) {
        let u: Vec<u16> = b[2..]
            .chunks_exact(2)
            .map(|c| u16::from_be_bytes([c[0], c[1]]))
            .collect();
        String::from_utf16_lossy(&u)
    } else {
        b.iter().map(|&c| c as char).collect()
    })
}

/// PDF date "D:YYYYMMDDHHmmSS…" → display text.
fn show_pdf_date(s: &str) -> String {
    let d = s.trim_start_matches("D:");
    if d.len() >= 14 && d[..14].chars().all(|c| c.is_ascii_digit()) {
        let tz = &d[14..];
        let tz = if tz.is_empty() || tz.starts_with('Z') {
            " UTC".to_string()
        } else {
            format!(" {}", tz.replace('\'', ""))
        };
        format!(
            "{}-{}-{} {}:{}:{}{}",
            &d[0..4],
            &d[4..6],
            &d[6..8],
            &d[8..10],
            &d[10..12],
            &d[12..14],
            tz
        )
    } else {
        s.to_string()
    }
}

/// Signature-validation report for every signature in the file.
pub fn validate(pdf: &[u8]) -> Result<String, String> {
    let doc = Document::load_mem(pdf).map_err(|e| format!("not a readable PDF ({e})"))?;
    if doc.is_encrypted() {
        return Err("the document is password-protected; remove the password first".into());
    }
    // (field name, signature dictionary)
    let mut sigs: Vec<(String, ObjectId, Dictionary)> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for (&id, o) in &doc.objects {
        let Ok(d) = o.as_dict() else { continue };
        if d.get(b"FT").and_then(Object::as_name).ok() != Some(b"Sig") {
            continue;
        }
        let Ok(v) = d.get(b"V") else { continue };
        let (vid, vd) = match v {
            Object::Reference(r) => match doc.get_dictionary(*r) {
                Ok(vd) => (*r, vd.clone()),
                Err(_) => continue,
            },
            Object::Dictionary(vd) => (id, vd.clone()),
            _ => continue,
        };
        if seen.insert(vid) {
            let name = str_of(&doc, d, b"T").unwrap_or_else(|| "(unnamed)".into());
            sigs.push((name, vid, vd));
        }
    }
    // loose signature dictionaries (fields lost)
    for (&id, o) in &doc.objects {
        let Ok(d) = o.as_dict() else { continue };
        if d.has(b"ByteRange") && d.has(b"Contents") && !seen.contains(&id) {
            seen.insert(id);
            sigs.push(("(not attached to a field)".into(), id, d.clone()));
        }
    }
    if sigs.is_empty() {
        return Ok("No digital signatures found.".into());
    }
    // order by position in the file (first byte range end)
    let br_of = |d: &Dictionary| -> Vec<i64> {
        d.get(b"ByteRange")
            .and_then(Object::as_array)
            .map(|a| a.iter().filter_map(|x| x.as_i64().ok()).collect())
            .unwrap_or_default()
    };
    sigs.sort_by_key(|(_, _, d)| br_of(d).get(1).copied().unwrap_or(i64::MAX));

    let mut out = String::new();
    let now = now_secs();
    let mut all_ok = true;
    for (i, (name, _id, d)) in sigs.iter().enumerate() {
        out.push_str(&format!("Signature {} — field \"{name}\"\n", i + 1));
        let sub = d
            .get(b"SubFilter")
            .and_then(Object::as_name)
            .map(|n| String::from_utf8_lossy(n).into_owned())
            .unwrap_or_else(|_| "?".into());
        let br = br_of(d);
        let contents = d
            .get(b"Contents")
            .and_then(Object::as_str)
            .map(<[u8]>::to_vec)
            .unwrap_or_default();
        let mut problems: Vec<String> = Vec::new();
        let ranges_ok = br.len() == 4
            && br[0] == 0
            && br.iter().all(|&x| x >= 0)
            && br[0] + br[1] <= br[2]
            && (br[2] + br[3]) as usize <= pdf.len();
        if !ranges_ok {
            out.push_str("  Status: INVALID — malformed /ByteRange\n\n");
            all_ok = false;
            continue;
        }
        let (a, b, c, len) = (
            br[1] as usize,
            br[2] as usize,
            br[2] as usize,
            br[3] as usize,
        );
        if pdf.get(a) != Some(&b'<') || pdf.get(b - 1) != Some(&b'>') {
            problems.push("the byte range gap isn't exactly the /Contents value".into());
        }
        let parts: [&[u8]; 2] = [&pdf[..a], &pdf[c..c + len]];
        let end = c + len;
        // trim the zero padding of the reserve
        let cms_len = read_tlv(&contents)
            .map(|(t, _)| t.raw.len())
            .unwrap_or(contents.len());
        let cms = &contents[..cms_len.min(contents.len())];
        let check = match sub.as_str() {
            "adbe.x509.rsa_sha1" => {
                out.push_str("  Status: NOT CHECKED — legacy adbe.x509.rsa_sha1 signatures aren't supported\n\n");
                all_ok = false;
                continue;
            }
            "adbe.pkcs7.sha1" => {
                let c = check_cms(cms, None);
                if let Some(ec) = &c.econtent {
                    if Hash::Sha1.digest(&parts) != *ec {
                        problems.push("the signed SHA-1 digest doesn't match the document".into());
                    }
                }
                c
            }
            _ => check_cms(cms, Some(&parts)),
        };
        problems.extend(check.problems.iter().cloned());
        let valid = check.ok && problems.iter().all(|p| p.contains("only the first"));
        all_ok &= valid;
        out.push_str(&format!(
            "  Status: {}\n",
            if valid {
                "VALID — the signed bytes are unchanged and the signature matches the certificate"
                    .to_string()
            } else {
                format!(
                    "INVALID — {}",
                    problems.first().cloned().unwrap_or_default()
                )
            }
        ));
        for p in problems.iter().skip(1) {
            out.push_str(&format!("  Problem: {p}\n"));
        }
        if end == pdf.len() {
            out.push_str("  Coverage: the whole file\n");
        } else {
            out.push_str(&format!(
                "  Coverage: revision ending at byte {end}; {} byte(s) were added after signing \
                 (later edits or signatures)\n",
                pdf.len() - end
            ));
        }
        out.push_str(&format!("  Format: {sub}"));
        if !check.algorithm.is_empty() {
            out.push_str(&format!(", {}", check.algorithm));
        }
        out.push('\n');
        if let Some(cert) = &check.signer_cert {
            if let Some(p) = cert_parts(cert) {
                out.push_str(&format!("  Signer: {}\n", name_string(p.subject.raw)));
                out.push_str(&format!("  Issuer: {}\n", name_string(p.issuer.raw)));
            }
            if let Some(v) = validity(cert) {
                let at = check.signing_time.unwrap_or(now);
                let state = if at < v.nb {
                    "not yet valid at signing time"
                } else if at > v.na {
                    "expired at signing time"
                } else if now > v.na {
                    "valid at signing time, expired now"
                } else {
                    "valid"
                };
                out.push_str(&format!(
                    "  Certificate validity: {} → {} ({state})\n",
                    v.not_before, v.not_after
                ));
            }
            let (self_signed, chain) = chain_report(cert, &check.certs);
            out.push_str(&format!(
                "  Self-signed: {}\n",
                if self_signed { "yes" } else { "no" }
            ));
            out.push_str(&format!(
                "  Chain ({} certificate(s) embedded): {chain}\n",
                check.certs.len()
            ));
        }
        if let Some(t) = check.signing_time {
            out.push_str(&format!(
                "  Signing time (CMS attribute): {}\n",
                fmt_time(t)
            ));
        }
        if let Some(m) = str_of(&doc, d, b"M") {
            out.push_str(&format!(
                "  Signing time (claimed, /M): {}\n",
                show_pdf_date(&m)
            ));
        }
        for (k, label) in [
            (&b"Name"[..], "Name"),
            (b"Reason", "Reason"),
            (b"Location", "Location"),
            (b"ContactInfo", "Contact"),
        ] {
            if let Some(v) = str_of(&doc, d, k).filter(|s| !s.trim().is_empty()) {
                out.push_str(&format!("  {label}: {v}\n"));
            }
        }
        if check.has_signing_cert_attr {
            out.push_str("  Signing-certificate attribute: present\n");
        }
        match &check.timestamp {
            Some(t) => out.push_str(&format!("  Timestamp: {t}\n")),
            None => out.push_str("  Timestamp: none\n"),
        }
        out.push('\n');
    }
    out.push_str(if all_ok {
        "All signatures verify. Trust in the signers is not checked — there is no certificate store; \
         compare the signer and issuer with what you expect."
    } else {
        "Some signatures did not verify. Trust in the signers is not checked."
    });
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::viewers::pdf::tools::repair::tests::sample;
    use der::Encode;
    use std::str::FromStr;
    use x509_cert::builder::{Builder, CertificateBuilder, Profile};
    use x509_cert::name::Name;
    use x509_cert::serial_number::SerialNumber;
    use x509_cert::time::Validity as XValidity;

    /// Test-only self-signed certificate + PKCS#8 key (generated per test).
    fn p256_cred() -> (Vec<u8>, Vec<u8>) {
        use pkcs8::EncodePrivateKey;
        let sk = p256::SecretKey::random(&mut rand_core::OsRng);
        let signer = p256::ecdsa::SigningKey::from(&sk);
        let spki = spki::SubjectPublicKeyInfoOwned::from_key(*signer.verifying_key()).unwrap();
        let builder = CertificateBuilder::new(
            Profile::Root,
            SerialNumber::from(42u32),
            XValidity::from_now(std::time::Duration::from_secs(3600 * 24)).unwrap(),
            Name::from_str("CN=OG Test Signer,O=og-files tests").unwrap(),
            spki,
            &signer,
        )
        .unwrap();
        let cert = builder
            .build::<p256::ecdsa::DerSignature>()
            .unwrap()
            .to_der()
            .unwrap();
        let key = sk.to_pkcs8_der().unwrap().as_bytes().to_vec();
        (cert, key)
    }

    fn rsa_cred() -> (Vec<u8>, Vec<u8>) {
        use pkcs8::EncodePrivateKey;
        let sk = rsa::RsaPrivateKey::new(&mut rand_core::OsRng, 2048).unwrap();
        let signer = rsa::pkcs1v15::SigningKey::<sha2::Sha256>::new(sk.clone());
        let spki = spki::SubjectPublicKeyInfoOwned::from_key(sk.to_public_key()).unwrap();
        let cert = CertificateBuilder::new(
            Profile::Root,
            SerialNumber::from(7u32),
            XValidity::from_now(std::time::Duration::from_secs(3600 * 24)).unwrap(),
            Name::from_str("CN=OG RSA Tester").unwrap(),
            spki,
            &signer,
        )
        .unwrap()
        .build::<rsa::pkcs1v15::Signature>()
        .unwrap()
        .to_der()
        .unwrap();
        (cert, sk.to_pkcs8_der().unwrap().as_bytes().to_vec())
    }

    fn pem(label: &str, der: &[u8]) -> String {
        let b = base64::engine::general_purpose::STANDARD.encode(der);
        let body: Vec<String> = b
            .as_bytes()
            .chunks(64)
            .map(|c| String::from_utf8_lossy(c).into_owned())
            .collect();
        format!(
            "-----BEGIN {label}-----\n{}\n-----END {label}-----\n",
            body.join("\n")
        )
    }

    fn p12(cert: &[u8], key: &[u8], pw: &str) -> Vec<u8> {
        let mut ks = p12_keystore::KeyStore::new();
        let c = p12_keystore::Certificate::from_der(cert).unwrap();
        let chain = p12_keystore::PrivateKeyChain::new(key, [1u8; 20], vec![c]);
        ks.add_entry("test", p12_keystore::KeyStoreEntry::PrivateKeyChain(chain));
        ks.writer(pw).write().unwrap()
    }

    #[test]
    fn sign_and_validate_p256_pem_invisible() {
        let (cert, key) = p256_cred();
        let file = format!("{}{}", pem("CERTIFICATE", &cert), pem("PRIVATE KEY", &key));
        let pdf = sample(2);
        let req = Request {
            reason: "Approved".into(),
            location: "Berlin".into(),
            ..Default::default()
        };
        let signed = sign(&pdf, file.as_bytes(), None, "", &req).unwrap();
        assert!(
            signed.starts_with(&pdf),
            "incremental update keeps the original bytes"
        );
        let r = validate(&signed).unwrap();
        assert!(r.contains("Status: VALID"), "{r}");
        assert!(r.contains("Coverage: the whole file"), "{r}");
        assert!(r.contains("ECDSA P-256 with SHA-256"), "{r}");
        assert!(r.contains("OG Test Signer"), "{r}");
        assert!(r.contains("Self-signed: yes"), "{r}");
        assert!(r.contains("Reason: Approved"), "{r}");
        assert!(r.contains("Signing-certificate attribute: present"), "{r}");
        // still a valid PDF with the same pages
        let d = Document::load_mem(&signed).unwrap();
        assert_eq!(d.get_pages().len(), 2);

        // tamper with one signed byte → invalid
        let mut bad = signed.clone();
        let i = memmem::find(&bad, b"Page 1 of").unwrap();
        bad[i] = b'X';
        let r = validate(&bad).unwrap();
        assert!(r.contains("Status: INVALID"), "{r}");

        // appending after signing is reported
        let mut more = signed.clone();
        more.extend_from_slice(b"\n% appended\n");
        let r = validate(&more).unwrap();
        assert!(r.contains("byte(s) were added after signing"), "{r}");
    }

    #[test]
    fn sign_rsa_p12_visible_and_countersign() {
        let (cert, key) = rsa_cred();
        let file = p12(&cert, &key, "secret");
        assert!(sign(&sample(1), &file, None, "wrong", &Request::default()).is_err());
        let req = Request {
            reason: "Reviewed (ok)".into(),
            location: "Zürich".into(),
            visible: Some(Visible {
                page: 0,
                anchor: crate::viewers::pdf::tools::Anchor::BottomRight,
            }),
            ..Default::default()
        };
        let once = sign(&sample(1), &file, None, "secret", &req).unwrap();
        let r = validate(&once).unwrap();
        assert!(r.contains("RSA 2048 PKCS#1 v1.5 with SHA-256"), "{r}");
        assert!(r.contains("Status: VALID"), "{r}");
        // visible widget renders
        assert!(
            crate::viewers::pdf::tools::repair::unrenderable_pages(&once)
                .unwrap()
                .is_empty()
        );
        // a second signature keeps the first valid
        let (c2, k2) = p256_cred();
        let f2 = format!("{}{}", pem("PRIVATE KEY", &k2), pem("CERTIFICATE", &c2));
        let twice = sign(&once, f2.as_bytes(), None, "", &Request::default()).unwrap();
        let r = validate(&twice).unwrap();
        assert_eq!(r.matches("Status: VALID").count(), 2, "{r}");
        assert!(r.contains("Signature1") && r.contains("Signature2"), "{r}");
        assert!(r.contains("were added after signing"), "{r}");
    }

    #[test]
    fn separate_key_file_and_mismatch() {
        let (cert, _) = p256_cred();
        let (_, other_key) = p256_cred();
        let e = sign(
            &sample(1),
            pem("CERTIFICATE", &cert).as_bytes(),
            Some(pem("PRIVATE KEY", &other_key).as_bytes()),
            "",
            &Request::default(),
        )
        .unwrap_err();
        assert!(e.contains("doesn't match"), "{e}");
    }

    #[test]
    fn signs_xref_stream_files() {
        // compressed (object + xref streams) input
        let pdf = crate::viewers::pdf::tools::structure::compress_lossless(&sample(3)).unwrap();
        assert!(memmem::find(&pdf, b"/XRef").is_some());
        let (cert, key) = p256_cred();
        let file = format!("{}{}", pem("CERTIFICATE", &cert), pem("PRIVATE KEY", &key));
        let signed = sign(&pdf, file.as_bytes(), None, "", &Request::default()).unwrap();
        let r = validate(&signed).unwrap();
        assert!(r.contains("Status: VALID"), "{r}");
        assert_eq!(Document::load_mem(&signed).unwrap().get_pages().len(), 3);
        assert!(hayro::hayro_syntax::Pdf::new(std::sync::Arc::new(signed)).is_ok());
    }

    #[test]
    fn no_signatures() {
        assert_eq!(
            validate(&sample(1)).unwrap(),
            "No digital signatures found."
        );
    }

    /// Cross-check with poppler's pdfsig / OpenSSL when installed.
    #[test]
    #[ignore]
    fn external_verifiers() {
        let (cert, key) = rsa_cred();
        let file = format!("{}{}", pem("CERTIFICATE", &cert), pem("PRIVATE KEY", &key));
        let signed = sign(&sample(1), file.as_bytes(), None, "", &Request::default()).unwrap();
        let dir = std::env::temp_dir().join(format!("og-sign-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("signed.pdf");
        std::fs::write(&p, &signed).unwrap();
        let out = std::process::Command::new("pdfsig").arg(&p).output();
        if let Ok(o) = out {
            let s = String::from_utf8_lossy(&o.stdout);
            println!("{s}");
            assert!(s.contains("Signature is Valid"), "{s}");
        }
        // openssl cms -verify on the detached content
        let doc = Document::load_mem(&signed).unwrap();
        let v = doc
            .objects
            .values()
            .find_map(|o| o.as_dict().ok().filter(|d| d.has(b"ByteRange")).cloned())
            .unwrap();
        let br: Vec<usize> = v
            .get(b"ByteRange")
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .map(|x| x.as_i64().unwrap() as usize)
            .collect();
        let contents = v.get(b"Contents").unwrap().as_str().unwrap();
        let (t, _) = read_tlv(contents).unwrap();
        std::fs::write(dir.join("sig.der"), t.raw).unwrap();
        let mut data = signed[..br[1]].to_vec();
        data.extend_from_slice(&signed[br[2]..br[2] + br[3]]);
        std::fs::write(dir.join("data.bin"), data).unwrap();
        std::fs::write(dir.join("cert.pem"), pem("CERTIFICATE", &cert)).unwrap();
        if let Ok(o) = std::process::Command::new("openssl")
            .args([
                "cms",
                "-verify",
                "-binary",
                "-inform",
                "DER",
                "-noverify",
                "-in",
            ])
            .arg(dir.join("sig.der"))
            .arg("-content")
            .arg(dir.join("data.bin"))
            .arg("-out")
            .arg("/dev/null")
            .output()
        {
            let e = String::from_utf8_lossy(&o.stderr);
            println!("{e}");
            assert!(e.contains("Verification successful"), "{e}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
