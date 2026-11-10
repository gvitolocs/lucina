//! The panel's touch controller: a Cypress TrueTouch Gen4 (cyttsp4) on I2C2, running the firmware in its own flash.
//!
//! The procedure is the one both Linux kernels on this device use, with what only serves diagnostics left out. Reset, wait
//! for the bootloader's heartbeat, tell it to start the application, read the sizes from the system-information mode,
//! change to the operational mode. After that every interrupt is one frame, and the chip sends no other until the host has
//! acknowledged it by flipping bit 7 of register 0. A reset is always first, as the kernel before may have left the chip in
//! the middle of that exchange.
//!
//! Coordinates stay as the chip gives them, as under the Linux on this device: user space flips X (`touch_invert_x`).

use alloc::{vec, vec::Vec};

use crate::Error;

/// The chip and its two lines.
pub trait TouchBus {
    /// Reads registers from `reg` on, in one transaction.
    fn read(&mut self, reg: u8, out: &mut [u8]) -> Result<(), Error>;
    /// Writes registers from `reg` on, in one transaction.
    fn write(&mut self, reg: u8, data: &[u8]) -> Result<(), Error>;
    /// Drives the reset line, which is active low.
    fn set_reset(&mut self, high: bool);
    /// Whether the interrupt line has fallen since the last call.
    fn take_edge(&mut self) -> bool;
    /// Waits at least this long.
    fn delay_ms(&mut self, ms: u32);
    /// Pulls the interrupt line low for a moment, which is what wakes a sleeping chip: a read does not.
    fn pulse_interrupt(&mut self) {}
}

/// The 7-bit address on I2C2.
pub const ADDR: u8 = 0x24;

const REG_MODE: u8 = 0;
const MODE_SLEEP: u8 = 0x02;
const SLEEP_SETTLE_MS: u32 = 50;
const WAKE_POLL_MS: u32 = 10;
const WAKE_POLLS: u32 = 50;
const MODE_TOGGLE: u8 = 0x80;
const MODE_MASK: u8 = 0x70;
const MODE_OPERATIONAL: u8 = 0x00;
const MODE_SYSINFO: u8 = 0x10;
const MODE_CHANGE: u8 = 0x08;
const BL_LAUNCH: [u8; 8] = [0xff, 0x01, 0x3b, 0x00, 0x00, 0x4f, 0x6d, 0x17];
const HEARTBEAT_TIMEOUT_MS: u32 = 500;
const MODE_TIMEOUT_MS: u32 = 1000;
const STARTUP_TRIES: u32 = 4;
const REP_STAT_BAD: u8 = 0x20;
const NUM_TOUCHES_MASK: u8 = 0x1f;
const EVENT_LIFT_OFF: u32 = 3;
/// The slots user space and the protocol have room for.
pub const MAX_SLOTS: usize = 32;

/// One field of a touch record.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Field {
    offset: usize,
    shift: u32,
    bytes: usize,
    max: u32,
}

impl Field {
    fn parse(loc: u8, size_bits: u8) -> Self {
        Self {
            offset: usize::from(loc & 0x1f),
            shift: u32::from(loc >> 5),
            bytes: usize::from(size_bits).div_ceil(8),
            max: if size_bits >= 32 {
                u32::MAX
            } else {
                1 << size_bits
            },
        }
    }

    fn get(&self, record: &[u8]) -> u32 {
        let mut value = 0u32;
        for index in 0..self.bytes {
            let byte = record.get(self.offset + index).copied().unwrap_or(0);
            value = value
                .wrapping_mul(256)
                .wrapping_add(u32::from(byte >> self.shift));
        }
        value & self.max.wrapping_sub(1)
    }
}

/// What the chip says about itself. Nothing here is fixed for the panel: it all comes from the chip.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Geometry {
    pub res_x: i32,
    pub res_y: i32,
    pub max_pressure: i32,
    pub slots: usize,
    rep_ofs: usize,
    tt_stat_ofs: usize,
    record_size: usize,
    max_touches: usize,
    x: Field,
    y: Field,
    pressure: Field,
    id: Field,
    event: Field,
    major: Field,
    minor: Field,
}

/// A finger on the panel, in the chip's coordinates.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Contact {
    pub slot: usize,
    pub x: i32,
    pub y: i32,
    pub pressure: i32,
    pub major: i32,
    pub minor: i32,
}

/// The controller.
pub struct Touch<B> {
    bus: B,
    geometry: Geometry,
}

fn be16(bytes: &[u8]) -> usize {
    (usize::from(bytes[0]) << 8) | usize::from(bytes[1])
}

impl<B: TouchBus> Touch<B> {
    /// Brings up the chip, trying again from the reset a few times, as a chip can miss one.
    pub fn start(bus: B) -> Result<Self, (B, Error)> {
        let mut touch = Self {
            bus,
            geometry: Geometry::default(),
        };
        let mut last = Error::TouchTimeout;
        for _ in 0..STARTUP_TRIES {
            match touch.startup() {
                Ok(()) => return Ok(touch),
                Err(err) => last = err,
            }
        }
        Err((touch.bus, last))
    }

    /// Puts the chip to sleep, as Linux's `cyttsp4_core` does: worth about 4 mA on this board. Its rails stay on.
    pub fn sleep(&mut self) -> Result<(), Error> {
        let mut mode = [0u8; 1];
        self.bus.read(REG_MODE, &mut mode)?;
        self.bus.write(REG_MODE, &[mode[0] | MODE_SLEEP])?;
        self.bus.delay_ms(SLEEP_SETTLE_MS);
        Ok(())
    }

    /// Wakes the chip, and waits for it to say so. If it does not, the caller starts it again.
    pub fn wake(&mut self) -> Result<(), Error> {
        let _ = self.bus.take_edge();
        self.bus.pulse_interrupt();
        for _ in 0..WAKE_POLLS {
            self.bus.delay_ms(WAKE_POLL_MS);
            if self.bus.take_edge() {
                return Ok(());
            }
        }
        Err(Error::TouchTimeout)
    }

    pub fn geometry(&self) -> &Geometry {
        &self.geometry
    }

    fn startup(&mut self) -> Result<(), Error> {
        self.bus.set_reset(true);
        self.bus.delay_ms(20);
        self.bus.set_reset(false);
        self.bus.delay_ms(40);
        let _ = self.bus.take_edge();
        self.bus.set_reset(true);
        self.bus.delay_ms(20);

        self.wait_edge(HEARTBEAT_TIMEOUT_MS, |head| {
            head[0] & 1 != 0 || head[1] != 0
        })?;
        self.bus.write(REG_MODE, &BL_LAUNCH)?;
        let head = self.wait_edge(MODE_TIMEOUT_MS, |head| {
            head[0] & 1 == 0
                && head[1] == 0
                && head[0] & MODE_CHANGE == 0
                && head[0] & MODE_MASK == MODE_SYSINFO
        })?;
        self.acknowledge(head)?;

        self.geometry = self.read_sysinfo()?;
        // The sizes were read in this mode's frame: that one is acknowledged too.
        let mut mode = [0u8; 1];
        self.bus.read(REG_MODE, &mut mode)?;
        self.acknowledge(mode[0])?;

        self.bus.read(REG_MODE, &mut mode)?;
        self.bus.write(
            REG_MODE,
            &[(mode[0] & !MODE_MASK) | MODE_OPERATIONAL | MODE_CHANGE],
        )?;
        let head = self.wait_edge(MODE_TIMEOUT_MS, |head| {
            head[0] & MODE_CHANGE == 0 && head[0] & MODE_MASK == MODE_OPERATIONAL
        })?;
        self.acknowledge(head)
    }

    /// Waits for an interrupt whose first bytes satisfy `done`, and returns the mode byte.
    fn wait_edge(&mut self, timeout_ms: u32, done: impl Fn(&[u8; 3]) -> bool) -> Result<u8, Error> {
        for _ in 0..timeout_ms {
            if self.bus.take_edge() {
                let mut head = [0u8; 3];
                self.bus.read(REG_MODE, &mut head)?;
                if done(&head) {
                    return Ok(head[0]);
                }
            }
            self.bus.delay_ms(1);
        }
        Err(Error::TouchTimeout)
    }

    fn acknowledge(&mut self, mode: u8) -> Result<(), Error> {
        if mode & MODE_CHANGE != 0 {
            return Ok(());
        }
        self.bus.write(REG_MODE, &[mode ^ MODE_TOGGLE])
    }

    fn read_sysinfo(&mut self) -> Result<Geometry, Error> {
        let mut map = [0u8; 16];
        self.bus.read(REG_MODE, &mut map)?;
        let pcfg_ofs = be16(&map[8..]);
        let opcfg_ofs = be16(&map[10..]);
        let ddata_ofs = be16(&map[12..]);
        if opcfg_ofs < pcfg_ofs + 13 || ddata_ofs < opcfg_ofs + 25 || ddata_ofs > 0xff {
            return Err(Error::TouchBadSysinfo);
        }

        let mut pcfg = [0u8; 13];
        self.bus.read(pcfg_ofs as u8, &mut pcfg)?;
        let res_x = (usize::from(pcfg[6] & 0x7f) << 8) | usize::from(pcfg[7]);
        let res_y = (usize::from(pcfg[8] & 0x7f) << 8) | usize::from(pcfg[9]);
        let max_pressure = be16(&pcfg[10..]);

        let mut opcfg = vec![0u8; (ddata_ofs - opcfg_ofs).min(64)];
        self.bus.read(opcfg_ofs as u8, &mut opcfg)?;
        let rep_ofs = usize::from(opcfg[1]);
        let tt_stat_ofs = usize::from(opcfg[5]);
        let max_touches = usize::from(opcfg[7] & 0x1f);
        let record_size = usize::from(opcfg[8] & 0x1f);
        let pair = |index: usize| {
            let at = 9 + index * 2;
            match (opcfg.get(at), opcfg.get(at + 1)) {
                (Some(&loc), Some(&size)) => Field::parse(loc, size),
                _ => Field::default(),
            }
        };
        let (x, y, pressure, id, event) = (pair(0), pair(1), pair(2), pair(3), pair(4));
        // After the seven old fields come the size of a button's record and two more bytes, then the new fields.
        let new_pair = |index: usize| {
            let at = 9 + 7 * 2 + 3 + index * 2;
            match (record_size > 6, opcfg.get(at), opcfg.get(at + 1)) {
                (true, Some(&loc), Some(&size)) => Field::parse(loc, size),
                _ => Field::default(),
            }
        };
        let (major, minor) = (new_pair(0), new_pair(1));
        if res_x == 0 || res_y == 0 || record_size == 0 || max_touches == 0 || id.bytes == 0 {
            return Err(Error::TouchBadSysinfo);
        }
        Ok(Geometry {
            res_x: res_x as i32,
            res_y: res_y as i32,
            max_pressure: max_pressure as i32,
            slots: (id.max as usize).clamp(1, MAX_SLOTS),
            rep_ofs,
            tt_stat_ofs,
            record_size,
            max_touches,
            x,
            y,
            pressure,
            id,
            event,
            major,
            minor,
        })
    }

    /// Reads a frame if the chip has one. `Ok(None)` means it has none now. The chip's own reset comes back here as an error,
    /// and the caller starts it again.
    pub fn poll(&mut self) -> Result<Option<Vec<Contact>>, Error> {
        if !self.bus.take_edge() {
            return Ok(None);
        }
        let mut head = [0u8; 3];
        self.bus.read(REG_MODE, &mut head)?;
        if head[0] & 1 != 0 || head[1] != 0 {
            return Err(Error::TouchReset);
        }
        if head[0] & MODE_MASK != MODE_OPERATIONAL && head[0] & MODE_CHANGE == 0 {
            return Err(Error::TouchReset);
        }
        let geometry = self.geometry;
        let mut status = vec![0u8; geometry.tt_stat_ofs + 1];
        self.bus.read(REG_MODE, &mut status)?;
        let rep_len = status.get(geometry.rep_ofs).copied().unwrap_or(0);
        let rep_stat = status.get(geometry.rep_ofs + 1).copied().unwrap_or(0);
        let touches =
            usize::from(status[geometry.tt_stat_ofs] & NUM_TOUCHES_MASK).min(geometry.max_touches);

        let mut contacts = Vec::new();
        let valid = rep_stat & REP_STAT_BAD == 0 && !(rep_len == 0 && touches > 0);
        if valid && touches > 0 {
            let mut records = vec![0u8; touches * geometry.record_size];
            self.bus
                .read((geometry.tt_stat_ofs + 1) as u8, &mut records)?;
            for record in records.chunks_exact(geometry.record_size) {
                if geometry.event.get(record) == EVENT_LIFT_OFF {
                    continue;
                }
                let slot = geometry.id.get(record) as usize;
                if slot >= geometry.slots {
                    continue;
                }
                let pressure = geometry.pressure.get(record) as i32;
                let (mut major, mut minor) = (
                    geometry.major.get(record) as i32,
                    geometry.minor.get(record) as i32,
                );
                if pressure > 0 && major == 0 {
                    major = 1;
                    minor = 1;
                }
                contacts.push(Contact {
                    slot,
                    x: geometry.x.get(record) as i32,
                    y: geometry.y.get(record) as i32,
                    pressure,
                    major,
                    minor,
                });
            }
        }
        self.acknowledge(head[0])?;
        if valid { Ok(Some(contacts)) } else { Ok(None) }
    }
}

/// An axis of the multi-touch protocol, by its Linux code.
pub const ABS_MT_SLOT: u8 = 0x2f;
pub const ABS_MT_TOUCH_MAJOR: u8 = 0x30;
pub const ABS_MT_TOUCH_MINOR: u8 = 0x31;
pub const ABS_MT_POSITION_X: u8 = 0x35;
pub const ABS_MT_POSITION_Y: u8 = 0x36;
pub const ABS_MT_TRACKING_ID: u8 = 0x39;
pub const ABS_MT_PRESSURE: u8 = 0x3a;

/// Turns frames into the events of the multi-touch protocol B: slots, with only what changed.
pub struct Slots {
    active: [Option<(i32, Contact)>; MAX_SLOTS],
    next_id: i32,
}

impl Default for Slots {
    fn default() -> Self {
        Self {
            active: [None; MAX_SLOTS],
            next_id: 0,
        }
    }
}

impl Slots {
    /// The axis events of this frame, without the sync that ends it.
    pub fn update(&mut self, frame: &[Contact], has_pressure: bool) -> Vec<(u8, i32)> {
        let mut events = Vec::new();
        for contact in frame {
            let slot = contact.slot;
            let before = self.active[slot];
            let mut out = Vec::new();
            let id = match before {
                Some((id, _)) => id,
                None => {
                    let id = self.next_id;
                    self.next_id = (self.next_id + 1) & 0x7fff;
                    out.push((ABS_MT_TRACKING_ID, id));
                    id
                }
            };
            let changed =
                |get: fn(&Contact) -> i32| before.is_none_or(|(_, old)| get(&old) != get(contact));
            if changed(|c| c.x) {
                out.push((ABS_MT_POSITION_X, contact.x));
            }
            if changed(|c| c.y) {
                out.push((ABS_MT_POSITION_Y, contact.y));
            }
            if has_pressure && changed(|c| c.pressure) {
                out.push((ABS_MT_PRESSURE, contact.pressure));
            }
            if changed(|c| c.major) {
                out.push((ABS_MT_TOUCH_MAJOR, contact.major));
            }
            if changed(|c| c.minor) {
                out.push((ABS_MT_TOUCH_MINOR, contact.minor));
            }
            if !out.is_empty() {
                events.push((ABS_MT_SLOT, slot as i32));
                events.extend(out);
            }
            self.active[slot] = Some((id, *contact));
        }
        for slot in 0..MAX_SLOTS {
            if self.active[slot].is_some() && !frame.iter().any(|contact| contact.slot == slot) {
                self.active[slot] = None;
                events.push((ABS_MT_SLOT, slot as i32));
                events.push((ABS_MT_TRACKING_ID, -1));
            }
        }
        events
    }
}

#[cfg(test)]
mod tests {
    use alloc::{collections::BTreeMap, rc::Rc};
    use core::cell::RefCell;

    use super::*;

    /// A chip that follows the protocol, with a record of 9 bytes: X and Y of 16 bits, pressure, id and event in a byte
    /// each, the event in the upper bits of the id's byte.
    #[derive(Default)]
    struct Chip {
        memory: BTreeMap<u8, u8>,
        reset: bool,
        edges: u32,
        mode: u8,
        writes: Vec<(u8, Vec<u8>)>,
        pulses: u32,
        acks: u32,
        no_heartbeat_for: u32,
    }

    const PCFG: u8 = 0x20;
    const OPCFG: u8 = 0x30;
    const DDATA: u8 = 0x60;
    const TT_STAT: u8 = 8;

    impl Chip {
        fn sysinfo(&mut self) {
            let map = [
                0, 0, 0, 0x70, 0, 0x10, 0, 0x18, 0, PCFG, 0, OPCFG, 0, DDATA, 0, 0x68,
            ];
            for (index, byte) in map.into_iter().enumerate() {
                self.memory.insert(index as u8, byte);
            }
            // res_x 758, res_y 1024, max_z 255.
            let pcfg = [20, 30, 0, 0, 0, 0, 0x02, 0xf6, 0x04, 0x00, 0, 255, 0];
            for (index, byte) in pcfg.into_iter().enumerate() {
                self.memory.insert(PCFG + index as u8, byte);
            }
            let mut opcfg = [0u8; 40];
            opcfg[1] = 6; // rep_ofs
            opcfg[5] = TT_STAT;
            opcfg[7] = 10; // max touches
            opcfg[8] = 9; // record size
            let fields = [
                (0, 16),
                (2, 16),
                (4, 8),
                (5, 5),
                (5 | (5 << 5), 3),
                (6, 8),
                (6, 8),
            ];
            for (index, (loc, size)) in fields.into_iter().enumerate() {
                opcfg[9 + index * 2] = loc;
                opcfg[10 + index * 2] = size;
            }
            // The new fields: major and minor.
            opcfg[26] = 7;
            opcfg[27] = 8;
            opcfg[28] = 8;
            opcfg[29] = 8;
            for (index, byte) in opcfg.into_iter().enumerate() {
                self.memory.insert(OPCFG + index as u8, byte);
            }
        }
    }

    #[derive(Clone)]
    struct Bus(Rc<RefCell<Chip>>);

    impl TouchBus for Bus {
        fn read(&mut self, reg: u8, out: &mut [u8]) -> Result<(), Error> {
            let chip = self.0.borrow();
            for (index, byte) in out.iter_mut().enumerate() {
                let at = reg as usize + index;
                *byte = if at == 0 {
                    chip.mode
                } else {
                    chip.memory.get(&(at as u8)).copied().unwrap_or(0)
                };
            }
            Ok(())
        }

        fn write(&mut self, reg: u8, data: &[u8]) -> Result<(), Error> {
            let mut chip = self.0.borrow_mut();
            chip.writes.push((reg, data.to_vec()));
            if data == BL_LAUNCH {
                chip.mode = MODE_SYSINFO;
                chip.memory.insert(1, 0);
                chip.sysinfo();
                chip.memory.insert(1, 0);
                chip.edges += 1;
            } else if data.len() == 1 && data[0] & MODE_CHANGE != 0 {
                chip.mode = data[0] & !MODE_CHANGE;
                chip.memory.insert(1, 0);
                chip.memory.insert(6, 0);
                chip.edges += 1;
            } else if data.len() == 1 && (data[0] ^ chip.mode) == MODE_TOGGLE {
                chip.mode = data[0];
                chip.acks += 1;
            }
            Ok(())
        }

        fn pulse_interrupt(&mut self) {
            self.0.borrow_mut().edges += 1;
        }

        fn set_reset(&mut self, high: bool) {
            let mut chip = self.0.borrow_mut();
            if high && chip.reset {
                chip.pulses += 1;
                if chip.no_heartbeat_for > 0 {
                    chip.no_heartbeat_for -= 1;
                } else {
                    chip.mode = 0x01;
                    chip.edges += 1;
                }
            }
            chip.reset = !high;
        }

        fn take_edge(&mut self) -> bool {
            let mut chip = self.0.borrow_mut();
            if chip.edges > 0 {
                chip.edges -= 1;
                return true;
            }
            false
        }

        fn delay_ms(&mut self, _ms: u32) {}
    }

    fn chip() -> (Rc<RefCell<Chip>>, Bus) {
        let chip = Rc::new(RefCell::new(Chip::default()));
        (chip.clone(), Bus(chip))
    }

    fn started() -> (Rc<RefCell<Chip>>, Touch<Bus>) {
        let (chip, bus) = chip();
        let touch = Touch::start(bus).map_err(|(_, err)| err).unwrap();
        (chip, touch)
    }

    fn record(id: u8, event: u8, x: u16, y: u16, pressure: u8) -> [u8; 9] {
        let mut record = [0u8; 9];
        record[..2].copy_from_slice(&x.to_be_bytes());
        record[2..4].copy_from_slice(&y.to_be_bytes());
        record[4] = pressure;
        record[5] = id | (event << 5);
        record
    }

    fn send(chip: &Rc<RefCell<Chip>>, records: &[[u8; 9]]) {
        let mut chip = chip.borrow_mut();
        chip.memory.insert(6, 5);
        chip.memory.insert(7, 0);
        chip.memory.insert(TT_STAT, records.len() as u8);
        for (index, byte) in records.iter().flatten().enumerate() {
            chip.memory.insert(TT_STAT + 1 + index as u8, *byte);
        }
        chip.edges += 1;
    }

    #[test]
    fn it_sleeps_on_the_mode_bit_and_wakes_on_a_pulse() {
        let (chip, mut touch) = started();
        let mode = chip.borrow().mode;
        touch.sleep().unwrap();
        assert_eq!(chip.borrow().writes.last().unwrap(), &(REG_MODE, vec![mode | MODE_SLEEP]));
        touch.wake().unwrap();
    }

    #[test]
    fn it_comes_up_with_the_sizes_the_chip_gives() {
        let (chip, touch) = started();
        let geometry = touch.geometry();
        assert_eq!(
            (geometry.res_x, geometry.res_y, geometry.max_pressure),
            (758, 1024, 255)
        );
        assert_eq!(geometry.slots, 32);
        assert_eq!(chip.borrow().pulses, 1);
        assert_eq!(chip.borrow().mode & MODE_MASK, MODE_OPERATIONAL);
    }

    #[test]
    fn the_reset_comes_first_and_the_application_is_started_once() {
        let (chip, _touch) = started();
        let chip = chip.borrow();
        let launches = chip
            .writes
            .iter()
            .filter(|(_, data)| data == &BL_LAUNCH)
            .count();
        assert_eq!(launches, 1);
        assert_eq!(chip.writes[0].1, BL_LAUNCH);
    }

    #[test]
    fn a_chip_that_misses_a_reset_gets_another() {
        let (chip, bus) = chip();
        chip.borrow_mut().no_heartbeat_for = 2;
        let touch = Touch::start(bus).map_err(|(_, err)| err).unwrap();
        assert_eq!(chip.borrow().pulses, 3);
        assert_eq!(touch.geometry().res_x, 758);
    }

    #[test]
    fn a_chip_that_never_answers_is_an_error_in_the_end() {
        let (chip, bus) = chip();
        chip.borrow_mut().no_heartbeat_for = 100;
        let err = Touch::start(bus).map(|_| ()).map_err(|(_, err)| err);
        assert_eq!(err, Err(Error::TouchTimeout));
        assert_eq!(chip.borrow().pulses, STARTUP_TRIES);
    }

    #[test]
    fn nothing_without_an_interrupt() {
        let (_chip, mut touch) = started();
        assert_eq!(touch.poll(), Ok(None));
    }

    #[test]
    fn a_frame_gives_its_contacts_and_is_acknowledged() {
        let (chip, mut touch) = started();
        let acks = chip.borrow().acks;
        send(&chip, &[record(1, 1, 100, 200, 30)]);
        let frame = touch.poll().unwrap().unwrap();
        assert_eq!(frame.len(), 1);
        assert_eq!(
            frame[0],
            Contact {
                slot: 1,
                x: 100,
                y: 200,
                pressure: 30,
                major: 1,
                minor: 1
            }
        );
        assert_eq!(chip.borrow().acks, acks + 1);
    }

    #[test]
    fn a_lift_off_is_not_a_contact_and_an_empty_frame_counts() {
        let (chip, mut touch) = started();
        send(&chip, &[record(1, 3, 100, 200, 0)]);
        assert_eq!(touch.poll().unwrap().unwrap(), []);
        send(&chip, &[]);
        assert_eq!(touch.poll().unwrap().unwrap(), []);
    }

    #[test]
    fn a_bad_frame_is_dropped_but_still_acknowledged() {
        let (chip, mut touch) = started();
        let acks = chip.borrow().acks;
        send(&chip, &[record(1, 1, 100, 200, 30)]);
        chip.borrow_mut().memory.insert(7, REP_STAT_BAD);
        assert_eq!(touch.poll(), Ok(None));
        assert_eq!(chip.borrow().acks, acks + 1);
    }

    #[test]
    fn a_chip_back_in_its_bootloader_is_reported() {
        let (chip, mut touch) = started();
        {
            let mut chip = chip.borrow_mut();
            chip.mode = 0x01;
            chip.edges += 1;
        }
        assert_eq!(touch.poll(), Err(Error::TouchReset));
    }

    #[test]
    fn slots_report_a_finger_down_a_move_and_up() {
        let mut slots = Slots::default();
        let mut contact = Contact {
            slot: 2,
            x: 10,
            y: 20,
            pressure: 5,
            major: 1,
            minor: 1,
        };
        assert_eq!(
            slots.update(&[contact], true),
            [
                (ABS_MT_SLOT, 2),
                (ABS_MT_TRACKING_ID, 0),
                (ABS_MT_POSITION_X, 10),
                (ABS_MT_POSITION_Y, 20),
                (ABS_MT_PRESSURE, 5),
                (ABS_MT_TOUCH_MAJOR, 1),
                (ABS_MT_TOUCH_MINOR, 1),
            ]
        );
        contact.x = 11;
        assert_eq!(
            slots.update(&[contact], true),
            [(ABS_MT_SLOT, 2), (ABS_MT_POSITION_X, 11)]
        );
        assert_eq!(slots.update(&[contact], true), []);
        assert_eq!(
            slots.update(&[], true),
            [(ABS_MT_SLOT, 2), (ABS_MT_TRACKING_ID, -1)]
        );
        let next = slots.update(&[contact], false);
        assert!(next.contains(&(ABS_MT_TRACKING_ID, 1)));
        assert!(!next.iter().any(|(code, _)| *code == ABS_MT_PRESSURE));
    }
}
