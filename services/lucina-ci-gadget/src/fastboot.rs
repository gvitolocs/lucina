//! The port as a fastboot device, for what runs before a kernel: the protocol of Amazon's bootloader, under its USB ids, so that
//! `lucina-fastboot` finds both. This only moves commands and data: what they mean on the card is the caller's, through
//! [`Action`]. One bulk pair; a command is one packet of at most 64 bytes and its reply one packet.

use crate::{
    DmaMem, Error, Hw,
    dcd::{Dcd, Done, EP0_IN, EP0_OUT, Ep, EpType, Speed, buf, completed_bit},
};

const BULK_IN: Ep = Ep::new(0x81);
const BULK_OUT: Ep = Ep::new(0x01);
const MPS_EP0: usize = 64;
const COMMAND_MAX: usize = 64;
/// As much as one transfer descriptor takes at any alignment.
const CHUNK: usize = 16 * 1024;

/// Where a download lands, from the memory's start.
pub const DATA_OFFSET: usize = 0x1_0000;
const COMMAND_BUF: usize = buf::ECM_RX;
const REPLY_BUF: usize = buf::ECM_TX;

/// Lab126's fastboot gadget: 1949:d0e0.
const DEVICE: [u8; 18] = [
    0x12, 0x01, 0x00, 0x02, 0x00, 0x00, 0x00, 0x40, 0x49, 0x19, 0xe0, 0xd0, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01,
];
const DEVICE_QUALIFIER: [u8; 10] = [0x0a, 0x06, 0x00, 0x02, 0x00, 0x00, 0x00, 0x40, 0x01, 0x00];
const CONFIG_LEN: usize = 32;

fn configuration(speed: Speed, other_speed: bool) -> [u8; CONFIG_LEN] {
    let (lo, hi) = match speed {
        Speed::High => (0x00, 0x02),
        Speed::Full => (0x40, 0x00),
    };
    let kind = if other_speed { 0x07 } else { 0x02 };
    #[rustfmt::skip]
    let descriptor = [
        0x09, kind, CONFIG_LEN as u8, 0x00, 0x01, 0x01, 0x00, 0xc0, 0x00,
        // The class, subclass and protocol of fastboot.
        0x09, 0x04, 0x00, 0x00, 0x02, 0xff, 0x42, 0x03, 0x00,
        0x07, 0x05, 0x81, 0x02, lo, hi, 0x00,
        0x07, 0x05, 0x01, 0x02, lo, hi, 0x00,
    ];
    descriptor
}

/// What a command is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command<'a> {
    GetVar(&'a str),
    /// `setvar:<name> <value>`, as Amazon's bootloader takes it: what follows the colon.
    SetVar(&'a str),
    Download(usize),
    Flash(&'a str),
    Boot,
    Reboot,
    Unknown,
}

pub fn parse(command: &[u8]) -> Command<'_> {
    let Ok(text) = core::str::from_utf8(command) else {
        return Command::Unknown;
    };
    let text = text.trim_end_matches(['\0', '\r', '\n']);
    if let Some(name) = text.strip_prefix("getvar:") {
        Command::GetVar(name)
    } else if let Some(rest) = text.strip_prefix("setvar:") {
        Command::SetVar(rest)
    } else if let Some(hex) = text.strip_prefix("download:") {
        usize::from_str_radix(hex, 16).map_or(Command::Unknown, Command::Download)
    } else if let Some(name) = text.strip_prefix("flash:") {
        Command::Flash(name)
    } else if text == "boot" {
        Command::Boot
    } else if text == "reboot" {
        Command::Reboot
    } else {
        Command::Unknown
    }
}

/// What the caller is to do. The reply is the caller's too: [`Fastboot::reply`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    /// `flash:` with a name in [`Fastboot::name`], of [`Fastboot::downloaded`] bytes from [`DATA_OFFSET`].
    Flash,
    /// `boot`, of what was downloaded.
    Boot,
    Reboot,
    /// `getvar:` of a name this does not answer itself, in [`Fastboot::name`]: the caller answers.
    GetVar,
    /// `setvar:`, with `<name> <value>` in [`Fastboot::name`].
    SetVar,
    /// A host talked: time to wait for one starts again.
    Activity,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Ep0 {
    Idle,
    DataIn,
    StatusIn,
    StatusOut,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Bulk {
    Off,
    Command,
    Data { received: usize, expected: usize },
}

pub struct Fastboot<H, M> {
    pub dcd: Dcd<H, M>,
    speed: Speed,
    ep0: Ep0,
    pending_address: Option<u8>,
    bulk: Bulk,
    max_download: usize,
    /// How much the last download brought.
    pub downloaded: usize,
    name: [u8; COMMAND_MAX],
    name_len: usize,
    /// What `getvar:bootloader` answers.
    pub bootloader: &'static str,
}

impl<H: Hw, M: DmaMem> Fastboot<H, M> {
    pub fn new(hw: H, mem: M, max_download: usize) -> Self {
        Self {
            dcd: Dcd::new(hw, mem),
            speed: Speed::Full,
            ep0: Ep0::Idle,
            pending_address: None,
            bulk: Bulk::Off,
            max_download,
            downloaded: 0,
            name: [0; COMMAND_MAX],
            name_len: 0,
            bootloader: "LucinaMinerva first stage",
        }
    }

    pub fn start(&mut self) -> Result<(), Error> {
        self.dcd.start()
    }

    /// The name that came with [`Action::Flash`] or [`Action::GetVar`], or what came with [`Action::SetVar`].
    pub fn name(&self) -> &str {
        core::str::from_utf8(&self.name[..self.name_len]).unwrap_or("")
    }

    /// Answers the command, as `OKAY`, `FAIL` and so on with their text.
    pub fn reply(&mut self, text: &str) {
        let len = text.len().min(COMMAND_MAX);
        self.dcd.mem.write(REPLY_BUF, &text.as_bytes()[..len]);
        self.dcd.start_transfer(BULK_IN, REPLY_BUF, len, false);
    }

    fn keep_name(&mut self, name: &str) {
        self.name_len = name.len().min(COMMAND_MAX);
        self.name[..self.name_len].copy_from_slice(&name.as_bytes()[..self.name_len]);
    }

    fn receive_command(&mut self) {
        self.bulk = Bulk::Command;
        self.dcd.start_transfer(BULK_OUT, COMMAND_BUF, 512, false);
    }

    fn receive_data(&mut self, received: usize, expected: usize) {
        self.bulk = Bulk::Data { received, expected };
        let start = received.next_multiple_of(CHUNK);
        debug_assert_eq!(start, received);
        let len = (expected - received).min(CHUNK);
        // The controller wants whole packets for what it is told to expect.
        let packets = len.next_multiple_of(512);
        self.dcd.start_transfer(BULK_OUT, DATA_OFFSET + received, packets, false);
    }

    /// Handles what happened. At most one action a call, which the caller answers with [`Self::reply`].
    pub fn service(&mut self) -> Option<Action> {
        let events = self.dcd.poll();
        if events.bus_reset {
            self.ep0 = Ep0::Idle;
            self.pending_address = None;
            self.bulk = Bulk::Off;
        }
        if let Some(speed) = events.speed {
            self.speed = speed;
        }
        for ep in [EP0_IN, EP0_OUT] {
            if events.completed & completed_bit(ep) != 0 {
                let done = self.dcd.finish(ep);
                self.on_ep0_complete(done);
            }
        }
        if events.completed & completed_bit(BULK_IN) != 0 {
            let _ = self.dcd.finish(BULK_IN);
        }
        let mut action = None;
        if events.completed & completed_bit(BULK_OUT) != 0 {
            let done = self.dcd.finish(BULK_OUT);
            action = self.on_bulk_out(done);
        }
        if let Some(setup) = events.setup {
            self.on_setup(setup);
        }
        self.dcd.unmask_interrupts();
        action
    }

    fn on_bulk_out(&mut self, done: Done) -> Option<Action> {
        match self.bulk {
            Bulk::Off => None,
            Bulk::Data { received, expected } => {
                if done.failed {
                    self.reply("FAILdownload failure");
                    self.receive_command();
                    return Some(Action::Activity);
                }
                let received = received + done.len;
                if received >= expected {
                    self.downloaded = expected;
                    self.reply("OKAY");
                    self.receive_command();
                } else if !received.is_multiple_of(CHUNK) {
                    self.reply("FAILa short packet in the middle");
                    self.receive_command();
                } else {
                    self.receive_data(received, expected);
                }
                Some(Action::Activity)
            }
            Bulk::Command => {
                let mut raw = [0u8; COMMAND_MAX];
                let len = done.len.min(COMMAND_MAX);
                self.dcd.mem.read(COMMAND_BUF, &mut raw[..len]);
                self.receive_command();
                Some(self.on_command(&raw[..len]))
            }
        }
    }

    fn on_command(&mut self, raw: &[u8]) -> Action {
        match parse(raw) {
            Command::GetVar("version") => self.reply("OKAY0.4"),
            Command::GetVar("product") => self.reply("OKAYKindle"),
            Command::GetVar("bootloader") => {
                let mut text = [0u8; COMMAND_MAX];
                let len = (4 + self.bootloader.len()).min(COMMAND_MAX);
                text[..4].copy_from_slice(b"OKAY");
                text[4..len].copy_from_slice(&self.bootloader.as_bytes()[..len - 4]);
                self.reply(core::str::from_utf8(&text[..len]).unwrap_or("FAIL"));
            }
            Command::GetVar("max-download-size") => {
                let mut text = [0u8; 12];
                text[..4].copy_from_slice(b"OKAY");
                hex8(self.max_download, &mut text[4..]);
                let text = text;
                self.reply(core::str::from_utf8(&text).unwrap_or("FAIL"));
            }
            Command::GetVar(name) => {
                self.keep_name(name);
                return Action::GetVar;
            }
            Command::Download(len) if len == 0 || len > self.max_download => self.reply("FAILthat size"),
            Command::Download(len) => {
                let mut text = [0u8; 12];
                text[..4].copy_from_slice(b"DATA");
                hex8(len, &mut text[4..]);
                let text = text;
                self.reply(core::str::from_utf8(&text).unwrap_or("FAIL"));
                self.dcd.flush(BULK_OUT.bit());
                self.receive_data(0, len);
            }
            Command::Flash(name) => {
                self.keep_name(name);
                return Action::Flash;
            }
            Command::SetVar(rest) => {
                self.keep_name(rest);
                return Action::SetVar;
            }
            Command::Boot => return Action::Boot,
            Command::Reboot => return Action::Reboot,
            Command::Unknown => self.reply("FAILunknown command"),
        }
        Action::Activity
    }

    fn on_ep0_complete(&mut self, done: Done) {
        match self.ep0 {
            Ep0::DataIn if done.ep == EP0_IN => {
                self.ep0 = Ep0::StatusOut;
                self.dcd.start_transfer(EP0_OUT, buf::EP0_OUT, 0, false);
            }
            Ep0::StatusIn if done.ep == EP0_IN => {
                if let Some(address) = self.pending_address.take() {
                    self.dcd.set_address(address);
                }
                self.ep0 = Ep0::Idle;
            }
            Ep0::StatusOut if done.ep == EP0_OUT => self.ep0 = Ep0::Idle,
            _ => {}
        }
    }

    fn status_in(&mut self) {
        self.ep0 = Ep0::StatusIn;
        self.dcd.start_transfer(EP0_IN, buf::EP0_IN, 0, false);
    }

    fn data_in(&mut self, data: &[u8], requested: u16) {
        let len = data.len().min(requested as usize).min(buf::EP0_SIZE);
        self.dcd.mem.write(buf::EP0_IN, &data[..len]);
        let zlp = len < requested as usize && len.is_multiple_of(MPS_EP0);
        self.ep0 = Ep0::DataIn;
        self.dcd.start_transfer(EP0_IN, buf::EP0_IN, len, zlp);
    }

    fn stall(&mut self) {
        self.ep0 = Ep0::Idle;
        self.dcd.stall_ep0();
    }

    fn on_setup(&mut self, raw: [u8; 8]) {
        let (request_type, request) = (raw[0], raw[1]);
        let value = u16::from_le_bytes([raw[2], raw[3]]);
        let index = u16::from_le_bytes([raw[4], raw[5]]);
        let length = u16::from_le_bytes([raw[6], raw[7]]);
        self.dcd.flush_ep0();
        self.ep0 = Ep0::Idle;
        match (request_type, request) {
            (0x80, 6) => self.get_descriptor(value, length),
            (0x00, 5) => {
                self.pending_address = Some(value as u8);
                self.status_in();
            }
            (0x00, 9) => {
                self.dcd.disable_ep(BULK_IN);
                self.dcd.disable_ep(BULK_OUT);
                self.bulk = Bulk::Off;
                if value == 1 {
                    let packet = match self.speed {
                        Speed::High => 512,
                        Speed::Full => 64,
                    };
                    self.dcd.enable_ep(BULK_IN, EpType::Bulk, packet);
                    self.dcd.enable_ep(BULK_OUT, EpType::Bulk, packet);
                    self.receive_command();
                }
                self.status_in();
            }
            (0x80, 8) => {
                let configured = self.bulk != Bulk::Off;
                self.data_in(&[configured as u8], length);
            }
            (0x80, 0) => self.data_in(&[1, 0], length),
            (0x81, 0) => self.data_in(&[0, 0], length),
            (0x82, 0) => {
                let halted = self.dcd.is_halted(Ep::new(index as u8));
                self.data_in(&[halted as u8, 0], length);
            }
            (0x02, 1 | 3) if value == 0 => {
                self.dcd.set_halt(Ep::new(index as u8), request == 3);
                self.status_in();
            }
            (0x81, 10) => self.data_in(&[0], length),
            (0x01, 11) => self.status_in(),
            _ => self.stall(),
        }
    }

    fn get_descriptor(&mut self, value: u16, length: u16) {
        let [index, kind] = value.to_le_bytes();
        match kind {
            1 => self.data_in(&DEVICE, length),
            2 => {
                let descriptor = configuration(self.speed, false);
                self.data_in(&descriptor, length);
            }
            3 if index == 0 => self.data_in(&[4, 3, 0x09, 0x04], length),
            6 => self.data_in(&DEVICE_QUALIFIER, length),
            7 => {
                let other = match self.speed {
                    Speed::High => Speed::Full,
                    Speed::Full => Speed::High,
                };
                let descriptor = configuration(other, true);
                self.data_in(&descriptor, length);
            }
            _ => self.stall(),
        }
    }
}

fn hex8(value: usize, out: &mut [u8]) {
    for (i, byte) in out.iter_mut().take(8).enumerate() {
        let digit = (value >> ((7 - i) * 4)) & 0xf;
        *byte = b"0123456789abcdef"[digit];
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_are_what_they_say() {
        assert_eq!(parse(b"getvar:version"), Command::GetVar("version"));
        assert_eq!(parse(b"download:00d9d000"), Command::Download(0xd9_d000));
        assert_eq!(parse(b"flash:spare"), Command::Flash("spare"));
        assert_eq!(parse(b"boot"), Command::Boot);
        assert_eq!(parse(b"reboot\0\0"), Command::Reboot);
        assert_eq!(parse(b"setvar:bootmode main"), Command::SetVar("bootmode main"));
    }

    #[test]
    fn nonsense_is_unknown() {
        assert_eq!(parse(b"download:xyz"), Command::Unknown);
        assert_eq!(parse(&[0xff, 0xfe]), Command::Unknown);
        assert_eq!(parse(b"erase:main"), Command::Unknown);
        assert_eq!(parse(b"oem unlock"), Command::Unknown);
    }

    #[test]
    fn the_host_finds_it_as_fastboot_under_amazons_ids() {
        assert_eq!(&DEVICE[8..12], &[0x49, 0x19, 0xe0, 0xd0]);
        let config = configuration(Speed::High, false);
        assert_eq!(config[2] as usize, config.len());
        assert_eq!(&config[14..17], &[0xff, 0x42, 0x03]);
        // wMaxPacketSize of both endpoints.
        assert_eq!(&config[22..24], &[0x00, 0x02]);
        assert_eq!(&config[29..31], &[0x00, 0x02]);
        assert_eq!(&configuration(Speed::Full, false)[22..24], &[0x40, 0x00]);
    }

    #[test]
    fn hex_is_eight_digits() {
        let mut out = [0u8; 8];
        hex8(0x00d9_d00f, &mut out);
        assert_eq!(&out, b"00d9d00f");
    }
}
