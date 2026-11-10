//! TLS 1.3 for Fortuna, LucinaOS on the Nintendo DSi: the client to the owner's Secretary gateway.
//!
//! - The protocol is embedded-tls (adopted: TLS 1.3, no_std, no allocator, AES-128-GCM, P-256 key exchange).
//! - The server check (`verify`) is rustls-webpki's (adopted: the chain, names, validity, key usage) to ISRG Root X1
//!   and X2 only (`anchors`), with RustCrypto signature algorithms (`algs`) and the DSi's time (`rules`, Verus).
//! - Randomness (`rng`): a ChaCha20 generator seeded by SHA-256 over what the DSi can offer (a seed file kept on the
//!   SD card, timer jitter, the clock, microphone noise when recording).
//!
//! The kernel gives a socket (`embedded_io::Read + Write`) and the time; `connect` does the rest.
#![cfg_attr(not(test), no_std)]

extern crate alloc;

pub mod algs;

pub mod anchors;
pub mod rng;
pub mod rules;
pub mod verify;

pub use embedded_tls::blocking::{Aes128GcmSha256, TlsConnection, TlsError};

use embedded_tls::blocking::{CryptoProvider, TlsConfig, TlsContext};

/// The cipher suite Fortuna offers: AES-128-GCM with SHA-256 (the gateway's first choice).
pub type Suite = Aes128GcmSha256;
/// The read buffer must hold a whole record: 16 KB of data plus the record overhead.
pub const READ_BUF: usize = 16_640;
/// Fortuna's own records stay small: requests, and uploads in 4 KB pieces.
pub const WRITE_BUF: usize = 4_352;

struct Provider<'r> {
    rng: &'r mut rng::Rng,
    verifier: verify::Verifier<<Suite as embedded_tls::blocking::TlsCipherSuite>::Hash>,
}

impl CryptoProvider for Provider<'_> {
    type CipherSuite = Suite;
    type Signature = p256::ecdsa::DerSignature;

    fn rng(&mut self) -> impl rand_core::CryptoRngCore {
        &mut *self.rng
    }

    fn verifier(&mut self) -> Result<&mut impl embedded_tls::blocking::TlsVerifier<Suite>, TlsError> {
        Ok(&mut self.verifier)
    }
}

/// Opens TLS on `socket` to `host`: the handshake with the full server check, and refused unless the server sent a
/// certificate webpki accepts and proved its key with CertificateVerify. `now` is the Unix time from the DSi's clock;
/// `built` the build time: a clock before it counts as never set (`rules::clock_set`) and nothing is sent. On a
/// refusal the reason is in the `Refusal`.
pub fn connect<'b, S: embedded_io::Read + embedded_io::Write>(
    socket: S,
    host: &str,
    now: u64,
    built: u64,
    rng: &mut rng::Rng,
    read_buf: &'b mut [u8],
    write_buf: &'b mut [u8],
) -> Result<TlsConnection<'b, S, Suite>, (TlsError, verify::Refusal)> {
    connect_with(socket, host, now, built, rng, read_buf, write_buf, &anchors::ANCHORS)
}

/// `connect` with other trust anchors: the tests' generated PKI. Not public: the DSi trusts X1 and X2 only.
#[allow(clippy::too_many_arguments)]
pub(crate) fn connect_with<'b, S: embedded_io::Read + embedded_io::Write>(
    socket: S,
    host: &str,
    now: u64,
    built: u64,
    rng: &mut rng::Rng,
    read_buf: &'b mut [u8],
    write_buf: &'b mut [u8],
    anchors: &'static [&'static [u8]],
) -> Result<TlsConnection<'b, S, Suite>, (TlsError, verify::Refusal)> {
    // an unset clock: no certificate's validity can be judged, so no connection (not even a byte sent)
    if !rules::clock_set(now, built) {
        return Err((TlsError::InvalidCertificate, verify::Refusal::ClockUnset));
    }
    let config = TlsConfig::new().with_server_name(host);
    let mut conn = TlsConnection::new(socket, read_buf, write_buf);
    let mut provider = Provider { rng, verifier: verify::Verifier::new(now, anchors) };
    match conn.open(TlsContext::new(&config, &mut provider)) {
        // embedded-tls finishes a handshake whose server skipped Certificate or CertificateVerify: refuse it here,
        // before any application data (open() sends only the client's Finished)
        Ok(()) if provider.verifier.authenticated() => Ok(conn),
        Ok(()) => Err((TlsError::InvalidCertificate, verify::Refusal::NotAuthenticated)),
        Err(e) => Err((e, provider.verifier.refusal)),
    }
}

#[cfg(test)]
mod mock_tests;
