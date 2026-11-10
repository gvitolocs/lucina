//! The crypto a WPA2-PSK supplicant needs, written here because the kernel build takes no external crates:
//! SHA-1 (FIPS 180-4), SHA-256, HMAC (RFC 2104), PBKDF2-HMAC-SHA1 (RFC 8018), the 802.11 PRF and KDF, AES-128
//! (FIPS 197), AES-CMAC (RFC 4493) and AES key wrap (RFC 3394).
//!
//! All of it runs only during a handshake, on the owner's thread. The AES here uses tables: its timing depends
//! on the key at cache level, which a remote peer cannot measure. CCMP on data frames is the hardware's work.

pub mod aes;
pub mod cmac;
pub mod hash;
pub mod keywrap;
pub mod prf;
pub mod secret;
