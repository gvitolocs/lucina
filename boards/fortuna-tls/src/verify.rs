//! The server check embedded-tls calls during the handshake, done by rustls-webpki (reviewed, used by rustls): the
//! chain from the server's certificate to ISRG Root X1 or X2 (signatures, CA and path-length constraints, key usage,
//! the TLS-server extended key usage, validity at `now`), the host name against the certificate's names, then the
//! server's CertificateVerify signature over the handshake with the certificate's own key. Fortuna adds the time
//! (the DSi's clock, refused when unset: `rules::clock_set`), a cap on the chain's length (`rules::chain_len_ok`),
//! the RustCrypto algorithms (`algs`), and `authenticated()`, which `connect` requires.
//!
//! Not checked: revocation (no OCSP or CRL fetch on a DSi; Let's Encrypt certificates live 90 days).

use alloc::vec::Vec;

use embedded_tls::blocking::{CertificateEntryRef, CertificateRef, CertificateVerifyRef, TlsCipherSuite, TlsError, TlsVerifier};
use embedded_tls::SignatureScheme;
use pki_types::{CertificateDer, ServerName, UnixTime};
use sha2::Digest;

use crate::algs;

/// Why the last handshake's server check failed, for the log (embedded-tls only says `InvalidCertificate`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    None,
    NoCertificate,
    Unreadable,
    WrongHost,
    Expired,
    UnknownRoot,
    Invalid,
    BadSignature,
    WrongScheme,
    /// The handshake ended without a checked certificate and CertificateVerify (embedded-tls lets a server skip them).
    NotAuthenticated,
    /// The DSi's clock has never been set: no certificate can be checked for validity.
    ClockUnset,
    /// More certificates than Fortuna takes (a cap on the path search's signature checks).
    TooManyCertificates,
}

impl Refusal {
    pub fn text(self) -> &'static str {
        match self {
            Refusal::None => "ok",
            Refusal::NoCertificate => "no certificate",
            Refusal::Unreadable => "certificate unreadable",
            Refusal::WrongHost => "certificate for another name",
            Refusal::Expired => "certificate not valid now",
            Refusal::UnknownRoot => "no chain to a trusted root",
            Refusal::Invalid => "certificate refused",
            Refusal::BadSignature => "server signature wrong",
            Refusal::WrongScheme => "server signature scheme refused",
            Refusal::NotAuthenticated => "server did not prove its certificate",
            Refusal::ClockUnset => "the DSi's clock is not set",
            Refusal::TooManyCertificates => "too many certificates",
        }
    }

    fn of(e: &webpki::Error) -> Refusal {
        use webpki::Error as E;
        match e {
            E::CertExpired { .. } | E::CertNotValidYet { .. } => Refusal::Expired,
            E::UnknownIssuer => Refusal::UnknownRoot,
            E::CertNotValidForName(_) => Refusal::WrongHost,
            E::BadDer | E::BadDerTime | E::TrailingData(_) => Refusal::Unreadable,
            _ => Refusal::Invalid,
        }
    }
}

/// The chain check alone, on the DER certificates as the server sent them (its own first): `Ok` when webpki finds a
/// path to one of `anchors` valid at `now` for `host`. Signatures through `algs::CHAIN` (the verified-signature cache).
pub fn check_chain(entries: &[&[u8]], host: &str, now: u64, anchors: &[&[u8]]) -> Result<(), Refusal> {
    check_chain_with(entries, host, now, anchors, algs::CHAIN)
}

/// `check_chain` with other signature algorithms (tests: counting ones, a cache of their own).
pub fn check_chain_with(
    entries: &[&[u8]],
    host: &str,
    now: u64,
    anchors: &[&[u8]],
    sig_algs: &[&dyn pki_types::SignatureVerificationAlgorithm],
) -> Result<(), Refusal> {
    let (leaf, rest) = entries.split_first().ok_or(Refusal::NoCertificate)?;
    if !crate::rules::chain_len_ok(entries.len().min(u32::MAX as usize) as u32) {
        return Err(Refusal::TooManyCertificates);
    }
    let leaf_der = CertificateDer::from(*leaf);
    let ee = webpki::EndEntityCert::try_from(&leaf_der).map_err(|e| Refusal::of(&e))?;
    let anchor_ders: Vec<CertificateDer> = anchors.iter().map(|a| CertificateDer::from(*a)).collect();
    let mut tas = Vec::new();
    for d in &anchor_ders {
        tas.push(webpki::anchor_from_trusted_cert(d).map_err(|_| Refusal::Unreadable)?.to_owned());
    }
    let inter: Vec<CertificateDer> = rest.iter().map(|c| CertificateDer::from(*c)).collect();
    ee.verify_for_usage(sig_algs, &tas, &inter, UnixTime::since_unix_epoch(core::time::Duration::from_secs(now)), webpki::KeyUsage::server_auth(), None, None)
        .map_err(|e| Refusal::of(&e))?;
    let name = ServerName::try_from(host).map_err(|_| Refusal::WrongHost)?;
    ee.verify_is_valid_for_subject_name(&name).map_err(|e| Refusal::of(&e))?;
    Ok(())
}

/// The handshake's server check. Crate-private on purpose: embedded-tls calls it only for the Certificate and
/// CertificateVerify messages a server chooses to send, and accepts a handshake without them (the review of
/// 2026-11-08). `crate::connect` refuses any handshake after which `authenticated()` is false; nothing else may open a
/// connection with it.
pub(crate) struct Verifier<H: Clone> {
    host: heapless::String<64>,
    now: u64,
    anchors: &'static [&'static [u8]],
    leaf: Vec<u8>,
    transcript: Option<H>,
    /// The latest Certificate passed `check_chain`.
    cert_ok: bool,
    /// Its CertificateVerify passed, with that certificate's key, over the transcript through it.
    sig_ok: bool,
    pub refusal: Refusal,
}

impl<H: Clone> Verifier<H> {
    /// `now`: Unix seconds from the DSi's clock (checked by `rules::clock_set`).
    pub fn new(now: u64, anchors: &'static [&'static [u8]]) -> Self {
        Verifier {
            host: heapless::String::new(),
            now,
            anchors,
            leaf: Vec::new(),
            transcript: None,
            cert_ok: false,
            sig_ok: false,
            refusal: Refusal::None,
        }
    }

    /// Both the certificate and its CertificateVerify were checked, in that order, and nothing came after them.
    pub fn authenticated(&self) -> bool {
        self.cert_ok && self.sig_ok
    }
}

impl<CS: TlsCipherSuite> TlsVerifier<CS> for Verifier<CS::Hash> {
    fn set_hostname_verification(&mut self, hostname: &str) -> Result<(), TlsError> {
        self.host.clear();
        self.host.push_str(hostname).map_err(|_| TlsError::InsufficientSpace)
    }

    fn verify_certificate(&mut self, transcript: &CS::Hash, cert: CertificateRef) -> Result<(), TlsError> {
        // a new Certificate replaces whatever was proved before it
        self.cert_ok = false;
        self.sig_ok = false;
        self.transcript = None;
        let mut ders: Vec<&[u8]> = Vec::new();
        for e in &cert.entries {
            if let CertificateEntryRef::X509(d) = e {
                ders.push(d);
            }
        }
        if self.host.is_empty() {
            self.refusal = Refusal::WrongHost;
            return Err(TlsError::InvalidCertificate);
        }
        crate::algs::pause();
        let checked = check_chain(&ders, &self.host, self.now, self.anchors);
        crate::algs::pause();
        if let Err(r) = checked {
            self.refusal = r;
            return Err(TlsError::InvalidCertificate);
        }
        self.leaf = ders[0].to_vec();
        self.transcript = Some(transcript.clone());
        self.cert_ok = true;
        Ok(())
    }

    fn verify_signature(&mut self, verify: CertificateVerifyRef) -> Result<(), TlsError> {
        self.sig_ok = false;
        let Some(hash) = self.transcript.take().filter(|_| self.cert_ok) else {
            self.refusal = Refusal::NoCertificate;
            return Err(TlsError::InvalidCertificate);
        };
        // RFC 8446 4.4.3: 64 spaces, the context string, a zero byte, the transcript hash.
        let mut msg: Vec<u8> = Vec::with_capacity(146);
        msg.resize(64, 0x20);
        msg.extend_from_slice(b"TLS 1.3, server CertificateVerify\x00");
        msg.extend_from_slice(&hash.finalize());
        // the plain algorithms, never `algs::CHAIN`: this signature is checked fresh on every connection
        let alg: &dyn pki_types::SignatureVerificationAlgorithm = match verify.signature_scheme {
            SignatureScheme::EcdsaSecp256r1Sha256 => &algs::ECDSA_P256_SHA256,
            SignatureScheme::EcdsaSecp384r1Sha384 => &algs::ECDSA_P384_SHA384,
            _ => {
                self.refusal = Refusal::WrongScheme;
                return Err(TlsError::InvalidSignatureScheme);
            }
        };
        let der = CertificateDer::from(self.leaf.as_slice());
        let ok = webpki::EndEntityCert::try_from(&der).is_ok_and(|ee| ee.verify_signature(alg, &msg, verify.signature).is_ok());
        if ok {
            self.sig_ok = true;
            Ok(())
        } else {
            self.refusal = Refusal::BadSignature;
            Err(TlsError::InvalidSignature)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::anchors::ANCHORS;

    const LEAF: &[u8] = include_bytes!("../tests/fixtures/getsecretary-1.der");
    const YE1: &[u8] = include_bytes!("../tests/fixtures/getsecretary-2.der");
    const YE_BY_X2: &[u8] = include_bytes!("../tests/fixtures/getsecretary-3.der");
    const X2_BY_X1: &[u8] = include_bytes!("../tests/fixtures/getsecretary-4.der");
    const ROOT_YR: &[u8] = include_bytes!("../tests/fixtures/isrg-root-yr.der");
    /// 2026-11-08 12:00 UTC.
    const NOW: u64 = 1_794_139_200;
    const HOST: &str = "getsecretary.tech";

    #[test]
    fn the_servers_chain_reaches_x1_or_x2() {
        assert_eq!(check_chain(&[LEAF, YE1, YE_BY_X2, X2_BY_X1], HOST, NOW, &ANCHORS), Ok(()));
        // X2 alone, and X1 alone (through X2's cross-signature by X1: RSA)
        assert_eq!(check_chain(&[LEAF, YE1, YE_BY_X2], HOST, NOW, &[crate::anchors::ROOT_X2_DER]), Ok(()));
        assert_eq!(check_chain(&[LEAF, YE1, YE_BY_X2, X2_BY_X1], HOST, NOW, &[crate::anchors::ROOT_X1_DER]), Ok(()));
        // Root YE alone: the leaf and YE1 are enough
        assert_eq!(check_chain(&[LEAF, YE1], HOST, NOW, &[crate::anchors::ROOT_YE_DER]), Ok(()));
    }

    // ---- what the path and the cache cost ----

    use core::sync::atomic::{AtomicU32, Ordering::Relaxed};
    use pki_types::{AlgorithmIdentifier, InvalidSignature, SignatureVerificationAlgorithm};

    /// An algorithm that counts the verifications it really does.
    #[derive(Debug)]
    struct Counting {
        alg: &'static dyn SignatureVerificationAlgorithm,
        n: &'static AtomicU32,
    }

    impl SignatureVerificationAlgorithm for Counting {
        fn verify_signature(&self, k: &[u8], m: &[u8], s: &[u8]) -> Result<(), InvalidSignature> {
            self.n.store(self.n.load(Relaxed) + 1, Relaxed);
            self.alg.verify_signature(k, m, s)
        }
        fn public_key_alg_id(&self) -> AlgorithmIdentifier {
            self.alg.public_key_alg_id()
        }
        fn signature_alg_id(&self) -> AlgorithmIdentifier {
            self.alg.signature_alg_id()
        }
    }

    /// `algs::ALL` counted, behind a cache of the test's own when `cached`.
    fn counted(cached: bool) -> (Vec<&'static dyn SignatureVerificationAlgorithm>, &'static AtomicU32) {
        let n: &'static AtomicU32 = Box::leak(Box::new(AtomicU32::new(0)));
        let cache: &'static algs::SigCache = Box::leak(Box::new(algs::SigCache::new()));
        let list = algs::ALL
            .iter()
            .map(|a| {
                let c: &'static Counting = Box::leak(Box::new(Counting { alg: *a, n }));
                if cached {
                    let w: &'static algs::Cached = Box::leak(Box::new(algs::Cached { alg: c, cache, kind: 1 }));
                    w as &'static dyn SignatureVerificationAlgorithm
                } else {
                    c as &'static dyn SignatureVerificationAlgorithm
                }
            })
            .collect();
        (list, n)
    }

    #[test]
    fn root_ye_saves_one_p384_check() {
        let (algs_, n) = counted(false);
        let chain = [LEAF, YE1, YE_BY_X2, X2_BY_X1];
        assert_eq!(check_chain_with(&chain, HOST, NOW, &[crate::anchors::ROOT_X1_DER, crate::anchors::ROOT_X2_DER], &algs_), Ok(()));
        assert_eq!(n.load(Relaxed), 3, "X1 and X2: leaf, YE1, Root YE by X2");
        n.store(0, Relaxed);
        assert_eq!(check_chain_with(&chain, HOST, NOW, &crate::anchors::ANCHORS, &algs_), Ok(()));
        assert_eq!(n.load(Relaxed), 2, "with Root YE pinned: leaf and YE1");
    }

    #[test]
    fn a_cached_chain_costs_no_signature_check_but_is_checked_again() {
        let (algs_, n) = counted(true);
        let chain = [LEAF, YE1, YE_BY_X2];
        assert_eq!(check_chain_with(&chain, HOST, NOW, &ANCHORS, &algs_), Ok(()));
        assert_eq!(n.load(Relaxed), 2);
        // the same chain again: webpki checks the path, the cache answers both signatures
        assert_eq!(check_chain_with(&chain, HOST, NOW, &ANCHORS, &algs_), Ok(()));
        assert_eq!(n.load(Relaxed), 2);
        // a warm cache changes no verdict: the time, the name and the root are judged on every connection
        assert_eq!(check_chain_with(&chain, HOST, 1_900_000_000, &ANCHORS, &algs_), Err(Refusal::Expired));
        assert_eq!(check_chain_with(&chain, HOST, 1_700_000_000, &ANCHORS, &algs_), Err(Refusal::Expired));
        assert_eq!(check_chain_with(&chain, "evil.getsecretary.tech", NOW, &ANCHORS, &algs_), Err(Refusal::WrongHost));
        assert_eq!(check_chain_with(&chain, HOST, NOW, &[ROOT_YR], &algs_), Err(Refusal::UnknownRoot));
        // a changed signature byte is new input: verified for real, and refused
        let mut leaf = LEAF.to_vec();
        let at = leaf.len() - 10;
        leaf[at] ^= 1;
        let before = n.load(Relaxed);
        assert!(check_chain_with(&[&leaf, YE1, YE_BY_X2], HOST, NOW, &ANCHORS, &algs_).is_err());
        assert!(n.load(Relaxed) > before, "the changed leaf was verified, not answered from the cache");
        // and a refusal is not remembered: the same changed leaf is verified (and refused) again
        let before = n.load(Relaxed);
        assert!(check_chain_with(&[&leaf, YE1, YE_BY_X2], HOST, NOW, &ANCHORS, &algs_).is_err());
        assert!(n.load(Relaxed) > before);
    }

    #[test]
    fn the_cache_forgets_the_oldest_first() {
        let cache = algs::SigCache::new();
        let id = |i: u8| [i; 32];
        for i in 0..crate::rules::SIG_CACHE as u8 {
            cache.put(&id(i));
        }
        assert!((0..crate::rules::SIG_CACHE as u8).all(|i| cache.has(&id(i))));
        cache.put(&id(200));
        assert!(cache.has(&id(200)) && !cache.has(&id(0)) && cache.has(&id(1)));
        // a key put twice takes one slot
        cache.put(&id(200));
        assert!(cache.has(&id(1)));
        assert!(!cache.has(&[7; 31].iter().copied().chain([8]).collect::<Vec<u8>>().try_into().unwrap()));
        cache.clear();
        assert!(!cache.has(&id(200)));
    }

    #[test]
    fn a_wrong_host_is_refused() {
        assert_eq!(check_chain(&[LEAF, YE1, YE_BY_X2], "getsecretary.tec", NOW, &ANCHORS), Err(Refusal::WrongHost));
        assert_eq!(check_chain(&[LEAF, YE1, YE_BY_X2], "evil.getsecretary.tech", NOW, &ANCHORS), Err(Refusal::WrongHost));
    }

    #[test]
    fn an_expired_certificate_is_refused() {
        // the leaf ends on 2027-01-19; a clock past it, and one before it began
        assert_eq!(check_chain(&[LEAF, YE1, YE_BY_X2], HOST, 1_900_000_000, &ANCHORS), Err(Refusal::Expired));
        assert_eq!(check_chain(&[LEAF, YE1, YE_BY_X2], HOST, 1_700_000_000, &ANCHORS), Err(Refusal::Expired));
    }

    #[test]
    fn an_unknown_root_is_refused() {
        assert_eq!(check_chain(&[LEAF, YE1, YE_BY_X2, X2_BY_X1], HOST, NOW, &[ROOT_YR]), Err(Refusal::UnknownRoot));
        assert_eq!(check_chain(&[LEAF], HOST, NOW, &ANCHORS), Err(Refusal::UnknownRoot));
        assert_eq!(check_chain(&[], HOST, NOW, &ANCHORS), Err(Refusal::NoCertificate));
    }

    #[test]
    fn changed_or_cut_certificates_are_refused() {
        let mut leaf = LEAF.to_vec();
        let at = leaf.len() - 10;
        leaf[at] ^= 1;
        assert!(check_chain(&[&leaf, YE1, YE_BY_X2], HOST, NOW, &ANCHORS).is_err());
        for n in [10, LEAF.len() / 2, LEAF.len() - 1] {
            assert!(check_chain(&[&LEAF[..n], YE1, YE_BY_X2], HOST, NOW, &ANCHORS).is_err(), "cut at {n}");
        }
        // an intermediate offered as the server's certificate
        assert!(check_chain(&[YE1, YE_BY_X2], HOST, NOW, &ANCHORS).is_err());
    }
}
