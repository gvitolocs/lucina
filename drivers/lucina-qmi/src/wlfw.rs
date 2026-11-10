// SPDX-License-Identifier: ISC
//
// Derived from Linux drivers/net/wireless/ath/ath10k/qmi_wlfw_v01.h and qmi_wlfw_v01.c (ISC licence, Copyright
// (c) 2018 The Linux Foundation, Copyright (c) 2022 Qualcomm Innovation Center, Inc.): the service and message
// ids, the limits, the structs and the element tables behind every `encode_body()` and `decode_tlv()` below.

//! The WLFW v01 messages (the WCN3990 firmware's QMI service) that `ath10k/qmi.c` uses to bring the radio up.
//!
//! One Rust struct per C struct, with the `*_valid` flags folded into `Option`s and the `*_len` counts into
//! `Vec`s. Field names are the C names. TLV numbers, widths and maxima are those of the `wlfw_*_ei` tables; each
//! `encode_body()` writes the TLVs in table order.
//!
//! The ath10k flow uses: `ind_register` (client id 0x4b4e454c, FW_READY and MSA_READY on), `host_cap`
//! (daemon_support 0, or the 8-bit variant of the quirk), `msa_info`, `msa_ready`, `cap`, `bdf_download`,
//! `cal_report`, `wlan_cfg`, `wlan_mode`, and `ini` (firmware log mode). `fw_ready` and `msa_ready` are the two
//! indications it handles. Not here, because this ath10k never sends or handles them: `request_mem`,
//! `respond_mem`, `cal_download`, `cal_update`, `initiate_cal_*`, `pin_connect_result`, `athdiag_*`, `vbatt`,
//! `mac_addr`, `rejuvenate`, `dynamic_feature_mask`, `m3_info`, `xo_cal` and `fw_init_done`. Their ids are in
//! [`msg_id`] so a received one can be named.

use crate::codec::{
    Body, Cursor, Error, LenWidth, Message, RESPONSE_TLV_TYPE, RESULT_SUCCESS, Writer,
};
use alloc::vec::Vec;

/// `WLFW_SERVICE_ID_V01`: the QRTR service number the firmware registers (69).
pub const SERVICE_ID: u32 = 0x45;
/// `WLFW_SERVICE_VERS_V01`.
pub const SERVICE_VERSION: u32 = 1;
/// The instance `ath10k_qmi_init()` looks up (`qmi_add_lookup(.., WLFW_SERVICE_ID_V01, WLFW_SERVICE_VERS_V01, 0)`).
pub const SERVICE_INSTANCE: u32 = 0;

/// What goes into the `instance` field of a QRTR lookup or server record: `version | instance << 8`
/// (`qmi_send_new_lookup()`). For WLFW as ath10k asks, that is 1.
pub const fn qrtr_instance(version: u32, instance: u32) -> u32 {
    version | (instance << 8)
}

/// `QMI_WLFW_*_V01` message ids. A request, its response and its indication share a number.
pub mod msg_id {
    pub const IND_REGISTER: u16 = 0x0020;
    pub const FW_READY_IND: u16 = 0x0021;
    pub const WLAN_MODE: u16 = 0x0022;
    pub const WLAN_CFG: u16 = 0x0023;
    pub const CAP: u16 = 0x0024;
    pub const BDF_DOWNLOAD: u16 = 0x0025;
    pub const CAL_REPORT: u16 = 0x0026;
    pub const CAL_DOWNLOAD: u16 = 0x0027;
    pub const INITIATE_CAL_DOWNLOAD_IND: u16 = 0x0028;
    pub const CAL_UPDATE: u16 = 0x0029;
    pub const INITIATE_CAL_UPDATE_IND: u16 = 0x002A;
    pub const MSA_READY_IND: u16 = 0x002B;
    pub const PIN_CONNECT_RESULT_IND: u16 = 0x002C;
    pub const MSA_INFO: u16 = 0x002D;
    pub const MSA_READY: u16 = 0x002E;
    pub const INI: u16 = 0x002F;
    pub const ATHDIAG_READ: u16 = 0x0030;
    pub const ATHDIAG_WRITE: u16 = 0x0031;
    pub const VBATT: u16 = 0x0032;
    pub const MAC_ADDR: u16 = 0x0033;
    pub const HOST_CAP: u16 = 0x0034;
    pub const REQUEST_MEM_IND: u16 = 0x0035;
    pub const RESPOND_MEM: u16 = 0x0036;
    pub const MEM_READY_IND: u16 = 0x0037;
    pub const FW_INIT_DONE_IND: u16 = 0x0038;
    pub const REJUVENATE_IND: u16 = 0x0039;
    pub const REJUVENATE_ACK: u16 = 0x003A;
    pub const DYNAMIC_FEATURE_MASK: u16 = 0x003B;
    pub const M3_INFO: u16 = 0x003C;
    pub const XO_CAL_IND: u16 = 0x003D;
}

pub const MAX_MEM_REG: usize = 2;
pub const MAX_NUM_CAL: usize = 5;
/// `QMI_WLFW_MAX_DATA_SIZE_V01`: the most board data one `bdf_download` request carries, and the chunk size
/// `ath10k_qmi_bdf_dnld_send_sync()` cuts the file into.
pub const MAX_DATA_SIZE: usize = 6144;
pub const MAX_NUM_CE: usize = 12;
pub const MAX_TIMESTAMP_LEN: usize = 32;
pub const MAX_NUM_GPIO: usize = 32;
pub const MAX_BUILD_ID_LEN: usize = 128;
pub const MAX_STR_LEN: usize = 16;
pub const MAX_NUM_SHADOW_REG: usize = 24;
pub const MAX_SHADOW_REG_V2: usize = 36;
pub const MAX_NUM_SVC: usize = 24;

/// `ATH10K_QMI_CLIENT_ID`: "LENK" in ASCII, little-endian.
pub const ATH10K_CLIENT_ID: u32 = 0x4b4e_454c;

/// `enum wlfw_driver_mode_enum_v01`.
pub mod driver_mode {
    pub const MISSION: i32 = 0;
    pub const FTM: i32 = 1;
    pub const EPPING: i32 = 2;
    pub const WALTEST: i32 = 3;
    pub const OFF: i32 = 4;
    pub const CCPM: i32 = 5;
    pub const QVIT: i32 = 6;
    pub const CALIBRATION: i32 = 7;
}

/// `enum wlfw_pipedir_enum_v01`.
pub mod pipedir {
    pub const NONE: u32 = 0;
    pub const IN: u32 = 1;
    pub const OUT: u32 = 2;
    pub const INOUT: u32 = 3;
}

/// `enum wlfw_mem_type_enum_v01`.
pub mod mem_type {
    pub const MSA: i32 = 0;
    pub const DDR: i32 = 1;
}

/// `QMI_WLFW_CE_ATTR_*_V01` flags of a copy engine.
pub mod ce_attr {
    pub const NO_SNOOP: u32 = 0x01;
    pub const BYTE_SWAP_DATA: u32 = 0x02;
    pub const SWIZZLE_DESCRIPTORS: u32 = 0x04;
    pub const DISABLE_INTR: u32 = 0x08;
    pub const ENABLE_POLL: u32 = 0x10;
}

/// Bits of `fw_status` in the indication-register response.
pub mod fw_status {
    pub const ALREADY_REGISTERED: u64 = 0x01;
    pub const FW_READY: u64 = 0x02;
    pub const MSA_READY: u64 = 0x04;
    pub const MEM_READY: u64 = 0x08;
    pub const FW_INIT_DONE: u64 = 0x10;
}

/// `struct qmi_response_type_v01`: the TLV 0x02 every response has.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Response {
    pub result: u16,
    pub error: u16,
}

impl Response {
    pub const OK: Response = Response {
        result: RESULT_SUCCESS,
        error: 0,
    };

    pub fn is_success(&self) -> bool {
        self.result == RESULT_SUCCESS
    }

    fn write(&self, b: &mut Body<'_>) {
        b.put_u16(self.result);
        b.put_u16(self.error);
    }

    fn read(c: &mut Cursor<'_>) -> Result<Response, Error> {
        Ok(Response {
            result: c.u16()?,
            error: c.u16()?,
        })
    }
}

fn put_resp(w: &mut Writer, r: &Response) -> Result<(), Error> {
    w.tlv(RESPONSE_TLV_TYPE, |b| {
        r.write(b);
        Ok(())
    })
}

/// `struct wlfw_ce_tgt_pipe_cfg_s_v01`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CeTgtPipeCfg {
    pub pipe_num: u32,
    /// [`pipedir`].
    pub pipe_dir: u32,
    pub nentries: u32,
    pub nbytes_max: u32,
    /// [`ce_attr`].
    pub flags: u32,
}

impl CeTgtPipeCfg {
    fn write(&self, b: &mut Body<'_>) {
        b.put_u32(self.pipe_num);
        b.put_u32(self.pipe_dir);
        b.put_u32(self.nentries);
        b.put_u32(self.nbytes_max);
        b.put_u32(self.flags);
    }

    fn read(c: &mut Cursor<'_>) -> Result<Self, Error> {
        Ok(CeTgtPipeCfg {
            pipe_num: c.u32()?,
            pipe_dir: c.u32()?,
            nentries: c.u32()?,
            nbytes_max: c.u32()?,
            flags: c.u32()?,
        })
    }
}

/// `struct wlfw_ce_svc_pipe_cfg_s_v01`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CeSvcPipeCfg {
    pub service_id: u32,
    pub pipe_dir: u32,
    pub pipe_num: u32,
}

impl CeSvcPipeCfg {
    fn write(&self, b: &mut Body<'_>) {
        b.put_u32(self.service_id);
        b.put_u32(self.pipe_dir);
        b.put_u32(self.pipe_num);
    }

    fn read(c: &mut Cursor<'_>) -> Result<Self, Error> {
        Ok(CeSvcPipeCfg {
            service_id: c.u32()?,
            pipe_dir: c.u32()?,
            pipe_num: c.u32()?,
        })
    }
}

/// `struct wlfw_shadow_reg_cfg_s_v01`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ShadowRegCfg {
    pub id: u16,
    pub offset: u16,
}

impl ShadowRegCfg {
    fn write(&self, b: &mut Body<'_>) {
        b.put_u16(self.id);
        b.put_u16(self.offset);
    }

    fn read(c: &mut Cursor<'_>) -> Result<Self, Error> {
        Ok(ShadowRegCfg {
            id: c.u16()?,
            offset: c.u16()?,
        })
    }
}

/// `struct wlfw_shadow_reg_v2_cfg_s_v01`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ShadowRegV2Cfg {
    pub addr: u32,
}

impl ShadowRegV2Cfg {
    fn write(&self, b: &mut Body<'_>) {
        b.put_u32(self.addr);
    }

    fn read(c: &mut Cursor<'_>) -> Result<Self, Error> {
        Ok(ShadowRegV2Cfg { addr: c.u32()? })
    }
}

/// `struct wlfw_memory_region_info_s_v01`: one region of the MSA the firmware wants mapped.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MemoryRegionInfo {
    pub region_addr: u64,
    pub size: u32,
    pub secure_flag: u8,
}

impl MemoryRegionInfo {
    fn write(&self, b: &mut Body<'_>) {
        b.put_u64(self.region_addr);
        b.put_u32(self.size);
        b.put_u8(self.secure_flag);
    }

    fn read(c: &mut Cursor<'_>) -> Result<Self, Error> {
        Ok(MemoryRegionInfo {
            region_addr: c.u64()?,
            size: c.u32()?,
            secure_flag: c.u8()?,
        })
    }
}

/// `struct wlfw_rf_chip_info_s_v01`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RfChipInfo {
    pub chip_id: u32,
    pub chip_family: u32,
}

/// `struct wlfw_rf_board_info_s_v01`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RfBoardInfo {
    pub board_id: u32,
}

/// `struct wlfw_soc_info_s_v01`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SocInfo {
    pub soc_id: u32,
}

/// `struct wlfw_fw_version_info_s_v01`. The timestamp is a C string of at most [`MAX_TIMESTAMP_LEN`] bytes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FwVersionInfo {
    pub fw_version: u32,
    pub fw_build_timestamp: Vec<u8>,
}

/// Messages with nothing but a `char placeholder`, and the ones that are only a response.
macro_rules! empty_message {
    ($(#[$m:meta])* $name:ident, $id:expr) => {
        $(#[$m])*
        #[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
        pub struct $name;

        impl Message for $name {
            const MSG_ID: u16 = $id;
            const MAX_LEN: usize = 0;

            fn encode_body(&self, _w: &mut Writer) -> Result<(), Error> {
                Ok(())
            }

            fn decode_tlv(&mut self, _ty: u8, _c: &mut Cursor<'_>) -> Result<bool, Error> {
                Ok(false)
            }
        }
    };
}

macro_rules! response_message {
    ($(#[$m:meta])* $name:ident, $id:expr) => {
        $(#[$m])*
        #[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
        pub struct $name {
            pub resp: Response,
        }

        impl Message for $name {
            const MSG_ID: u16 = $id;
            const MAX_LEN: usize = 7;

            fn encode_body(&self, w: &mut Writer) -> Result<(), Error> {
                put_resp(w, &self.resp)
            }

            fn decode_tlv(&mut self, ty: u8, c: &mut Cursor<'_>) -> Result<bool, Error> {
                match ty {
                    RESPONSE_TLV_TYPE => self.resp = Response::read(c)?,
                    _ => return Ok(false),
                }
                Ok(true)
            }
        }
    };
}

empty_message!(
    /// `struct wlfw_fw_ready_ind_msg_v01`: the firmware finished booting.
    FwReadyInd,
    msg_id::FW_READY_IND
);
empty_message!(
    /// `struct wlfw_msa_ready_ind_msg_v01`: the firmware is done with the MSA memory set-up and wants the
    /// board data.
    MsaReadyInd,
    msg_id::MSA_READY_IND
);
empty_message!(
    /// `struct wlfw_cap_req_msg_v01`.
    CapReq,
    msg_id::CAP
);
empty_message!(
    /// `struct wlfw_msa_ready_req_msg_v01`.
    MsaReadyReq,
    msg_id::MSA_READY
);

response_message!(
    /// `struct wlfw_wlan_mode_resp_msg_v01`.
    WlanModeResp,
    msg_id::WLAN_MODE
);
response_message!(
    /// `struct wlfw_wlan_cfg_resp_msg_v01`.
    WlanCfgResp,
    msg_id::WLAN_CFG
);
response_message!(
    /// `struct wlfw_bdf_download_resp_msg_v01`.
    BdfDownloadResp,
    msg_id::BDF_DOWNLOAD
);
response_message!(
    /// `struct wlfw_cal_report_resp_msg_v01`.
    CalReportResp,
    msg_id::CAL_REPORT
);
response_message!(
    /// `struct wlfw_msa_ready_resp_msg_v01`.
    MsaReadyResp,
    msg_id::MSA_READY
);
response_message!(
    /// `struct wlfw_ini_resp_msg_v01`.
    IniResp,
    msg_id::INI
);
response_message!(
    /// `struct wlfw_host_cap_resp_msg_v01`.
    HostCapResp,
    msg_id::HOST_CAP
);

/// `struct wlfw_ind_register_req_msg_v01`: which indications the host wants. Each `*_enable` is a `u8` in the C
/// and the firmware only looks at it being non-zero.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IndRegisterReq {
    pub fw_ready_enable: Option<u8>,
    pub initiate_cal_download_enable: Option<u8>,
    pub initiate_cal_update_enable: Option<u8>,
    pub msa_ready_enable: Option<u8>,
    pub pin_connect_result_enable: Option<u8>,
    pub client_id: Option<u32>,
    pub request_mem_enable: Option<u8>,
    pub mem_ready_enable: Option<u8>,
    pub fw_init_done_enable: Option<u8>,
    pub rejuvenate_enable: Option<u32>,
    pub xo_cal_enable: Option<u8>,
}

impl Message for IndRegisterReq {
    const MSG_ID: u16 = msg_id::IND_REGISTER;
    const MAX_LEN: usize = 50;

    fn encode_body(&self, w: &mut Writer) -> Result<(), Error> {
        w.opt_u8(0x10, self.fw_ready_enable)?;
        w.opt_u8(0x11, self.initiate_cal_download_enable)?;
        w.opt_u8(0x12, self.initiate_cal_update_enable)?;
        w.opt_u8(0x13, self.msa_ready_enable)?;
        w.opt_u8(0x14, self.pin_connect_result_enable)?;
        w.opt_u32(0x15, self.client_id)?;
        w.opt_u8(0x16, self.request_mem_enable)?;
        w.opt_u8(0x17, self.mem_ready_enable)?;
        w.opt_u8(0x18, self.fw_init_done_enable)?;
        w.opt_u32(0x19, self.rejuvenate_enable)?;
        w.opt_u8(0x1A, self.xo_cal_enable)
    }

    fn decode_tlv(&mut self, ty: u8, c: &mut Cursor<'_>) -> Result<bool, Error> {
        match ty {
            0x10 => self.fw_ready_enable = Some(c.u8()?),
            0x11 => self.initiate_cal_download_enable = Some(c.u8()?),
            0x12 => self.initiate_cal_update_enable = Some(c.u8()?),
            0x13 => self.msa_ready_enable = Some(c.u8()?),
            0x14 => self.pin_connect_result_enable = Some(c.u8()?),
            0x15 => self.client_id = Some(c.u32()?),
            0x16 => self.request_mem_enable = Some(c.u8()?),
            0x17 => self.mem_ready_enable = Some(c.u8()?),
            0x18 => self.fw_init_done_enable = Some(c.u8()?),
            0x19 => self.rejuvenate_enable = Some(c.u32()?),
            0x1A => self.xo_cal_enable = Some(c.u8()?),
            _ => return Ok(false),
        }
        Ok(true)
    }
}

/// `struct wlfw_ind_register_resp_msg_v01`. `fw_status` is a [`fw_status`] bit set.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct IndRegisterResp {
    pub resp: Response,
    pub fw_status: Option<u64>,
}

impl Message for IndRegisterResp {
    const MSG_ID: u16 = msg_id::IND_REGISTER;
    const MAX_LEN: usize = 18;

    fn encode_body(&self, w: &mut Writer) -> Result<(), Error> {
        put_resp(w, &self.resp)?;
        w.opt_u64(0x10, self.fw_status)
    }

    fn decode_tlv(&mut self, ty: u8, c: &mut Cursor<'_>) -> Result<bool, Error> {
        match ty {
            RESPONSE_TLV_TYPE => self.resp = Response::read(c)?,
            0x10 => self.fw_status = Some(c.u64()?),
            _ => return Ok(false),
        }
        Ok(true)
    }
}

/// `struct wlfw_host_cap_req_msg_v01`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HostCapReq {
    pub daemon_support: Option<u32>,
    pub wake_msi: Option<u32>,
    pub gpios: Option<Vec<u32>>,
    pub nm_modem: Option<u8>,
    pub bdf_support: Option<u8>,
    pub bdf_cache_support: Option<u8>,
    pub m3_support: Option<u8>,
    pub m3_cache_support: Option<u8>,
    pub cal_filesys_support: Option<u8>,
    pub cal_cache_support: Option<u8>,
    pub cal_done: Option<u8>,
    pub mem_bucket: Option<u32>,
    pub mem_cfg_mode: Option<u8>,
}

impl Message for HostCapReq {
    const MSG_ID: u16 = msg_id::HOST_CAP;
    const MAX_LEN: usize = 189;

    fn encode_body(&self, w: &mut Writer) -> Result<(), Error> {
        w.opt_u32(0x10, self.daemon_support)?;
        w.opt_u32(0x11, self.wake_msi)?;
        // The count row has `elem_size = sizeof(u32)`, so the count is two bytes wide.
        w.opt_array(
            0x12,
            LenWidth::U16,
            MAX_NUM_GPIO,
            self.gpios.as_deref(),
            |b, v| {
                b.put_u32(*v);
                Ok(())
            },
        )?;
        w.opt_u8(0x13, self.nm_modem)?;
        w.opt_u8(0x14, self.bdf_support)?;
        w.opt_u8(0x15, self.bdf_cache_support)?;
        w.opt_u8(0x16, self.m3_support)?;
        w.opt_u8(0x17, self.m3_cache_support)?;
        w.opt_u8(0x18, self.cal_filesys_support)?;
        w.opt_u8(0x19, self.cal_cache_support)?;
        w.opt_u8(0x1A, self.cal_done)?;
        w.opt_u32(0x1B, self.mem_bucket)?;
        w.opt_u8(0x1C, self.mem_cfg_mode)
    }

    fn decode_tlv(&mut self, ty: u8, c: &mut Cursor<'_>) -> Result<bool, Error> {
        match ty {
            0x10 => self.daemon_support = Some(c.u32()?),
            0x11 => self.wake_msi = Some(c.u32()?),
            0x12 => self.gpios = Some(c.array(LenWidth::U16, MAX_NUM_GPIO, |c| c.u32())?),
            0x13 => self.nm_modem = Some(c.u8()?),
            0x14 => self.bdf_support = Some(c.u8()?),
            0x15 => self.bdf_cache_support = Some(c.u8()?),
            0x16 => self.m3_support = Some(c.u8()?),
            0x17 => self.m3_cache_support = Some(c.u8()?),
            0x18 => self.cal_filesys_support = Some(c.u8()?),
            0x19 => self.cal_cache_support = Some(c.u8()?),
            0x1A => self.cal_done = Some(c.u8()?),
            0x1B => self.mem_bucket = Some(c.u32()?),
            0x1C => self.mem_cfg_mode = Some(c.u8()?),
            _ => return Ok(false),
        }
        Ok(true)
    }
}

/// `wlfw_host_cap_8bit_req_msg_v01_ei`: the table `ATH10K_SNOC_FLAG_8BIT_HOST_CAP_QUIRK` firmware gets. Only
/// `daemon_support`, and as one byte instead of four (the C reads the low byte of the same `u32` field).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct HostCapReq8bit {
    pub daemon_support: Option<u8>,
}

impl Message for HostCapReq8bit {
    const MSG_ID: u16 = msg_id::HOST_CAP;
    const MAX_LEN: usize = 189;

    fn encode_body(&self, w: &mut Writer) -> Result<(), Error> {
        w.opt_u8(0x10, self.daemon_support)
    }

    fn decode_tlv(&mut self, ty: u8, c: &mut Cursor<'_>) -> Result<bool, Error> {
        match ty {
            0x10 => self.daemon_support = Some(c.u8()?),
            _ => return Ok(false),
        }
        Ok(true)
    }
}

/// `struct wlfw_msa_info_req_msg_v01`: where the host's MSA memory is.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MsaInfoReq {
    pub msa_addr: u64,
    pub size: u32,
}

impl Message for MsaInfoReq {
    const MSG_ID: u16 = msg_id::MSA_INFO;
    const MAX_LEN: usize = 18;

    fn encode_body(&self, w: &mut Writer) -> Result<(), Error> {
        w.tlv_u64(0x01, self.msa_addr)?;
        w.tlv_u32(0x02, self.size)
    }

    fn decode_tlv(&mut self, ty: u8, c: &mut Cursor<'_>) -> Result<bool, Error> {
        match ty {
            0x01 => self.msa_addr = c.u64()?,
            0x02 => self.size = c.u32()?,
            _ => return Ok(false),
        }
        Ok(true)
    }
}

/// `struct wlfw_msa_info_resp_msg_v01`: up to [`MAX_MEM_REG`] regions inside the memory the host offered.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MsaInfoResp {
    pub resp: Response,
    pub mem_region_info: Vec<MemoryRegionInfo>,
}

impl Message for MsaInfoResp {
    const MSG_ID: u16 = msg_id::MSA_INFO;
    const MAX_LEN: usize = 37;

    fn encode_body(&self, w: &mut Writer) -> Result<(), Error> {
        put_resp(w, &self.resp)?;
        w.tlv_array(
            0x03,
            LenWidth::U8,
            MAX_MEM_REG,
            &self.mem_region_info,
            |b, r| {
                r.write(b);
                Ok(())
            },
        )
    }

    fn decode_tlv(&mut self, ty: u8, c: &mut Cursor<'_>) -> Result<bool, Error> {
        match ty {
            RESPONSE_TLV_TYPE => self.resp = Response::read(c)?,
            0x03 => {
                self.mem_region_info =
                    c.array(LenWidth::U8, MAX_MEM_REG, MemoryRegionInfo::read)?;
            }
            _ => return Ok(false),
        }
        Ok(true)
    }
}

/// `struct wlfw_cap_resp_msg_v01`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CapResp {
    pub resp: Response,
    pub chip_info: Option<RfChipInfo>,
    pub board_info: Option<RfBoardInfo>,
    pub soc_info: Option<SocInfo>,
    pub fw_version_info: Option<FwVersionInfo>,
    /// A C string of at most [`MAX_BUILD_ID_LEN`] bytes.
    pub fw_build_id: Option<Vec<u8>>,
    pub num_macs: Option<u8>,
}

impl Message for CapResp {
    const MSG_ID: u16 = msg_id::CAP;
    const MAX_LEN: usize = 207;

    fn encode_body(&self, w: &mut Writer) -> Result<(), Error> {
        put_resp(w, &self.resp)?;
        w.opt_tlv(0x10, self.chip_info.as_ref(), |b, v| {
            b.put_u32(v.chip_id);
            b.put_u32(v.chip_family);
            Ok(())
        })?;
        w.opt_tlv(0x11, self.board_info.as_ref(), |b, v| {
            b.put_u32(v.board_id);
            Ok(())
        })?;
        w.opt_tlv(0x12, self.soc_info.as_ref(), |b, v| {
            b.put_u32(v.soc_id);
            Ok(())
        })?;
        w.opt_tlv(0x13, self.fw_version_info.as_ref(), |b, v| {
            b.put_u32(v.fw_version);
            b.put_str(MAX_TIMESTAMP_LEN + 1, &v.fw_build_timestamp)
        })?;
        w.opt_str(0x14, MAX_BUILD_ID_LEN + 1, self.fw_build_id.as_deref())?;
        w.opt_u8(0x15, self.num_macs)
    }

    fn decode_tlv(&mut self, ty: u8, c: &mut Cursor<'_>) -> Result<bool, Error> {
        match ty {
            RESPONSE_TLV_TYPE => self.resp = Response::read(c)?,
            0x10 => {
                self.chip_info = Some(RfChipInfo {
                    chip_id: c.u32()?,
                    chip_family: c.u32()?,
                });
            }
            0x11 => self.board_info = Some(RfBoardInfo { board_id: c.u32()? }),
            0x12 => self.soc_info = Some(SocInfo { soc_id: c.u32()? }),
            0x13 => {
                self.fw_version_info = Some(FwVersionInfo {
                    fw_version: c.u32()?,
                    fw_build_timestamp: c.str_prefixed(MAX_TIMESTAMP_LEN + 1)?,
                });
            }
            0x14 => self.fw_build_id = Some(c.str_rest(MAX_BUILD_ID_LEN + 1)?),
            0x15 => self.num_macs = Some(c.u8()?),
            _ => return Ok(false),
        }
        Ok(true)
    }
}

/// `struct wlfw_bdf_download_req_msg_v01`: one chunk of the board data file.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BdfDownloadReq {
    /// Mandatory (TLV 0x01); ath10k sets it to 1.
    pub valid: u8,
    /// `enum wlfw_cal_temp_id_enum_v01`; ath10k sends 0.
    pub file_id: Option<i32>,
    pub total_size: Option<u32>,
    pub seg_id: Option<u32>,
    /// At most [`MAX_DATA_SIZE`] bytes, with a two-byte count in front.
    pub data: Option<Vec<u8>>,
    /// 1 on the last chunk: the firmware then checks the file's CRC.
    pub end: Option<u8>,
    pub bdf_type: Option<u8>,
}

impl Message for BdfDownloadReq {
    const MSG_ID: u16 = msg_id::BDF_DOWNLOAD;
    const MAX_LEN: usize = 6182;

    fn encode_body(&self, w: &mut Writer) -> Result<(), Error> {
        w.tlv_u8(0x01, self.valid)?;
        w.opt_i32(0x10, self.file_id)?;
        w.opt_u32(0x11, self.total_size)?;
        w.opt_u32(0x12, self.seg_id)?;
        w.opt_array(
            0x13,
            LenWidth::U16,
            MAX_DATA_SIZE,
            self.data.as_deref(),
            |b, v| {
                b.put_u8(*v);
                Ok(())
            },
        )?;
        w.opt_u8(0x14, self.end)?;
        w.opt_u8(0x15, self.bdf_type)
    }

    fn decode_tlv(&mut self, ty: u8, c: &mut Cursor<'_>) -> Result<bool, Error> {
        match ty {
            0x01 => self.valid = c.u8()?,
            0x10 => self.file_id = Some(c.i32()?),
            0x11 => self.total_size = Some(c.u32()?),
            0x12 => self.seg_id = Some(c.u32()?),
            0x13 => self.data = Some(c.array(LenWidth::U16, MAX_DATA_SIZE, |c| c.u8())?),
            0x14 => self.end = Some(c.u8()?),
            0x15 => self.bdf_type = Some(c.u8()?),
            _ => return Ok(false),
        }
        Ok(true)
    }
}

/// `struct wlfw_cal_report_req_msg_v01`: which calibration data the host holds (none, in this ath10k).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CalReportReq {
    /// `enum wlfw_cal_temp_id_enum_v01` ids, at most [`MAX_NUM_CAL`].
    pub meta_data: Vec<i32>,
    pub xo_cal_data: Option<u8>,
}

impl Message for CalReportReq {
    const MSG_ID: u16 = msg_id::CAL_REPORT;
    const MAX_LEN: usize = 28;

    fn encode_body(&self, w: &mut Writer) -> Result<(), Error> {
        w.tlv_array(0x01, LenWidth::U8, MAX_NUM_CAL, &self.meta_data, |b, v| {
            b.put_i32(*v);
            Ok(())
        })?;
        w.opt_u8(0x10, self.xo_cal_data)
    }

    fn decode_tlv(&mut self, ty: u8, c: &mut Cursor<'_>) -> Result<bool, Error> {
        match ty {
            0x01 => self.meta_data = c.array(LenWidth::U8, MAX_NUM_CAL, |c| c.i32())?,
            0x10 => self.xo_cal_data = Some(c.u8()?),
            _ => return Ok(false),
        }
        Ok(true)
    }
}

/// `struct wlfw_wlan_mode_req_msg_v01`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WlanModeReq {
    /// [`driver_mode`].
    pub mode: i32,
    pub hw_debug: Option<u8>,
}

impl Message for WlanModeReq {
    const MSG_ID: u16 = msg_id::WLAN_MODE;
    const MAX_LEN: usize = 11;

    fn encode_body(&self, w: &mut Writer) -> Result<(), Error> {
        w.tlv_i32(0x01, self.mode)?;
        w.opt_u8(0x10, self.hw_debug)
    }

    fn decode_tlv(&mut self, ty: u8, c: &mut Cursor<'_>) -> Result<bool, Error> {
        match ty {
            0x01 => self.mode = c.i32()?,
            0x10 => self.hw_debug = Some(c.u8()?),
            _ => return Ok(false),
        }
        Ok(true)
    }
}

/// `struct wlfw_wlan_cfg_req_msg_v01`: the copy-engine layout the host will use.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WlanCfgReq {
    /// A C string of at most [`MAX_STR_LEN`] bytes.
    pub host_version: Option<Vec<u8>>,
    /// At most [`MAX_NUM_CE`].
    pub tgt_cfg: Option<Vec<CeTgtPipeCfg>>,
    /// At most [`MAX_NUM_SVC`].
    pub svc_cfg: Option<Vec<CeSvcPipeCfg>>,
    /// At most [`MAX_NUM_SHADOW_REG`].
    pub shadow_reg: Option<Vec<ShadowRegCfg>>,
    /// At most [`MAX_SHADOW_REG_V2`].
    pub shadow_reg_v2: Option<Vec<ShadowRegV2Cfg>>,
}

impl Message for WlanCfgReq {
    const MSG_ID: u16 = msg_id::WLAN_CFG;
    const MAX_LEN: usize = 803;

    fn encode_body(&self, w: &mut Writer) -> Result<(), Error> {
        w.opt_str(0x10, MAX_STR_LEN + 1, self.host_version.as_deref())?;
        w.opt_array(
            0x11,
            LenWidth::U8,
            MAX_NUM_CE,
            self.tgt_cfg.as_deref(),
            |b, v| {
                v.write(b);
                Ok(())
            },
        )?;
        w.opt_array(
            0x12,
            LenWidth::U8,
            MAX_NUM_SVC,
            self.svc_cfg.as_deref(),
            |b, v| {
                v.write(b);
                Ok(())
            },
        )?;
        w.opt_array(
            0x13,
            LenWidth::U8,
            MAX_NUM_SHADOW_REG,
            self.shadow_reg.as_deref(),
            |b, v| {
                v.write(b);
                Ok(())
            },
        )?;
        w.opt_array(
            0x14,
            LenWidth::U8,
            MAX_SHADOW_REG_V2,
            self.shadow_reg_v2.as_deref(),
            |b, v| {
                v.write(b);
                Ok(())
            },
        )
    }

    fn decode_tlv(&mut self, ty: u8, c: &mut Cursor<'_>) -> Result<bool, Error> {
        match ty {
            0x10 => self.host_version = Some(c.str_rest(MAX_STR_LEN + 1)?),
            0x11 => {
                self.tgt_cfg = Some(c.array(LenWidth::U8, MAX_NUM_CE, CeTgtPipeCfg::read)?);
            }
            0x12 => {
                self.svc_cfg = Some(c.array(LenWidth::U8, MAX_NUM_SVC, CeSvcPipeCfg::read)?);
            }
            0x13 => {
                self.shadow_reg =
                    Some(c.array(LenWidth::U8, MAX_NUM_SHADOW_REG, ShadowRegCfg::read)?);
            }
            0x14 => {
                self.shadow_reg_v2 =
                    Some(c.array(LenWidth::U8, MAX_SHADOW_REG_V2, ShadowRegV2Cfg::read)?);
            }
            _ => return Ok(false),
        }
        Ok(true)
    }
}

/// `struct wlfw_ini_req_msg_v01`: firmware log on or off.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct IniReq {
    pub enablefwlog: Option<u8>,
}

impl Message for IniReq {
    const MSG_ID: u16 = msg_id::INI;
    const MAX_LEN: usize = 4;

    fn encode_body(&self, w: &mut Writer) -> Result<(), Error> {
        w.opt_u8(0x10, self.enablefwlog)
    }

    fn decode_tlv(&mut self, ty: u8, c: &mut Cursor<'_>) -> Result<bool, Error> {
        match ty {
            0x10 => self.enablefwlog = Some(c.u8()?),
            _ => return Ok(false),
        }
        Ok(true)
    }
}
