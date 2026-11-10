//! A Reed-Solomon code over bytes, for the saved log, as Linux's ramoops has (`fs/pstore/ram_core.c`, `lib/reed_solomon`).
//!
//! The Galaxy Book Go's firmware reset flips 1.57% of the bits of this RAM, which is 12% of the bytes (2026-11-02). 170 bytes of parity on
//! 85 of data correct any 85 wrong bytes in the 255, which is a third. That holds up to about 3.3% of flipped bits with hardly a block
//! lost, because DRAM forgets faster when it is hot and one measurement is not the worst case. It takes the room that 3 copies took, which
//! at 1.57% still left a wrong byte in every 170. That the flips go mostly one way (93% from 1 to 0) does not help: the other 7% would break
//! any code that counts on it.
//!
//! Encoding takes no memory but its 170 bytes and can run in a panic. A byte costs a table lookup and 22 XORs of 8 bytes, because it runs on
//! every line of the kernel log. A block that is right is told by encoding it again; correcting one is a good deal slower, a fraction of a
//! millisecond, so that a boot does not do it in its way.

/// The parity bytes of a block.
pub const PARITY: usize = 170;

/// The most data a block holds. A block may hold less, with the same parity.
pub const MAX_DATA: usize = 255 - PARITY;

/// How many wrong bytes a block can have.
pub const CORRECTS: usize = PARITY / 2;

const fn tables() -> ([u8; 512], [u8; 256]) {
    let mut exp = [0u8; 512];
    let mut log = [0u8; 256];
    let mut x = 1u16;
    let mut i = 0;
    while i < 255 {
        exp[i] = x as u8;
        exp[i + 255] = x as u8;
        log[x as usize] = i as u8;
        x <<= 1;
        if x & 0x100 != 0 {
            x ^= 0x11d;
        }
        i += 1;
    }
    exp[510] = exp[0];
    exp[511] = exp[1];
    (exp, log)
}

const TABLES: ([u8; 512], [u8; 256]) = tables();
static EXP: [u8; 512] = TABLES.0;
static LOG: [u8; 256] = TABLES.1;

/// The product of (x + a^i) for i below `PARITY`, without its top term, highest power first.
const fn generator() -> [u8; PARITY] {
    let (exp, log) = tables();
    let mut g = [0u8; PARITY + 1];
    g[0] = 1;
    let mut i = 0;
    while i < PARITY {
        let mut j = i + 1;
        while j > 0 {
            let product = if g[j] == 0 {
                0
            } else {
                exp[log[g[j] as usize] as usize + i]
            };
            g[j] = g[j - 1] ^ product;
            j -= 1;
        }
        g[0] = if g[0] == 0 {
            0
        } else {
            exp[log[g[0] as usize] as usize + i]
        };
        i += 1;
    }
    let mut out = [0u8; PARITY];
    let mut k = 0;
    while k < PARITY {
        out[k] = g[PARITY - 1 - k];
        k += 1;
    }
    out
}

/// What each feedback byte adds to the parity: the generator times that byte. 43 KiB, for the sake of the kernel log.
const fn rows() -> [[u8; PARITY]; 256] {
    let (exp, log) = tables();
    let generator = generator();
    let mut rows = [[0u8; PARITY]; 256];
    let mut feedback = 1;
    while feedback < 256 {
        let mut i = 0;
        while i < PARITY {
            if generator[i] != 0 {
                rows[feedback][i] =
                    exp[log[feedback] as usize + log[generator[i] as usize] as usize];
            }
            i += 1;
        }
        feedback += 1;
    }
    rows
}

static ROWS: [[u8; PARITY]; 256] = rows();

#[cfg(test)]
static GENERATOR: [u8; PARITY] = generator();

#[inline]
fn mul(a: u8, b: u8) -> u8 {
    if a == 0 || b == 0 {
        0
    } else {
        EXP[usize::from(LOG[usize::from(a)]) + usize::from(LOG[usize::from(b)])]
    }
}

#[inline]
fn div(a: u8, b: u8) -> u8 {
    if a == 0 {
        0
    } else {
        EXP[usize::from(LOG[usize::from(a)]) + 255 - usize::from(LOG[usize::from(b)])]
    }
}

/// Works out a block's parity as its data arrives, so that a log line adds to it without reading the block back.
#[derive(Clone, Copy)]
pub struct Encoder {
    parity: [u8; PARITY],
}

impl Encoder {
    pub const fn new() -> Self {
        Self {
            parity: [0; PARITY],
        }
    }

    pub fn push(&mut self, data: &[u8]) {
        for &byte in data {
            let feedback = byte ^ self.parity[0];
            self.parity.copy_within(1.., 0);
            self.parity[PARITY - 1] = 0;
            let row = &ROWS[usize::from(feedback)];
            let mut at = 0;
            while at + 8 <= PARITY {
                let mut a = [0u8; 8];
                let mut b = [0u8; 8];
                a.copy_from_slice(&self.parity[at..at + 8]);
                b.copy_from_slice(&row[at..at + 8]);
                let xor = u64::from_ne_bytes(a) ^ u64::from_ne_bytes(b);
                self.parity[at..at + 8].copy_from_slice(&xor.to_ne_bytes());
                at += 8;
            }
            while at < PARITY {
                self.parity[at] ^= row[at];
                at += 1;
            }
        }
    }

    /// The parity of what was pushed so far.
    pub fn parity(&self) -> &[u8; PARITY] {
        &self.parity
    }
}

impl Default for Encoder {
    fn default() -> Self {
        Self::new()
    }
}

/// The parity of some data.
pub fn parity_of(data: &[u8]) -> [u8; PARITY] {
    let mut encoder = Encoder::new();
    encoder.push(data);
    encoder.parity
}

/// Corrects a block in place and returns how many bytes were wrong. `None` means too many were: nothing is changed, and the bytes
/// are as good as they came. With very many wrong bytes this can also return a wrong answer, about once in `CORRECTS!` tries, so
/// the callers have a checksum over what they read.
pub fn decode(data: &mut [u8], parity: &mut [u8; PARITY]) -> Option<usize> {
    let n = data.len() + PARITY;
    if n > 255 {
        return None;
    }
    if parity_of(data) == *parity {
        return Some(0);
    }
    let mut syndromes = [0u8; PARITY];
    let mut any = 0;
    for (j, syndrome) in syndromes.iter_mut().enumerate() {
        let mut value = 0u8;
        for &byte in data.iter().chain(parity.iter()) {
            value = if value == 0 {
                byte
            } else {
                EXP[usize::from(LOG[usize::from(value)]) + j] ^ byte
            };
        }
        *syndrome = value;
        any |= value;
    }
    if any == 0 {
        return Some(0);
    }

    // Berlekamp-Massey: the polynomial whose roots tell where the errors are.
    let mut locator = [0u8; PARITY + 1];
    let mut previous = [0u8; PARITY + 1];
    locator[0] = 1;
    previous[0] = 1;
    let (mut errors, mut shift, mut last) = (0usize, 1usize, 1u8);
    for k in 0..PARITY {
        let mut discrepancy = syndromes[k];
        for i in 1..=errors {
            discrepancy ^= mul(locator[i], syndromes[k - i]);
        }
        if discrepancy == 0 {
            shift += 1;
            continue;
        }
        let scale = div(discrepancy, last);
        let before = locator;
        for i in 0..=PARITY - shift {
            locator[i + shift] ^= mul(scale, previous[i]);
        }
        if 2 * errors <= k {
            errors = k + 1 - errors;
            previous = before;
            last = discrepancy;
            shift = 1;
        } else {
            shift += 1;
        }
    }
    if errors > CORRECTS {
        return None;
    }

    // What the errors are.
    let mut evaluator = [0u8; PARITY];
    for (k, value) in evaluator.iter_mut().enumerate() {
        for i in 0..=k.min(errors) {
            *value ^= mul(locator[i], syndromes[k - i]);
        }
    }

    // Where they are: every place in the block, tried against the roots, then the size of each error (Forney).
    let mut places = [0u8; CORRECTS];
    let mut sizes = [0u8; CORRECTS];
    let mut found = 0;
    for place in 0..n {
        let power = n - 1 - place;
        let inverse = EXP[255 - power];
        let mut value = 0u8;
        for i in (0..=errors).rev() {
            value = mul(value, inverse) ^ locator[i];
        }
        if value != 0 {
            continue;
        }
        if found == errors {
            return None;
        }
        let mut top = 0u8;
        for i in (0..PARITY).rev() {
            top = mul(top, inverse) ^ evaluator[i];
        }
        // The derivative: only the odd powers stay in this field.
        let mut bottom = 0u8;
        let mut i = errors | 1;
        loop {
            if i <= errors {
                bottom = mul(bottom, mul(inverse, inverse)) ^ locator[i];
            }
            if i == 1 {
                break;
            }
            i -= 2;
        }
        if bottom == 0 {
            return None;
        }
        places[found] = place as u8;
        sizes[found] = mul(EXP[power], div(top, bottom));
        found += 1;
    }
    if found != errors {
        return None;
    }
    for (&place, &size) in places[..found].iter().zip(&sizes) {
        let place = usize::from(place);
        if place < data.len() {
            data[place] ^= size;
        } else {
            parity[place - data.len()] ^= size;
        }
    }
    Some(found)
}

#[cfg(test)]
mod tests {
    extern crate std;
    use std::{vec, vec::Vec};

    use super::*;

    /// Fixed numbers, so that a failure repeats.
    struct Random(u64);

    impl Random {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }
    }

    fn data_of(len: usize, random: &mut Random) -> Vec<u8> {
        (0..len).map(|_| random.next() as u8).collect()
    }

    /// Changes `count` different bytes of a block.
    fn damage(data: &mut [u8], parity: &mut [u8; PARITY], count: usize, random: &mut Random) {
        let n = data.len() + PARITY;
        let mut places: Vec<usize> = (0..n).collect();
        for done in 0..count {
            let pick = done + (random.next() as usize) % (n - done);
            places.swap(done, pick);
            let change = (random.next() % 255 + 1) as u8;
            let place = places[done];
            if place < data.len() {
                data[place] ^= change;
            } else {
                parity[place - data.len()] ^= change;
            }
        }
    }

    #[test]
    fn the_field_is_right() {
        assert_eq!(EXP[255], 1);
        assert_eq!(EXP[8], 0x1d);
        for x in 1..=255u8 {
            assert_eq!(EXP[usize::from(LOG[usize::from(x)])], x);
            assert_eq!(div(mul(x, 0x53), 0x53), x);
        }
    }

    #[test]
    fn the_rows_are_the_generator_times_the_byte() {
        for feedback in 0..=255u8 {
            for (i, &g) in GENERATOR.iter().enumerate() {
                assert_eq!(ROWS[usize::from(feedback)][i], mul(feedback, g));
            }
        }
    }

    #[test]
    fn the_generator_has_its_roots() {
        for (i, &root) in EXP[..PARITY].iter().enumerate() {
            let mut value = 1u8;
            for &g in &GENERATOR {
                value = mul(value, root) ^ g;
            }
            assert_eq!(value, 0, "a^{i}");
        }
    }

    #[test]
    fn a_good_block_needs_nothing() {
        let mut random = Random(0x1234_5678_9abc_def1);
        for len in [0, 1, 3, 20, 32, 84, MAX_DATA] {
            let mut data = data_of(len, &mut random);
            let want = data.clone();
            let mut parity = parity_of(&data);
            assert_eq!(decode(&mut data, &mut parity), Some(0), "{len}");
            assert_eq!(data, want);
        }
    }

    #[test]
    fn parity_can_be_worked_out_in_pieces() {
        let mut random = Random(0x0fed_cba9_8765_4321);
        let data = data_of(MAX_DATA, &mut random);
        let mut encoder = Encoder::new();
        for piece in data.chunks(13) {
            encoder.push(piece);
        }
        assert_eq!(encoder.parity(), &parity_of(&data));
    }

    #[test]
    fn up_to_85_wrong_bytes_are_corrected_at_any_length() {
        let mut random = Random(0xdead_beef_0bad_f00d);
        for len in [1, 3, 20, 32, 84, MAX_DATA] {
            for count in [1, 2, 3, 10, 31, 32, 33, 50, 63, 64, 84, CORRECTS] {
                for _ in 0..4 {
                    let want = data_of(len, &mut random);
                    let want_parity = parity_of(&want);
                    let (mut data, mut parity) = (want.clone(), want_parity);
                    damage(&mut data, &mut parity, count, &mut random);
                    assert_eq!(decode(&mut data, &mut parity), Some(count), "{len} {count}");
                    assert_eq!(data, want, "{len} {count}");
                    assert_eq!(parity, want_parity, "{len} {count}");
                }
            }
        }
    }

    #[test]
    fn errors_in_the_parity_alone_are_corrected() {
        let data = vec![0x41u8; MAX_DATA];
        let want = parity_of(&data);
        let mut parity = want;
        parity[0] ^= 1;
        parity[PARITY - 1] ^= 0x80;
        let mut read = data.clone();
        assert_eq!(decode(&mut read, &mut parity), Some(2));
        assert_eq!((read, parity), (data, want));
    }

    #[test]
    fn zeros_are_a_good_block() {
        let mut data = [0u8; MAX_DATA];
        let mut parity = [0u8; PARITY];
        assert_eq!(decode(&mut data, &mut parity), Some(0));
        data[9] = 0x20;
        parity[3] = 0x04;
        assert_eq!(decode(&mut data, &mut parity), Some(2));
        assert_eq!(data, [0; MAX_DATA]);
    }

    #[test]
    fn too_many_wrong_bytes_are_refused_and_change_nothing() {
        let mut random = Random(0x0123_4567_89ab_cdef);
        for count in [CORRECTS + 1, CORRECTS + 2, 100, 150, 220] {
            for _ in 0..8 {
                let mut data = data_of(MAX_DATA, &mut random);
                let mut parity = parity_of(&data);
                damage(&mut data, &mut parity, count, &mut random);
                let (before, before_parity) = (data.clone(), parity);
                assert_eq!(decode(&mut data, &mut parity), None, "{count}");
                assert_eq!((data, parity), (before, before_parity));
            }
        }
    }

    #[test]
    fn a_block_too_long_is_refused() {
        let mut data = [0u8; MAX_DATA + 1];
        assert_eq!(decode(&mut data, &mut [0; PARITY]), None);
    }
}
