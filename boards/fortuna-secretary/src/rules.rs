// The integer rules under Fortuna's HTTP reader and its sign-in: how a chunk size is read from untrusted hex, how
// much of a body a piece of input may fill, and when the pairing asks the server for a new code or polls the one on
// screen. Verus checks this very file: `//@` lines hold the proof, see scripts/verify-shipped.sh (D-011). `http.rs` and
// `pairing.rs` call these functions and keep no copy of the rules.

/// The largest body Fortuna reads: a timeline page or an agent's rows. Larger answers are refused, not cut.
pub const MAX_BODY: u32 = 512 * 1024;
/// The largest chunk size the reader accepts (it never exceeds the body cap).
pub const MAX_CHUNK: u32 = MAX_BODY;
/// How many codes one pairing shows before it waits for a press (`signin.rs` MAX_CODES).
pub const MAX_CODES: u32 = 5;
/// Poll every 2 s while a code is on screen.
pub const POLL_MS: u64 = 2000;
/// After an error that is not a rate limit, ask again in 10 s.
pub const RETRY_MS: u64 = 10_000;

/// The value of one hex digit, or 16 for a byte that is not one.
pub fn hex_value(b: u8) -> u32 //@ret
//@ ensures ret <= 16, (ret < 16) == ((48 <= b && b <= 57) || (97 <= b && b <= 102) || (65 <= b && b <= 70)),
{
    if b >= 48 && b <= 57 {
        (b - 48) as u32
    } else if b >= 97 && b <= 102 {
        (b - 87) as u32
    } else if b >= 65 && b <= 70 {
        (b - 55) as u32
    } else {
        16
    }
}

/// A chunk size with one more hex digit, or `None` once it passes `MAX_CHUNK`.
pub fn push_hex(acc: u32, digit: u32) -> Option<u32> //@ret
//@ requires acc <= MAX_CHUNK, digit < 16,
//@ ensures match ret { Some(v) => v == acc * 16 + digit && v <= MAX_CHUNK, None => acc * 16 + digit > MAX_CHUNK },
{
    let v = acc * 16 + digit;
    if v > MAX_CHUNK {
        return None;
    }
    Some(v)
}

/// How many bytes of `avail` input go into a body that still lacks `remaining` bytes and already holds `held` of at
/// most `cap`: never more than either, and `None` when the body would pass its cap.
pub fn body_take(remaining: u32, avail: u32, held: u32, cap: u32) -> Option<u32> //@ret
//@ requires held <= cap,
//@ ensures match ret { Some(n) => n <= remaining && n <= avail && held + n <= cap && (n == remaining || n == avail), None => true },
{
    let n = if remaining < avail { remaining } else { avail };
    if n > cap - held {
        return None;
    }
    Some(n)
}

/// What the pairing does on a tick.
pub const ACT_IDLE: u8 = 0;
pub const ACT_START: u8 = 1;
pub const ACT_POLL: u8 = 2;
pub const ACT_PAUSE: u8 = 3;

/// The pairing's next step: nothing before `next`; with no code on screen (or an expired one) a new code, unless
/// `MAX_CODES` were shown already, which pauses until a press; else a poll of the code on screen. `active` is false
/// once it was denied, paused or done.
pub fn pairing_action(active: bool, now: u64, next: u64, has_code: bool, expires: u64, codes: u32) -> u8 //@ret
//@ ensures
//@     !active ==> ret == ACT_IDLE,
//@     active && now < next ==> ret == ACT_IDLE,
//@     ret == ACT_POLL ==> active && now >= next && has_code && now < expires,
//@     ret == ACT_START ==> active && now >= next && (!has_code || now >= expires) && codes < MAX_CODES,
//@     ret == ACT_PAUSE ==> codes >= MAX_CODES,
{
    if !active || now < next {
        return ACT_IDLE;
    }
    if has_code && now < expires {
        return ACT_POLL;
    }
    if codes >= MAX_CODES {
        return ACT_PAUSE;
    }
    ACT_START
}

/// Whether a device token has the server's shape: `sec_` and 43 URL-safe base64 letters. Nothing else is ever sent
/// as a bearer or kept in the token file.
pub fn token_shape(t: &[u8]) -> bool //@ret
//@ ensures ret ==> t.len() == 47 && t@[0] == 115u8 && t@[1] == 101u8 && t@[2] == 99u8 && t@[3] == 95u8,
{
    if t.len() != 47 || t[0] != 115 || t[1] != 101 || t[2] != 99 || t[3] != 95 {
        return false;
    }
    let mut i: usize = 4;
    while i < 47
    //@ invariant 4 <= i <= 47, t.len() == 47,
    //@ decreases 47 - i,
    {
        let b = t[i];
        let ok = (b >= 48 && b <= 57) || (b >= 65 && b <= 90) || (b >= 97 && b <= 122) || b == 45 || b == 95;
        if !ok {
            return false;
        }
        i += 1;
    }
    true
}

// ---- the voice socket's frames (RFC 6455, `ws.rs`) ----

/// The largest frame Fortuna takes from the voice socket: the protocol's 64 KiB message cap (section 10).
pub const WS_MAX_PAYLOAD: u64 = 65_536;

/// The bytes of a frame's header before its payload: two, then two or eight for a longer length, then four for the
/// mask (every frame Fortuna sends is masked; the server's never are).
pub fn ws_head_len(n: u64, masked: bool) -> u32 //@ret
//@ ensures ret == (if n < 126 { 2u32 } else if n <= 65535 { 4u32 } else { 10u32 }) + (if masked { 4u32 } else { 0u32 }),
{
    let base = if n < 126 {
        2
    } else if n <= 65535 {
        4
    } else {
        10
    };
    if masked {
        base + 4
    } else {
        base
    }
}

/// The size of the extended length after the 7-bit length field `l7`: none, two bytes (126) or eight (127).
pub fn ws_ext_bytes(l7: u8) -> u32 //@ret
//@ requires l7 <= 127,
//@ ensures (l7 < 126 ==> ret == 0), (l7 == 126 ==> ret == 2), (l7 == 127 ==> ret == 8),
{
    if l7 < 126 {
        0
    } else if l7 == 126 {
        2
    } else {
        8
    }
}

/// Whether a frame from the server is one Fortuna reads: whole (the protocol never fragments), no extension bits,
/// unmasked (RFC 6455 5.1: a masked server frame fails the connection), text, binary or a control frame of at most 125
/// bytes, and within the cap.
pub fn ws_frame_ok(fin: bool, rsv: u8, masked: bool, opcode: u8, len: u64) -> bool //@ret
//@ ensures ret == (fin && rsv == 0 && !masked && len <= WS_MAX_PAYLOAD && (opcode == 1 || opcode == 2 || ((opcode == 8 || opcode == 9 || opcode == 10) && len <= 125))),
{
    if !fin || rsv != 0 || masked || len > WS_MAX_PAYLOAD {
        return false;
    }
    opcode == 1 || opcode == 2 || ((opcode == 8 || opcode == 9 || opcode == 10) && len <= 125)
}
