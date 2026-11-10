extern crate std;

use std::{format, string::String, vec, vec::Vec};

use super::*;

/// A HID-over-I2C device on a recording bus: register reads come from `mem`, plain reads from `inputs`.
struct FakeBus {
    mem: Vec<u8>,
    inputs: Vec<Vec<u8>>,
    writes: Vec<Vec<u8>>,
    sleeps: Vec<u32>,
    /// The microsecond pauses, as [`Bus::sleep_us`] got them.
    sleeps_us: Vec<u32>,
    /// The buffer length of each plain read.
    read_lens: Vec<usize>,
    /// Everything in order: `w<opcode>` for a command, `s<ms>` for a sleep, `u<us>` for a microsecond pause.
    log: Vec<String>,
    /// How many of the next writes and register reads fail.
    failures: u32,
    /// How many of the next plain reads fail.
    fail_reads: u32,
    /// What each write-then-read wrote.
    write_reads: Vec<Vec<u8>>,
}

impl FakeBus {
    fn fail(&mut self) -> Result<(), ()> {
        if self.failures > 0 {
            self.failures -= 1;
            Err(())
        } else {
            Ok(())
        }
    }
}

impl Bus for FakeBus {
    type Error = ();

    fn write(&mut self, data: &[u8]) -> Result<(), ()> {
        self.log.push(format!("w{}", data[3]));
        self.fail()?;
        self.writes.push(data.to_vec());
        Ok(())
    }

    fn read(&mut self, buf: &mut [u8]) -> Result<(), ()> {
        self.read_lens.push(buf.len());
        if self.fail_reads > 0 {
            self.fail_reads -= 1;
            return Err(());
        }
        if buf.len() < 2 {
            // The 1-byte probe-address read: not an input report.
            buf.fill(0);
            return Ok(());
        }
        let input = if self.inputs.is_empty() {
            vec![0, 0]
        } else {
            self.inputs.remove(0)
        };
        buf.fill(0);
        let n = input.len().min(buf.len());
        buf[..n].copy_from_slice(&input[..n]);
        Ok(())
    }

    fn write_read(&mut self, data: &[u8], buf: &mut [u8]) -> Result<(), ()> {
        self.fail()?;
        self.write_reads.push(data.to_vec());
        let reg = usize::from(u16::from_le_bytes([data[0], data[1]]));
        buf.copy_from_slice(&self.mem[reg..reg + buf.len()]);
        Ok(())
    }

    fn sleep_ms(&mut self, ms: u32) {
        self.sleeps.push(ms);
        self.log.push(format!("s{ms}"));
    }

    fn sleep_us(&mut self, us: u32) {
        self.sleeps_us.push(us);
        self.log.push(format!("u{us}"));
    }

    fn may_retry(_error: &()) -> bool {
        // The fake's choice comes from a thread-local as this has no self.
        RETRYABLE.with(|r| r.get())
    }
}

std::thread_local! {
    static RETRYABLE: std::cell::Cell<bool> = const { std::cell::Cell::new(true) };
}

/// HID 1.11 Appendix E.6: the boot keyboard report descriptor.
const BOOT_KEYBOARD: &[u8] = &[
    0x05, 0x01, 0x09, 0x06, 0xa1, 0x01, 0x05, 0x07, 0x19, 0xe0, 0x29, 0xe7, 0x15, 0x00, 0x25, 0x01,
    0x75, 0x01, 0x95, 0x08, 0x81, 0x02, 0x95, 0x01, 0x75, 0x08, 0x81, 0x01, 0x95, 0x05, 0x75, 0x01,
    0x05, 0x08, 0x19, 0x01, 0x29, 0x05, 0x91, 0x02, 0x95, 0x01, 0x75, 0x03, 0x91, 0x01, 0x95, 0x06,
    0x75, 0x08, 0x15, 0x00, 0x25, 0x65, 0x05, 0x07, 0x19, 0x00, 0x29, 0x65, 0x81, 0x00, 0xc0,
];

fn descriptor_bytes() -> [u8; 30] {
    [
        0x1e,
        0x00,
        0x00,
        0x01,
        BOOT_KEYBOARD.len() as u8,
        0x00,
        0x00,
        0x01,
        0x24,
        0x00,
        0x0a,
        0x00,
        0x25,
        0x00,
        0x00,
        0x00,
        0x22,
        0x00,
        0x23,
        0x00,
        0xf2,
        0x0c,
        0x28,
        0x90,
        0x00,
        0x01,
        0,
        0,
        0,
        0,
    ]
}

fn fake_bus(inputs: Vec<Vec<u8>>) -> FakeBus {
    let mut mem = vec![0u8; 0x200];
    mem[0x20..0x20 + 30].copy_from_slice(&descriptor_bytes());
    // The descriptor names register 0x0100 for the report descriptor.
    mem[0x100..0x100 + BOOT_KEYBOARD.len()].copy_from_slice(BOOT_KEYBOARD);
    FakeBus {
        mem,
        inputs,
        writes: Vec::new(),
        sleeps: Vec::new(),
        sleeps_us: Vec::new(),
        read_lens: Vec::new(),
        log: Vec::new(),
        failures: 0,
        fail_reads: 0,
        write_reads: Vec::new(),
    }
}

fn device(inputs: Vec<Vec<u8>>) -> Device<FakeBus> {
    Device::probe(fake_bus(inputs), 0x20).unwrap()
}

#[test]
fn descriptor_is_checked() {
    let d = Descriptor::parse::<()>(&descriptor_bytes()).unwrap();
    assert_eq!(
        (d.vendor_id, d.product_id, d.command_reg, d.max_input_len),
        (0x0cf2, 0x9028, 0x22, 10)
    );
    let mut bad = descriptor_bytes();
    bad[0] = 0x1f;
    assert_eq!(
        Descriptor::parse::<()>(&bad),
        Err(Error::DescriptorLength(0x1f))
    );
    let mut bad = descriptor_bytes();
    bad[3] = 0x02;
    assert_eq!(
        Descriptor::parse::<()>(&bad),
        Err(Error::DescriptorVersion(0x0200))
    );
}

#[test]
fn commands_and_reports_follow_the_spec() {
    let mut dev = device(vec![
        vec![0, 0],
        vec![0x0a, 0x00, 0, 0, 4, 0, 0, 0, 0, 0],
        vec![0x01, 0x00],
    ]);
    dev.set_power(true).unwrap();
    dev.reset().unwrap();
    dev.set_power(false).unwrap();
    // Command register 0x0022: SET_POWER ON, RESET, SET_POWER SLEEP.
    assert_eq!(
        dev.bus_mut().writes,
        [
            vec![0x22, 0x00, 0x00, 0x08],
            vec![0x22, 0x00, 0x00, 0x01],
            vec![0x22, 0x00, 0x01, 0x08]
        ]
    );
    let mut buf = [0u8; 64];
    assert_eq!(dev.read_input(&mut buf), Ok(Input::Empty));
    assert_eq!(
        dev.read_input(&mut buf),
        Ok(Input::Report(&[0, 0, 4, 0, 0, 0, 0, 0][..]))
    );
    assert_eq!(dev.read_input(&mut buf), Err(Error::BadInput(1)));
    let mut rd = [0u8; 128];
    assert_eq!(dev.report_descriptor(&mut rd).unwrap(), BOOT_KEYBOARD);
    assert_eq!(
        dev.report_descriptor(&mut [0u8; 8]),
        Err(Error::BufferTooSmall)
    );
}

#[test]
fn report_descriptor_lengths_0_and_above_4096_are_refused() {
    for (len, ok) in [(0u16, false), (4096, true), (4097, false)] {
        let mut bus = fake_bus(vec![]);
        bus.mem[0x24] = len as u8;
        bus.mem[0x25] = (len >> 8) as u8;
        // The report descriptor register is 0x0100; make the memory long enough for 4096 bytes.
        bus.mem.resize(0x100 + 4096, 0);
        let mut dev = Device::probe(bus, 0x20).unwrap();
        let mut buf = vec![0u8; 4096];
        let result = dev.report_descriptor(&mut buf);
        if ok {
            assert_eq!(result.unwrap().len(), 4096);
        } else {
            assert_eq!(result, Err(Error::BadReportDescLen(len)));
        }
    }
}

#[test]
fn input_reads_take_w_max_input_length() {
    // wMaxInputLength 256: 256 bytes, never fewer than Linux's min(wMaxInputLength, bufsize).
    let mut bus = fake_bus(vec![]);
    bus.mem[0x2a] = 0x00;
    bus.mem[0x2b] = 0x01;
    let mut dev = Device::probe(bus, 0x20).unwrap();
    let mut buf = [0u8; 256];
    assert_eq!(dev.read_input(&mut buf), Ok(Input::Empty));
    assert_eq!(dev.bus_mut().read_lens, [1, 256]);

    // wMaxInputLength 10: the read is 10 bytes.
    let mut dev = device(vec![]);
    let mut buf = [0u8; 256];
    assert_eq!(dev.read_input(&mut buf), Ok(Input::Empty));
    assert_eq!(dev.bus_mut().read_lens, [1, 10]);
}

#[test]
fn boot_keyboard_format_and_events() {
    let fmt = KeyboardFormat::parse(BOOT_KEYBOARD).unwrap();
    assert_eq!(fmt.report_id, None);
    let f = fmt.fields();
    assert_eq!(f.len(), 2);
    assert_eq!(
        (
            f[0].bit_offset,
            f[0].size,
            f[0].count,
            f[0].usage_min,
            f[0].variable
        ),
        (0, 1, 8, 0xe0, true)
    );
    assert_eq!(
        (
            f[1].bit_offset,
            f[1].size,
            f[1].count,
            f[1].usage_min,
            f[1].variable
        ),
        (16, 8, 6, 0, false)
    );

    let mut kb = Keyboard::new(fmt);
    let mut ev = Vec::new();
    kb.feed(&[0, 0, 4, 0, 0, 0, 0, 0], |e| ev.push(e));
    assert_eq!(
        ev,
        [KeyEvent {
            usage: 4,
            code: 30,
            pressed: true
        }]
    );
    ev.clear();
    kb.feed(&[0x02, 0, 4, 0, 0, 0, 0, 0], |e| ev.push(e));
    assert_eq!(
        ev,
        [KeyEvent {
            usage: 0xe1,
            code: 42,
            pressed: true
        }]
    );
    ev.clear();
    // Rollover changes nothing; then everything is released, releases in usage order.
    kb.feed(&[0x02, 0, 1, 1, 1, 1, 1, 1], |e| ev.push(e));
    assert!(ev.is_empty());
    kb.feed(&[0, 0, 0, 0, 0, 0, 0, 0], |e| ev.push(e));
    assert_eq!(
        ev,
        [
            KeyEvent {
                usage: 4,
                code: 30,
                pressed: false
            },
            KeyEvent {
                usage: 0xe1,
                code: 42,
                pressed: false
            }
        ]
    );
    ev.clear();
    // Enter and Space together, then a short report (ignored).
    kb.feed(&[0, 0, 0x28, 0x2c, 0, 0, 0, 0], |e| ev.push(e));
    assert_eq!(ev.iter().map(|e| e.code).collect::<Vec<_>>(), [28, 57]);
    ev.clear();
    kb.feed(&[0, 0], |e| ev.push(e));
    assert!(ev.is_empty());
}

#[test]
fn report_ids_and_other_collections() {
    // Report 1: keyboard (modifiers + 6 keys); report 2: consumer control; report 3: a vendor absolute axis.
    let desc: &[u8] = &[
        0x05, 0x01, 0x09, 0x06, 0xa1, 0x01, 0x85, 0x01, 0x05, 0x07, 0x19, 0xe0, 0x29, 0xe7, 0x15,
        0x00, 0x25, 0x01, 0x75, 0x01, 0x95, 0x08, 0x81, 0x02, 0x95, 0x06, 0x75, 0x08, 0x15, 0x00,
        0x26, 0xff, 0x00, 0x19, 0x00, 0x2a, 0xff, 0x00, 0x81, 0x00, 0xc0, 0x05, 0x0c, 0x09, 0x01,
        0xa1, 0x01, 0x85, 0x02, 0x75, 0x10, 0x95, 0x01, 0x15, 0x00, 0x26, 0xff, 0x03, 0x19, 0x00,
        0x2a, 0xff, 0x03, 0x81, 0x00, 0xc0, 0x06, 0x00, 0xff, 0x09, 0x01, 0xa1, 0x01, 0x85, 0x03,
        0x09, 0x30, 0x75, 0x08, 0x95, 0x01, 0x81, 0x02, 0xc0,
    ];
    let fmt = KeyboardFormat::parse(desc).unwrap();
    assert_eq!(fmt.report_id, Some(1));
    assert_eq!(
        fmt.fields()[1].bit_offset,
        8,
        "no reserved byte in this one"
    );
    let mut kb = Keyboard::new(fmt);
    let mut ev = Vec::new();
    kb.feed(&[2, 0xe9, 0x00], |e| ev.push(e));
    assert_eq!(
        ev.iter().map(|e| (e.code, e.pressed)).collect::<Vec<_>>(),
        [(115, true)],
        "consumer report: VOLUMEUP pressed"
    );
    ev.clear();
    kb.feed(&[2, 0x00, 0x00], |e| ev.push(e));
    assert_eq!(
        ev.iter().map(|e| (e.code, e.pressed)).collect::<Vec<_>>(),
        [(115, false)],
        "VOLUMEUP released"
    );
    ev.clear();
    kb.feed(&[3, 0x7f], |e| ev.push(e));
    assert!(ev.is_empty(), "other reports ignored");
    kb.feed(&[1, 0x01, 0x06, 0, 0, 0, 0, 0], |e| ev.push(e));
    assert_eq!(
        ev.iter().map(|e| (e.code, e.pressed)).collect::<Vec<_>>(),
        [(46, true), (29, true)]
    );
}

#[test]
fn consumer_array_keys() {
    // Report 1: keyboard modifiers; report 2: consumer control, a 2-element array of 16-bit usages 0..0x3ff.
    let desc: &[u8] = &[
        0x05, 0x01, 0x09, 0x06, 0xa1, 0x01, 0x85, 0x01, 0x05, 0x07, 0x19, 0xe0, 0x29, 0xe7, 0x15,
        0x00, 0x25, 0x01, 0x75, 0x01, 0x95, 0x08, 0x81, 0x02, 0xc0, 0x05, 0x0c, 0x09, 0x01, 0xa1,
        0x01, 0x85, 0x02, 0x75, 0x10, 0x95, 0x02, 0x15, 0x00, 0x26, 0xff, 0x03, 0x19, 0x00, 0x2a,
        0xff, 0x03, 0x81, 0x00, 0xc0,
    ];
    let fmt = KeyboardFormat::parse(desc).unwrap();
    assert_eq!(fmt.report_id, Some(1));
    assert_eq!(fmt.consumer_report_id, Some(2));
    let mut kb = Keyboard::new(fmt);
    let mut ev = Vec::new();
    let codes = |ev: &[KeyEvent]| ev.iter().map(|e| (e.code, e.pressed)).collect::<Vec<_>>();
    kb.feed(&[2, 0x6f, 0x00, 0x00, 0x00], |e| ev.push(e));
    assert_eq!(codes(&ev), [(225, true)]);
    ev.clear();
    kb.feed(&[2, 0x00, 0x00, 0x00, 0x00], |e| ev.push(e));
    assert_eq!(codes(&ev), [(225, false)]);
    ev.clear();
    kb.feed(&[2, 0x70, 0x00, 0x00, 0x00], |e| ev.push(e));
    assert_eq!(codes(&ev), [(224, true)]);
    ev.clear();
    kb.feed(&[2, 0x00, 0x00, 0x00, 0x00], |e| ev.push(e));
    assert_eq!(codes(&ev), [(224, false)]);
    ev.clear();
    // An unmapped usage: no event.
    kb.feed(&[2, 0x01, 0x00, 0x00, 0x00], |e| ev.push(e));
    assert!(ev.is_empty());
    kb.feed(&[2, 0x00, 0x00, 0x00, 0x00], |e| ev.push(e));
    assert!(ev.is_empty());
    // Two usages held at once, released one by one.
    kb.feed(&[2, 0x6f, 0x00, 0x70, 0x00], |e| ev.push(e));
    assert_eq!(codes(&ev), [(225, true), (224, true)]);
    ev.clear();
    kb.feed(&[2, 0x6f, 0x00, 0x00, 0x00], |e| ev.push(e));
    assert_eq!(codes(&ev), [(224, false)]);
    ev.clear();
    kb.feed(&[2, 0x00, 0x00, 0x00, 0x00], |e| ev.push(e));
    assert_eq!(codes(&ev), [(225, false)]);
}

#[test]
fn consumer_variable_bits() {
    // Report 1: keyboard modifiers; report 2: consumer control, one bit each for Usage 0x6f and 0x70, then padding.
    let desc: &[u8] = &[
        0x05, 0x01, 0x09, 0x06, 0xa1, 0x01, 0x85, 0x01, 0x05, 0x07, 0x19, 0xe0, 0x29, 0xe7, 0x15,
        0x00, 0x25, 0x01, 0x75, 0x01, 0x95, 0x08, 0x81, 0x02, 0xc0, 0x05, 0x0c, 0x09, 0x01, 0xa1,
        0x01, 0x85, 0x02, 0x09, 0x6f, 0x09, 0x70, 0x75, 0x01, 0x95, 0x02, 0x81, 0x02, 0x95, 0x06,
        0x81, 0x03, 0xc0,
    ];
    let fmt = KeyboardFormat::parse(desc).unwrap();
    assert_eq!(fmt.consumer_report_id, Some(2));
    let mut kb = Keyboard::new(fmt);
    let mut ev = Vec::new();
    let codes = |ev: &[KeyEvent]| ev.iter().map(|e| (e.code, e.pressed)).collect::<Vec<_>>();
    kb.feed(&[2, 0x01], |e| ev.push(e));
    assert_eq!(codes(&ev), [(225, true)]);
    ev.clear();
    kb.feed(&[2, 0x03], |e| ev.push(e));
    assert_eq!(codes(&ev), [(224, true)]);
    ev.clear();
    kb.feed(&[2, 0x02], |e| ev.push(e));
    assert_eq!(codes(&ev), [(225, false)]);
    ev.clear();
    kb.feed(&[2, 0x00], |e| ev.push(e));
    assert_eq!(codes(&ev), [(224, false)]);
}

#[test]
fn consumer_key_table() {
    assert_eq!(consumer_key(0x6f), Some(KEY_BRIGHTNESSUP));
    assert_eq!(consumer_key(0x70), Some(KEY_BRIGHTNESSDOWN));
    assert_eq!(consumer_key(0xe9), Some(115));
    assert_eq!(consumer_key(0x9999), None);
    let all: Vec<u16> = consumer_keys().collect();
    assert_eq!(all.len(), 23);
    assert!(all.contains(&KEY_BRIGHTNESSUP) && all.contains(&KEY_BRIGHTNESSDOWN));
}

#[test]
fn nkro_bitmap_keyboards() {
    // Modifiers, then one bit per usage 0x00..0x67.
    let desc: &[u8] = &[
        0x05, 0x01, 0x09, 0x06, 0xa1, 0x01, 0x05, 0x07, 0x19, 0xe0, 0x29, 0xe7, 0x15, 0x00, 0x25,
        0x01, 0x75, 0x01, 0x95, 0x08, 0x81, 0x02, 0x19, 0x00, 0x29, 0x67, 0x95, 0x68, 0x81, 0x02,
        0xc0,
    ];
    let fmt = KeyboardFormat::parse(desc).unwrap();
    let mut kb = Keyboard::new(fmt);
    let mut report = [0u8; 14];
    let mut press = |usage: usize| report[1 + usage / 8] |= 1 << (usage % 8);
    press(0x04); // A
    press(0x29); // Escape
    let mut ev = Vec::new();
    kb.feed(&report, |e| ev.push(e));
    assert_eq!(ev.iter().map(|e| e.code).collect::<Vec<_>>(), [30, 1]);
}

#[test]
fn no_keyboard_no_format() {
    // A mouse: buttons and X/Y on the Button and Generic Desktop pages.
    let mouse: &[u8] = &[
        0x05, 0x01, 0x09, 0x02, 0xa1, 0x01, 0x05, 0x09, 0x19, 0x01, 0x29, 0x03, 0x75, 0x01, 0x95,
        0x03, 0x81, 0x02, 0x05, 0x01, 0x09, 0x30, 0x09, 0x31, 0x75, 0x08, 0x95, 0x02, 0x81, 0x06,
        0xc0,
    ];
    assert_eq!(KeyboardFormat::parse(mouse), None);
    assert_eq!(KeyboardFormat::parse(&[0x75]), None, "truncated item");
}

/// HID 1.11 Appendix E.10: the boot mouse, as a touch pad in mouse mode looks: 3 buttons, X, Y, relative.
const BOOT_MOUSE: &[u8] = &[
    0x05, 0x01, 0x09, 0x02, 0xa1, 0x01, 0x09, 0x01, 0xa1, 0x00, 0x05, 0x09, 0x19, 0x01, 0x29, 0x03,
    0x15, 0x00, 0x25, 0x01, 0x95, 0x03, 0x75, 0x01, 0x81, 0x02, 0x95, 0x01, 0x75, 0x05, 0x81, 0x01,
    0x05, 0x01, 0x09, 0x30, 0x09, 0x31, 0x15, 0x81, 0x25, 0x7f, 0x75, 0x08, 0x95, 0x02, 0x81, 0x06,
    0xc0, 0xc0,
];

/// A Precision Touchpad's collections: report 1 a mouse (buttons, X, Y, wheel), report 3 the touch pad (tip switch,
/// contact id, 16-bit X and Y up to 3200 and 2000, second finger's fields after them, clickpad button).
const PTP: &[u8] = &[
    // Report 1: mouse
    0x05, 0x01, 0x09, 0x02, 0xa1, 0x01, 0x85, 0x01, 0x09, 0x01, 0xa1, 0x00, 0x05, 0x09, 0x19, 0x01,
    0x29, 0x02, 0x15, 0x00, 0x25, 0x01, 0x75, 0x01, 0x95, 0x02, 0x81, 0x02, 0x95, 0x06, 0x81, 0x03,
    0x05, 0x01, 0x09, 0x30, 0x09, 0x31, 0x09, 0x38, 0x15, 0x81, 0x25, 0x7f, 0x75, 0x08, 0x95, 0x03,
    0x81, 0x06, 0xc0, 0xc0, // Report 3: touch pad, one finger collection then another
    0x05, 0x0d, 0x09, 0x05, 0xa1, 0x01, 0x85, 0x03, 0x09, 0x22, 0xa1, 0x02, 0x15, 0x00, 0x25, 0x01,
    0x09, 0x42, 0x75, 0x01, 0x95, 0x01, 0x81, 0x02, 0x95, 0x07, 0x81, 0x03, 0x05, 0x01, 0x26, 0x80,
    0x0c, 0x75, 0x10, 0x95, 0x01, 0x09, 0x30, 0x81, 0x02, 0x26, 0xd0, 0x07, 0x09, 0x31, 0x81, 0x02,
    0xc0, 0x05, 0x0d, 0x09, 0x22, 0xa1, 0x02, 0x25, 0x01, 0x09, 0x42, 0x75, 0x01, 0x95, 0x01, 0x81,
    0x02, 0x95, 0x07, 0x81, 0x03, 0x05, 0x01, 0x26, 0x80, 0x0c, 0x75, 0x10, 0x95, 0x01, 0x09, 0x30,
    0x81, 0x02, 0x26, 0xd0, 0x07, 0x09, 0x31, 0x81, 0x02, 0xc0, 0x05, 0x09, 0x09, 0x01, 0x15, 0x00,
    0x25, 0x01, 0x75, 0x01, 0x95, 0x01, 0x81, 0x02, 0x95, 0x07, 0x81, 0x03, 0xc0,
];

/// The same touch pad without the mouse collection.
fn ptp_only() -> Vec<u8> {
    let start = PTP
        .windows(4)
        .position(|w| w == [0x05, 0x0d, 0x09, 0x05])
        .unwrap();
    PTP[start..].to_vec()
}

#[test]
fn relative_pointer_moves_and_clicks() {
    let fmt = PointerFormat::parse(BOOT_MOUSE).unwrap();
    assert_eq!(fmt.report_id, None);
    assert_eq!(fmt.motion, Motion::Relative);
    assert_eq!(fmt.buttons, Some((0, 3)));
    let mut p = Pointer::new(fmt);
    let mut ev = Vec::new();
    p.feed(&[0x00, 0x05, 0xfd], |e| ev.push(e));
    assert_eq!(ev, [PointerEvent::Move { dx: 5, dy: -3 }]);
    ev.clear();
    p.feed(&[0x01, 0x00, 0x00], |e| ev.push(e));
    assert_eq!(
        ev,
        [PointerEvent::Button {
            index: 0,
            pressed: true
        }]
    );
    ev.clear();
    p.feed(&[0x00, 0x00, 0x00], |e| ev.push(e));
    assert_eq!(
        ev,
        [PointerEvent::Button {
            index: 0,
            pressed: false
        }]
    );
    ev.clear();
    p.feed(&[0x00], |e| ev.push(e));
    assert!(ev.is_empty(), "too short");
    assert_eq!(KeyboardFormat::parse(BOOT_MOUSE), None);
    assert_eq!(PointerFormat::parse(BOOT_KEYBOARD), None);
}

#[test]
fn touchpad_starts_in_its_mouse_collection() {
    let fmt = PointerFormat::parse(PTP).unwrap();
    assert_eq!(fmt.report_id, Some(1));
    assert_eq!(fmt.motion, Motion::Relative);
    let mut p = Pointer::new(fmt);
    let mut ev = Vec::new();
    p.feed(&[3, 1, 0x10, 0, 0x10, 0, 0, 0, 0, 0, 0, 1], |e| ev.push(e));
    assert!(ev.is_empty(), "the touch pad report is not this format's");
    p.feed(&[1, 0x02, 0x03, 0x04, 0xff], |e| ev.push(e));
    assert_eq!(
        ev,
        [
            PointerEvent::Move { dx: 3, dy: 4 },
            PointerEvent::Wheel(-1),
            PointerEvent::Button {
                index: 1,
                pressed: true
            }
        ]
    );
}

#[test]
fn absolute_touchpad_becomes_relative() {
    let desc = ptp_only();
    let fmt = PointerFormat::parse(&desc).unwrap();
    assert_eq!(fmt.report_id, Some(3));
    let Motion::Absolute { x_max, y_max, tip } = fmt.motion else {
        panic!("{:?}", fmt.motion);
    };
    assert_eq!((x_max, y_max, tip.bit_offset), (3200, 2000, 0));
    assert_eq!(
        (fmt.x.bit_offset, fmt.y.bit_offset),
        (8, 24),
        "the first finger's fields"
    );
    let mut p = Pointer::new(fmt);
    let report = |tip: u8, x: u16, y: u16, button: u8| {
        let (xb, yb) = (x.to_le_bytes(), y.to_le_bytes());
        [3, tip, xb[0], xb[1], yb[0], yb[1], 0, 0, 0, 0, 0, button]
    };
    let mut ev = Vec::new();
    // Touching down moves nothing; then 320 of 3200 is a tenth of the pad's 1600 units.
    p.feed(&report(1, 1000, 1000, 0), |e| ev.push(e));
    assert!(ev.is_empty());
    p.feed(&report(1, 1320, 1000, 0), |e| ev.push(e));
    assert_eq!(ev, [PointerEvent::Move { dx: 160, dy: 0 }]);
    ev.clear();
    // Y uses the same scale as X, and small moves add up.
    p.feed(&report(1, 1320, 1320, 0), |e| ev.push(e));
    assert_eq!(ev, [PointerEvent::Move { dx: 0, dy: 160 }]);
    ev.clear();
    p.feed(&report(1, 1321, 1320, 0), |e| ev.push(e));
    assert_eq!(ev, []);
    p.feed(&report(1, 1322, 1320, 0), |e| ev.push(e));
    assert_eq!(ev, [PointerEvent::Move { dx: 1, dy: 0 }]);
    ev.clear();
    // Lifting ends the stroke: the next touch does not jump.
    p.feed(&report(0, 1322, 1320, 0), |e| ev.push(e));
    p.feed(&report(1, 100, 100, 0), |e| ev.push(e));
    assert!(ev.is_empty());
    // The clickpad's button.
    p.feed(&report(1, 100, 100, 1), |e| ev.push(e));
    assert_eq!(
        ev,
        [PointerEvent::Button {
            index: 0,
            pressed: true
        }]
    );
}

#[test]
fn probe_address_is_a_one_byte_read_with_one_retry_after_500_us() {
    // The first 1-byte read fails: one retry after 500 us, then the descriptor is read once.
    let mut bus = fake_bus(vec![]);
    bus.fail_reads = 1;
    let mut dev = Device::probe(bus, 0x20).unwrap();
    assert_eq!(dev.descriptor().vendor_id, 0x0cf2);
    assert_eq!(dev.bus_mut().sleeps_us, [500]);
    assert_eq!(dev.bus_mut().read_lens, [1, 1]);
    assert!(dev.bus_mut().sleeps.is_empty(), "no millisecond pause");

    // Both reads fail: nothing at this address, as `i2c_hid_probe_address` returns -ENXIO.
    let mut bus = fake_bus(vec![]);
    bus.fail_reads = 2;
    assert!(matches!(Device::probe(bus, 0x20), Err(Error::Bus(()))));

    // The descriptor read itself is not retried, as `i2c_hid_fetch_hid_descriptor` has no retries.
    let mut bus = fake_bus(vec![]);
    bus.failures = 1;
    let sleeps = bus.sleeps.clone();
    let sleeps_us = bus.sleeps_us.clone();
    assert!(matches!(Device::probe(bus, 0x20), Err(Error::Bus(()))));
    assert!(sleeps_us.is_empty() && sleeps.is_empty());
}

#[test]
fn power_on_is_tried_twice_500_us_apart_and_takes_its_60_ms() {
    let mut dev = device(vec![]);
    dev.bus_mut().failures = 1;
    dev.set_power(true).unwrap();
    assert_eq!(
        dev.bus_mut().log,
        ["w8", "u500", "w8", "s60"],
        "60 ms only after it went through"
    );

    let mut dev = device(vec![]);
    dev.bus_mut().failures = 2;
    assert_eq!(dev.set_power(true), Err(Error::Bus(())));
    assert!(
        !dev.bus_mut().sleeps.contains(&60),
        "no wait for a device that never heard"
    );

    // SLEEP is not retried, as in Linux.
    let mut dev = device(vec![]);
    dev.bus_mut().failures = 1;
    assert_eq!(dev.set_power(false), Err(Error::Bus(())));
    assert!(dev.bus_mut().sleeps.is_empty() && dev.bus_mut().sleeps_us.is_empty());
}

#[test]
fn errors_the_bus_calls_final_are_not_retried() {
    RETRYABLE.with(|r| r.set(false));
    let mut bus = fake_bus(vec![]);
    bus.fail_reads = 1;
    assert!(matches!(Device::probe(bus, 0x20), Err(Error::Bus(()))));
    let mut dev = device(vec![]);
    dev.bus_mut().failures = 1;
    assert_eq!(dev.set_power(true), Err(Error::Bus(())));
    assert_eq!(dev.bus_mut().log, ["w8"]);
    RETRYABLE.with(|r| r.set(true));
}

/// `s10` entries are the acknowledgement wait's ticks.
fn without_ticks(log: &[String]) -> Vec<&str> {
    log.iter()
        .map(String::as_str)
        .filter(|e| *e != "s10")
        .collect()
}

#[test]
fn hwreset_waits_60_ms_after_every_power_on_as_linux() {
    let mut dev = device(vec![]);
    let mut buf = [0u8; 64];
    let reset = dev.hwreset(&mut buf, || None).unwrap();
    assert_eq!(
        reset,
        HwReset {
            ack: ResetAck::Acknowledged,
            tries: 1
        }
    );
    assert_eq!(
        dev.bus_mut().log,
        ["w8", "s60", "w1", "s10", "w8", "s60"],
        "power on, 60 ms, reset, the acknowledgement, power on, 60 ms"
    );
}

#[test]
fn hwreset_retries_a_failed_start_after_a_second() {
    let mut dev = device(vec![]);
    let mut buf = [0u8; 64];
    // Both tries of the first power on fail.
    dev.bus_mut().failures = 2;
    let reset = dev.hwreset(&mut buf, || None).unwrap();
    assert_eq!(reset.tries, 2);
    assert_eq!(
        without_ticks(&dev.bus_mut().log),
        [
            "w8", "u500", "w8", "s1000", "w8", "s60", "w1", "w8", "s60"
        ],
    );
}

#[test]
fn hwreset_gives_up_after_linux_s_four_tries() {
    let mut dev = device(vec![]);
    let mut buf = [0u8; 64];
    dev.bus_mut().failures = u32::MAX;
    assert_eq!(dev.hwreset(&mut buf, || None), Err(Error::Bus(())));
    assert_eq!(
        dev.bus_mut().sleeps.iter().filter(|s| **s == 1000).count(),
        4
    );
    assert_eq!(dev.bus_mut().log.iter().filter(|e| *e == "w8").count(), 8);
}

#[test]
fn a_reset_command_that_fails_puts_the_device_to_sleep_and_tries_again() {
    let mut buf = [0u8; 64];
    // A device whose second write, the reset, fails.
    struct Script(FakeBus, u32);

    impl Bus for Script {
        type Error = ();
        fn write(&mut self, data: &[u8]) -> Result<(), ()> {
            self.1 += 1;
            if self.1 == 2 {
                self.0.log.push(format!("x{}", data[3]));
                return Err(());
            }
            self.0.write(data)
        }
        fn read(&mut self, buf: &mut [u8]) -> Result<(), ()> {
            self.0.read(buf)
        }
        fn write_read(&mut self, data: &[u8], buf: &mut [u8]) -> Result<(), ()> {
            self.0.write_read(data, buf)
        }
        fn sleep_ms(&mut self, ms: u32) {
            self.0.sleep_ms(ms)
        }
    }
    let mut dev = Device::probe(Script(fake_bus(vec![]), 0), 0x20).unwrap();
    let reset = dev.hwreset(&mut buf, || None).unwrap();
    assert_eq!(reset.tries, 2);
    assert_eq!(
        without_ticks(&dev.bus_mut().0.log),
        [
            "w8", "s60", "x1", "w8", "s1000", "w8", "s60", "w1", "w8", "s60"
        ],
        "SLEEP (opcode 8 again) and a second's pause after the reset that failed"
    );
    assert_eq!(dev.bus_mut().0.writes[1], [0x22, 0x00, 0x01, 0x08]);
}

#[test]
fn an_empty_report_is_the_acknowledgement_only_with_the_line_asserted() {
    // Asserted: taken at once.
    let mut dev = device(vec![vec![0, 0]]);
    let mut buf = [0u8; 64];
    let reset = dev.hwreset(&mut buf, || Some(true)).unwrap();
    assert_eq!(reset.ack, ResetAck::Acknowledged);

    // High: the device reads empty before it is done, so the whole second is waited out, and the reads happen anyway.
    let mut dev = device(vec![]);
    let reset = dev.hwreset(&mut buf, || Some(false)).unwrap();
    assert_eq!(reset.ack, ResetAck::NotAcknowledged { read_empty: true });
    assert_eq!(
        dev.bus_mut().sleeps.iter().filter(|s| **s == 10).count(),
        100
    );

    // The line drops halfway: the reads before that were blind, and the one it asks for is the acknowledgement.
    let mut dev = device(vec![vec![4, 0, 1, 2]]);
    let mut ticks = 0;
    let reset = dev
        .hwreset(&mut buf, || {
            ticks += 1;
            Some(ticks > 50)
        })
        .unwrap();
    assert_eq!(reset.ack, ResetAck::Acknowledged);
    assert_eq!(ticks, 51);
}

#[test]
fn no_acknowledgement_is_only_a_warning_as_in_linux() {
    let mut dev = device(vec![vec![0x0a, 0, 1, 2, 3, 4, 5, 6, 7, 8]; 300]);
    let mut buf = [0u8; 64];
    let reset = dev.hwreset(&mut buf, || None).unwrap();
    assert_eq!(
        reset,
        HwReset {
            ack: ResetAck::NotAcknowledged { read_empty: false },
            tries: 1
        }
    );
    assert_eq!(
        dev.bus_mut().sleeps.iter().filter(|s| **s == 10).count(),
        100
    );
}

#[test]
fn the_gate_reads_while_asserted_and_every_100_ms_whatever_the_line_says() {
    let mut gate = ReadGate::new();
    // A line that never moves: one read per 100 ms.
    let reads: Vec<u32> = (0..30).map(|_| gate.tick(Some(false))).collect();
    let at: Vec<usize> = reads
        .iter()
        .enumerate()
        .filter(|(_, r)| **r != 0)
        .map(|(i, _)| i)
        .collect();
    assert_eq!(at, [9, 19, 29]);
    assert!(reads.iter().all(|r| *r <= 1));
    // Asserted: reads this tick.
    assert_eq!(gate.tick(Some(true)), ReadGate::MAX_READS);
    // A line that has moved works: high means nothing to read, for as long as it stays high.
    assert_eq!((0..100).map(|_| gate.tick(Some(false))).sum::<u32>(), 0);
    assert_eq!(gate.tick(Some(true)), ReadGate::MAX_READS);
    // No line: every tick, once.
    let mut gate = ReadGate::new();
    assert!((0..20).all(|_| gate.tick(None) == 1));
}

#[test]
fn stale_reports_are_harmless() {
    // A keyboard's report read again changes nothing.
    let format = KeyboardFormat::parse(BOOT_KEYBOARD).unwrap();
    let mut keyboard = Keyboard::new(format);
    let report = [0, 0, 4, 0, 0, 0, 0, 0];
    let mut events = 0;
    keyboard.feed(&report, |_| events += 1);
    assert_eq!(events, 1);
    keyboard.feed(&report, |_| events += 1);
    assert_eq!(events, 1);

    // A device answering a read nobody asked for with its last report again: not fresh with the line high.
    let mut gate = ReadGate::new();
    assert!(gate.fresh(&[1, 2, 3], Some(true)));
    assert!(!gate.fresh(&[1, 2, 3], Some(false)));
    assert!(gate.fresh(&[1, 2, 4], Some(false)), "new data is news");
    assert!(
        gate.fresh(&[1, 2, 4], Some(true)),
        "the device asked to be read"
    );
    assert!(gate.fresh(&[1, 2, 4], None), "no line to tell");
    assert!(gate.fresh(&[1, 2], Some(false)), "another length");
    // Longer than what is kept: compared by what is kept, and the length.
    let long = [7u8; 100];
    assert!(gate.fresh(&long, Some(true)));
    assert!(!gate.fresh(&long, Some(false)));
}

/// A keyboard like the Book Go's: its line follows its queue, and a read with nothing queued gets idle bytes whose
/// "length" is 0x8102.
struct IdleBytes {
    queue: std::rc::Rc<std::cell::RefCell<std::collections::VecDeque<Vec<u8>>>>,

    reads_with_empty_queue: std::rc::Rc<std::cell::Cell<u32>>,
    desc: [u8; 30],
}

impl Bus for IdleBytes {
    type Error = ();

    fn write(&mut self, _data: &[u8]) -> Result<(), ()> {
        Ok(())
    }

    fn read(&mut self, buf: &mut [u8]) -> Result<(), ()> {
        if buf.len() < 2 {
            // The 1-byte probe-address read: not an input report.
            buf.fill(0);
            return Ok(());
        }
        match self.queue.borrow_mut().pop_front() {
            Some(report) => {
                buf.fill(0);
                buf[..report.len()].copy_from_slice(&report);
            }
            None => {
                self.reads_with_empty_queue
                    .set(self.reads_with_empty_queue.get() + 1);
                buf.fill(0x55);
                buf[..2].copy_from_slice(&[0x02, 0x81]);
            }
        }
        Ok(())
    }

    fn write_read(&mut self, _data: &[u8], buf: &mut [u8]) -> Result<(), ()> {
        buf.copy_from_slice(&self.desc[..buf.len()]);
        Ok(())
    }

    fn sleep_ms(&mut self, _ms: u32) {}
}

struct Typing {
    dev: Device<IdleBytes>,
    queue: std::rc::Rc<std::cell::RefCell<std::collections::VecDeque<Vec<u8>>>>,
    idle_reads: std::rc::Rc<std::cell::Cell<u32>>,
    keyboard: Keyboard,
    keys: Vec<(u16, bool)>,
}

fn typing() -> Typing {
    let queue = std::rc::Rc::new(std::cell::RefCell::new(std::collections::VecDeque::new()));
    let idle_reads = std::rc::Rc::new(std::cell::Cell::new(0));
    let bus = IdleBytes {
        queue: queue.clone(),
        reads_with_empty_queue: idle_reads.clone(),
        desc: descriptor_bytes(),
    };
    Typing {
        dev: Device::probe(bus, 0x20).unwrap(),
        queue,
        idle_reads,
        keyboard: Keyboard::new(KeyboardFormat::parse(BOOT_KEYBOARD).unwrap()),
        keys: Vec::new(),
    }
}

/// Key `usage` down or up, as the device's input register has it: length 10, then a boot report.
fn key_report(usage: u8) -> Vec<u8> {
    vec![10, 0, 0, 0, usage, 0, 0, 0, 0, 0]
}

impl Typing {
    fn tick(&mut self, line_works: bool) -> Result<(), Error<()>> {
        let mut buf = [0u8; 16];
        let queue = self.queue.clone();
        let (keyboard, keys) = (&mut self.keyboard, &mut self.keys);
        self.dev.poll(
            &mut buf,
            || Some(line_works && !queue.borrow().is_empty()),
            |report, _| keyboard.feed(report, |e| keys.push((e.code, e.pressed))),
        )
    }
}

#[test]
fn every_key_report_arrives_and_idle_bytes_are_never_read_once_the_line_has_moved() {
    let mut t = typing();
    t.tick(true).unwrap();
    // 'a' down, up, 'b' down, up; in a burst and spread out, with idle time around them.
    for usage in [4, 0, 5, 0] {
        t.queue.borrow_mut().push_back(key_report(usage));
    }
    for _ in 0..3 {
        t.tick(true).unwrap();
    }
    for usage in [6, 0] {
        for _ in 0..50 {
            t.tick(true).unwrap();
        }
        t.queue.borrow_mut().push_back(key_report(usage));
    }
    for _ in 0..50 {
        t.tick(true).unwrap();
    }
    assert_eq!(
        t.keys,
        [
            (30, true),
            (30, false),
            (48, true),
            (48, false),
            (46, true),
            (46, false)
        ]
    );
    assert_eq!(
        t.idle_reads.get(),
        0,
        "as in Linux, no read but on the line"
    );
    assert_eq!(t.dev.take_first_dropped(), None);
    assert!(t.dev.line_moved());
}

#[test]
fn idle_bytes_before_the_line_has_moved_are_dropped_quietly_and_told_once() {
    let mut t = typing();
    for _ in 0..1000 {
        t.tick(true).unwrap();
    }
    assert_eq!(
        t.idle_reads.get(),
        100,
        "every 100 ms, the line having stayed high"
    );
    assert_eq!(t.dev.take_first_dropped(), Some(Error::BadInput(0x8102)));
    assert_eq!(t.dev.take_first_dropped(), None, "once");
    assert!(!t.dev.line_moved());
}

#[test]
fn a_line_stuck_high_still_gets_real_keys_through_the_100_ms_reads() {
    let mut t = typing();
    for usage in [4, 0] {
        for _ in 0..100 {
            t.tick(false).unwrap();
        }
        t.queue.borrow_mut().push_back(key_report(usage));
    }
    for _ in 0..100 {
        t.tick(false).unwrap();
    }
    assert_eq!(t.keys, [(30, true), (30, false)]);
}

#[test]
fn a_bad_length_with_the_line_asserted_is_reported_as_linux_does() {
    let mut t = typing();
    t.queue.borrow_mut().push_back(vec![0x02, 0x81]);
    assert_eq!(t.tick(true), Err(Error::BadInput(0x8102)));
    // And the next report still arrives.
    t.queue.borrow_mut().push_back(key_report(4));
    t.tick(true).unwrap();
    assert_eq!(t.keys, [(30, true)]);
}

#[test]
fn with_no_line_a_bad_length_is_dropped() {
    let mut dev = device(vec![vec![0x02, 0x81]]);
    let mut buf = [0u8; 16];
    assert_eq!(dev.poll(&mut buf, || None, |_, _| {}), Ok(()));
    assert_eq!(dev.take_first_dropped(), Some(Error::BadInput(0x8102)));
}

#[test]
fn set_feature_is_i2c_hid_set_or_send_report() {
    let mut dev = device(vec![]);
    // Input Mode = 3 in feature report 4: command register 0x22, (3 << 4) | 4, SET_REPORT; data register 0x23; length 4
    // (itself, the ID, one byte); ID 4; 3.
    dev.set_feature(4, &[3]).unwrap();
    assert_eq!(
        dev.bus_mut().writes.last().unwrap(),
        &vec![0x22, 0x00, 0x34, 0x03, 0x23, 0x00, 0x04, 0x00, 0x04, 0x03]
    );
    // An ID of 15 or more goes in a third command byte.
    dev.set_feature(0x20, &[1, 2]).unwrap();
    assert_eq!(
        dev.bus_mut().writes.last().unwrap(),
        &vec![0x22, 0x00, 0x3f, 0x03, 0x20, 0x23, 0x00, 0x05, 0x00, 0x20, 0x01, 0x02]
    );
}

#[test]
fn get_feature_is_i2c_hid_get_report() {
    let mut dev = device(vec![]);
    // The fake answers a write-then-read from the first register named, here the command register 0x22.
    dev.bus_mut().mem[0x22..0x26].copy_from_slice(&[0x04, 0x00, 0x02, 0x05]);
    let mut buf = [0u8; 8];
    let report = dev.get_feature(2, 2, &mut buf).unwrap().to_vec();
    assert_eq!(report, vec![0x02, 0x05]);
    // Command register, (3 << 4) | 2, GET_REPORT, data register.
    assert_eq!(dev.bus_mut().write_reads.last().unwrap(), &vec![0x22, 0x00, 0x32, 0x02, 0x23, 0x00]);
    // Another report ID in the answer is refused, an empty answer is empty.
    dev.bus_mut().mem[0x22..0x26].copy_from_slice(&[0x04, 0x00, 0x07, 0x05]);
    assert_eq!(dev.get_feature(2, 2, &mut buf), Err(Error::WrongReport { asked: 2, got: 7 }));
    dev.bus_mut().mem[0x22..0x24].copy_from_slice(&[0x02, 0x00]);
    assert_eq!(dev.get_feature(2, 2, &mut buf), Ok(&[][..]));
}

/// The Galaxy Book Go's keyboard (0cf2:9028), its report descriptor as the laptop logged it (2026-11-08, `lucina.hid=report`):
/// report 1 keyboard, 3 Consumer Control (one 16-bit usage 0-0x3ff), 5 vendor page 0xff7f, 6 Wireless Radio Button,
/// 13 System Control buttons.
pub(crate) const GBGO_KEYBOARD: &[u8] = &[
        0x05, 0x01, 0x09, 0x06, 0xa1, 0x01, 0x85, 0x01, 0x05, 0x07, 0x19, 0xe0, 0x29, 0xe7, 0x15,
        0x00, 0x25, 0x01, 0x75, 0x01, 0x95, 0x08, 0x81, 0x02, 0x95, 0x01, 0x75, 0x08, 0x81, 0x03,
        0x95, 0x05, 0x75, 0x01, 0x05, 0x08, 0x19, 0x01, 0x29, 0x05, 0x91, 0x02, 0x95, 0x01, 0x75,
        0x03, 0x91, 0x03, 0x95, 0x06, 0x75, 0x08, 0x15, 0x00, 0x25, 0xff, 0x05, 0x07, 0x19, 0x00,
        0x29, 0xff, 0x81, 0x00, 0xc0, 0x05, 0x0c, 0x09, 0x01, 0xa1, 0x01, 0x85, 0x03, 0x95, 0x01,
        0x75, 0x10, 0x15, 0x00, 0x26, 0xff, 0x03, 0x19, 0x00, 0x2a, 0xff, 0x03, 0x81, 0x00, 0xc0,
        0x06, 0x7f, 0xff, 0x09, 0x01, 0xa1, 0x01, 0x85, 0x05, 0x19, 0x00, 0x29, 0xff, 0x15, 0x00,
        0x25, 0xff, 0x75, 0x08, 0x95, 0x08, 0x81, 0x00, 0xc0, 0x05, 0x01, 0x09, 0x0c, 0xa1, 0x01,
        0x85, 0x06, 0x15, 0x00, 0x25, 0x01, 0x09, 0xc6, 0x95, 0x01, 0x75, 0x01, 0x81, 0x06, 0x75,
        0x07, 0x81, 0x03, 0xc0, 0x05, 0x01, 0x09, 0x80, 0xa1, 0x01, 0x85, 0x0d, 0x05, 0x09, 0x19,
        0x01, 0x29, 0x02, 0x15, 0x00, 0x25, 0x01, 0x75, 0x01, 0x95, 0x02, 0x81, 0x02, 0x95, 0x06,
        0x81, 0x03, 0x95, 0x08, 0x19, 0x01, 0x29, 0xff, 0x91, 0x02, 0xc0,
];

#[test]
fn the_laptops_consumer_keys() {
    let fmt = KeyboardFormat::parse(GBGO_KEYBOARD).unwrap();
    assert_eq!(fmt.report_id, Some(1));
    let mut kb = Keyboard::new(fmt);
    let mut ev = Vec::new();
    for (usage, code) in [(0x6fu16, 225u16), (0x70, 224), (0xe9, 115), (0xea, 114), (0xe2, 113)] {
        ev.clear();
        kb.feed(&[3, usage as u8, (usage >> 8) as u8], |e| ev.push(e));
        kb.feed(&[3, 0, 0], |e| ev.push(e));
        assert_eq!(
            ev.iter().map(|e| (e.code, e.pressed)).collect::<Vec<_>>(),
            [(code, true), (code, false)],
            "usage {usage:#x}"
        );
    }
}

#[test]
fn the_laptops_radio_button() {
    let fmt = KeyboardFormat::parse(GBGO_KEYBOARD).unwrap();
    assert_eq!(fmt.radio, Some((6, 0)));
    let mut kb = Keyboard::new(fmt);
    let mut ev = Vec::new();
    kb.feed(&[6, 0x01], |e| ev.push(e));
    kb.feed(&[6, 0x01], |e| ev.push(e));
    kb.feed(&[6, 0x00], |e| ev.push(e));
    assert_eq!(ev.iter().map(|e| (e.code, e.pressed)).collect::<Vec<_>>(), [(247, true), (247, false)]);
    ev.clear();
    kb.feed(&[5, 1, 2, 3, 4, 5, 6, 7, 8], |e| ev.push(e));
    kb.feed(&[13, 0x01], |e| ev.push(e));
    assert!(ev.is_empty(), "the vendor and system control reports change nothing");
}
