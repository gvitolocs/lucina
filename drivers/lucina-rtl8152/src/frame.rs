//! The bulk framing (U-Boot `struct tx_desc`, `struct rx_desc`, `r8152_send_common`, `r8152_eth_recv`; Linux
//! `rx_bottom` for several frames per transfer).
//!
//! Transmit: any number of frames per transfer (Linux `r8152_tx_agg_fill`), each on a 4-byte boundary, an 8-byte descriptor (`opts1` with the length and the first- and last-segment
//! bits, `opts2` zero: checksums are done in software) and the frame. The chip pads short frames itself.
//!
//! Receive: a transfer holds any number of frames. Each is a 24-byte descriptor (`opts1` bits 14:0 are the length
//! with the 4-byte FCS, which comes along), the frame and the FCS; the next descriptor starts at the next multiple of
//! 8 bytes from the start of the transfer. Nothing in a received transfer is trusted: a length that does not fit ends
//! the transfer and is counted, it never reads outside it.

use alloc::vec::Vec;
use core::ops::Range;

pub const TX_DESC_LEN: usize = 8;
pub const RX_DESC_LEN: usize = 24;
/// The Ethernet header: a shorter frame cannot be sent.
pub const MIN_TX_FRAME: usize = 14;
/// The largest frame the driver hands to the chip: 1514 plus a VLAN tag.
pub const MAX_TX_FRAME: usize = 1518;
/// The longest frame taken from a descriptor: PLA_RMS is 1522 with the FCS.
pub const MAX_RX_FRAME: usize = 1518;
/// Linux stops at a descriptor shorter than this (`ETH_ZLEN`, FCS included in the count).
pub const MIN_RX_LEN: usize = 60;
pub const RX_FCS_LEN: usize = 4;
pub const RX_ALIGN: usize = 8;
/// Linux's `tx_desc.align` for this chip.
pub const TX_ALIGN: usize = 4;
/// The longest transmit transfer. Linux's `agg_buf_sz`; the chip's limit is not documented, so not above what Linux sends.
pub const TX_AGG_LEN: usize = 16 * 1024;

const TX_FS: u32 = 1 << 31;
const TX_LS: u32 = 1 << 30;
const RX_LEN_MASK: u32 = 0x7fff;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TxError {
    /// Shorter than an Ethernet header.
    TooShort(usize),
    /// Longer than [`MAX_TX_FRAME`].
    TooLong(usize),
}

/// Descriptor and frame as one transfer.
pub fn encode_tx(frame: &[u8]) -> Result<Vec<u8>, TxError> {
    if frame.len() < MIN_TX_FRAME {
        return Err(TxError::TooShort(frame.len()));
    }
    if frame.len() > MAX_TX_FRAME {
        return Err(TxError::TooLong(frame.len()));
    }
    let opts1 = frame.len() as u32 | TX_FS | TX_LS;
    let mut out = Vec::with_capacity(TX_DESC_LEN + frame.len());
    out.extend_from_slice(&opts1.to_le_bytes());
    out.extend_from_slice(&[0; 4]);
    out.extend_from_slice(frame);
    Ok(out)
}

/// Adds a frame to a transmit transfer. `Ok(false)`: it does not fit, and the transfer stays as it was.
pub fn append_tx(agg: &mut Vec<u8>, frame: &[u8]) -> Result<bool, TxError> {
    if frame.len() < MIN_TX_FRAME {
        return Err(TxError::TooShort(frame.len()));
    }
    if frame.len() > MAX_TX_FRAME {
        return Err(TxError::TooLong(frame.len()));
    }
    let start = agg.len().next_multiple_of(TX_ALIGN);
    if start + TX_DESC_LEN + frame.len() > TX_AGG_LEN {
        return Ok(false);
    }
    agg.resize(start, 0);
    let opts1 = frame.len() as u32 | TX_FS | TX_LS;
    agg.extend_from_slice(&opts1.to_le_bytes());
    agg.extend_from_slice(&[0; 4]);
    agg.extend_from_slice(frame);
    Ok(true)
}

/// Why a received frame was not delivered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RxError {
    /// Fewer bytes left than a descriptor needs. Ends the transfer.
    TruncatedDescriptor { left: usize },
    /// A non-zero length below [`MIN_RX_LEN`]. Ends the transfer: the next descriptor's place is not trustworthy.
    Runt { len: usize },
    /// The frame runs past the end of the transfer. Ends the transfer.
    Truncated { len: usize, left: usize },
    /// Longer than [`MAX_RX_FRAME`] plus FCS. Skipped: the descriptor did say where the next one is.
    TooLong { len: usize },
}

/// Walks the frames of one received transfer.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RxCursor {
    pos: usize,
}

impl RxCursor {
    pub fn new() -> Self {
        Self { pos: 0 }
    }

    /// The next frame's bytes in `buf`, an error for something that is no frame, or `None` at the end. A zero length
    /// is the quiet end (nothing but padding is left).
    pub fn next(&mut self, buf: &[u8]) -> Option<Result<Range<usize>, RxError>> {
        let left = buf.len().saturating_sub(self.pos);
        if left == 0 {
            return None;
        }
        if left < RX_DESC_LEN {
            self.pos = buf.len();
            return Some(Err(RxError::TruncatedDescriptor { left }));
        }
        let raw: [u8; 4] = buf[self.pos..self.pos + 4].try_into().ok()?;
        let len = (u32::from_le_bytes(raw) & RX_LEN_MASK) as usize;
        if len == 0 {
            self.pos = buf.len();
            return None;
        }
        if len < MIN_RX_LEN {
            self.pos = buf.len();
            return Some(Err(RxError::Runt { len }));
        }
        let start = self.pos + RX_DESC_LEN;
        if len > buf.len() - start {
            self.pos = buf.len();
            return Some(Err(RxError::Truncated {
                len,
                left: buf.len() - start,
            }));
        }
        self.pos = (start + len).next_multiple_of(RX_ALIGN).min(buf.len());
        if len > MAX_RX_FRAME + RX_FCS_LEN {
            return Some(Err(RxError::TooLong { len }));
        }
        Some(Ok(start..start + len - RX_FCS_LEN))
    }

    /// Whether anything is left to look at in a transfer of `len` bytes.
    pub fn has_more(&self, len: usize) -> bool {
        self.pos < len
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use alloc::vec;
    use alloc::vec::Vec;

    /// An Ethernet frame of `len` bytes filled from `seed`.
    #[test]
    fn frames_in_one_transfer_each_have_a_descriptor_on_a_four_byte_boundary() {
        let mut agg = Vec::new();
        assert_eq!(append_tx(&mut agg, &frame(61, 1)), Ok(true));
        assert_eq!(append_tx(&mut agg, &frame(60, 2)), Ok(true));
        assert_eq!(
            agg.len(),
            72 + 8 + 60,
            "three bytes of padding after 8 + 61"
        );
        assert_eq!(agg[..8], [61, 0, 0, 0xc0, 0, 0, 0, 0]);
        assert_eq!(agg[69..72], [0, 0, 0]);
        assert_eq!(agg[72..80], [60, 0, 0, 0xc0, 0, 0, 0, 0]);
        assert_eq!(agg[80..], frame(60, 2)[..]);
    }

    #[test]
    fn a_transfer_ends_where_the_next_frame_would_not_fit() {
        let mut agg = Vec::new();
        let mut frames = 0;
        while append_tx(&mut agg, &frame(1514, 3)) == Ok(true) {
            frames += 1;
        }
        assert_eq!(frames, 10);
        assert!(agg.len() <= TX_AGG_LEN);
        let before = agg.clone();
        assert_eq!(append_tx(&mut agg, &frame(1514, 3)), Ok(false));
        assert_eq!(agg, before, "a frame that does not fit leaves it as it was");
        assert_eq!(append_tx(&mut agg, &[0; 13]), Err(TxError::TooShort(13)));
        assert_eq!(append_tx(&mut agg, &[0; 1519]), Err(TxError::TooLong(1519)));
    }

    pub(crate) fn frame(len: usize, seed: u8) -> Vec<u8> {
        (0..len).map(|i| seed.wrapping_add(i as u8)).collect()
    }

    /// A received transfer with these frames, as the chip lays it out.
    pub(crate) fn batch(frames: &[&[u8]]) -> Vec<u8> {
        let mut out = Vec::new();
        for f in frames {
            out.resize(out.len().next_multiple_of(RX_ALIGN), 0);
            let len = (f.len() + RX_FCS_LEN) as u32;
            out.extend_from_slice(&len.to_le_bytes());
            out.extend_from_slice(&[0xa5; RX_DESC_LEN - 4]); // opts2..opts6: nothing in them is used
            out.extend_from_slice(f);
            out.extend_from_slice(&[0xee; RX_FCS_LEN]);
        }
        out
    }

    fn collect(buf: &[u8]) -> Vec<Result<Vec<u8>, RxError>> {
        let mut cursor = RxCursor::new();
        let mut out = Vec::new();
        while let Some(item) = cursor.next(buf) {
            out.push(item.map(|range| buf[range].to_vec()));
        }
        out
    }

    #[test]
    fn tx_descriptor_as_u_boot_writes_it() {
        let f = frame(60, 1);
        let out = encode_tx(&f).unwrap();
        assert_eq!(out.len(), 68);
        // 60 | FS | LS, little endian; opts2 zero.
        assert_eq!(&out[..8], &[60, 0, 0, 0xc0, 0, 0, 0, 0]);
        assert_eq!(&out[8..68], &f[..]);
        assert_eq!(
            out.capacity(),
            out.len(),
            "into_boxed_slice does not reallocate"
        );
    }

    #[test]
    fn tx_short_frames_stay_short() {
        // ARP is 42 bytes: U-Boot, Linux and OpenBSD send it as it is and the chip pads.
        let out = encode_tx(&frame(42, 2)).unwrap();
        assert_eq!(out.len(), 50);
        assert_eq!(out[0], 42);
    }

    #[test]
    fn tx_limits() {
        assert_eq!(encode_tx(&[0; 13]), Err(TxError::TooShort(13)));
        assert_eq!(encode_tx(&[]), Err(TxError::TooShort(0)));
        assert_eq!(encode_tx(&[0; 14]).unwrap().len(), 22);
        assert_eq!(encode_tx(&[0; 1518]).unwrap().len(), 1526);
        assert_eq!(encode_tx(&[0; 1519]), Err(TxError::TooLong(1519)));
    }

    #[test]
    fn rx_one_frame_without_its_fcs() {
        let f = frame(64, 3);
        assert_eq!(collect(&batch(&[&f])), vec![Ok(f)]);
    }

    #[test]
    fn rx_empty_transfer_is_nothing() {
        assert_eq!(collect(&[]), vec![]);
    }

    #[test]
    fn rx_several_frames_on_8_byte_boundaries() {
        // 60 + 4 + 24 = 88 is aligned already; 61 + 4 + 24 = 89 needs 7 bytes of padding.
        let (a, b, c) = (frame(60, 4), frame(61, 5), frame(1514, 6));
        assert_eq!(collect(&batch(&[&a, &b, &c])), vec![Ok(a), Ok(b), Ok(c)]);
    }

    #[test]
    fn rx_opts1_high_bits_are_not_the_length() {
        let f = frame(100, 7);
        let mut buf = batch(&[&f]);
        buf[1] |= 0x80; // RD_CRC, which Linux does not look at either
        buf[2] = 0xff;
        buf[3] = 0xff;
        assert_eq!(collect(&buf), vec![Ok(f)]);
    }

    #[test]
    fn rx_trailing_zeros_end_quietly() {
        let f = frame(60, 8);
        let mut buf = batch(&[&f]);
        buf.resize(512, 0);
        assert_eq!(collect(&buf), vec![Ok(f)]);
    }

    #[test]
    fn rx_short_tail_is_reported_once() {
        let f = frame(60, 9);
        let mut buf = batch(&[&f]);
        buf.extend_from_slice(&[1; 10]);
        assert_eq!(
            collect(&buf),
            vec![Ok(f), Err(RxError::TruncatedDescriptor { left: 10 })]
        );
    }

    #[test]
    fn rx_runt_length_ends_the_transfer() {
        let mut buf = vec![0u8; 200];
        buf[0] = 59;
        assert_eq!(collect(&buf), vec![Err(RxError::Runt { len: 59 })]);
    }

    #[test]
    fn rx_shortest_frame_u_boot_would_underflow_on() {
        // A length of 3 would wrap U-Boot's `packet_len -= CRC_SIZE`.
        let mut buf = vec![0u8; 100];
        buf[0] = 3;
        assert_eq!(collect(&buf), vec![Err(RxError::Runt { len: 3 })]);
        // The smallest accepted length gives a 56-byte frame.
        let mut buf = vec![0u8; 100];
        buf[0] = 60;
        assert_eq!(collect(&buf)[0].as_ref().unwrap().len(), 56);
    }

    #[test]
    fn rx_length_past_the_end_stops_at_the_end() {
        let mut buf = vec![0u8; 200];
        buf[..4].copy_from_slice(&0x7fffu32.to_le_bytes());
        assert_eq!(
            collect(&buf),
            vec![Err(RxError::Truncated {
                len: 0x7fff,
                left: 176
            })]
        );
    }

    #[test]
    fn rx_frame_cut_by_the_end_of_the_transfer() {
        let f = frame(100, 10);
        let mut buf = batch(&[&f]);
        buf.truncate(buf.len() - 1);
        assert_eq!(
            collect(&buf),
            vec![Err(RxError::Truncated {
                len: 104,
                left: 103
            })]
        );
    }

    #[test]
    fn rx_too_long_is_skipped_and_the_next_frame_still_comes() {
        let long = frame(MAX_RX_FRAME + 1, 11);
        let next = frame(60, 12);
        assert_eq!(
            collect(&batch(&[&long, &next])),
            vec![Err(RxError::TooLong { len: 1523 }), Ok(next)]
        );
    }

    #[test]
    fn rx_longest_frame_is_taken() {
        let f = frame(MAX_RX_FRAME, 13);
        assert_eq!(collect(&batch(&[&f])), vec![Ok(f)]);
    }

    #[test]
    fn rx_descriptor_exactly_at_the_end_has_no_room_for_a_frame() {
        let mut buf = vec![0u8; RX_DESC_LEN];
        buf[0] = 60;
        assert_eq!(
            collect(&buf),
            vec![Err(RxError::Truncated { len: 60, left: 0 })]
        );
    }

    #[test]
    fn rx_arbitrary_bytes_stay_inside_and_end() {
        let mut state = 0x2545_f491_4f6c_dd1du64;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        for _ in 0..20_000 {
            let len = (next() % 2048) as usize;
            let mut buf: Vec<u8> = (0..len).map(|_| next() as u8).collect();
            // Half the time make the first length plausible, so that walks go past the first descriptor.
            if len >= 4 && next() & 1 == 0 {
                let plausible = 60 + (next() % 400) as u32;
                buf[..4].copy_from_slice(&plausible.to_le_bytes());
            }
            let mut cursor = RxCursor::new();
            let mut steps = 0;
            while let Some(item) = cursor.next(&buf) {
                if let Ok(range) = item {
                    assert!(range.end <= buf.len());
                    assert!(range.len() <= MAX_RX_FRAME);
                    assert!(range.start >= RX_DESC_LEN);
                }
                steps += 1;
                assert!(steps <= buf.len(), "the cursor must advance");
            }
            assert!(!cursor.has_more(buf.len()));
        }
    }
}
