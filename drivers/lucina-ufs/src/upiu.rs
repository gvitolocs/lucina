//! UTP transfer request descriptors (UTRD), physical region description table (PRDT) entries and UFS protocol
//! information units (UPIU). Layouts from JESD223 section 6 and JESD220 section 10, as Linux `struct
//! utp_transfer_req_desc`, `struct ufshcd_sg_entry`, `struct utp_upiu_req` and `struct utp_upiu_rsp` lay them out.
//! UTRD and PRDT fields are little endian; UPIU fields are big endian.

use crate::{Error, scsi::Sense};

/// Size of one UTRD.
pub const UTRD_SIZE: usize = 32;
/// Size of one PRDT entry.
pub const PRDT_ENTRY_SIZE: usize = 16;
/// Largest data byte count of one PRDT entry (18-bit field holding count - 1).
pub const PRDT_ENTRY_MAX: usize = 256 * 1024;
/// Header plus transaction specific fields of every UPIU (Linux `GENERAL_UPIU_REQUEST_SIZE`).
pub const UPIU_HEADER_AND_TSF: usize = 32;
/// Space Linux reserves for the command and for the response UPIU in a command descriptor (`ALIGNED_UPIU_SIZE`).
pub const UPIU_AREA: usize = 512;
/// Largest descriptor a query reads (`QUERY_DESC_MAX_SIZE`).
pub const QUERY_DESC_MAX: u16 = 255;

/// UTRD DW0: command type "UFS storage" (bits 31:28 = 1; UFSHCI 2.0 and later use it for every request).
const UTRD_CT_UFS_STORAGE: u32 = 1 << 28;
/// UTRD DW0: data direction, bits 26:25.
const UTRD_DD_SHIFT: u32 = 25;
/// UTRD DW0: interrupt on completion.
const UTRD_INTERRUPT: u32 = 1 << 24;

/// Overall Command Status values (UTRD DW2 bits 7:0).
pub const OCS_SUCCESS: u8 = 0x0;
pub const OCS_INVALID_CMD_TABLE_ATTR: u8 = 0x1;
pub const OCS_INVALID_PRDT_ATTR: u8 = 0x2;
pub const OCS_MISMATCH_DATA_BUF_SIZE: u8 = 0x3;
pub const OCS_MISMATCH_RESP_UPIU_SIZE: u8 = 0x4;
pub const OCS_PEER_COMM_FAILURE: u8 = 0x5;
pub const OCS_ABORTED: u8 = 0x6;
pub const OCS_FATAL_ERROR: u8 = 0x7;
pub const OCS_DEVICE_FATAL_ERROR: u8 = 0x8;
/// What software writes before ringing the door bell, so a descriptor the controller never completed is visible.
pub const OCS_INVALID_COMMAND_STATUS: u8 = 0xf;

/// UPIU transaction types.
pub const TT_NOP_OUT: u8 = 0x00;
pub const TT_COMMAND: u8 = 0x01;
pub const TT_QUERY_REQ: u8 = 0x16;
pub const TT_NOP_IN: u8 = 0x20;
pub const TT_RESPONSE: u8 = 0x21;
pub const TT_QUERY_RSP: u8 = 0x36;
pub const TT_REJECT: u8 = 0x3f;

/// Command UPIU flags.
pub const CMD_FLAG_WRITE: u8 = 0x20;
pub const CMD_FLAG_READ: u8 = 0x40;

/// Query functions.
pub const QUERY_FN_READ: u8 = 0x01;
pub const QUERY_FN_WRITE: u8 = 0x81;
/// Query opcodes.
pub const QUERY_OP_READ_DESC: u8 = 0x01;
pub const QUERY_OP_READ_ATTR: u8 = 0x03;
pub const QUERY_OP_READ_FLAG: u8 = 0x05;
pub const QUERY_OP_SET_FLAG: u8 = 0x06;

/// Data direction of a transfer request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DataDir {
    None,
    /// Host to device (a write).
    ToDevice,
    /// Device to host (a read).
    FromDevice,
}

impl DataDir {
    const fn bits(self) -> u32 {
        match self {
            DataDir::None => 0,
            DataDir::ToDevice => 1,
            DataDir::FromDevice => 2,
        }
    }
}

/// A transfer request descriptor; offsets and lengths are in bytes here and stored in dwords.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Utrd {
    pub dir: DataDir,
    pub interrupt: bool,
    /// Device address of the command descriptor (command UPIU), 128-byte aligned.
    pub ucd: u64,
    /// Offset of the response UPIU from `ucd`, and its size.
    pub rsp_offset: u16,
    pub rsp_len: u16,
    /// Offset of the PRDT from `ucd`, and its number of entries.
    pub prdt_offset: u16,
    pub prdt_entries: u16,
}

impl Utrd {
    pub fn encode(&self) -> [u8; UTRD_SIZE] {
        let mut d = [0u8; UTRD_SIZE];
        let mut dw0 = UTRD_CT_UFS_STORAGE | (self.dir.bits() << UTRD_DD_SHIFT);
        if self.interrupt {
            dw0 |= UTRD_INTERRUPT;
        }
        d[0..4].copy_from_slice(&dw0.to_le_bytes());
        d[8..12].copy_from_slice(&(OCS_INVALID_COMMAND_STATUS as u32).to_le_bytes());
        d[16..24].copy_from_slice(&self.ucd.to_le_bytes());
        d[24..26].copy_from_slice(&(self.rsp_len / 4).to_le_bytes());
        d[26..28].copy_from_slice(&(self.rsp_offset / 4).to_le_bytes());
        d[28..30].copy_from_slice(&self.prdt_entries.to_le_bytes());
        d[30..32].copy_from_slice(&(self.prdt_offset / 4).to_le_bytes());
        d
    }
}

/// The Overall Command Status of a UTRD the controller wrote back, masked as Linux `ufshcd_get_tr_ocs` does
/// (`MASK_OCS`, ufshci.h).
pub const MASK_OCS: u8 = 0x0f;

pub fn ocs(utrd: &[u8; UTRD_SIZE]) -> u8 {
    utrd[8] & MASK_OCS
}

/// One PRDT entry: `len` bytes at device address `addr`. Both must be dword multiples (JESD223: "the data byte count
/// shall be a dword multiple"), and `len` at most [`PRDT_ENTRY_MAX`].
pub fn prdt_entry(addr: u64, len: usize) -> Result<[u8; PRDT_ENTRY_SIZE], Error> {
    if len == 0 || len > PRDT_ENTRY_MAX || !len.is_multiple_of(4) || !addr.is_multiple_of(4) {
        return Err(Error::BadBuffer);
    }
    let mut e = [0u8; PRDT_ENTRY_SIZE];
    e[0..8].copy_from_slice(&addr.to_le_bytes());
    e[12..16].copy_from_slice(&((len - 1) as u32).to_le_bytes());
    Ok(e)
}

fn header(tt: u8, flags: u8, lun: u8, tag: u8, func: u8) -> [u8; UPIU_HEADER_AND_TSF] {
    let mut u = [0u8; UPIU_HEADER_AND_TSF];
    u[0] = tt;
    u[1] = flags;
    u[2] = lun;
    u[3] = tag;
    // u[4]: initiator ID and command set type, 0 = SCSI.
    u[5] = func;
    u
}

/// NOP OUT, the ping Linux sends until the device answers (`ufshcd_verify_dev_init`).
pub fn nop_out(tag: u8) -> [u8; UPIU_HEADER_AND_TSF] {
    header(TT_NOP_OUT, 0, 0, tag, 0)
}

/// Command UPIU carrying a SCSI CDB (at most 16 bytes) for `lun`, moving `expected_len` bytes.
pub fn command(
    lun: u8,
    tag: u8,
    dir: DataDir,
    expected_len: u32,
    cdb: &[u8],
) -> [u8; UPIU_HEADER_AND_TSF] {
    let flags = match dir {
        DataDir::None => 0,
        DataDir::ToDevice => CMD_FLAG_WRITE,
        DataDir::FromDevice => CMD_FLAG_READ,
    };
    let mut u = header(TT_COMMAND, flags, lun, tag, 0);
    u[12..16].copy_from_slice(&expected_len.to_be_bytes());
    let n = cdb.len().min(16);
    u[16..16 + n].copy_from_slice(&cdb[..n]);
    u
}

/// A query request (`struct utp_upiu_query` in the transaction specific fields).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Query {
    pub func: u8,
    pub opcode: u8,
    pub idn: u8,
    pub index: u8,
    pub selector: u8,
    /// Descriptor length requested (read descriptor).
    pub length: u16,
    /// Attribute value (write attribute); unused for reads and flags.
    pub value: u32,
}

impl Query {
    pub const fn read_desc(idn: u8, index: u8) -> Self {
        Self {
            func: QUERY_FN_READ,
            opcode: QUERY_OP_READ_DESC,
            idn,
            index,
            selector: 0,
            length: QUERY_DESC_MAX,
            value: 0,
        }
    }
    pub const fn read_attr(idn: u8, index: u8) -> Self {
        Self {
            func: QUERY_FN_READ,
            opcode: QUERY_OP_READ_ATTR,
            idn,
            index,
            selector: 0,
            length: 0,
            value: 0,
        }
    }
    pub const fn read_flag(idn: u8) -> Self {
        Self {
            func: QUERY_FN_READ,
            opcode: QUERY_OP_READ_FLAG,
            idn,
            index: 0,
            selector: 0,
            length: 0,
            value: 0,
        }
    }
    pub const fn set_flag(idn: u8) -> Self {
        Self {
            func: QUERY_FN_WRITE,
            opcode: QUERY_OP_SET_FLAG,
            idn,
            index: 0,
            selector: 0,
            length: 0,
            value: 0,
        }
    }

    pub fn encode(&self, tag: u8) -> [u8; UPIU_HEADER_AND_TSF] {
        let mut u = header(TT_QUERY_REQ, 0, 0, tag, self.func);
        u[12] = self.opcode;
        u[13] = self.idn;
        u[14] = self.index;
        u[15] = self.selector;
        u[18..20].copy_from_slice(&self.length.to_be_bytes());
        u[20..24].copy_from_slice(&self.value.to_be_bytes());
        u
    }
}

/// The common UPIU header fields of a response.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Header {
    pub tt: u8,
    pub flags: u8,
    pub lun: u8,
    pub tag: u8,
    pub func: u8,
    pub response: u8,
    pub status: u8,
    pub data_segment_len: u16,
}

pub fn parse_header(u: &[u8]) -> Result<Header, Error> {
    if u.len() < 12 {
        return Err(Error::BadData("UPIU header"));
    }
    Ok(Header {
        tt: u[0] & 0x3f,
        flags: u[1],
        lun: u[2],
        tag: u[3],
        func: u[5],
        response: u[6],
        status: u[7],
        data_segment_len: u16::from_be_bytes([u[10], u[11]]),
    })
}

/// SCSI status GOOD.
pub const SCSI_GOOD: u8 = 0x00;
pub const SCSI_CHECK_CONDITION: u8 = 0x02;
pub const SCSI_BUSY: u8 = 0x08;
pub const SCSI_TASK_SET_FULL: u8 = 0x28;
/// SCSI status TASK ABORTED, which Linux's SCSI layer retries like BUSY.
pub const SCSI_TASK_ABORTED: u8 = 0x40;
/// Response UPIU flag: the residual count overflowed, so the residual is not meaningful (Linux
/// `UPIU_RSP_FLAG_OVERFLOW`).
pub const RSP_FLAG_OVERFLOW: u8 = 0x40;

/// What a RESPONSE UPIU says about a SCSI command.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CommandResponse {
    pub response: u8,
    pub status: u8,
    /// Flags byte of the response header.
    pub flags: u8,
    pub residual: u32,
    pub sense: Option<Sense>,
}

pub fn parse_command_response(u: &[u8]) -> Result<CommandResponse, Error> {
    let h = parse_header(u)?;
    if h.tt != TT_RESPONSE {
        return Err(Error::BadResponse {
            expected: TT_RESPONSE,
            got: h.tt,
        });
    }
    if u.len() < UPIU_HEADER_AND_TSF {
        return Err(Error::BadData("response UPIU"));
    }
    let residual = u32::from_be_bytes([u[12], u[13], u[14], u[15]]);
    // Data segment: sense data length (2 bytes, big endian), then the sense data.
    let sense = if h.data_segment_len >= 2 && u.len() >= UPIU_HEADER_AND_TSF + 2 {
        let len = u16::from_be_bytes([u[32], u[33]]) as usize;
        let end = (UPIU_HEADER_AND_TSF + 2 + len).min(u.len());
        Sense::parse(&u[UPIU_HEADER_AND_TSF + 2..end])
    } else {
        None
    };
    Ok(CommandResponse {
        response: h.response,
        status: h.status,
        flags: h.flags,
        residual,
        sense,
    })
}

/// What a QUERY RESPONSE UPIU returns.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct QueryResponse {
    pub response: u8,
    pub opcode: u8,
    pub idn: u8,
    pub length: u16,
    pub value: u32,
    /// Length of the data segment (the descriptor) that follows the 32-byte header.
    pub data_len: u16,
}

pub fn parse_query_response(u: &[u8]) -> Result<QueryResponse, Error> {
    let h = parse_header(u)?;
    if h.tt != TT_QUERY_RSP {
        return Err(Error::BadResponse {
            expected: TT_QUERY_RSP,
            got: h.tt,
        });
    }
    if u.len() < UPIU_HEADER_AND_TSF {
        return Err(Error::BadData("query response UPIU"));
    }
    Ok(QueryResponse {
        response: h.response,
        opcode: u[12],
        idn: u[13],
        length: u16::from_be_bytes([u[18], u[19]]),
        value: u32::from_be_bytes([u[20], u[21], u[22], u[23]]),
        data_len: h.data_segment_len,
    })
}
