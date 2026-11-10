//! The signature algorithms webpki checks with, on RustCrypto (webpki brings none without ring or aws-lc, neither of
//! which builds for the DSi's ARMv5): ECDSA P-256 and P-384 with SHA-256/384, and RSA PKCS#1 v1.5 with SHA-256/384
//! (ISRG Root X1 signs with it). Each is only "verify this signature with this key"; the X.509 and the path are
//! webpki's.
//!
//! Certificates are checked with `CHAIN`: the same algorithms behind a cache of signatures already verified since boot
//! (`SigCache`, RAM only). A P-384 check costs ~1.9 s on the DSi's ARM9 and the gateway's chain needs two, on every new
//! connection; the cache answers an identical check (same algorithm, key, signed bytes and signature, hashed with
//! SHA-256) without the arithmetic. webpki still builds and checks the whole path on every connection (the validity
//! times at that moment, the host name, CA and path-length constraints, key usage, the anchors): only the signature
//! arithmetic over identical bytes is remembered. The server's CertificateVerify never goes through the cache: its
//! signature is over this connection's own transcript and is checked fresh with the leaf's key (`verify.rs` uses the
//! plain algorithms for it). Failures are never stored.

use core::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering::Relaxed};

use pki_types::{alg_id, AlgorithmIdentifier, InvalidSignature, SignatureVerificationAlgorithm};
use sha2::{Digest, Sha256, Sha384};

// ---- the host's hooks: a yield between the expensive steps, a microsecond clock for their cost ----
//
// On the DSi the handshake runs on a cooperative cothread: a signature check that never yields keeps the main cothread
// (the watchdog's heartbeat) and Wi-Fi from running. v1.57's first handshake on the real ARM9 ran past the watchdog's
// 15 s inside P-384 arithmetic. The kernel installs a yield and a clock; each verification yields before and after
// itself and counts its time here.

static PAUSE: AtomicUsize = AtomicUsize::new(0);
static CLOCK_US: AtomicUsize = AtomicUsize::new(0);

/// Installs the host's yield and its microsecond clock (both plain functions; one cothread uses TLS).
pub fn set_hooks(pause: fn(), clock_us: fn() -> u32) {
    PAUSE.store(pause as usize, Relaxed);
    CLOCK_US.store(clock_us as usize, Relaxed);
}

pub(crate) fn pause() {
    let p = PAUSE.load(Relaxed);
    if p != 0 {
        // SAFETY: only `set_hooks` stores here, from a `fn()`.
        let f: fn() = unsafe { core::mem::transmute::<usize, fn()>(p) };
        f();
    }
}

fn clock_us() -> u32 {
    let c = CLOCK_US.load(Relaxed);
    if c == 0 {
        return 0;
    }
    // SAFETY: only `set_hooks` stores here, from a `fn() -> u32`.
    let f: fn() -> u32 = unsafe { core::mem::transmute::<usize, fn() -> u32>(c) };
    f()
}

/// What the verifications cost since `take_costs`: per kind (0 P-256, 1 P-384, 2 RSA) the count, the total and the
/// longest in microseconds, and how many checks the cache answered instead.
pub struct Cost {
    pub n: u32,
    pub total_us: u32,
    pub max_us: u32,
    pub cached: u32,
}

static N: [AtomicU32; 3] = [AtomicU32::new(0), AtomicU32::new(0), AtomicU32::new(0)];
static TOTAL: [AtomicU32; 3] = [AtomicU32::new(0), AtomicU32::new(0), AtomicU32::new(0)];
static MAX: [AtomicU32; 3] = [AtomicU32::new(0), AtomicU32::new(0), AtomicU32::new(0)];
static CACHED: [AtomicU32; 3] = [AtomicU32::new(0), AtomicU32::new(0), AtomicU32::new(0)];

fn timed<R>(kind: usize, f: impl FnOnce() -> R) -> R {
    pause();
    let t0 = clock_us();
    let r = f();
    let us = clock_us().wrapping_sub(t0);
    N[kind].store(N[kind].load(Relaxed) + 1, Relaxed);
    TOTAL[kind].store(TOTAL[kind].load(Relaxed).wrapping_add(us), Relaxed);
    if us > MAX[kind].load(Relaxed) {
        MAX[kind].store(us, Relaxed);
    }
    pause();
    r
}

/// The costs so far, and zero for the next handshake.
pub fn take_costs() -> [Cost; 3] {
    core::array::from_fn(|k| {
        let c = Cost { n: N[k].load(Relaxed), total_us: TOTAL[k].load(Relaxed), max_us: MAX[k].load(Relaxed), cached: CACHED[k].load(Relaxed) };
        N[k].store(0, Relaxed);
        TOTAL[k].store(0, Relaxed);
        MAX[k].store(0, Relaxed);
        CACHED[k].store(0, Relaxed);
        c
    })
}

#[derive(Debug)]
pub struct Ecdsa {
    curve: Curve,
    hash: Hash,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Curve {
    P256,
    P384,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hash {
    Sha256,
    Sha384,
}

fn digest(h: Hash, msg: &[u8], out: &mut [u8; 48]) -> usize {
    match h {
        Hash::Sha256 => {
            out[..32].copy_from_slice(&Sha256::digest(msg));
            32
        }
        Hash::Sha384 => {
            out.copy_from_slice(&Sha384::digest(msg));
            48
        }
    }
}

/// `key`: the uncompressed point; `sig`: DER. Used for certificates (through webpki) and for the server's
/// CertificateVerify (through webpki's `EndEntityCert::verify_signature`).
pub fn ecdsa_verify(curve: Curve, hash: Hash, key: &[u8], msg: &[u8], sig: &[u8]) -> bool {
    timed(if curve == Curve::P256 { 0 } else { 1 }, || ecdsa_verify_now(curve, hash, key, msg, sig))
}

fn ecdsa_verify_now(curve: Curve, hash: Hash, key: &[u8], msg: &[u8], sig: &[u8]) -> bool {
    use ecdsa::signature::hazmat::PrehashVerifier;
    let mut d = [0u8; 48];
    let n = digest(hash, msg, &mut d);
    match curve {
        Curve::P256 => {
            let (Ok(k), Ok(s)) = (p256::ecdsa::VerifyingKey::from_sec1_bytes(key), p256::ecdsa::Signature::from_der(sig)) else {
                return false;
            };
            k.verify_prehash(&d[..n], &s).is_ok()
        }
        Curve::P384 => {
            let (Ok(k), Ok(s)) = (p384::ecdsa::VerifyingKey::from_sec1_bytes(key), p384::ecdsa::Signature::from_der(sig)) else {
                return false;
            };
            k.verify_prehash(&d[..n], &s).is_ok()
        }
    }
}

impl SignatureVerificationAlgorithm for Ecdsa {
    fn verify_signature(&self, public_key: &[u8], message: &[u8], signature: &[u8]) -> Result<(), InvalidSignature> {
        ecdsa_verify(self.curve, self.hash, public_key, message, signature).then_some(()).ok_or(InvalidSignature)
    }
    fn public_key_alg_id(&self) -> AlgorithmIdentifier {
        match self.curve {
            Curve::P256 => alg_id::ECDSA_P256,
            Curve::P384 => alg_id::ECDSA_P384,
        }
    }
    fn signature_alg_id(&self) -> AlgorithmIdentifier {
        match self.hash {
            Hash::Sha256 => alg_id::ECDSA_SHA256,
            Hash::Sha384 => alg_id::ECDSA_SHA384,
        }
    }
}

#[derive(Debug)]
pub struct RsaPkcs1 {
    hash: Hash,
}

impl SignatureVerificationAlgorithm for RsaPkcs1 {
    fn verify_signature(&self, public_key: &[u8], message: &[u8], signature: &[u8]) -> Result<(), InvalidSignature> {
        timed(2, || self.verify_now(public_key, message, signature))
    }
    fn public_key_alg_id(&self) -> AlgorithmIdentifier {
        alg_id::RSA_ENCRYPTION
    }
    fn signature_alg_id(&self) -> AlgorithmIdentifier {
        match self.hash {
            Hash::Sha256 => alg_id::RSA_PKCS1_SHA256,
            Hash::Sha384 => alg_id::RSA_PKCS1_SHA384,
        }
    }
}

impl RsaPkcs1 {
    fn verify_now(&self, public_key: &[u8], message: &[u8], signature: &[u8]) -> Result<(), InvalidSignature> {
        use rsa::pkcs1::DecodeRsaPublicKey;
        let key = rsa::RsaPublicKey::from_pkcs1_der(public_key).map_err(|_| InvalidSignature)?;
        // webpki's own floor for RSA keys
        if rsa::traits::PublicKeyParts::size(&key) < 2048 / 8 {
            return Err(InvalidSignature);
        }
        let mut d = [0u8; 48];
        let n = digest(self.hash, message, &mut d);
        let scheme = match self.hash {
            Hash::Sha256 => rsa::Pkcs1v15Sign::new::<Sha256>(),
            Hash::Sha384 => rsa::Pkcs1v15Sign::new::<Sha384>(),
        };
        key.verify(scheme, &d[..n], signature).map_err(|_| InvalidSignature)
    }
}

pub static ECDSA_P256_SHA256: Ecdsa = Ecdsa { curve: Curve::P256, hash: Hash::Sha256 };
pub static ECDSA_P256_SHA384: Ecdsa = Ecdsa { curve: Curve::P256, hash: Hash::Sha384 };
pub static ECDSA_P384_SHA256: Ecdsa = Ecdsa { curve: Curve::P384, hash: Hash::Sha256 };
pub static ECDSA_P384_SHA384: Ecdsa = Ecdsa { curve: Curve::P384, hash: Hash::Sha384 };
pub static RSA_PKCS1_SHA256: RsaPkcs1 = RsaPkcs1 { hash: Hash::Sha256 };
pub static RSA_PKCS1_SHA384: RsaPkcs1 = RsaPkcs1 { hash: Hash::Sha384 };

/// What certificates may be signed with.
pub static ALL: &[&dyn SignatureVerificationAlgorithm] =
    &[&ECDSA_P256_SHA256, &ECDSA_P256_SHA384, &ECDSA_P384_SHA256, &ECDSA_P384_SHA384, &RSA_PKCS1_SHA256, &RSA_PKCS1_SHA384];

// ---- the verified-signature cache ----

/// Signatures verified since boot, as SHA-256 over (algorithm, key, signed bytes, signature): `rules::SIG_CACHE` of
/// them, the oldest replaced first. RAM only, so a reboot starts empty. Plain loads and stores (the ARMv5 has no
/// compare-and-swap); on the DSi one cothread does TLS. A slot read while another thread writes it (host tests) can
/// only fail to match: a hit needs all 256 bits equal to a key that was verified.
pub struct SigCache {
    keys: [[AtomicU32; 8]; crate::rules::SIG_CACHE as usize],
    used: [AtomicBool; crate::rules::SIG_CACHE as usize],
    next: AtomicU32,
}

#[allow(clippy::declare_interior_mutable_const)]
const ZERO_KEY: [AtomicU32; 8] = [const { AtomicU32::new(0) }; 8];

impl SigCache {
    #[allow(clippy::new_without_default)]
    pub const fn new() -> SigCache {
        SigCache {
            keys: [ZERO_KEY; crate::rules::SIG_CACHE as usize],
            used: [const { AtomicBool::new(false) }; crate::rules::SIG_CACHE as usize],
            next: AtomicU32::new(0),
        }
    }

    fn words(id: &[u8; 32]) -> [u32; 8] {
        core::array::from_fn(|i| u32::from_le_bytes([id[4 * i], id[4 * i + 1], id[4 * i + 2], id[4 * i + 3]]))
    }

    pub fn has(&self, id: &[u8; 32]) -> bool {
        let w = Self::words(id);
        self.keys.iter().zip(&self.used).any(|(k, u)| u.load(Relaxed) && k.iter().zip(&w).all(|(a, b)| a.load(Relaxed) == *b))
    }

    pub fn put(&self, id: &[u8; 32]) {
        if self.has(id) {
            return;
        }
        let at = self.next.load(Relaxed).min(crate::rules::SIG_CACHE - 1);
        self.next.store(crate::rules::next_slot(at), Relaxed);
        let i = at as usize;
        self.used[i].store(false, Relaxed);
        for (k, b) in self.keys[i].iter().zip(Self::words(id)) {
            k.store(b, Relaxed);
        }
        self.used[i].store(true, Relaxed);
    }

    /// Forgets everything (tests).
    pub fn clear(&self) {
        for u in &self.used {
            u.store(false, Relaxed);
        }
    }
}

/// The DSi's cache, behind `CHAIN`.
pub static CACHE: SigCache = SigCache::new();

/// What the cache keys on: every input of the verification, each with its length.
pub fn cache_id(alg: &dyn SignatureVerificationAlgorithm, key: &[u8], msg: &[u8], sig: &[u8]) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(b"fortuna-tls verified signature v1\0");
    let (pk, sa) = (alg.public_key_alg_id(), alg.signature_alg_id());
    for part in [pk.as_ref(), sa.as_ref(), key, msg, sig] {
        h.update((part.len() as u32).to_be_bytes());
        h.update(part);
    }
    h.finalize().into()
}

/// An algorithm behind a cache: a signature already verified with exactly these inputs passes without the
/// arithmetic; a new one is verified, and stored only when it passed.
pub struct Cached {
    pub alg: &'static dyn SignatureVerificationAlgorithm,
    pub cache: &'static SigCache,
    /// For the counters: 0 P-256, 1 P-384, 2 RSA.
    pub kind: usize,
}

impl core::fmt::Debug for Cached {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "Cached({:?})", self.alg)
    }
}

impl core::fmt::Debug for SigCache {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("SigCache")
    }
}

impl SignatureVerificationAlgorithm for Cached {
    fn verify_signature(&self, public_key: &[u8], message: &[u8], signature: &[u8]) -> Result<(), InvalidSignature> {
        let id = cache_id(self.alg, public_key, message, signature);
        if self.cache.has(&id) {
            CACHED[self.kind].store(CACHED[self.kind].load(Relaxed) + 1, Relaxed);
            return Ok(());
        }
        let r = self.alg.verify_signature(public_key, message, signature);
        if r.is_ok() {
            self.cache.put(&id);
        }
        r
    }
    fn public_key_alg_id(&self) -> AlgorithmIdentifier {
        self.alg.public_key_alg_id()
    }
    fn signature_alg_id(&self) -> AlgorithmIdentifier {
        self.alg.signature_alg_id()
    }
}

static C_P256_SHA256: Cached = Cached { alg: &ECDSA_P256_SHA256, cache: &CACHE, kind: 0 };
static C_P256_SHA384: Cached = Cached { alg: &ECDSA_P256_SHA384, cache: &CACHE, kind: 0 };
static C_P384_SHA256: Cached = Cached { alg: &ECDSA_P384_SHA256, cache: &CACHE, kind: 1 };
static C_P384_SHA384: Cached = Cached { alg: &ECDSA_P384_SHA384, cache: &CACHE, kind: 1 };
static C_RSA_SHA256: Cached = Cached { alg: &RSA_PKCS1_SHA256, cache: &CACHE, kind: 2 };
static C_RSA_SHA384: Cached = Cached { alg: &RSA_PKCS1_SHA384, cache: &CACHE, kind: 2 };

/// What certificates are checked with: `ALL` behind the DSi's cache. Never used for CertificateVerify.
pub static CHAIN: &[&dyn SignatureVerificationAlgorithm] =
    &[&C_P256_SHA256, &C_P256_SHA384, &C_P384_SHA256, &C_P384_SHA384, &C_RSA_SHA256, &C_RSA_SHA384];
