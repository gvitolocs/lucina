//! Published vectors for every primitive. Where a source lists no vector for a case we need (the KDF with 802.11's
//! label, the wrap of a 24-byte key) the expected value came from another implementation (Python hashlib and hmac,
//! OpenSSL's id-aes128-wrap), as noted.

use lucina_wifi_mac::crypto::aes::Aes128;
use lucina_wifi_mac::crypto::cmac::aes128_cmac;
use lucina_wifi_mac::crypto::hash::{
    Digest, Sha1, Sha256, hmac_sha1, hmac_sha256, pbkdf2_hmac_sha1,
};
use lucina_wifi_mac::crypto::keywrap::{aes_unwrap, aes_wrap};
use lucina_wifi_mac::crypto::prf::{kdf_sha256, prf_sha1};
use lucina_wifi_mac::crypto::secret::{SecretBytes, SecretVec, ct_eq, wipe};
use lucina_wifi_mac::wpa::derive_psk;

fn hex(s: &str) -> Vec<u8> {
    assert!(s.len().is_multiple_of(2));
    (0..s.len() / 2)
        .map(|i| u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).unwrap())
        .collect()
}

fn sha1(data: &[u8]) -> Vec<u8> {
    let mut d = Sha1::new();
    d.update(data);
    let mut out = [0; 20];
    d.finish(&mut out);
    out.to_vec()
}

fn sha256(data: &[u8]) -> Vec<u8> {
    let mut d = Sha256::new();
    d.update(data);
    let mut out = [0; 32];
    d.finish(&mut out);
    out.to_vec()
}

#[test]
fn sha1_rfc3174_and_fips() {
    assert_eq!(
        sha1(b"abc"),
        hex("a9993e364706816aba3e25717850c26c9cd0d89d")
    );
    assert_eq!(
        sha1(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
        hex("84983e441c3bd26ebaae4aa1f95129e5e54670f1")
    );
    assert_eq!(
        sha1(&vec![b'a'; 1_000_000]),
        hex("34aa973cd4c4daa4f61eeb2bdbad27316534016f")
    );
    // RFC 3174's test 4: 80 bytes of "01234567" repeated.
    assert_eq!(
        sha1(&b"01234567".repeat(80)),
        hex("dea356a2cddd90c7a7ecedc5ebb563934f460452")
    );
    assert_eq!(sha1(b""), hex("da39a3ee5e6b4b0d3255bfef95601890afd80709"));
}

#[test]
fn sha1_any_split_gives_the_same_hash() {
    let data: Vec<u8> = (0..200u32).map(|i| i as u8).collect();
    let whole = sha1(&data);
    for split in 0..data.len() {
        let mut d = Sha1::new();
        d.update(&data[..split]);
        d.update(&data[split..]);
        let mut out = [0; 20];
        d.finish(&mut out);
        assert_eq!(out.to_vec(), whole, "split at {split}");
    }
}

#[test]
fn sha256_fips() {
    assert_eq!(
        sha256(b"abc"),
        hex("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad")
    );
    assert_eq!(
        sha256(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
        hex("248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1")
    );
    assert_eq!(
        sha256(&vec![b'a'; 1_000_000]),
        hex("cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0")
    );
}

#[test]
fn hmac_sha1_rfc2202() {
    let cases: [(&[u8], &[u8], &str); 5] = [
        (
            &[0x0b; 20],
            b"Hi There",
            "b617318655057264e28bc0b6fb378c8ef146be00",
        ),
        (
            b"Jefe",
            b"what do ya want for nothing?",
            "effcdf6ae5eb2fa2d27416d5f184df9c259a7c79",
        ),
        (
            &[0xaa; 20],
            &[0xdd; 50],
            "125d7342b9ac11cd91a39af48aa17b4f63f175d3",
        ),
        (
            &[0xaa; 80],
            b"Test Using Larger Than Block-Size Key - Hash Key First",
            "aa4ae5e15272d00e95705637ce8a3b55ed402112",
        ),
        (
            &[0xaa; 80],
            b"Test Using Larger Than Block-Size Key and Larger Than One Block-Size Data",
            "e8e99d0f45237d786d6bbaa7965c7808bbff1a91",
        ),
    ];
    for (key, data, want) in cases {
        assert_eq!(hmac_sha1(key, data).to_vec(), hex(want));
    }
}

#[test]
fn hmac_sha256_rfc4231() {
    assert_eq!(
        hmac_sha256(&[0x0b; 20], b"Hi There").to_vec(),
        hex("b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7")
    );
}

#[test]
fn pbkdf2_rfc6070() {
    let cases: [(&[u8], &[u8], u32, &str); 5] = [
        (
            b"password",
            b"salt",
            1,
            "0c60c80f961f0e71f3a9b524af6012062fe037a6",
        ),
        (
            b"password",
            b"salt",
            2,
            "ea6c014dc72d6f8ccd1ed92ace1d41f0d8de8957",
        ),
        (
            b"password",
            b"salt",
            4096,
            "4b007901b765489abead49d926f721d065a429c1",
        ),
        (
            b"passwordPASSWORDpassword",
            b"saltSALTsaltSALTsaltSALTsaltSALTsalt",
            4096,
            "3d2eec4fe41c849b80c8d83662c0e44a8b291a964cf2f07038",
        ),
        (
            b"pass\0word",
            b"sa\0lt",
            4096,
            "56fa6aa75548099dcc37d7f03425e0c3",
        ),
    ];
    for (pw, salt, n, want) in cases {
        let want = hex(want);
        let mut out = vec![0; want.len()];
        pbkdf2_hmac_sha1(pw, salt, n, &mut out);
        assert_eq!(out, want);
    }
}

/// IEEE 802.11-2020 Annex J.4.2, the passphrase-to-PSK examples. The third one's value also matches Python's
/// `hashlib.pbkdf2_hmac`; `wpa_passphrase` from wpa_supplicant 2.11 agreed on the first when this was written.
#[test]
fn psk_annex_j() {
    let cases: [(&[u8], &[u8], &str); 3] = [
        (
            b"password",
            b"IEEE",
            "f42c6fc52df0ebef9ebb4b90b38a5f902e83fe1b135a70e23aed762e9710a12e",
        ),
        (
            b"ThisIsAPassword",
            b"ThisIsASSID",
            "0dc0d6eb90555ed6419756b9a15ec3e3209b63df707dd508d14581f8982721af",
        ),
        (
            b"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            b"ZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZ",
            "becb93866bb8c3832cb777c2f559807c8c59afcb6eae734885001300a981cc62",
        ),
    ];
    for (pw, ssid, want) in cases {
        assert_eq!(derive_psk(pw, ssid).expose().to_vec(), hex(want));
    }
}

/// The PRF test vectors of IEEE 802.11 Annex J.3 (same as wpa_supplicant's `test-sha1`).
#[test]
fn prf_annex_j() {
    let cases: [(&[u8], &[u8], &str); 3] = [
        (
            &[0x0b; 20],
            b"Hi There",
            "bcd4c650b30b9684951829e0d75f9d54b862175ed9f00606e17d8da35402ffee\
             75df78c3d31e0f889f012120c0862beb67753e7439ae242edb8373698356cf5a",
        ),
        (
            b"Jefe",
            b"what do ya want for nothing?",
            "51f4de5b33f249adf81aeb713a3c20f4fe631446fabdfa58244759ae58ef9009\
             a99abf4eac2ca5fa87e692c440eb40023e7babb206d61de7b92f41529092b8fc",
        ),
        (
            &[0xaa; 20],
            &[0xdd; 50],
            "e1ac546ec4cb636f9976487be5c86be17a0252ca5d8d8df12cfb0473525249ce\
             9dd8d177ead710bc9b590547239107aef7b4abd43d87f0a68f1cbd9e2b6f7607",
        ),
    ];
    for (key, data, want) in cases {
        let want = hex(want);
        let mut out = vec![0; want.len()];
        prf_sha1(key, b"prefix", data, &mut out);
        assert_eq!(out, want);
    }
}

/// Expected value from Python's hmac with 802.11's KDF written out.
#[test]
fn kdf_sha256_against_python() {
    let key: Vec<u8> = (0..32).collect();
    let ctx: Vec<u8> = (0..76).collect();
    let mut out = [0; 48];
    kdf_sha256(&key, b"Pairwise key expansion", &ctx, &mut out);
    assert_eq!(
        out.to_vec(),
        hex(
            "ca5d2b63205753bc5b26c31a135d1fb964f1ce5d87062246c4da6433d5422ca19065f6c58c3da70e0756992606528f4d"
        )
    );
}

#[test]
fn aes_fips197() {
    let cases = [
        // Appendix C.1.
        (
            "000102030405060708090a0b0c0d0e0f",
            "00112233445566778899aabbccddeeff",
            "69c4e0d86a7b0430d8cdb78070b4c55a",
        ),
        // Appendix B.
        (
            "2b7e151628aed2a6abf7158809cf4f3c",
            "3243f6a8885a308d313198a2e0370734",
            "3925841d02dc09fbdc118597196a0b32",
        ),
    ];
    for (key, plain, cipher) in cases {
        let key: [u8; 16] = hex(key).try_into().unwrap();
        let aes = Aes128::new(&key);
        let mut block: [u8; 16] = hex(plain).try_into().unwrap();
        aes.encrypt_block(&mut block);
        assert_eq!(block.to_vec(), hex(cipher));
        aes.decrypt_block(&mut block);
        assert_eq!(block.to_vec(), hex(plain));
    }
}

#[test]
fn aes_decrypt_inverts_encrypt_on_many_blocks() {
    let key: [u8; 16] = core::array::from_fn(|i| (i * 17) as u8);
    let aes = Aes128::new(&key);
    for seed in 0..=255u8 {
        let plain: [u8; 16] = core::array::from_fn(|i| seed.wrapping_mul(i as u8 + 3));
        let mut b = plain;
        aes.encrypt_block(&mut b);
        assert_ne!(b, plain);
        aes.decrypt_block(&mut b);
        assert_eq!(b, plain);
    }
}

#[test]
fn cmac_rfc4493() {
    let key: [u8; 16] = hex("2b7e151628aed2a6abf7158809cf4f3c").try_into().unwrap();
    let msg = hex(
        "6bc1bee22e409f96e93d7e117393172aae2d8a571e03ac9c9eb76fac45af8e51\
         30c81c46a35ce411e5fbc1191a0a52eff69f2445df4f9b17ad2b417be66c3710",
    );
    let cases = [
        (0, "bb1d6929e95937287fa37d129b756746"),
        (16, "070a16b46b4d4144f79bdd9dd04a287c"),
        (40, "dfa66747de9ae63030ca32611497c827"),
        (64, "51f0bebf7e3b9d92fc49741779363cfe"),
    ];
    for (len, want) in cases {
        assert_eq!(aes128_cmac(&key, &msg[..len]).to_vec(), hex(want));
    }
}

#[test]
fn key_wrap_rfc3394_and_openssl() {
    let kek: [u8; 16] = hex("000102030405060708090a0b0c0d0e0f").try_into().unwrap();
    // RFC 3394 section 4.1.
    let plain = hex("00112233445566778899aabbccddeeff");
    let wrapped = hex("1fa68b0a8112b447aef34bd8fb5a7b829d3e862371d2cfe5");
    assert_eq!(aes_wrap(&kek, &plain).unwrap(), wrapped);
    assert_eq!(
        aes_unwrap(&kek, &wrapped).unwrap().expose(),
        plain.as_slice()
    );
    // 24 bytes: from `openssl enc -id-aes128-wrap`, a size the RFC lists only with longer KEKs.
    let plain = hex("00112233445566778899aabbccddeeff0001020304050607");
    let wrapped = hex("889671106535a9f86d9f9a262f674569efa38d7535aac77527cab92855bddd6e");
    assert_eq!(aes_wrap(&kek, &plain).unwrap(), wrapped);
    assert_eq!(
        aes_unwrap(&kek, &wrapped).unwrap().expose(),
        plain.as_slice()
    );
}

#[test]
fn key_unwrap_refuses_damage_and_bad_lengths() {
    let kek = [7u8; 16];
    let plain = [9u8; 32];
    let wrapped = aes_wrap(&kek, &plain).unwrap();
    for i in 0..wrapped.len() {
        let mut bad = wrapped.clone();
        bad[i] ^= 0x40;
        assert!(aes_unwrap(&kek, &bad).is_none(), "byte {i}");
    }
    assert!(aes_unwrap(&[8u8; 16], &wrapped).is_none());
    assert!(aes_unwrap(&kek, &wrapped[..wrapped.len() - 1]).is_none());
    assert!(aes_unwrap(&kek, &wrapped[..16]).is_none());
    assert!(aes_unwrap(&kek, &[]).is_none());
    assert!(aes_wrap(&kek, &[0; 8]).is_none());
    assert!(aes_wrap(&kek, &[0; 17]).is_none());
}

#[test]
fn secrets_compare_and_never_print() {
    assert!(ct_eq(b"abc", b"abc"));
    assert!(!ct_eq(b"abc", b"abd"));
    assert!(!ct_eq(b"abc", b"ab"));
    let s = SecretBytes::new([0xAB; 16]);
    assert_eq!(format!("{s:?}"), "<16 secret bytes>");
    let v = SecretVec::from_slice(&[0xCD; 5]);
    assert_eq!(format!("{v:?}"), "<5 secret bytes>");
    assert_eq!(s, SecretBytes::new([0xAB; 16]));
    assert_ne!(s, SecretBytes::new([0xAC; 16]));
    let mut buf = [1u8; 9];
    wipe(&mut buf);
    assert_eq!(buf, [0; 9]);
}
