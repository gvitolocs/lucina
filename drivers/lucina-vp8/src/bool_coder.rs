//! The boolean entropy encoder of RFC 6386 section 7.

pub struct BoolEncoder {
    out: Vec<u8>,
    range: u32,
    bottom: u32,
    bit_count: i32,
}

impl BoolEncoder {
    pub fn new() -> BoolEncoder {
        BoolEncoder { out: Vec::new(), range: 255, bottom: 0, bit_count: 24 }
    }

    pub fn reset(&mut self) {
        self.out.clear();
        self.range = 255;
        self.bottom = 0;
        self.bit_count = 24;
    }

    /// A carry out of `bottom` ripples into what is already written.
    fn add_one(&mut self) {
        let mut i = self.out.len();
        while i > 0 {
            i -= 1;
            if self.out[i] == 255 {
                self.out[i] = 0;
            } else {
                self.out[i] += 1;
                break;
            }
        }
    }

    #[inline]
    pub fn put(&mut self, prob: u8, bit: bool) {
        let split = 1 + (((self.range - 1) * prob as u32) >> 8);
        if bit {
            let (b, carried) = self.bottom.overflowing_add(split);
            self.bottom = b;
            self.range -= split;
            if carried {
                self.add_one();
            }
        } else {
            self.range = split;
        }
        while self.range < 128 {
            self.range <<= 1;
            if self.bottom & (1 << 31) != 0 {
                self.add_one();
            }
            self.bottom <<= 1;
            self.bit_count -= 1;
            if self.bit_count == 0 {
                self.out.push((self.bottom >> 24) as u8);
                self.bottom &= (1 << 24) - 1;
                self.bit_count = 8;
            }
        }
    }

    #[inline]
    pub fn put_bit(&mut self, bit: bool) {
        self.put(128, bit);
    }

    /// `n` bits, most significant first.
    pub fn put_literal(&mut self, v: u32, n: u32) {
        for i in (0..n).rev() {
            self.put_bit(v >> i & 1 == 1);
        }
    }

    /// Flushes: what remains of `bottom` goes out.
    pub fn finish(&mut self) {
        // As in the RFC's flush_bool_encoder: enough zero-probability-free bits to push the state through.
        for _ in 0..32 {
            self.put_bit(false);
        }
    }

    pub fn bytes(&self) -> &[u8] {
        &self.out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The decoder of RFC 6386 section 7.3, to read back what was written.
    struct Dec<'a> {
        input: &'a [u8],
        at: usize,
        range: u32,
        value: u32,
        bit_count: i32,
    }

    impl<'a> Dec<'a> {
        fn new(input: &'a [u8]) -> Dec<'a> {
            let value = (input[0] as u32) << 8 | input[1] as u32;
            Dec { input, at: 2, range: 255, value, bit_count: 0 }
        }
        fn get(&mut self, prob: u8) -> bool {
            let split = 1 + (((self.range - 1) * prob as u32) >> 8);
            let big = split << 8;
            let bit = if self.value >= big {
                self.range -= split;
                self.value -= big;
                true
            } else {
                self.range = split;
                false
            };
            while self.range < 128 {
                self.value <<= 1;
                self.range <<= 1;
                self.bit_count += 1;
                if self.bit_count == 8 {
                    self.bit_count = 0;
                    self.value |= self.input.get(self.at).copied().unwrap_or(0) as u32;
                    self.at += 1;
                }
            }
            bit
        }
    }

    #[test]
    fn what_is_written_reads_back() {
        let mut seed = 42u32;
        let mut rnd = move || {
            seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            seed >> 16
        };
        let items: Vec<(u8, bool)> = (0..20000)
            .map(|_| {
                let p = (rnd() % 255 + 1) as u8;
                // Mostly what the probability says, sometimes not: the carry path needs both.
                let b = (rnd() % 256) as u8 >= p;
                (p, b)
            })
            .collect();
        let mut e = BoolEncoder::new();
        for &(p, b) in &items {
            e.put(p, b);
        }
        e.finish();
        let mut d = Dec::new(e.bytes());
        for (i, &(p, b)) in items.iter().enumerate() {
            assert_eq!(d.get(p), b, "item {i}");
        }
    }

    #[test]
    fn a_literal_is_most_significant_first() {
        let mut e = BoolEncoder::new();
        e.put_literal(0b1011, 4);
        e.finish();
        let mut d = Dec::new(e.bytes());
        assert!(d.get(128));
        assert!(!d.get(128));
        assert!(d.get(128));
        assert!(d.get(128));
    }
}
