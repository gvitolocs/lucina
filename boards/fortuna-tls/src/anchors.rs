//! Fortuna's trust anchors (the coordinator's pin, 2026-11-08): Let's Encrypt's ISRG Root X1 (RSA 4096), ISRG Root X2
//! (ECDSA P-384) and ISRG Root YE (ECDSA P-384, added in v1.61 with the coordinator's approval), as DER in `anchors/`,
//! nothing else. The gateway's chain today is leaf ← YE1 ← Root YE (cross-signed by X2) ← X2; webpki tries anchors
//! before intermediates, so with YE pinned the path stops at YE: two P-384 checks instead of three (~1.9 s each on the
//! DSi). SHA-256 fingerprints, checked by the tests:
//! X1 96BCEC06264976F37460779ACF28C5A7CFE8A3C0AAE11A8FFCEE05C0BDDF08C6,
//! X2 69729B8E15A86EFC177A57AFB7171DFC64ADD28C2FCA8CF1507E34453CCB1470 (Mozilla's root store),
//! YE E14FFCAD5B0025731006CAA43A121A22D8E9700F4FB9CF852F02A708AA5D5666 (letsencrypt.org/certs/gen-y/root-ye.der,
//! fetched 2026-11-08; its key is the one in the gateway's X2-signed Root YE, which the tests check).

pub const ROOT_X1_DER: &[u8] = include_bytes!("../anchors/isrg-root-x1.der");
pub const ROOT_X2_DER: &[u8] = include_bytes!("../anchors/isrg-root-x2.der");
pub const ROOT_YE_DER: &[u8] = include_bytes!("../anchors/isrg-root-ye.der");
pub const ANCHORS: [&[u8]; 3] = [ROOT_X1_DER, ROOT_X2_DER, ROOT_YE_DER];

#[cfg(test)]
mod tests {
    use super::*;
    use pki_types::CertificateDer;
    use sha2::{Digest, Sha256};

    fn hex(b: &[u8]) -> String {
        b.iter().map(|x| format!("{x:02X}")).collect()
    }

    #[test]
    fn fingerprints() {
        assert_eq!(hex(&Sha256::digest(ROOT_X1_DER)), "96BCEC06264976F37460779ACF28C5A7CFE8A3C0AAE11A8FFCEE05C0BDDF08C6");
        assert_eq!(hex(&Sha256::digest(ROOT_X2_DER)), "69729B8E15A86EFC177A57AFB7171DFC64ADD28C2FCA8CF1507E34453CCB1470");
        assert_eq!(hex(&Sha256::digest(ROOT_YE_DER)), "E14FFCAD5B0025731006CAA43A121A22D8E9700F4FB9CF852F02A708AA5D5666");
    }

    /// The pinned Root YE is the root the gateway's X2-signed Root YE certifies: same subject, same key.
    #[test]
    fn root_ye_is_the_gateways_root_ye() {
        let gw = CertificateDer::from(&include_bytes!("../tests/fixtures/getsecretary-3.der")[..]);
        let pinned = CertificateDer::from(ROOT_YE_DER);
        let a = webpki::anchor_from_trusted_cert(&gw).unwrap();
        let b = webpki::anchor_from_trusted_cert(&pinned).unwrap();
        assert_eq!(a.subject.as_ref(), b.subject.as_ref());
        assert_eq!(a.subject_public_key_info.as_ref(), b.subject_public_key_info.as_ref());
    }
}
