//! The gadget: a CDC ECM network function and a CDC ACM serial function, with the descriptors of Linux's `g_cdc` on this
//! device, so that a host treats it as the same device as under the Linux kernel.
//!
//! Interface 0 and 1 are ECM (control, data), 2 and 3 are ACM (control, data). The ACM data path takes what the host
//! sends and drops it; it is here so that the device looks the same, and for a console later.

use alloc::vec::Vec;

use crate::{
    DmaMem, Error, Hw,
    dcd::{Dcd, Done, EP0_IN, EP0_OUT, Ep, EpType, Speed, buf, completed_bit},
};

const ECM_IN: Ep = Ep::new(0x81);
const ECM_OUT: Ep = Ep::new(0x01);
const ECM_NOTIFY: Ep = Ep::new(0x82);
const ACM_IN: Ep = Ep::new(0x83);
const ACM_OUT: Ep = Ep::new(0x02);
const ACM_NOTIFY: Ep = Ep::new(0x84);

const ECM_CONTROL_INTERFACE: u8 = 0;
const ECM_DATA_INTERFACE: u8 = 1;

const CONFIG_LEN: usize = 154;
const MPS_FULL: u32 = 64;
const MPS_HIGH: u32 = 512;
const MPS_EP0: usize = 64;

/// The most that an Ethernet frame without FCS takes here, also the `wMaxSegmentSize` of the descriptors.
pub const MAX_FRAME: usize = 1514;

/// The device descriptor.
pub const DEVICE: [u8; 18] = [
    0x12, 0x01, 0x00, 0x02, 0x02, 0x00, 0x00, 0x40, // USB 2.0, class CDC, ep0 64 bytes
    0x25, 0x05, 0xaa, 0xa4, 0x01,
    0x07, // 0525:a4aa "CDC Composite: ECM + ACM", bcdDevice 7.01
    0x01, 0x02, 0x00, 0x01, // manufacturer 1, product 2, no serial, one configuration
];

const DEVICE_QUALIFIER: [u8; 10] = [0x0a, 0x06, 0x00, 0x02, 0x02, 0x00, 0x00, 0x40, 0x01, 0x00];

/// The configuration descriptor at this speed. An "other speed" one differs in its type byte.
pub fn configuration(speed: Speed, other_speed: bool) -> [u8; CONFIG_LEN] {
    let (bulk_lo, bulk_hi, interval) = match speed {
        Speed::High => (0x00, 0x02, 0x09),
        Speed::Full => (0x40, 0x00, 0x20),
    };
    let kind = if other_speed { 0x07 } else { 0x02 };
    #[rustfmt::skip]
    let descriptor = [
        // Configuration: 154 bytes, 4 interfaces, value 1, self-powered, no power drawn.
        0x09, kind, 0x9a, 0x00, 0x04, 0x01, 0x00, 0xc0, 0x00,
        // ECM: association, control interface, Header, Union, Ethernet (MAC string 5, 1514 bytes), notification.
        0x08, 0x0b, 0x00, 0x02, 0x02, 0x06, 0x00, 0x07,
        0x09, 0x04, 0x00, 0x00, 0x01, 0x02, 0x06, 0x00, 0x04,
        0x05, 0x24, 0x00, 0x10, 0x01,
        0x05, 0x24, 0x06, 0x00, 0x01,
        0x0d, 0x24, 0x0f, 0x05, 0x00, 0x00, 0x00, 0x00, 0xea, 0x05, 0x00, 0x00, 0x00,
        0x07, 0x05, 0x82, 0x03, 0x10, 0x00, interval,
        // ECM data: alternate 0 has no endpoints, alternate 1 the bulk pair.
        0x09, 0x04, 0x01, 0x00, 0x00, 0x0a, 0x00, 0x00, 0x00,
        0x09, 0x04, 0x01, 0x01, 0x02, 0x0a, 0x00, 0x00, 0x06,
        0x07, 0x05, 0x81, 0x02, bulk_lo, bulk_hi, 0x00,
        0x07, 0x05, 0x01, 0x02, bulk_lo, bulk_hi, 0x00,
        // ACM: association, control interface, Header, Call Management, ACM, Union, notification.
        0x08, 0x0b, 0x02, 0x02, 0x02, 0x02, 0x01, 0x0a,
        0x09, 0x04, 0x02, 0x00, 0x01, 0x02, 0x02, 0x01, 0x08,
        0x05, 0x24, 0x00, 0x10, 0x01,
        0x05, 0x24, 0x01, 0x00, 0x03,
        0x04, 0x24, 0x02, 0x02,
        0x05, 0x24, 0x06, 0x02, 0x03,
        0x07, 0x05, 0x84, 0x03, 0x0a, 0x00, interval,
        // ACM data.
        0x09, 0x04, 0x03, 0x00, 0x02, 0x0a, 0x00, 0x00, 0x09,
        0x07, 0x05, 0x83, 0x02, bulk_lo, bulk_hi, 0x00,
        0x07, 0x05, 0x02, 0x02, bulk_lo, bulk_hi, 0x00,
    ];
    descriptor
}

/// The strings. Their numbers are those in the descriptors. The host's MAC address is number 5.
fn string(index: u8) -> Option<&'static str> {
    Some(match index {
        1 => "LucinaMinerva",
        2 => "CDC Composite Gadget",
        4 => "CDC Ethernet Control Model (ECM)",
        5 => "EE4900000000",
        6 => "CDC Ethernet Data",
        7 => "CDC ECM",
        8 => "CDC Abstract Control Model (ACM)",
        9 => "CDC ACM Data",
        10 => "CDC Serial",
        _ => return None,
    })
}

fn string_descriptor(text: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(2 + text.len() * 2);
    out.push(0);
    out.push(0x03);
    for unit in text.encode_utf16() {
        out.extend_from_slice(&unit.to_le_bytes());
    }
    out[0] = out.len() as u8;
    out
}

/// A parsed setup packet.
#[derive(Clone, Copy, Debug)]
struct Setup {
    request_type: u8,
    request: u8,
    value: u16,
    index: u16,
    length: u16,
}

impl Setup {
    fn parse(raw: [u8; 8]) -> Self {
        Self {
            request_type: raw[0],
            request: raw[1],
            value: u16::from_le_bytes([raw[2], raw[3]]),
            index: u16::from_le_bytes([raw[4], raw[5]]),
            length: u16::from_le_bytes([raw[6], raw[7]]),
        }
    }
}

/// What ep0 waits for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Ep0 {
    Idle,
    /// The data is going in; the host's status comes next.
    DataIn,
    /// The status is going in, and the address follows it if there is one.
    StatusIn,
    /// The host's status is coming out.
    StatusOut,
    /// The data of `SET_LINE_CODING` is coming out.
    LineCoding,
}

/// Where the ECM notifications are.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Notify {
    Idle,
    Connection,
    Speed,
}

/// The gadget. Run [`Gadget::service`] on every interrupt.
pub struct Gadget<H, M> {
    pub dcd: Dcd<H, M>,
    speed: Speed,
    configured: bool,
    ecm_alt: u8,
    ep0: Ep0,
    pending_address: Option<u8>,
    filter: u16,
    line_coding: [u8; 7],
    control_line_state: u16,
    notify: Notify,
    /// The length of a received frame in its buffer, until [`Gadget::take_frame`].
    rx_frame: Option<usize>,
    tx_busy: bool,
    /// Frames that arrived with an error, or longer than a frame can be.
    pub rx_errors: u32,
    /// Sends that ended with an error.
    pub tx_errors: u32,
}

impl<H: Hw, M: DmaMem> Gadget<H, M> {
    pub fn new(hw: H, mem: M) -> Self {
        Self {
            dcd: Dcd::new(hw, mem),
            speed: Speed::Full,
            configured: false,
            ecm_alt: 0,
            ep0: Ep0::Idle,
            pending_address: None,
            filter: 0,
            line_coding: [0; 7],
            control_line_state: 0,
            notify: Notify::Idle,
            rx_frame: None,
            tx_busy: false,
            rx_errors: 0,
            tx_errors: 0,
        }
    }

    /// Connects to the host.
    pub fn start(&mut self) -> Result<(), Error> {
        self.dcd.start()
    }

    /// Goes off the bus for a sleep: the host sees an unplug, and not a device that stopped answering.
    pub fn disconnect(&mut self) {
        self.dcd.disconnect();
        self.on_bus_reset();
    }

    /// Comes back on the bus after [`Self::disconnect`], from the beginning: the host enumerates it anew.
    pub fn reconnect(&mut self) -> Result<(), Error> {
        self.on_bus_reset();
        self.dcd.start()
    }

    /// Whether the host has opened the network function.
    pub fn is_online(&self) -> bool {
        self.configured && self.ecm_alt == 1
    }

    /// Handles what happened, and lets interrupts through again.
    pub fn service(&mut self) {
        let events = self.dcd.poll();
        if events.bus_reset {
            self.on_bus_reset();
        }
        if let Some(speed) = events.speed {
            self.speed = speed;
        }
        for (ep, handler) in [
            (EP0_IN, Self::on_ep0_complete as fn(&mut Self, Done)),
            (EP0_OUT, Self::on_ep0_complete),
            (ECM_OUT, Self::on_ecm_rx),
            (ECM_IN, Self::on_ecm_tx),
            (ECM_NOTIFY, Self::on_notify),
            (ACM_OUT, Self::on_acm_rx),
        ] {
            if events.completed & completed_bit(ep) != 0 {
                let done = self.dcd.finish(ep);
                handler(self, done);
            }
        }
        if let Some(setup) = events.setup {
            self.on_setup(Setup::parse(setup));
        }
        self.dcd.unmask_interrupts();
    }

    fn on_bus_reset(&mut self) {
        self.configured = false;
        self.ecm_alt = 0;
        self.ep0 = Ep0::Idle;
        self.pending_address = None;
        self.notify = Notify::Idle;
        self.rx_frame = None;
        self.tx_busy = false;
    }

    // --- ep0 ---

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
            Ep0::LineCoding if done.ep == EP0_OUT => {
                if done.len == self.line_coding.len() {
                    let mut coding = [0; 7];
                    self.dcd.mem.read(buf::EP0_OUT, &mut coding);
                    self.line_coding = coding;
                }
                self.status_in();
            }
            _ => {}
        }
    }

    fn status_in(&mut self) {
        self.ep0 = Ep0::StatusIn;
        self.dcd.start_transfer(EP0_IN, buf::EP0_IN, 0, false);
    }

    /// Sends the data stage of a control read, cut to what the host asked for.
    fn data_in(&mut self, data: &[u8], requested: u16) {
        let len = data.len().min(requested as usize).min(buf::EP0_SIZE);
        self.dcd.mem.write(buf::EP0_IN, &data[..len]);
        // A reply shorter than asked for ends with a short packet, which an exact multiple does not have.
        let zlp = len < requested as usize && len.is_multiple_of(MPS_EP0);
        self.ep0 = Ep0::DataIn;
        self.dcd.start_transfer(EP0_IN, buf::EP0_IN, len, zlp);
    }

    fn stall(&mut self) {
        self.ep0 = Ep0::Idle;
        self.dcd.stall_ep0();
    }

    fn on_setup(&mut self, setup: Setup) {
        self.dcd.flush_ep0();
        self.ep0 = Ep0::Idle;
        match (setup.request_type, setup.request) {
            (0x80, 6) => self.get_descriptor(setup),
            (0x00, 5) => {
                self.pending_address = Some(setup.value as u8);
                self.status_in();
            }
            (0x00, 9) => self.set_configuration(setup.value),
            (0x80, 8) => self.data_in(&[self.configured as u8], setup.length),
            // Self-powered.
            (0x80, 0) => self.data_in(&[1, 0], setup.length),
            (0x81, 0) => self.data_in(&[0, 0], setup.length),
            (0x82, 0) => {
                let halted = self.dcd.is_halted(Ep::new(setup.index as u8));
                self.data_in(&[halted as u8, 0], setup.length);
            }
            (0x02, 1 | 3) if setup.value == 0 => {
                self.dcd
                    .set_halt(Ep::new(setup.index as u8), setup.request == 3);
                self.status_in();
            }
            (0x81, 10) => match setup.index as u8 {
                ECM_DATA_INTERFACE => self.data_in(&[self.ecm_alt], setup.length),
                0 | 2 | 3 => self.data_in(&[0], setup.length),
                _ => self.stall(),
            },
            (0x01, 11) => self.set_interface(setup.index as u8, setup.value as u8),
            // SET_ETHERNET_PACKET_FILTER: stored, every frame goes through whatever it says, as in Linux.
            (0x21, 0x43) if setup.index == ECM_CONTROL_INTERFACE as u16 => {
                self.filter = setup.value;
                self.status_in();
            }
            (0x21, 0x20) if setup.length == 7 => {
                self.ep0 = Ep0::LineCoding;
                self.dcd.start_transfer(EP0_OUT, buf::EP0_OUT, 7, false);
            }
            (0xa1, 0x21) => {
                let coding = self.line_coding;
                self.data_in(&coding, setup.length);
            }
            (0x21, 0x22) => {
                self.control_line_state = setup.value;
                self.status_in();
            }
            (0x21, 0x23) => self.status_in(),
            _ => self.stall(),
        }
    }

    fn get_descriptor(&mut self, setup: Setup) {
        let [index, kind] = setup.value.to_le_bytes();
        match kind {
            1 => self.data_in(&DEVICE, setup.length),
            2 => {
                let descriptor = configuration(self.speed, false);
                self.data_in(&descriptor, setup.length);
            }
            3 if index == 0 => self.data_in(&[4, 3, 0x09, 0x04], setup.length),
            3 => match string(index) {
                Some(text) => {
                    let descriptor = string_descriptor(text);
                    self.data_in(&descriptor, setup.length);
                }
                None => self.stall(),
            },
            6 => self.data_in(&DEVICE_QUALIFIER, setup.length),
            7 => {
                let other = match self.speed {
                    Speed::High => Speed::Full,
                    Speed::Full => Speed::High,
                };
                let descriptor = configuration(other, true);
                self.data_in(&descriptor, setup.length);
            }
            _ => self.stall(),
        }
    }

    fn bulk_packet(&self) -> u32 {
        match self.speed {
            Speed::High => MPS_HIGH,
            Speed::Full => MPS_FULL,
        }
    }

    fn set_configuration(&mut self, value: u16) {
        match value {
            0 => {
                self.deconfigure();
                self.status_in();
            }
            1 => {
                self.deconfigure();
                self.configured = true;
                let packet = self.bulk_packet();
                self.dcd.enable_ep(ECM_NOTIFY, EpType::Interrupt, 16);
                self.dcd.enable_ep(ACM_NOTIFY, EpType::Interrupt, 10);
                self.dcd.enable_ep(ACM_IN, EpType::Bulk, packet);
                self.dcd.enable_ep(ACM_OUT, EpType::Bulk, packet);
                self.prime_acm_rx();
                self.status_in();
            }
            _ => self.stall(),
        }
    }

    fn deconfigure(&mut self) {
        self.set_ecm_data(false);
        self.notify = Notify::Idle;
        self.configured = false;
        for ep in [ECM_NOTIFY, ACM_NOTIFY, ACM_IN, ACM_OUT] {
            self.dcd.disable_ep(ep);
        }
    }

    fn set_interface(&mut self, interface: u8, alternate: u8) {
        match (interface, alternate) {
            (ECM_DATA_INTERFACE, 0 | 1) if self.configured => {
                self.set_ecm_data(alternate == 1);
                self.ecm_alt = alternate;
                self.status_in();
                // The host learns the link state after each choice of alternate setting. One it has not read yet
                // goes, as a transfer cannot be replaced while the endpoint is primed.
                self.dcd.flush(ECM_NOTIFY.bit());
                self.notify = Notify::Idle;
                self.send_notification();
            }
            (0 | 2 | 3, 0) if self.configured => self.status_in(),
            _ => self.stall(),
        }
    }

    fn set_ecm_data(&mut self, on: bool) {
        self.rx_frame = None;
        self.tx_busy = false;
        if on {
            let packet = self.bulk_packet();
            self.dcd.enable_ep(ECM_IN, EpType::Bulk, packet);
            self.dcd.enable_ep(ECM_OUT, EpType::Bulk, packet);
            self.prime_ecm_rx();
        } else {
            self.dcd.disable_ep(ECM_IN);
            self.dcd.disable_ep(ECM_OUT);
            self.ecm_alt = 0;
        }
    }

    // --- Notifications ---

    /// `NETWORK_CONNECTION` (up), then, when that one is through, `SPEED_CHANGE`.
    fn send_notification(&mut self) {
        let (packet, len, next): ([u8; 16], usize, Notify) = match self.notify {
            Notify::Idle => {
                let mut packet = [0; 16];
                packet[..8].copy_from_slice(&[
                    0xa1,
                    0x00,
                    0x01,
                    0x00,
                    ECM_CONTROL_INTERFACE,
                    0,
                    0,
                    0,
                ]);
                (packet, 8, Notify::Connection)
            }
            Notify::Connection => {
                // What Linux reports: 13 packets of 512 bytes per microframe, or 19 of 64 per frame.
                let bits: u32 = match self.speed {
                    Speed::High => 13 * 512 * 8 * 1000 * 8,
                    Speed::Full => 19 * 64 * 1000 * 8,
                };
                let mut packet = [0; 16];
                packet[..8].copy_from_slice(&[0xa1, 0x2a, 0, 0, ECM_CONTROL_INTERFACE, 0, 8, 0]);
                packet[8..12].copy_from_slice(&bits.to_le_bytes());
                packet[12..16].copy_from_slice(&bits.to_le_bytes());
                (packet, 16, Notify::Speed)
            }
            Notify::Speed => return,
        };
        self.dcd.mem.write(buf::NOTIFY, &packet[..len]);
        self.notify = next;
        self.dcd.start_transfer(ECM_NOTIFY, buf::NOTIFY, len, false);
    }

    fn on_notify(&mut self, _done: Done) {
        if self.notify == Notify::Speed {
            self.notify = Notify::Idle;
        } else {
            self.send_notification();
        }
    }

    // --- The data path ---

    fn prime_ecm_rx(&mut self) {
        self.dcd
            .start_transfer(ECM_OUT, buf::ECM_RX, buf::ECM_SIZE, false);
    }

    fn prime_acm_rx(&mut self) {
        self.dcd
            .start_transfer(ACM_OUT, buf::ACM_RX, buf::ACM_SIZE, false);
    }

    fn on_ecm_rx(&mut self, done: Done) {
        if !self.is_online() {
            return;
        }
        if done.failed || done.len > MAX_FRAME {
            self.rx_errors += 1;
            self.prime_ecm_rx();
        } else if done.len == 0 {
            // A zero-length packet between frames.
            self.prime_ecm_rx();
        } else {
            self.rx_frame = Some(done.len);
        }
    }

    fn on_ecm_tx(&mut self, done: Done) {
        self.tx_busy = false;
        if done.failed {
            self.tx_errors += 1;
        }
    }

    fn on_acm_rx(&mut self, _done: Done) {
        if self.configured {
            self.prime_acm_rx();
        }
    }

    /// Whether a frame is waiting for [`Gadget::take_frame`].
    pub fn has_frame(&self) -> bool {
        self.rx_frame.is_some()
    }

    /// Takes the frame that arrived, if one did, and gets ready for the next one. `out` must hold [`MAX_FRAME`] bytes.
    pub fn take_frame(&mut self, out: &mut [u8]) -> Option<usize> {
        let len = self.rx_frame.take()?;
        self.dcd.mem.read(buf::ECM_RX, &mut out[..len]);
        self.prime_ecm_rx();
        Some(len)
    }

    /// Whether [`Gadget::send_frame`] would take a frame now.
    pub fn can_send(&self) -> bool {
        self.is_online() && !self.tx_busy
    }

    /// Sends a frame, if it can. A length that is an exact multiple of the packet size gets a zero-length packet after
    /// it, which ends the frame on the host's side.
    pub fn send_frame(&mut self, frame: &[u8]) -> bool {
        if !self.can_send() || frame.len() > MAX_FRAME || frame.is_empty() {
            return false;
        }
        self.dcd.mem.write(buf::ECM_TX, frame);
        let zlp = (frame.len() as u32).is_multiple_of(self.bulk_packet());
        self.tx_busy = true;
        self.dcd
            .start_transfer(ECM_IN, buf::ECM_TX, frame.len(), zlp);
        true
    }
}

#[cfg(test)]
mod tests {
    use alloc::vec;

    use super::*;
    use crate::{
        dcd::{DMA_SIZE, buf},
        model::{Model, ModelMem},
    };

    /// The configuration descriptor as `g_cdc` on the Linux kernel of this device gives it at high speed.
    #[rustfmt::skip]
    const LINUX_HIGH_SPEED: [u8; 154] = [
        0x09,0x02,0x9a,0x00,0x04,0x01,0x00,0xc0,0x00,
        0x08,0x0b,0x00,0x02,0x02,0x06,0x00,0x07,
        0x09,0x04,0x00,0x00,0x01,0x02,0x06,0x00,0x04,
        0x05,0x24,0x00,0x10,0x01, 0x05,0x24,0x06,0x00,0x01,
        0x0d,0x24,0x0f,0x05,0x00,0x00,0x00,0x00,0xea,0x05,0x00,0x00,0x00,
        0x07,0x05,0x82,0x03,0x10,0x00,0x09,
        0x09,0x04,0x01,0x00,0x00,0x0a,0x00,0x00,0x00,
        0x09,0x04,0x01,0x01,0x02,0x0a,0x00,0x00,0x06,
        0x07,0x05,0x81,0x02,0x00,0x02,0x00, 0x07,0x05,0x01,0x02,0x00,0x02,0x00,
        0x08,0x0b,0x02,0x02,0x02,0x02,0x01,0x0a,
        0x09,0x04,0x02,0x00,0x01,0x02,0x02,0x01,0x08,
        0x05,0x24,0x00,0x10,0x01, 0x05,0x24,0x01,0x00,0x03, 0x04,0x24,0x02,0x02, 0x05,0x24,0x06,0x02,0x03,
        0x07,0x05,0x84,0x03,0x0a,0x00,0x09,
        0x09,0x04,0x03,0x00,0x02,0x0a,0x00,0x00,0x09,
        0x07,0x05,0x83,0x02,0x00,0x02,0x00, 0x07,0x05,0x02,0x02,0x00,0x02,0x00,
    ];

    #[test]
    fn the_descriptors_are_those_of_linuxs_gadget() {
        assert_eq!(configuration(Speed::High, false), LINUX_HIGH_SPEED);
        assert_eq!(
            DEVICE,
            [
                0x12, 0x01, 0x00, 0x02, 0x02, 0x00, 0x00, 0x40, 0x25, 0x05, 0xaa, 0xa4, 0x01, 0x07,
                0x01, 0x02, 0x00, 0x01
            ]
        );
        assert_eq!(string(5), Some("EE4900000000"));
    }

    #[test]
    fn at_full_speed_only_the_endpoint_sizes_and_intervals_differ() {
        let high = configuration(Speed::High, false);
        let full = configuration(Speed::Full, false);
        let differing = high.iter().zip(&full).filter(|(a, b)| a != b).count();
        // Four bulk endpoints with two size bytes each, two notification intervals.
        assert_eq!(differing, 10);
        assert_eq!(configuration(Speed::Full, true)[1], 7);
    }

    #[test]
    fn every_descriptor_in_the_configuration_has_a_length_that_walks_to_the_end() {
        let descriptor = configuration(Speed::High, false);
        let mut at = 0;
        let mut interfaces = 0;
        while at < descriptor.len() {
            assert_ne!(descriptor[at], 0);
            if descriptor[at + 1] == 4 {
                interfaces += 1;
            }
            at += descriptor[at] as usize;
        }
        assert_eq!(at, 154);
        assert_eq!(u16::from_le_bytes([descriptor[2], descriptor[3]]), 154);
        // Alternate settings count as interface descriptors but not in bNumInterfaces.
        assert_eq!(interfaces, descriptor[4] + 1);
    }

    fn gadget() -> Gadget<Model, ModelMem> {
        let model = Model::new();
        let mem = model.mem(DMA_SIZE);
        let mut gadget = Gadget::new(model, mem);
        gadget.start().unwrap();
        gadget
    }

    /// Runs a control read and returns what the device sent.
    fn control_in(gadget: &mut Gadget<Model, ModelMem>, setup: [u8; 8]) -> Vec<u8> {
        gadget.dcd.hw.host_setup(&mut gadget.dcd.mem, setup);
        gadget.service();
        let data = gadget.dcd.hw.host_in(&mut gadget.dcd.mem, EP0_IN);
        gadget.service();
        // The host's status stage.
        gadget.dcd.hw.host_out(&mut gadget.dcd.mem, EP0_OUT, &[]);
        gadget.service();
        data
    }

    /// Runs a control request without data; the status stage goes in.
    fn control_no_data(gadget: &mut Gadget<Model, ModelMem>, setup: [u8; 8]) {
        gadget.dcd.hw.host_setup(&mut gadget.dcd.mem, setup);
        gadget.service();
        assert_eq!(gadget.dcd.hw.host_in(&mut gadget.dcd.mem, EP0_IN), vec![]);
        gadget.service();
    }

    fn enumerate(gadget: &mut Gadget<Model, ModelMem>) {
        gadget.dcd.hw.host_bus_reset(true);
        gadget.service();
        assert_eq!(
            control_in(gadget, [0x80, 6, 0, 1, 0, 0, 64, 0]),
            DEVICE[..18]
        );
        control_no_data(gadget, [0x00, 5, 7, 0, 0, 0, 0, 0]);
        assert_eq!(gadget.dcd.hw.address(), 7);
        assert_eq!(
            control_in(gadget, [0x80, 6, 0, 2, 0, 0, 0xff, 0]),
            LINUX_HIGH_SPEED
        );
        control_no_data(gadget, [0x00, 9, 1, 0, 0, 0, 0, 0]);
        control_no_data(gadget, [0x01, 11, 1, 0, 1, 0, 0, 0]);
        assert!(gadget.is_online());
    }

    #[test]
    fn a_host_enumerates_it_and_the_address_comes_after_the_status_stage() {
        let mut gadget = gadget();
        gadget.dcd.hw.host_bus_reset(true);
        gadget.service();
        gadget
            .dcd
            .hw
            .host_setup(&mut gadget.dcd.mem, [0x00, 5, 9, 0, 0, 0, 0, 0]);
        gadget.service();
        assert_eq!(gadget.dcd.hw.address(), 0);
        gadget.dcd.hw.host_in(&mut gadget.dcd.mem, EP0_IN);
        gadget.service();
        assert_eq!(gadget.dcd.hw.address(), 9);
    }

    #[test]
    fn descriptors_are_cut_to_what_the_host_asks_for() {
        let mut gadget = gadget();
        gadget.dcd.hw.host_bus_reset(true);
        gadget.service();
        assert_eq!(
            control_in(&mut gadget, [0x80, 6, 0, 2, 0, 0, 9, 0]),
            LINUX_HIGH_SPEED[..9]
        );
        assert_eq!(
            control_in(&mut gadget, [0x80, 6, 0, 1, 0, 0, 8, 0]),
            DEVICE[..8]
        );
    }

    #[test]
    fn a_reply_of_whole_packets_shorter_than_asked_for_ends_with_a_zero_length_packet() {
        let mut gadget = gadget();
        gadget.dcd.hw.host_bus_reset(true);
        gadget.service();
        gadget
            .dcd
            .hw
            .host_setup(&mut gadget.dcd.mem, [0x80, 6, 0, 2, 0, 0, 0xff, 0]);
        gadget.service();
        // 154 bytes is not a multiple of 64.
        assert!(!gadget.dcd.hw.has_second_td(&gadget.dcd.mem, EP0_IN));
        gadget.dcd.hw.host_in(&mut gadget.dcd.mem, EP0_IN);
        gadget.service();
        gadget.dcd.hw.host_out(&mut gadget.dcd.mem, EP0_OUT, &[]);
        gadget.service();
        gadget.data_in(&[0; 64], 255);
        assert!(gadget.dcd.hw.has_second_td(&gadget.dcd.mem, EP0_IN));
    }

    #[test]
    fn unknown_requests_stall() {
        let mut gadget = gadget();
        gadget.dcd.hw.host_bus_reset(true);
        gadget.service();
        gadget
            .dcd
            .hw
            .host_setup(&mut gadget.dcd.mem, [0x21, 0x40, 0, 0, 0, 0, 0, 0]);
        gadget.service();
        assert!(gadget.dcd.hw.ep0_stalled());
        // Vendor strings out of range too.
        gadget
            .dcd
            .hw
            .host_setup(&mut gadget.dcd.mem, [0x80, 6, 99, 3, 9, 4, 64, 0]);
        gadget.service();
        assert!(gadget.dcd.hw.ep0_stalled());
    }

    #[test]
    fn the_mac_string_and_language_are_served() {
        let mut gadget = gadget();
        gadget.dcd.hw.host_bus_reset(true);
        gadget.service();
        assert_eq!(
            control_in(&mut gadget, [0x80, 6, 0, 3, 0, 0, 255, 0]),
            [4, 3, 0x09, 0x04]
        );
        let mac = control_in(&mut gadget, [0x80, 6, 5, 3, 9, 4, 255, 0]);
        assert_eq!(mac[0], 26);
        assert_eq!(mac[1], 3);
        let text: Vec<u8> = mac[2..].iter().step_by(2).copied().collect();
        assert_eq!(text, b"EE4900000000");
    }

    #[test]
    fn opening_the_data_interface_sends_connection_then_speed() {
        let mut gadget = gadget();
        enumerate(&mut gadget);
        let connection = gadget.dcd.hw.host_in(&mut gadget.dcd.mem, ECM_NOTIFY);
        assert_eq!(connection, [0xa1, 0x00, 0x01, 0x00, 0, 0, 0, 0]);
        gadget.service();
        let speed = gadget.dcd.hw.host_in(&mut gadget.dcd.mem, ECM_NOTIFY);
        let mut expected = vec![0xa1, 0x2a, 0, 0, 0, 0, 8, 0];
        expected.extend_from_slice(&425_984_000u32.to_le_bytes());
        expected.extend_from_slice(&425_984_000u32.to_le_bytes());
        assert_eq!(speed, expected);
    }

    #[test]
    fn choosing_the_alternate_setting_again_replaces_an_unread_notification() {
        let mut gadget = gadget();
        enumerate(&mut gadget);
        // The first notification is still on the endpoint, unread.
        assert!(gadget.dcd.hw.is_primed(ECM_NOTIFY));
        let flushes = gadget.dcd.hw.flushes_of(ECM_NOTIFY.bit());
        gadget
            .dcd
            .hw
            .host_setup(&mut gadget.dcd.mem, [0x01, 11, 1, 0, 1, 0, 0, 0]);
        gadget.service();
        assert_eq!(gadget.dcd.hw.flushes_of(ECM_NOTIFY.bit()), flushes + 1);
        assert_eq!(
            gadget.dcd.hw.host_in(&mut gadget.dcd.mem, ECM_NOTIFY),
            [0xa1, 0x00, 0x01, 0x00, 0, 0, 0, 0]
        );
    }

    #[test]
    fn frames_come_in_and_the_next_receive_starts_when_one_is_taken() {
        let mut gadget = gadget();
        enumerate(&mut gadget);
        assert!(!gadget.has_frame());
        gadget
            .dcd
            .hw
            .host_out(&mut gadget.dcd.mem, ECM_OUT, &[0xab; 60]);
        gadget.service();
        assert!(gadget.has_frame());
        // Not primed again until the frame is taken, so that it cannot be overwritten.
        assert!(!gadget.dcd.hw.is_primed(ECM_OUT));
        let mut out = [0; MAX_FRAME];
        assert_eq!(gadget.take_frame(&mut out), Some(60));
        assert_eq!(out[..60], [0xab; 60]);
        assert!(gadget.dcd.hw.is_primed(ECM_OUT));
        assert_eq!(gadget.take_frame(&mut out), None);
    }

    #[test]
    fn frames_go_out_one_at_a_time() {
        let mut gadget = gadget();
        enumerate(&mut gadget);
        assert!(gadget.send_frame(&[0x55; 100]));
        assert!(!gadget.can_send());
        assert!(!gadget.send_frame(&[0x66; 100]));
        assert_eq!(
            gadget.dcd.hw.host_in(&mut gadget.dcd.mem, ECM_IN),
            vec![0x55; 100]
        );
        gadget.service();
        assert!(gadget.can_send());
    }

    #[test]
    fn a_frame_of_whole_packets_gets_a_zero_length_packet() {
        let mut gadget = gadget();
        enumerate(&mut gadget);
        assert!(gadget.send_frame(&[1; 1024]));
        assert!(gadget.dcd.hw.has_second_td(&gadget.dcd.mem, ECM_IN));
        assert_eq!(
            gadget.dcd.hw.host_in(&mut gadget.dcd.mem, ECM_IN).len(),
            1024
        );
        gadget.service();
        assert!(gadget.send_frame(&[1; 1000]));
        assert!(!gadget.dcd.hw.has_second_td(&gadget.dcd.mem, ECM_IN));
    }

    #[test]
    fn nothing_flows_until_the_host_selects_the_alternate_setting() {
        let mut gadget = gadget();
        gadget.dcd.hw.host_bus_reset(true);
        gadget.service();
        assert!(!gadget.send_frame(&[1; 60]));
        control_no_data(&mut gadget, [0x00, 9, 1, 0, 0, 0, 0, 0]);
        assert!(!gadget.send_frame(&[1; 60]));
        control_no_data(&mut gadget, [0x01, 11, 1, 0, 1, 0, 0, 0]);
        assert!(gadget.send_frame(&[1; 60]));
        control_no_data(&mut gadget, [0x01, 11, 0, 0, 1, 0, 0, 0]);
        assert!(!gadget.is_online());
        assert!(!gadget.send_frame(&[1; 60]));
    }

    #[test]
    fn a_bus_reset_takes_it_offline_and_it_can_enumerate_again() {
        let mut gadget = gadget();
        enumerate(&mut gadget);
        gadget.dcd.hw.host_bus_reset(true);
        gadget.service();
        assert!(!gadget.is_online());
        assert_eq!(gadget.dcd.hw.address(), 0);
        enumerate(&mut gadget);
    }

    #[test]
    fn at_full_speed_the_endpoints_are_configured_for_it() {
        let mut gadget = gadget();
        gadget.dcd.hw.host_bus_reset(false);
        gadget.service();
        assert_eq!(
            control_in(&mut gadget, [0x80, 6, 0, 2, 0, 0, 0xff, 0]),
            configuration(Speed::Full, false)
        );
        control_no_data(&mut gadget, [0x00, 9, 1, 0, 0, 0, 0, 0]);
        control_no_data(&mut gadget, [0x01, 11, 1, 0, 1, 0, 0, 0]);
        assert_eq!(gadget.dcd.hw.qh_max_packet(&gadget.dcd.mem, ECM_IN), 64);
        assert!(gadget.send_frame(&[1; 128]));
        assert!(gadget.dcd.hw.has_second_td(&gadget.dcd.mem, ECM_IN));
    }

    #[test]
    fn a_new_setup_packet_during_the_copy_is_read_again() {
        let mut gadget = gadget();
        gadget.dcd.hw.host_bus_reset(true);
        gadget.service();
        gadget
            .dcd
            .hw
            .host_setup(&mut gadget.dcd.mem, [0x80, 6, 0, 3, 0, 0, 255, 0]);
        gadget.dcd.hw.trip_once = Some([0x80, 8, 0, 0, 0, 0, 1, 0]);
        gadget.service();
        // The second packet, GET_CONFIGURATION, is the one answered.
        assert_eq!(gadget.dcd.hw.host_in(&mut gadget.dcd.mem, EP0_IN), [0]);
    }

    #[test]
    fn status_bits_are_only_acknowledged_when_set() {
        let mut gadget = gadget();
        gadget.dcd.hw.host_bus_reset(true);
        gadget.service();
        assert_eq!(
            gadget.dcd.hw.status_acks.last().copied(),
            Some(gadget.dcd.hw.last_status)
        );
    }

    #[test]
    fn the_line_coding_goes_in_and_comes_back() {
        let mut gadget = gadget();
        gadget.dcd.hw.host_bus_reset(true);
        gadget.service();
        let coding = [0x00, 0xc2, 0x01, 0x00, 0x00, 0x00, 0x08];
        gadget
            .dcd
            .hw
            .host_setup(&mut gadget.dcd.mem, [0x21, 0x20, 0, 0, 2, 0, 7, 0]);
        gadget.service();
        gadget
            .dcd
            .hw
            .host_out(&mut gadget.dcd.mem, EP0_OUT, &coding);
        gadget.service();
        assert_eq!(gadget.dcd.hw.host_in(&mut gadget.dcd.mem, EP0_IN), vec![]);
        gadget.service();
        assert_eq!(
            control_in(&mut gadget, [0xa1, 0x21, 0, 0, 2, 0, 7, 0]),
            coding
        );
    }

    #[test]
    fn the_queue_heads_are_where_the_controller_is_told() {
        let gadget = gadget();
        assert_eq!(gadget.dcd.hw.list_address(), gadget.dcd.mem.paddr());
        assert_eq!(gadget.dcd.mem.paddr() & 0x7ff, 0);
        assert_eq!(buf::QH, 0);
    }
}
