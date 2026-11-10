//! Bulk transfer framing.
//!
//! **TX** (one frame per bulk OUT transfer, as U-Boot sends): `le32 length`, `le32 hdr2`, frame. `hdr2` is
//! [`TX_HDR2_PADDING`] when `length + 8` is a multiple of the bulk-out max packet size, else 0.
//!
//! **RX** (several frames per bulk IN transfer):
//!
//! ```text
//! <ee ee frame 1> <pad to 8> ... <ee ee frame N> <pad to 8>
//! <le32 header 1> [<le32 dummy header>] ... <le32 header N> [<le32 dummy header>] [<4 bytes pad>]
//! <le32 rx_hdr: packet count (low 16 bits), offset of header 1 (high 16 bits)>
//! ```
//!
//! A per-packet header holds `pkt_len` in bits 16..29 (13 bits): the frame plus the 2-byte `ee ee` alignment
//! prefix. The next packet starts at the next 8-byte boundary. Newer firmware adds a dummy header (`pkt_len` 0,
//! drop bit set) after every real one and counts it in the packet count (Linux's comment in `ax88179_rx_fixup`).
//!
//! Where the originals differ, this parser follows the stricter one:
//! * the header array must lie inside the transfer, in front of `rx_hdr` (Linux; U-Boot only checks the offset);
//! * dummy headers are skipped (Linux; U-Boot would hand up a zero-length packet);
//! * frames with the CRC or drop bit, and runts shorter than an Ethernet header, are reported as errors and skipped
//!   (Linux and NetBSD; U-Boot drops CRC errors in the MAC through `RX_CTL_DROPCRCERR` only);
//! * a frame is `pkt_len - 2` bytes after the prefix (Linux's last-packet path); U-Boot returns `pkt_len` bytes, i.e.
//!   2 bytes past the frame. NetBSD also trims a 4-byte FCS on pre-"A" chips; Linux does not, and nothing above
//!   Ethernet reads past the IP length, so the FCS is left in place rather than risk cutting a frame short.

use crate::regs::{
    RXHDR_CRC_ERR, RXHDR_DROP_ERR, RXHDR_L3CSUM_ERR, RXHDR_L4CSUM_ERR, TX_HDR2_PADDING,
};

/// Bytes in front of every transmitted frame.
pub const TX_HEADER_LEN: usize = 8;
/// Ethernet header length: the shortest frame accepted in either direction.
pub const ETH_HLEN: usize = 14;
/// Longest frame [`encode_tx`] accepts: 1514 plus one VLAN tag, without FCS.
pub const MAX_TX_FRAME: usize = 1518;
/// Longest frame the RX header can describe (13-bit length minus the 2-byte prefix).
pub const MAX_RX_FRAME: usize = 0x1fff - 2;

/// The 8-byte TX header for a frame of `frame_len` bytes.
pub fn tx_header(frame_len: usize, max_packet: u16) -> [u8; TX_HEADER_LEN] {
    let mut hdr2 = 0u32;
    if max_packet != 0 && (frame_len + TX_HEADER_LEN).is_multiple_of(max_packet as usize) {
        hdr2 |= TX_HDR2_PADDING;
    }
    let mut out = [0u8; TX_HEADER_LEN];
    out[..4].copy_from_slice(&(frame_len as u32).to_le_bytes());
    out[4..].copy_from_slice(&hdr2.to_le_bytes());
    out
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TxError {
    /// Shorter than an Ethernet header.
    TooShort(usize),
    /// Longer than [`MAX_TX_FRAME`].
    TooLong(usize),
    /// `out` cannot hold header and frame.
    BufferTooSmall { needed: usize },
}

/// Writes header and frame to `out`; returns the bulk transfer length (`frame.len() + 8`).
pub fn encode_tx(frame: &[u8], max_packet: u16, out: &mut [u8]) -> Result<usize, TxError> {
    if frame.len() < ETH_HLEN {
        return Err(TxError::TooShort(frame.len()));
    }
    if frame.len() > MAX_TX_FRAME {
        return Err(TxError::TooLong(frame.len()));
    }
    let total = frame.len() + TX_HEADER_LEN;
    let out = out
        .get_mut(..total)
        .ok_or(TxError::BufferTooSmall { needed: total })?;
    out[..TX_HEADER_LEN].copy_from_slice(&tx_header(frame.len(), max_packet));
    out[TX_HEADER_LEN..].copy_from_slice(frame);
    Ok(total)
}

/// A bulk IN transfer the parser rejects as a whole.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RxBatchError {
    /// Fewer than the 4 bytes of `rx_hdr`.
    TooShort(usize),
    /// The header array (`count` entries at `offset`) does not fit in front of `rx_hdr` in `len` bytes.
    HeaderOutOfRange {
        offset: usize,
        count: u16,
        len: usize,
    },
}

/// One packet the parser skips; later packets of the same transfer are still delivered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RxFrameError {
    /// The device flagged a CRC error.
    Crc { header: u32 },
    /// The device flagged the packet as dropped.
    Dropped { header: u32 },
    /// `pkt_len` too short for prefix plus Ethernet header.
    Runt { pkt_len: usize },
    /// The packet runs past the start of the header array; the rest of the transfer is discarded.
    Truncated { offset: usize, pkt_len: usize },
}

/// One received Ethernet frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RxFrame<'a> {
    /// The frame from the destination address on (the `ee ee` prefix removed).
    pub data: &'a [u8],
    /// Its per-packet header word.
    pub header: u32,
}

impl RxFrame<'_> {
    /// The device found a bad IPv4 header checksum (only meaningful with RX checksum offload on).
    pub fn l3_checksum_error(&self) -> bool {
        self.header & RXHDR_L3CSUM_ERR != 0
    }
    /// The device found a bad TCP/UDP checksum.
    pub fn l4_checksum_error(&self) -> bool {
        self.header & RXHDR_L4CSUM_ERR != 0
    }
}

/// Parse position inside one bulk IN transfer. Plain offsets, so the driver can keep it next to its own buffer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RxCursor {
    /// Offset of the next per-packet header.
    header: usize,
    /// Header entries left (dummy entries included).
    remaining: u16,
    /// Offset of the next packet.
    data: usize,
    /// End of the packet area: the start of the header array.
    data_end: usize,
}

impl RxCursor {
    /// Validates the trailer of `buf` (one whole transfer).
    pub fn new(buf: &[u8]) -> Result<Self, RxBatchError> {
        let len = buf.len();
        if len < 4 {
            return Err(RxBatchError::TooShort(len));
        }
        let rx_hdr = read_le32(buf, len - 4);
        let count = rx_hdr as u16;
        let offset = (rx_hdr >> 16) as usize;
        if count == 0 {
            return Ok(Self {
                header: 0,
                remaining: 0,
                data: 0,
                data_end: 0,
            });
        }
        if offset + count as usize * 4 > len - 4 {
            return Err(RxBatchError::HeaderOutOfRange { offset, count, len });
        }
        Ok(Self {
            header: offset,
            remaining: count,
            data: 0,
            data_end: offset,
        })
    }

    /// Header entries not yet looked at.
    pub fn remaining(&self) -> u16 {
        self.remaining
    }

    /// The next frame or skipped packet; `None` once the transfer is used up. `buf` must be the buffer given to
    /// [`RxCursor::new`].
    pub fn next<'a>(&mut self, buf: &'a [u8]) -> Option<Result<RxFrame<'a>, RxFrameError>> {
        while self.remaining > 0 {
            let header = read_le32(buf, self.header);
            self.header += 4;
            self.remaining -= 1;
            let pkt_len = ((header >> 16) & 0x1fff) as usize;
            if pkt_len == 0 {
                continue; // dummy header
            }
            let start = self.data;
            if start + pkt_len > self.data_end {
                self.remaining = 0;
                return Some(Err(RxFrameError::Truncated {
                    offset: start,
                    pkt_len,
                }));
            }
            self.data = (start + ((pkt_len + 7) & !7)).min(self.data_end);
            if header & RXHDR_CRC_ERR != 0 {
                return Some(Err(RxFrameError::Crc { header }));
            }
            if header & RXHDR_DROP_ERR != 0 {
                return Some(Err(RxFrameError::Dropped { header }));
            }
            if pkt_len < 2 + ETH_HLEN {
                return Some(Err(RxFrameError::Runt { pkt_len }));
            }
            return Some(Ok(RxFrame {
                data: &buf[start + 2..start + pkt_len],
                header,
            }));
        }
        None
    }
}

/// Iterator over the packets of one bulk IN transfer.
pub struct RxBatch<'a> {
    buf: &'a [u8],
    cursor: RxCursor,
}

impl<'a> RxBatch<'a> {
    pub fn parse(buf: &'a [u8]) -> Result<Self, RxBatchError> {
        Ok(Self {
            buf,
            cursor: RxCursor::new(buf)?,
        })
    }
}

impl<'a> Iterator for RxBatch<'a> {
    type Item = Result<RxFrame<'a>, RxFrameError>;

    fn next(&mut self) -> Option<Self::Item> {
        self.cursor.next(self.buf)
    }
}

fn read_le32(buf: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([buf[at], buf[at + 1], buf[at + 2], buf[at + 3]])
}

#[cfg(test)]
pub(crate) mod tests {
    extern crate std;
    use std::vec::Vec;

    use super::*;

    /// Builds a transfer the way the device lays it out. `dummy`: newer firmware's extra header per packet.
    pub(crate) fn batch(packets: &[(&[u8], u32)], dummy: bool) -> Vec<u8> {
        let mut buf = Vec::new();
        for (frame, _) in packets {
            buf.extend_from_slice(&[0xee, 0xee]);
            buf.extend_from_slice(frame);
            while buf.len() % 8 != 0 {
                buf.push(0);
            }
        }
        let offset = buf.len();
        let mut count = 0u16;
        for (frame, flags) in packets {
            let pkt_len = frame.len() as u32 + 2;
            buf.extend_from_slice(&((pkt_len << 16) | flags).to_le_bytes());
            count += 1;
            if dummy {
                buf.extend_from_slice(&RXHDR_DROP_ERR.to_le_bytes());
                count += 1;
            }
        }
        if dummy {
            buf.extend_from_slice(&[0; 4]); // padding2: rx_hdr ends on an 8-byte boundary
        }
        buf.extend_from_slice(&((offset as u32) << 16 | count as u32).to_le_bytes());
        buf
    }

    pub(crate) fn frame(len: usize, seed: u8) -> Vec<u8> {
        (0..len).map(|i| seed.wrapping_add(i as u8)).collect()
    }

    fn frames(buf: &[u8]) -> Vec<Result<Vec<u8>, RxFrameError>> {
        RxBatch::parse(buf)
            .unwrap()
            .map(|r| r.map(|f| f.data.to_vec()))
            .collect()
    }

    #[test]
    fn tx_header_without_padding() {
        assert_eq!(tx_header(60, 1024), [60, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(tx_header(1514, 1024), [0xea, 0x05, 0, 0, 0, 0, 0, 0]);
        // 1514 + 8 = 1522: not a multiple of 512 either.
        assert_eq!(tx_header(1514, 512), [0xea, 0x05, 0, 0, 0, 0, 0, 0]);
    }

    #[test]
    fn tx_header_sets_padding_on_packet_boundaries() {
        // 1016 + 8 = 1024: one full SuperSpeed packet.
        assert_eq!(
            tx_header(1016, 1024),
            [0xf8, 0x03, 0, 0, 0x00, 0x80, 0x00, 0x80]
        );
        // High speed: 504 + 8 = 512 and 1016 + 8 = 1024 both end on a 512-byte packet.
        assert_eq!(tx_header(504, 512)[4..], [0x00, 0x80, 0x00, 0x80]);
        assert_eq!(tx_header(1016, 512)[4..], [0x00, 0x80, 0x00, 0x80]);
        assert_eq!(tx_header(504, 1024)[4..], [0, 0, 0, 0]);
        // Every length that hits a boundary, and only those.
        for len in ETH_HLEN..=MAX_TX_FRAME {
            let padded = tx_header(len, 512)[4..] != [0, 0, 0, 0];
            assert_eq!(padded, (len + 8) % 512 == 0, "len {len}");
        }
    }

    #[test]
    fn encode_tx_writes_header_and_frame() {
        let f = frame(60, 1);
        let mut out = [0u8; 2048];
        assert_eq!(encode_tx(&f, 1024, &mut out), Ok(68));
        assert_eq!(out[..8], tx_header(60, 1024));
        assert_eq!(&out[8..68], &f[..]);
    }

    #[test]
    fn encode_tx_rejects_bad_lengths() {
        let mut out = [0u8; 2048];
        assert_eq!(
            encode_tx(&[0; 13], 1024, &mut out),
            Err(TxError::TooShort(13))
        );
        assert_eq!(
            encode_tx(&[0; 1519], 1024, &mut out),
            Err(TxError::TooLong(1519))
        );
        assert_eq!(
            encode_tx(&[0; 100], 1024, &mut out[..107]),
            Err(TxError::BufferTooSmall { needed: 108 })
        );
        assert_eq!(
            encode_tx(&[0; MAX_TX_FRAME], 1024, &mut out),
            Ok(MAX_TX_FRAME + 8)
        );
    }

    #[test]
    fn rx_single_frame() {
        let f = frame(60, 7);
        let buf = batch(&[(&f, 0)], false);
        assert_eq!(frames(&buf), [Ok(f)]);
    }

    #[test]
    fn rx_several_frames_at_every_alignment() {
        // Lengths with every remainder mod 8 once the 2-byte prefix is added, plus a full-size frame.
        let lens = [60, 61, 62, 63, 64, 65, 66, 67, 1514, 14];
        let fs: Vec<Vec<u8>> = lens
            .iter()
            .enumerate()
            .map(|(i, &l)| frame(l, (i as u8).wrapping_mul(31)))
            .collect();
        let packets: Vec<(&[u8], u32)> = fs.iter().map(|f| (&f[..], 0)).collect();
        for dummy in [false, true] {
            let buf = batch(&packets, dummy);
            let got = frames(&buf);
            assert_eq!(got.len(), fs.len());
            for (g, f) in got.iter().zip(&fs) {
                assert_eq!(g.as_ref().unwrap(), f);
            }
        }
    }

    #[test]
    fn rx_skips_dummy_headers() {
        let a = frame(98, 3);
        let b = frame(342, 9);
        let buf = batch(&[(&a, 0), (&b, 0)], true);
        // rx_hdr counts 4 entries; two are dummies.
        assert_eq!(read_le32(&buf, buf.len() - 4) as u16, 4);
        assert_eq!(buf.len() % 8, 0);
        assert_eq!(frames(&buf), [Ok(a), Ok(b)]);
    }

    #[test]
    fn rx_reports_crc_drop_and_runt_but_keeps_going() {
        let good1 = frame(60, 1);
        let crc = frame(60, 2);
        let dropped = frame(80, 3);
        let runt = frame(10, 4);
        let good2 = frame(1514, 5);
        let buf = batch(
            &[
                (&good1, 0),
                (&crc, RXHDR_CRC_ERR),
                (&dropped, RXHDR_DROP_ERR),
                (&runt, 0),
                (&good2, 0),
            ],
            false,
        );
        let got = frames(&buf);
        assert_eq!(got.len(), 5);
        assert_eq!(got[0], Ok(good1));
        assert!(matches!(got[1], Err(RxFrameError::Crc { .. })));
        assert!(matches!(got[2], Err(RxFrameError::Dropped { .. })));
        assert_eq!(got[3], Err(RxFrameError::Runt { pkt_len: 12 }));
        assert_eq!(got[4], Ok(good2));
    }

    #[test]
    fn rx_checksum_flags_are_passed_up() {
        let f = frame(60, 1);
        let buf = batch(&[(&f, RXHDR_L4CSUM_ERR)], false);
        let got = RxBatch::parse(&buf).unwrap().next().unwrap().unwrap();
        assert!(got.l4_checksum_error());
        assert!(!got.l3_checksum_error());
    }

    #[test]
    fn rx_short_transfers() {
        assert_eq!(RxBatch::parse(&[]).err(), Some(RxBatchError::TooShort(0)));
        assert_eq!(
            RxBatch::parse(&[1, 0, 0]).err(),
            Some(RxBatchError::TooShort(3))
        );
        // Just an rx_hdr with no packets: an empty batch, not an error.
        assert_eq!(frames(&[0, 0, 0, 0]), []);
        assert_eq!(frames(&[0, 0, 0x10, 0x00]), []);
    }

    #[test]
    fn rx_header_array_out_of_range() {
        // Offset past the end.
        let mut buf = std::vec![0u8; 64];
        buf[60..].copy_from_slice(&((100u32 << 16) | 1).to_le_bytes());
        assert_eq!(
            RxBatch::parse(&buf).err(),
            Some(RxBatchError::HeaderOutOfRange {
                offset: 100,
                count: 1,
                len: 64
            })
        );
        // Offset fine, but the count makes the array run into rx_hdr.
        let f = frame(60, 1);
        let mut buf = batch(&[(&f, 0)], false);
        let n = buf.len();
        let offset = (read_le32(&buf, n - 4) >> 16) as usize;
        buf[n - 4..].copy_from_slice(&((offset as u32) << 16 | 2).to_le_bytes());
        assert_eq!(
            RxBatch::parse(&buf).err(),
            Some(RxBatchError::HeaderOutOfRange {
                offset,
                count: 2,
                len: n
            })
        );
        // A huge count (u16::MAX) is refused before anything is read.
        buf[n - 4..].copy_from_slice(&((offset as u32) << 16 | 0xffff).to_le_bytes());
        assert!(matches!(
            RxBatch::parse(&buf),
            Err(RxBatchError::HeaderOutOfRange { .. })
        ));
    }

    #[test]
    fn rx_truncated_packet_ends_the_batch() {
        let a = frame(60, 1);
        let b = frame(60, 2);
        let mut buf = batch(&[(&a, 0), (&b, 0)], false);
        let n = buf.len();
        let offset = (read_le32(&buf, n - 4) >> 16) as usize;
        // Second header claims 200 bytes, more than is left in front of the header array.
        buf[offset + 4..offset + 8].copy_from_slice(&(202u32 << 16).to_le_bytes());
        let got = frames(&buf);
        assert_eq!(got[0], Ok(a));
        assert_eq!(
            got[1],
            Err(RxFrameError::Truncated {
                offset: 64,
                pkt_len: 202
            })
        );
        assert_eq!(got.len(), 2);
    }

    #[test]
    fn rx_last_packet_may_end_unpadded_at_the_header_array() {
        // U-Boot/Linux allow the header array to start right after the last packet's bytes (no padding).
        let f = frame(61, 1); // pkt_len 63, padded 64
        let mut buf = std::vec![0xee, 0xee];
        buf.extend_from_slice(&f);
        let offset = buf.len(); // 63, not 8-aligned
        buf.extend_from_slice(&(63u32 << 16).to_le_bytes());
        buf.extend_from_slice(&((offset as u32) << 16 | 1).to_le_bytes());
        assert_eq!(frames(&buf), [Ok(f)]);
    }

    #[test]
    fn rx_garbage_never_panics() {
        // Deterministic pseudo-random transfers: parsing must stay in bounds.
        let mut x = 0x2545_f491_4f6c_dd1du64;
        for _ in 0..20_000 {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            let len = (x % 300) as usize;
            let mut buf: Vec<u8> = (0..len).map(|i| (x >> (i % 56)) as u8).collect();
            // Keep the trailer plausible half the time so the packet loop runs.
            if len >= 4 && x & 1 == 0 {
                let off = (x >> 8) as usize % len;
                let cnt = 1 + (x >> 20) as u32 % 8;
                buf[len - 4..].copy_from_slice(&((off as u32) << 16 | cnt).to_le_bytes());
            }
            if let Ok(b) = RxBatch::parse(&buf) {
                for f in b.take(100).flatten() {
                    assert!(f.data.len() >= ETH_HLEN);
                }
            }
        }
    }
}
