//! From the framebuffer's pixels to what the controller reads.

/// Turns one row of XRGB8888 into gray for the update buffer. With a 5-bit waveform only the top nibble may be set: that
/// gives the 16 levels of Amazon's output table for such a waveform, and other bits make the controller run the wrong
/// transition. `mono` forces black and white at a threshold of 50%.
pub fn gray_row(src: &[u8], dst: &mut [u8], five_bit: bool, mono: bool) {
    for (pixel, out) in src.as_chunks::<4>().0.iter().zip(dst.iter_mut()) {
        let (b, g, r) = (pixel[0], pixel[1], pixel[2]);
        let gray = if r == g && g == b {
            r
        } else {
            ((3 * u32::from(r) + 6 * u32::from(g) + u32::from(b)) / 10) as u8
        };
        *out = if mono {
            if gray >= 0x80 { 0xf0 } else { 0x00 }
        } else if five_bit {
            gray & 0xf0
        } else {
            gray
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn xrgb(r: u8, g: u8, b: u8) -> [u8; 4] {
        [b, g, r, 0]
    }

    fn one(pixel: [u8; 4], five_bit: bool, mono: bool) -> u8 {
        let mut out = [0];
        gray_row(&pixel, &mut out, five_bit, mono);
        out[0]
    }

    #[test]
    fn gray_stays_itself_and_a_five_bit_waveform_loses_the_low_nibble() {
        assert_eq!(one(xrgb(0xff, 0xff, 0xff), true, false), 0xf0);
        assert_eq!(one(xrgb(0x00, 0x00, 0x00), true, false), 0x00);
        assert_eq!(one(xrgb(0x8c, 0x8c, 0x8c), true, false), 0x80);
        assert_eq!(one(xrgb(0x8c, 0x8c, 0x8c), false, false), 0x8c);
    }

    #[test]
    fn color_is_weighted_as_in_linux() {
        assert_eq!(one(xrgb(100, 200, 50), false, false), 155);
    }

    #[test]
    fn monochrome_splits_at_the_middle() {
        assert_eq!(one(xrgb(0x7f, 0x7f, 0x7f), true, true), 0x00);
        assert_eq!(one(xrgb(0x80, 0x80, 0x80), true, true), 0xf0);
    }

    #[test]
    fn a_short_output_ends_the_row_without_a_panic() {
        let row = [xrgb(1, 1, 1), xrgb(2, 2, 2), xrgb(3, 3, 3)].concat();
        let mut out = [0; 2];
        gray_row(&row, &mut out, false, false);
        assert_eq!(out, [1, 2]);
    }
}
