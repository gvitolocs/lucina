//! CDC-NCM: NTB parameters and 16-bit NTBs.
//!
//! An NCM data interface carries NCM Transfer Blocks (NCM 1.0 section 3): a transfer header (NTH16), one or more
//! datagram pointer tables (NDP16, chained by `wNextNdpIndex`) and the Ethernet frames they point at. One bulk
//! transfer carries one NTB. Only the 16-bit format is used; it is mandatory for devices (NCM 1.0 section 3.2)
//! and is what Linux's cdc_ncm selects unless told otherwise.
//!
//! Bring-up, as Linux's `cdc_ncm_bind_common` / `cdc_ncm_setup` does it (with the data interface at alternate
//! setting 0): `GET_NTB_PARAMETERS` ([`NtbParams::parse`]), `SET_NTB_INPUT_SIZE` to the receive size the host
//! picks ([`NtbParams::rx_size`]), `SET_CRC_MODE` (no CRC) and `SET_MAX_DATAGRAM_SIZE` only when the device
//! announces them in bmNetworkCapabilities, then alternate setting 1.

use core::ops::Range;

/// Class requests (NCM 1.0 table 6-2), to the control interface.
pub const GET_NTB_PARAMETERS: u8 = 0x80;
pub const SET_NTB_FORMAT: u8 = 0x84;
pub const GET_NTB_INPUT_SIZE: u8 = 0x85;
pub const SET_NTB_INPUT_SIZE: u8 = 0x86;
pub const GET_MAX_DATAGRAM_SIZE: u8 = 0x87;
pub const SET_MAX_DATAGRAM_SIZE: u8 = 0x88;
pub const SET_CRC_MODE: u8 = 0x8a;

/// bmNetworkCapabilities bits (NCM 1.0 table 5-2).
pub const NCAP_ETH_FILTER: u8 = 1 << 0;
pub const NCAP_NET_ADDRESS: u8 = 1 << 1;
pub const NCAP_ENCAP_COMMAND: u8 = 1 << 2;
pub const NCAP_MAX_DATAGRAM_SIZE: u8 = 1 << 3;
pub const NCAP_CRC_MODE: u8 = 1 << 4;
pub const NCAP_NTB_INPUT_SIZE: u8 = 1 << 5;

/// Length of the `GET_NTB_PARAMETERS` answer.
pub const NTB_PARAMETERS_LEN: usize = 28;

const NTH16_SIGNATURE: u32 = u32::from_le_bytes(*b"NCMH");
const NDP16_NOCRC_SIGNATURE: u32 = u32::from_le_bytes(*b"NCM0");
const NDP16_CRC_SIGNATURE: u32 = u32::from_le_bytes(*b"NCM1");
const NTH16_LEN: usize = 12;
const NDP16_HEADER_LEN: usize = 8;

/// The receive NTB size the host asks for, Linux's `CDC_NCM_NTB_DEF_SIZE_RX` for USB 2 (16 KiB), and the
/// smallest it accepts from a device (`USB_CDC_NCM_NTB_MIN_IN_SIZE`).
pub const DEFAULT_RX_SIZE: u32 = 16384;
pub const MIN_NTB_SIZE: u32 = 2048;
/// Datagrams per transmitted NTB, Linux's `CDC_NCM_DPT_DATAGRAMS_MAX` limit for NDP16.
pub const MAX_TX_DATAGRAMS: usize = 40;

/// The device's NTB parameters (NCM 1.0 table 6-3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NtbParams {
    pub formats: u16,
    pub in_max_size: u32,
    pub in_divisor: u16,
    pub in_remainder: u16,
    pub in_alignment: u16,
    pub out_max_size: u32,
    pub out_divisor: u16,
    pub out_remainder: u16,
    pub out_alignment: u16,
    pub out_max_datagrams: u16,
}

impl NtbParams {
    /// Parses the answer to `GET_NTB_PARAMETERS`, sanitizing values as Linux's `cdc_ncm_update_rxtx_max` and
    /// `cdc_ncm_setup` do: divisors and alignments that are zero, not a power of two or larger than the NTB are
    /// replaced by 4, the remainder is taken modulo the divisor, and an output size below 2048 bytes is rejected.
    pub fn parse(buf: &[u8]) -> Option<Self> {
        if buf.len() < NTB_PARAMETERS_LEN {
            return None;
        }
        let w = |at: usize| u16::from_le_bytes([buf[at], buf[at + 1]]);
        let d = |at: usize| u32::from_le_bytes([buf[at], buf[at + 1], buf[at + 2], buf[at + 3]]);
        let mut params = NtbParams {
            formats: w(2),
            in_max_size: d(4),
            in_divisor: w(8),
            in_remainder: w(10),
            in_alignment: w(12),
            out_max_size: d(16),
            out_divisor: w(20),
            out_remainder: w(22),
            out_alignment: w(24),
            out_max_datagrams: w(26),
        };
        if params.formats & 1 == 0
            || params.out_max_size < MIN_NTB_SIZE
            || params.in_max_size < MIN_NTB_SIZE
        {
            return None;
        }
        let tidy = |value: u16, limit: u32| {
            if value == 0 || !value.is_power_of_two() || value as u32 > limit {
                4
            } else {
                value
            }
        };
        let out_limit = params.out_max_size.min(u16::MAX as u32);
        params.out_divisor = tidy(params.out_divisor, out_limit);
        params.out_alignment = tidy(params.out_alignment, out_limit);
        params.out_remainder %= params.out_divisor;
        params.in_divisor = tidy(params.in_divisor, params.in_max_size);
        params.in_alignment = tidy(params.in_alignment, params.in_max_size);
        Some(params)
    }

    /// The receive NTB size to ask for with `SET_NTB_INPUT_SIZE`: the device's maximum, capped at
    /// [`DEFAULT_RX_SIZE`] and at 64 KiB less one byte (NTB16 lengths are 16-bit).
    pub fn rx_size(&self) -> u32 {
        self.in_max_size.min(DEFAULT_RX_SIZE).min(u16::MAX as u32)
    }

    /// The largest NTB the host sends: the device's maximum, capped like [`Self::rx_size`].
    pub fn tx_size(&self) -> usize {
        self.out_max_size.min(DEFAULT_RX_SIZE).min(u16::MAX as u32) as usize
    }

    /// The most datagrams per transmitted NTB (0 from the device means no limit).
    pub fn tx_datagrams(&self) -> usize {
        match self.out_max_datagrams {
            0 => MAX_TX_DATAGRAMS,
            n => (n as usize).min(MAX_TX_DATAGRAMS),
        }
    }
}

/// Why a received NTB was dropped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NtbError {
    /// Shorter than an NTH16, or the block length is longer than the transfer.
    Short,
    /// The NTH16 or an NDP16 has the wrong signature or header length.
    BadHeader,
    /// An NDP16 lies outside the block, is too short, or the chain loops.
    BadNdp,
}

fn le16(buf: &[u8], at: usize) -> usize {
    u16::from_le_bytes([buf[at], buf[at + 1]]) as usize
}

fn le32(buf: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([buf[at], buf[at + 1], buf[at + 2], buf[at + 3]])
}

/// A received 16-bit NTB: [`Ntb16::datagrams`] yields the byte range of each frame in the transfer.
#[derive(Debug)]
pub struct Ntb16<'a> {
    block: &'a [u8],
    sequence: u16,
    first_ndp: usize,
}

impl<'a> Ntb16<'a> {
    /// Checks the NTH16 (Linux `cdc_ncm_rx_verify_nth16`): signature, header length, a block length that fits
    /// the transfer (0 means the whole transfer) and a first NDP inside the block.
    pub fn parse(transfer: &'a [u8]) -> Result<Self, NtbError> {
        if transfer.len() < NTH16_LEN + NDP16_HEADER_LEN {
            return Err(NtbError::Short);
        }
        if le32(transfer, 0) != NTH16_SIGNATURE || le16(transfer, 4) != NTH16_LEN {
            return Err(NtbError::BadHeader);
        }
        let block_len = match le16(transfer, 8) {
            0 => transfer.len(),
            n if n > transfer.len() => return Err(NtbError::Short),
            n => n,
        };
        let first_ndp = le16(transfer, 10);
        if first_ndp < NTH16_LEN || first_ndp + NDP16_HEADER_LEN > block_len {
            return Err(NtbError::BadNdp);
        }
        Ok(Ntb16 {
            block: &transfer[..block_len],
            sequence: le16(transfer, 6) as u16,
            first_ndp,
        })
    }

    /// wSequence of the NTH16.
    pub fn sequence(&self) -> u16 {
        self.sequence
    }

    /// Calls `frame` with the range of every datagram, following the NDP chain (Linux `cdc_ncm_rx_fixup`).
    /// Datagrams that are shorter than an Ethernet header or run past the block are skipped and counted; the
    /// return value is that count, or an error for a broken NDP (frames before it have been handed out).
    pub fn datagrams(&self, mut frame: impl FnMut(Range<usize>)) -> Result<usize, NtbError> {
        let block = self.block;
        let mut ndp = self.first_ndp;
        let mut skipped = 0;
        // An NDP16 is at least 16 bytes, so a block cannot hold more than this many; more means a loop.
        let mut budget = block.len() / 16 + 1;
        loop {
            if ndp + NDP16_HEADER_LEN > block.len() || !ndp.is_multiple_of(4) {
                return Err(NtbError::BadNdp);
            }
            let signature = le32(block, ndp);
            if signature != NDP16_NOCRC_SIGNATURE && signature != NDP16_CRC_SIGNATURE {
                return Err(NtbError::BadHeader);
            }
            let ndp_len = le16(block, ndp + 4);
            // At least the header and two entries (one datagram, one terminator), whole entries only.
            if ndp_len < NDP16_HEADER_LEN + 8
                || !ndp_len.is_multiple_of(4)
                || ndp + ndp_len > block.len()
            {
                return Err(NtbError::BadNdp);
            }
            let entries = (ndp_len - NDP16_HEADER_LEN) / 4;
            for i in 0..entries {
                let at = ndp + NDP16_HEADER_LEN + 4 * i;
                let (offset, len) = (le16(block, at), le16(block, at + 2));
                if offset == 0 || len == 0 {
                    break;
                }
                if offset + len > block.len() || len < crate::ETH_HLEN {
                    skipped += 1;
                    continue;
                }
                frame(offset..offset + len);
            }
            let next = le16(block, ndp + 6);
            if next == 0 {
                return Ok(skipped);
            }
            budget -= 1;
            if budget == 0 {
                return Err(NtbError::BadNdp);
            }
            ndp = next;
        }
    }
}

fn align_up(value: usize, align: usize) -> usize {
    value.div_ceil(align) * align
}

/// Builds one 16-bit NTB for transmission into a caller's buffer.
///
/// Layout: NTH16, then the datagrams, each starting where `offset % wNdpOutDivisor == wNdpOutPayloadRemainder`
/// (NCM 1.0 section 3.3.4), then the NDP16 at the next `wNdpOutAlignment` boundary, as Linux places it by default
/// (at the end, after the datagrams). The NTB's length goes up by one zero byte when it would end on a packet
/// boundary, as Linux's `cdc_ncm_fill_tx_frame` forces a short packet.
pub struct Ntb16Builder<'b> {
    buf: &'b mut [u8],
    params: NtbParams,
    /// Where the next datagram may start (before alignment).
    end: usize,
    entries: [(u16, u16); MAX_TX_DATAGRAMS],
    count: usize,
    max_datagrams: usize,
}

impl<'b> Ntb16Builder<'b> {
    /// Starts an NTB in `buf`, which is cut to the device's maximum NTB size.
    pub fn new(buf: &'b mut [u8], params: NtbParams) -> Self {
        let size = buf.len().min(params.tx_size());
        Ntb16Builder {
            buf: &mut buf[..size],
            params,
            end: NTH16_LEN,
            entries: [(0, 0); MAX_TX_DATAGRAMS],
            count: 0,
            max_datagrams: params.tx_datagrams(),
        }
    }

    fn datagram_start(&self) -> usize {
        let divisor = self.params.out_divisor as usize;
        let remainder = self.params.out_remainder as usize;
        let base = align_up(self.end, divisor) + remainder;
        if base >= self.end + divisor {
            base - divisor
        } else {
            base
        }
    }

    fn ndp_len(count: usize) -> usize {
        align_up(NDP16_HEADER_LEN + 4 * (count + 1), 4)
    }

    /// Adds a frame. Returns `false` (and adds nothing) when it does not fit or the NTB holds the most datagrams
    /// the device takes.
    pub fn push(&mut self, frame: &[u8]) -> bool {
        if self.count == self.max_datagrams {
            return false;
        }
        let start = self.datagram_start();
        let end = start + frame.len();
        let ndp = align_up(end, self.params.out_alignment as usize);
        if ndp + Self::ndp_len(self.count + 1) > self.buf.len() {
            return false;
        }
        self.buf[self.end..start].fill(0);
        self.buf[start..end].copy_from_slice(frame);
        self.entries[self.count] = (start as u16, frame.len() as u16);
        self.count += 1;
        self.end = end;
        true
    }

    /// Number of datagrams added so far.
    pub fn len(&self) -> usize {
        self.count
    }

    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// Writes the NTH16 and the NDP16 and returns the NTB's length. `max_packet` is the bulk OUT endpoint's
    /// maximum packet size, for the short-packet byte.
    pub fn finish(self, sequence: u16, max_packet: u16) -> usize {
        let ndp = align_up(self.end, self.params.out_alignment as usize);
        let ndp_len = Self::ndp_len(self.count);
        let mut len = ndp + ndp_len;
        if max_packet != 0 && len.is_multiple_of(max_packet as usize) && len < self.buf.len() {
            len += 1;
        }
        let buf = self.buf;
        buf[self.end..len].fill(0);
        buf[0..4].copy_from_slice(&NTH16_SIGNATURE.to_le_bytes());
        buf[4..6].copy_from_slice(&(NTH16_LEN as u16).to_le_bytes());
        buf[6..8].copy_from_slice(&sequence.to_le_bytes());
        buf[8..10].copy_from_slice(&(len as u16).to_le_bytes());
        buf[10..12].copy_from_slice(&(ndp as u16).to_le_bytes());
        buf[ndp..ndp + 4].copy_from_slice(&NDP16_NOCRC_SIGNATURE.to_le_bytes());
        buf[ndp + 4..ndp + 6].copy_from_slice(&(ndp_len as u16).to_le_bytes());
        buf[ndp + 6..ndp + 8].copy_from_slice(&0u16.to_le_bytes());
        for (i, (offset, length)) in self.entries[..self.count].iter().enumerate() {
            let at = ndp + NDP16_HEADER_LEN + 4 * i;
            buf[at..at + 2].copy_from_slice(&offset.to_le_bytes());
            buf[at + 2..at + 4].copy_from_slice(&length.to_le_bytes());
        }
        // The terminating entry (and any alignment slack) is already zero.
        len
    }
}

#[cfg(test)]
mod tests {
    use alloc::vec::Vec;

    use super::*;

    /// Linux f_ncm's (the gadget's) parameters: NTB16 only, 16 KiB in, 16 KiB out, divisor 4, alignment 4.
    fn gadget_params() -> [u8; 28] {
        let mut p = [0u8; 28];
        p[0..2].copy_from_slice(&28u16.to_le_bytes());
        p[2..4].copy_from_slice(&1u16.to_le_bytes());
        p[4..8].copy_from_slice(&16384u32.to_le_bytes());
        p[8..10].copy_from_slice(&4u16.to_le_bytes());
        p[12..14].copy_from_slice(&4u16.to_le_bytes());
        p[16..20].copy_from_slice(&16384u32.to_le_bytes());
        p[20..22].copy_from_slice(&4u16.to_le_bytes());
        p[24..26].copy_from_slice(&4u16.to_le_bytes());
        p
    }

    fn frame(len: usize, tag: u8) -> Vec<u8> {
        (0..len).map(|i| tag.wrapping_add(i as u8)).collect()
    }

    #[test]
    fn params_parse_and_sanitize() {
        let p = NtbParams::parse(&gadget_params()).unwrap();
        assert_eq!(p.in_max_size, 16384);
        assert_eq!(p.rx_size(), 16384);
        assert_eq!(p.tx_size(), 16384);
        assert_eq!(p.tx_datagrams(), MAX_TX_DATAGRAMS);

        let mut odd = gadget_params();
        odd[20..22].copy_from_slice(&6u16.to_le_bytes()); // not a power of two
        odd[22..24].copy_from_slice(&9u16.to_le_bytes());
        odd[24..26].copy_from_slice(&0u16.to_le_bytes());
        let p = NtbParams::parse(&odd).unwrap();
        assert_eq!((p.out_divisor, p.out_remainder, p.out_alignment), (4, 1, 4));

        let mut tiny = gadget_params();
        tiny[16..20].copy_from_slice(&1024u32.to_le_bytes());
        assert_eq!(NtbParams::parse(&tiny), None);
        let mut no16 = gadget_params();
        no16[2..4].copy_from_slice(&2u16.to_le_bytes());
        assert_eq!(NtbParams::parse(&no16), None);
        assert_eq!(NtbParams::parse(&gadget_params()[..20]), None);
    }

    #[test]
    fn build_then_parse_round_trip() {
        let params = NtbParams::parse(&gadget_params()).unwrap();
        let mut buf = alloc::vec![0xffu8; 16384];
        let frames = [frame(60, 1), frame(1514, 2), frame(99, 3)];
        let mut b = Ntb16Builder::new(&mut buf, params);
        for f in &frames {
            assert!(b.push(f));
        }
        assert_eq!(b.len(), 3);
        let len = b.finish(7, 512);
        let ntb = Ntb16::parse(&buf[..len]).unwrap();
        assert_eq!(ntb.sequence(), 7);
        let mut got = Vec::new();
        assert_eq!(
            ntb.datagrams(|r| {
                assert_eq!(r.start % 4, 0);
                got.push(buf[r].to_vec())
            }),
            Ok(0)
        );
        assert_eq!(got, frames);
    }

    #[test]
    fn remainder_and_alignment() {
        let mut raw = gadget_params();
        raw[20..22].copy_from_slice(&8u16.to_le_bytes());
        raw[22..24].copy_from_slice(&2u16.to_le_bytes());
        raw[24..26].copy_from_slice(&16u16.to_le_bytes());
        let params = NtbParams::parse(&raw).unwrap();
        let mut buf = alloc::vec![0u8; 4096];
        let mut b = Ntb16Builder::new(&mut buf, params);
        assert!(b.push(&frame(61, 0)));
        assert!(b.push(&frame(61, 0)));
        let len = b.finish(0, 512);
        let ntb = Ntb16::parse(&buf[..len]).unwrap();
        let mut starts = Vec::new();
        ntb.datagrams(|r| starts.push(r.start)).unwrap();
        assert!(starts.iter().all(|s| s % 8 == 2), "{starts:?}");
        assert_eq!(le16(&buf, 10) % 16, 0);
    }

    #[test]
    fn short_packet_byte() {
        let params = NtbParams::parse(&gadget_params()).unwrap();
        // NTH 12 + frame at 12 of length L, NDP at align4(12+L) of 16 bytes. L = 100: NDP at 112, end 128.
        let mut buf = alloc::vec![0u8; 4096];
        let mut b = Ntb16Builder::new(&mut buf, params);
        assert!(b.push(&frame(100, 0)));
        let len = b.finish(0, 64);
        assert_eq!(len, 129);
        assert_eq!(le16(&buf, 8), 129);
        assert!(Ntb16::parse(&buf[..len]).is_ok());
    }

    #[test]
    fn full_ntb_refuses_more() {
        let params = NtbParams::parse(&gadget_params()).unwrap();
        let mut buf = alloc::vec![0u8; 4096];
        let mut b = Ntb16Builder::new(&mut buf, params);
        assert!(b.push(&frame(1514, 0)));
        assert!(b.push(&frame(1514, 0)));
        assert!(!b.push(&frame(1514, 0)));
        assert_eq!(b.len(), 2);
    }

    #[test]
    fn parse_rejects_bad_blocks() {
        let params = NtbParams::parse(&gadget_params()).unwrap();
        let mut buf = alloc::vec![0u8; 2048];
        let mut b = Ntb16Builder::new(&mut buf, params);
        assert!(b.push(&frame(60, 0)));
        let len = b.finish(0, 512);
        let good = buf[..len].to_vec();

        assert_eq!(Ntb16::parse(&good[..10]).unwrap_err(), NtbError::Short);
        let mut sig = good.clone();
        sig[0] = b'X';
        assert_eq!(Ntb16::parse(&sig).unwrap_err(), NtbError::BadHeader);
        let mut long = good.clone();
        long[8..10].copy_from_slice(&((len + 1) as u16).to_le_bytes());
        assert_eq!(Ntb16::parse(&long).unwrap_err(), NtbError::Short);
        let mut ndp_out = good.clone();
        ndp_out[10..12].copy_from_slice(&(len as u16).to_le_bytes());
        assert_eq!(Ntb16::parse(&ndp_out).unwrap_err(), NtbError::BadNdp);

        // A datagram pointing past the block is skipped, not handed up.
        let ndp = le16(&good, 10);
        let mut past = good.clone();
        past[ndp + 10..ndp + 12].copy_from_slice(&2000u16.to_le_bytes());
        let ntb = Ntb16::parse(&past).unwrap();
        let mut n = 0;
        assert_eq!(ntb.datagrams(|_| n += 1), Ok(1));
        assert_eq!(n, 0);

        // An NDP chain that points at itself ends with an error instead of looping.
        let mut looped = good.clone();
        looped[ndp + 6..ndp + 8].copy_from_slice(&(ndp as u16).to_le_bytes());
        let ntb = Ntb16::parse(&looped).unwrap();
        assert_eq!(ntb.datagrams(|_| {}), Err(NtbError::BadNdp));
    }

    #[test]
    fn two_ndps_chained() {
        // NTH, datagram A at 12 (20 bytes), datagram B at 32 (20 bytes), NDP1 at 52 -> NDP2 at 68.
        let mut b = alloc::vec![0u8; 84];
        b[0..4].copy_from_slice(b"NCMH");
        b[4..6].copy_from_slice(&12u16.to_le_bytes());
        b[8..10].copy_from_slice(&84u16.to_le_bytes());
        b[10..12].copy_from_slice(&52u16.to_le_bytes());
        for (ndp, next, dg) in [(52usize, 68u16, 12u16), (68, 0, 32)] {
            b[ndp..ndp + 4].copy_from_slice(b"NCM0");
            b[ndp + 4..ndp + 6].copy_from_slice(&16u16.to_le_bytes());
            b[ndp + 6..ndp + 8].copy_from_slice(&next.to_le_bytes());
            b[ndp + 8..ndp + 10].copy_from_slice(&dg.to_le_bytes());
            b[ndp + 10..ndp + 12].copy_from_slice(&20u16.to_le_bytes());
        }
        let ntb = Ntb16::parse(&b).unwrap();
        let mut got = Vec::new();
        assert_eq!(ntb.datagrams(|r| got.push(r)), Ok(0));
        assert_eq!(got, [12..32, 32..52]);
    }
}
