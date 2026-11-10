//! A stand-in for the Qualcomm watchdog on QEMU, which has no such device: the kernel's registers are plain guest RAM in a file
//! that QEMU shares (`memory-backend-file,share=on`), and this program, on the host, acts on them like the hardware. It clears
//! `RST` after a pet, counts down from each pet, reports the bark's time, and ends at the bite for the test script to do QEMU's
//! `system_reset`. It cannot raise the bark interrupt, and it knows nothing of what Qualcomm's firmware or hypervisor do.
//!
//! usage: qemu-model <guest RAM file> <offset of the registers in it>

use std::{
    fs::OpenOptions,
    io::Write,
    os::unix::fs::FileExt,
    thread::sleep,
    time::{Duration, Instant},
};

use lucina_wdt::qcom::{BARK_TIME, BITE_TIME, DEFAULT_RATE, EN, ENABLE, RST, STS};

fn main() {
    let mut args = std::env::args().skip(1);
    let path = args
        .next()
        .expect("usage: qemu-model <guest RAM file> <offset>");
    let base: u64 = args
        .next()
        .and_then(|s| u64::from_str_radix(s.trim_start_matches("0x"), 16).ok())
        .expect("the offset in hexadecimal");
    let ram = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&path)
        .expect("open the guest RAM file");
    let read = |reg: usize| {
        let mut word = [0u8; 4];
        ram.read_exact_at(&mut word, base + reg as u64)
            .expect("read a register");
        u32::from_le_bytes(word)
    };
    let write = |reg: usize, value: u32| {
        ram.write_all_at(&value.to_le_bytes(), base + reg as u64)
            .expect("write a register");
    };

    let mut last_pet = Instant::now();
    let mut pets = 0u64;
    let mut was_enabled = false;
    let mut barked = false;
    loop {
        let enabled = read(EN) & ENABLE != 0;
        if enabled != was_enabled {
            println!(
                "wdt-model: {} bark {} bite {}",
                if enabled { "enabled" } else { "disabled" },
                read(BARK_TIME),
                read(BITE_TIME)
            );
            was_enabled = enabled;
            last_pet = Instant::now();
            barked = false;
        }
        if read(RST) != 0 {
            write(RST, 0);
            last_pet = Instant::now();
            pets += 1;
            barked = false;
            if pets == 1 || pets.is_multiple_of(5) {
                println!("wdt-model: pet {pets}");
            }
        }
        if enabled {
            let seconds = last_pet.elapsed().as_secs_f64();
            if !barked && seconds >= f64::from(read(BARK_TIME)) / f64::from(DEFAULT_RATE) {
                println!(
                    "wdt-model: bark after {seconds:.1} s (no interrupt: QEMU has none to raise)"
                );
                barked = true;
            }
            if seconds >= f64::from(read(BITE_TIME)) / f64::from(DEFAULT_RATE) {
                write(STS, 1);
                write(EN, 0);
                println!("wdt-model: bite after {seconds:.1} s, {pets} pets");
                std::io::stdout().flush().expect("flush");
                return;
            }
        }
        std::io::stdout().flush().expect("flush");
        sleep(Duration::from_millis(100));
    }
}
