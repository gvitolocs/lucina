//! A scripted TLS 1.3 server on the host, to prove that `connect` refuses every handshake that skips or breaks the
//! server's authentication (the review of 2026-11-08: embedded-tls accepts EncryptedExtensions → Finished with no
//! Certificate or CertificateVerify, so `connect` itself must insist that both were checked), and a generated test
//! PKI (rcgen) for the chain defects webpki must refuse. The server speaks just enough TLS 1.3 for embedded-tls:
//! ServerHello with a P-256 share and TLS_AES_128_GCM_SHA256, then one encrypted record per handshake message, in the
//! order the test asks for, with a correct Finished (an active attacker can always compute one: it chose its share).

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::time::Duration;
use std::vec::Vec;

use aes_gcm::aead::{AeadInPlace, KeyInit};
use aes_gcm::{Aes128Gcm, Nonce};
use embedded_io_adapters::std::FromStd;
use hmac::{Hmac, Mac};
use p256::ecdsa::signature::Signer;
use rcgen::{BasicConstraints, CertificateParams, CustomExtension, DnType, ExtendedKeyUsagePurpose, IsCa, Issuer, KeyPair, KeyUsagePurpose};
use sha2::{Digest, Sha256};

use crate::rng::{Pool, Rng};
use crate::verify::{check_chain, Refusal};

const HOST: &str = "getsecretary.tech";
/// 2026-11-08 12:00 UTC, and a build date before it.
const NOW: u64 = 1_794_139_200;
const BUILT: u64 = 1_794_000_000;

// ---- the key schedule (RFC 8446 7.1) ----

fn hkdf_extract(salt: &[u8], ikm: &[u8]) -> [u8; 32] {
    let (prk, _) = hkdf::Hkdf::<Sha256>::extract(Some(salt), ikm);
    prk.into()
}

fn expand_label(secret: &[u8], label: &str, ctx: &[u8], len: usize) -> Vec<u8> {
    let full = [b"tls13 ".as_slice(), label.as_bytes()].concat();
    let mut info = Vec::new();
    info.extend_from_slice(&(len as u16).to_be_bytes());
    info.push(full.len() as u8);
    info.extend_from_slice(&full);
    info.push(ctx.len() as u8);
    info.extend_from_slice(ctx);
    let hk = hkdf::Hkdf::<Sha256>::from_prk(secret).unwrap();
    let mut out = std::vec![0u8; len];
    hk.expand(&info, &mut out).unwrap();
    out
}

/// What the scripted server sends after ServerHello, each in its own encrypted record.
#[derive(Clone)]
pub enum Msg {
    EncryptedExtensions,
    Certificate(Vec<Vec<u8>>),
    /// A CertificateVerify signed with this P-256 key (PKCS#8), over the transcript so far.
    CertificateVerify(Vec<u8>),
    Finished,
}

fn handshake_msg(ty: u8, body: &[u8]) -> Vec<u8> {
    let mut m = std::vec![ty];
    m.extend_from_slice(&(body.len() as u32).to_be_bytes()[1..]);
    m.extend_from_slice(body);
    m
}

fn read_record(s: &mut TcpStream) -> Option<(u8, Vec<u8>)> {
    let mut h = [0u8; 5];
    s.read_exact(&mut h).ok()?;
    let len = u16::from_be_bytes([h[3], h[4]]) as usize;
    let mut b = std::vec![0u8; len];
    s.read_exact(&mut b).ok()?;
    Some((h[0], b))
}

/// The ClientHello's legacy session id and its P-256 key share.
fn parse_client_hello(ch: &[u8]) -> (Vec<u8>, Vec<u8>) {
    let mut i = 4 + 2 + 32;
    let sid = ch[i + 1..i + 1 + ch[i] as usize].to_vec();
    i += 1 + ch[i] as usize;
    i += 2 + u16::from_be_bytes([ch[i], ch[i + 1]]) as usize;
    i += 1 + ch[i] as usize;
    let end = i + 2 + u16::from_be_bytes([ch[i], ch[i + 1]]) as usize;
    i += 2;
    while i + 4 <= end {
        let ty = u16::from_be_bytes([ch[i], ch[i + 1]]);
        let len = u16::from_be_bytes([ch[i + 2], ch[i + 3]]) as usize;
        let data = &ch[i + 4..i + 4 + len];
        if ty == 0x0033 {
            let mut j = 2;
            while j + 4 <= data.len() {
                let g = u16::from_be_bytes([data[j], data[j + 1]]);
                let kl = u16::from_be_bytes([data[j + 2], data[j + 3]]) as usize;
                if g == 0x0017 {
                    return (sid, data[j + 4..j + 4 + kl].to_vec());
                }
                j += 4 + kl;
            }
        }
        i += 4 + len;
    }
    panic!("no P-256 key share in the ClientHello");
}

/// Serves one handshake: ServerHello, then `script`; then reads what the client answers and closes.
fn serve(mut s: TcpStream, script: Vec<Msg>) {
    let _ = s.set_read_timeout(Some(Duration::from_secs(10)));
    let Some((22, ch)) = read_record(&mut s) else { return };
    let (sid, client_pub) = parse_client_hello(&ch);
    let mut transcript = Sha256::new();
    transcript.update(&ch);

    let secret = p256::ecdh::EphemeralSecret::random(&mut rand_core::OsRng);
    let ours = p256::EncodedPoint::from(secret.public_key());
    let theirs = p256::PublicKey::from_sec1_bytes(&client_pub).unwrap();
    let shared = secret.diffie_hellman(&theirs);

    let mut sh = Vec::new();
    sh.extend_from_slice(&[0x03, 0x03]);
    sh.extend_from_slice(&[0x5a; 32]);
    sh.push(sid.len() as u8);
    sh.extend_from_slice(&sid);
    sh.extend_from_slice(&[0x13, 0x01, 0x00]);
    let mut ext = Vec::new();
    ext.extend_from_slice(&[0x00, 0x2b, 0x00, 0x02, 0x03, 0x04]);
    let pt = ours.as_bytes();
    ext.extend_from_slice(&[0x00, 0x33]);
    ext.extend_from_slice(&((4 + pt.len()) as u16).to_be_bytes());
    ext.extend_from_slice(&[0x00, 0x17]);
    ext.extend_from_slice(&(pt.len() as u16).to_be_bytes());
    ext.extend_from_slice(pt);
    sh.extend_from_slice(&(ext.len() as u16).to_be_bytes());
    sh.extend_from_slice(&ext);
    let sh = handshake_msg(2, &sh);
    transcript.update(&sh);
    let mut rec = std::vec![22, 0x03, 0x03];
    rec.extend_from_slice(&(sh.len() as u16).to_be_bytes());
    rec.extend_from_slice(&sh);
    let _ = s.write_all(&rec);

    let empty_hash = Sha256::digest([]);
    let early = hkdf_extract(&[0u8; 32], &[0u8; 32]);
    let derived = expand_label(&early, "derived", &empty_hash, 32);
    let hs = hkdf_extract(&derived, shared.raw_secret_bytes());
    let s_hs = expand_label(&hs, "s hs traffic", &transcript.clone().finalize(), 32);
    let key = expand_label(&s_hs, "key", &[], 16);
    let iv = expand_label(&s_hs, "iv", &[], 12);
    let finished_key = expand_label(&s_hs, "finished", &[], 32);
    let aead = Aes128Gcm::new_from_slice(&key).unwrap();
    let mut seq: u64 = 0;

    for m in script {
        let msg = match m {
            Msg::EncryptedExtensions => handshake_msg(8, &[0, 0]),
            Msg::Certificate(chain) => {
                let mut list = Vec::new();
                for c in &chain {
                    list.extend_from_slice(&(c.len() as u32).to_be_bytes()[1..]);
                    list.extend_from_slice(c);
                    list.extend_from_slice(&[0, 0]);
                }
                let mut body = std::vec![0u8];
                body.extend_from_slice(&(list.len() as u32).to_be_bytes()[1..]);
                body.extend_from_slice(&list);
                handshake_msg(11, &body)
            }
            Msg::CertificateVerify(pkcs8) => {
                use p256::pkcs8::DecodePrivateKey;
                let key = p256::ecdsa::SigningKey::from_pkcs8_der(&pkcs8).unwrap();
                let mut content = std::vec![0x20u8; 64];
                content.extend_from_slice(b"TLS 1.3, server CertificateVerify\x00");
                content.extend_from_slice(&transcript.clone().finalize());
                let sig: p256::ecdsa::DerSignature = key.sign(&content);
                let mut body = std::vec![0x04, 0x03];
                body.extend_from_slice(&(sig.as_bytes().len() as u16).to_be_bytes());
                body.extend_from_slice(sig.as_bytes());
                handshake_msg(15, &body)
            }
            Msg::Finished => {
                let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(&finished_key).unwrap();
                mac.update(&transcript.clone().finalize());
                handshake_msg(20, &mac.finalize().into_bytes())
            }
        };
        transcript.update(&msg);
        let mut inner = msg.clone();
        inner.push(22);
        let len = inner.len() + 16;
        let aad = [23u8, 0x03, 0x03, (len >> 8) as u8, len as u8];
        let mut nonce = [0u8; 12];
        nonce.copy_from_slice(&iv);
        for (k, b) in seq.to_be_bytes().iter().enumerate() {
            nonce[4 + k] ^= b;
        }
        aead.encrypt_in_place(Nonce::from_slice(&nonce), &aad, &mut inner).unwrap();
        let mut out = aad.to_vec();
        out.extend_from_slice(&inner);
        let _ = s.write_all(&out);
        seq += 1;
    }
    // the client's answer (its Finished), then close
    let _ = read_record(&mut s);
    let _ = read_record(&mut s);
}

fn rng() -> Rng {
    let mut p = Pool::new();
    p.mix(b"mock tests");
    Rng::from_pool(p)
}

/// `connect` with `anchors` against a server that plays `script`.
fn handshake(script: Vec<Msg>, anchors: &'static [&'static [u8]], now: u64) -> Result<(), Refusal> {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap();
    let t = std::thread::spawn(move || {
        if let Ok((s, _)) = l.accept() {
            serve(s, script);
        }
    });
    let sock = TcpStream::connect(addr).unwrap();
    sock.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    let mut r = rng();
    let (mut rb, mut wb) = (std::vec![0u8; crate::READ_BUF], std::vec![0u8; crate::WRITE_BUF]);
    let res = crate::connect_with(FromStd::new(sock), HOST, now, BUILT, &mut r, &mut rb, &mut wb, anchors).map(|_| ()).map_err(|(_, r)| r);
    let _ = t.join();
    res
}

// ---- a test PKI ----

struct Ca {
    params: CertificateParams,
    key: KeyPair,
    der: Vec<u8>,
}

fn ca_params(name: &str, path_len: Option<u8>) -> CertificateParams {
    let mut p = CertificateParams::new(Vec::<String>::new()).unwrap();
    p.distinguished_name.push(DnType::CommonName, name);
    p.is_ca = IsCa::Ca(match path_len {
        Some(n) => BasicConstraints::Constrained(n),
        None => BasicConstraints::Unconstrained,
    });
    p.key_usages = std::vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
    p.not_before = rcgen::date_time_ymd(2025, 1, 1);
    p.not_after = rcgen::date_time_ymd(2030, 1, 1);
    p
}

fn root() -> Ca {
    let key = KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256).unwrap();
    let params = ca_params("Fortuna Test Root", None);
    let der = params.self_signed(&key).unwrap().der().to_vec();
    Ca { params, key, der }
}

fn sub(issuer: &Ca, params: CertificateParams) -> Ca {
    let key = KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256).unwrap();
    let der = params.signed_by(&key, &Issuer::from_params(&issuer.params, &issuer.key)).unwrap().der().to_vec();
    Ca { params, key, der }
}

fn leaf_params(name: &str) -> CertificateParams {
    let mut p = CertificateParams::new(std::vec![name.to_string()]).unwrap();
    p.distinguished_name.push(DnType::CommonName, name);
    p.key_usages = std::vec![KeyUsagePurpose::DigitalSignature];
    p.extended_key_usages = std::vec![ExtendedKeyUsagePurpose::ServerAuth];
    p.not_before = rcgen::date_time_ymd(2026, 1, 1);
    p.not_after = rcgen::date_time_ymd(2027, 1, 1);
    p
}

fn leak(d: Vec<u8>) -> &'static [&'static [u8]] {
    let d: &'static [u8] = std::boxed::Box::leak(d.into_boxed_slice());
    std::boxed::Box::leak(std::vec![d].into_boxed_slice())
}

const LEAF: &[u8] = include_bytes!("../tests/fixtures/getsecretary-1.der");
const YE1: &[u8] = include_bytes!("../tests/fixtures/getsecretary-2.der");
const YE_BY_X2: &[u8] = include_bytes!("../tests/fixtures/getsecretary-3.der");

fn real_chain() -> Vec<Vec<u8>> {
    std::vec![LEAF.to_vec(), YE1.to_vec(), YE_BY_X2.to_vec()]
}

// ---- the handshakes ----

#[test]
fn the_mock_is_a_real_tls_server() {
    // the control: a correct chain to the test root and a correct CertificateVerify connect
    let r = root();
    let ica = sub(&r, ca_params("Fortuna Test CA", Some(0)));
    let leaf = sub(&ica, leaf_params(HOST));
    let script = std::vec![
        Msg::EncryptedExtensions,
        Msg::Certificate(std::vec![leaf.der.clone(), ica.der.clone()]),
        Msg::CertificateVerify(leaf.key.serialize_der()),
        Msg::Finished,
    ];
    assert_eq!(handshake(script.clone(), leak(r.der.clone()), NOW), Ok(()));
    // the same server against Fortuna's real anchors: no chain to X1 or X2
    assert_eq!(handshake(script, &crate::anchors::ANCHORS, NOW), Err(Refusal::UnknownRoot));
}

#[test]
fn no_certificate_at_all_is_refused() {
    // the bypass: EncryptedExtensions then Finished
    assert_eq!(handshake(std::vec![Msg::EncryptedExtensions, Msg::Finished], &crate::anchors::ANCHORS, NOW), Err(Refusal::NotAuthenticated));
}

#[test]
fn a_certificate_without_certificate_verify_is_refused() {
    // the real public chain, which anyone can send, and no proof of its key
    let script = std::vec![Msg::EncryptedExtensions, Msg::Certificate(real_chain()), Msg::Finished];
    assert_eq!(handshake(script, &crate::anchors::ANCHORS, NOW), Err(Refusal::NotAuthenticated));
}

#[test]
fn certificate_verify_with_another_key_is_refused() {
    let other = KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256).unwrap();
    let script = std::vec![
        Msg::EncryptedExtensions,
        Msg::Certificate(real_chain()),
        Msg::CertificateVerify(other.serialize_der()),
        Msg::Finished,
    ];
    assert_eq!(handshake(script, &crate::anchors::ANCHORS, NOW), Err(Refusal::BadSignature));
}

#[test]
fn a_cached_chain_still_needs_its_certificate_verify() {
    // a good handshake puts this chain's signatures in the DSi's cache; the same chain with a CertificateVerify by
    // another key is still refused (CertificateVerify never goes through the cache), and the good one still connects
    let r = root();
    let ica = sub(&r, ca_params("Fortuna Cache CA", Some(0)));
    let leaf = sub(&ica, leaf_params(HOST));
    let anchors = leak(r.der.clone());
    let chain = std::vec![leaf.der.clone(), ica.der.clone()];
    let good = std::vec![Msg::EncryptedExtensions, Msg::Certificate(chain.clone()), Msg::CertificateVerify(leaf.key.serialize_der()), Msg::Finished];
    assert_eq!(handshake(good.clone(), anchors, NOW), Ok(()));
    let other = KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256).unwrap();
    let forged = std::vec![Msg::EncryptedExtensions, Msg::Certificate(chain.clone()), Msg::CertificateVerify(other.serialize_der()), Msg::Finished];
    assert_eq!(handshake(forged, anchors, NOW), Err(Refusal::BadSignature));
    let skipped = std::vec![Msg::EncryptedExtensions, Msg::Certificate(chain), Msg::Finished];
    assert_eq!(handshake(skipped, anchors, NOW), Err(Refusal::NotAuthenticated));
    assert_eq!(handshake(good, anchors, NOW), Ok(()));
}

#[test]
fn certificate_verify_before_the_certificate_is_refused() {
    let r = root();
    let leaf = sub(&r, leaf_params(HOST));
    let script = std::vec![
        Msg::EncryptedExtensions,
        Msg::CertificateVerify(leaf.key.serialize_der()),
        Msg::Certificate(std::vec![leaf.der.clone()]),
        Msg::Finished,
    ];
    assert!(handshake(script, leak(r.der.clone()), NOW).is_err());
}

#[test]
fn a_second_certificate_after_verify_is_refused() {
    // proof of one key, then a swap to another certificate before Finished
    let r = root();
    let a = sub(&r, leaf_params(HOST));
    let b = sub(&r, leaf_params(HOST));
    let script = std::vec![
        Msg::EncryptedExtensions,
        Msg::Certificate(std::vec![a.der.clone()]),
        Msg::CertificateVerify(a.key.serialize_der()),
        Msg::Certificate(std::vec![b.der.clone()]),
        Msg::Finished,
    ];
    assert_eq!(handshake(script, leak(r.der.clone()), NOW), Err(Refusal::NotAuthenticated));
}

#[test]
fn an_unset_clock_is_refused_before_any_byte() {
    // the DSi's RTC never set (2000-01-01): no connection at all, whatever the server would send
    let r = root();
    let leaf = sub(&r, leaf_params(HOST));
    let script = std::vec![
        Msg::EncryptedExtensions,
        Msg::Certificate(std::vec![leaf.der.clone()]),
        Msg::CertificateVerify(leaf.key.serialize_der()),
        Msg::Finished,
    ];
    assert_eq!(handshake(script, leak(r.der.clone()), 946_684_800), Err(Refusal::ClockUnset));
}

// ---- chain defects (webpki, through Fortuna's check_chain) ----

#[test]
fn chain_defects_are_refused() {
    let r = root();
    let anchors: &[&[u8]] = &[&r.der];
    let ica = sub(&r, ca_params("Fortuna Test CA", Some(0)));
    let ok = sub(&ica, leaf_params(HOST));
    assert_eq!(check_chain(&[&ok.der, &ica.der], HOST, NOW, anchors), Ok(()));

    // an issuer that is not a CA
    let mut not_ca = ca_params("Not a CA", None);
    not_ca.is_ca = IsCa::ExplicitNoCa;
    let not_ca = sub(&r, not_ca);
    let under = sub(&not_ca, leaf_params(HOST));
    assert!(check_chain(&[&under.der, &not_ca.der], HOST, NOW, anchors).is_err(), "non-CA issuer");

    // path length 0 exceeded by a second intermediate
    let ica2 = sub(&ica, ca_params("Below pathLen 0", None));
    let deep = sub(&ica2, leaf_params(HOST));
    assert!(check_chain(&[&deep.der, &ica2.der, &ica.der], HOST, NOW, anchors).is_err(), "pathLen exceeded");

    // no serverAuth EKU (clientAuth only)
    let mut p = leaf_params(HOST);
    p.extended_key_usages = std::vec![ExtendedKeyUsagePurpose::ClientAuth];
    let client_only = sub(&ica, p);
    assert!(check_chain(&[&client_only.der, &ica.der], HOST, NOW, anchors).is_err(), "missing serverAuth");

    // an unknown critical extension
    let mut p = leaf_params(HOST);
    let mut ext = CustomExtension::from_oid_content(&[1, 3, 6, 1, 4, 1, 99999, 1], std::vec![0x05, 0x00]);
    ext.set_criticality(true);
    p.custom_extensions = std::vec![ext];
    let crit = sub(&ica, p);
    assert!(check_chain(&[&crit.der, &ica.der], HOST, NOW, anchors).is_err(), "unknown critical extension");

    // a leaf that is a CA
    let mut p = leaf_params(HOST);
    p.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    let ca_leaf = sub(&ica, p);
    assert!(check_chain(&[&ca_leaf.der, &ica.der], HOST, NOW, anchors).is_err(), "CA as the server");

    // a wildcard for the parent name only
    let wild = sub(&ica, leaf_params("*.getsecretary.tech"));
    assert_eq!(check_chain(&[&wild.der, &ica.der], HOST, NOW, anchors), Err(Refusal::WrongHost));

    // an expired intermediate
    let mut p = ca_params("Expired CA", Some(0));
    p.not_after = rcgen::date_time_ymd(2026, 6, 1);
    let old = sub(&r, p);
    let under_old = sub(&old, leaf_params(HOST));
    assert!(check_chain(&[&under_old.der, &old.der], HOST, NOW, anchors).is_err(), "expired intermediate");

    // not yet valid
    let mut p = leaf_params(HOST);
    p.not_before = rcgen::date_time_ymd(2027, 1, 1);
    p.not_after = rcgen::date_time_ymd(2028, 1, 1);
    let early = sub(&ica, p);
    assert_eq!(check_chain(&[&early.der, &ica.der], HOST, NOW, anchors), Err(Refusal::Expired));

    // swapped order, a self-signed leaf, garbage and truncated DER
    assert!(check_chain(&[&ica.der, &ok.der], HOST, NOW, anchors).is_err(), "swapped order");
    let self_key = KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256).unwrap();
    let selfie = leaf_params(HOST).self_signed(&self_key).unwrap().der().to_vec();
    assert!(check_chain(&[&selfie], HOST, NOW, anchors).is_err(), "self-signed leaf");
    assert!(check_chain(&[&[0x30, 0x03, 0x02, 0x01, 0x00]], HOST, NOW, anchors).is_err(), "garbage");
    assert!(check_chain(&[&ok.der[..ok.der.len() / 2], &ica.der], HOST, NOW, anchors).is_err(), "truncated");

    // more intermediates than Fortuna takes: refused before any signature is checked
    let many: Vec<&[u8]> = [&ok.der[..], &ica.der, &ica.der, &ica.der, &ica.der, &ica.der].to_vec();
    assert_eq!(check_chain(&many, HOST, NOW, anchors), Err(Refusal::TooManyCertificates));
}
