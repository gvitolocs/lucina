// The integer rules Fortuna adds around the certificate check (the check itself is rustls-webpki's): whether the
// DSi's clock can be trusted at all, and how long a chain may be. Verus checks this very file: `//@` lines hold the
// proof, see scripts/verify-shipped.sh (D-011). `verify.rs` and `lib.rs` call these functions and keep no copy of the
// rules.

/// Intermediates Fortuna takes after the server's certificate. The gateway sends three; a cap keeps a forged chain
/// from making webpki's path search check dozens of P-384 signatures on the ARM9 (seconds each).
pub const MAX_INTERMEDIATES: u32 = 4;

/// Whether the DSi's clock (the RTC, Unix seconds) has been set: never earlier than Fortuna's build. An unset clock
/// (2000-01-01) refuses every connection ("Set the DSi's clock"): taking the build date instead would freeze time
/// there, accepting certificates that expired after the build and refusing ones issued after it.
pub fn clock_set(clock: u64, built: u64) -> bool //@ret
//@ ensures ret == (clock >= built),
{
    clock >= built
}

/// Whether a chain of `n` certificates (the server's first) is short enough to check.
pub fn chain_len_ok(n: u32) -> bool //@ret
//@ ensures ret == (n >= 1 && n <= MAX_INTERMEDIATES + 1),
{
    n >= 1 && n <= MAX_INTERMEDIATES + 1
}

/// Verified certificate signatures remembered since boot (`algs::SigCache`): a chain is three, the slots cover a
/// few chains (a renewed certificate, both of Let's Encrypt's intermediates).
pub const SIG_CACHE: u32 = 16;

/// The cache slot after `i`: the oldest entry is replaced first, and the index never leaves the array.
pub fn next_slot(i: u32) -> u32 //@ret
//@ requires i < SIG_CACHE,
//@ ensures ret < SIG_CACHE, ret as int == (i as int + 1) % (SIG_CACHE as int),
{
    if i + 1 >= SIG_CACHE {
        0
    } else {
        i + 1
    }
}
