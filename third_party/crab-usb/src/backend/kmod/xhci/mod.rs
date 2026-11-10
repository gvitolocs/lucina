pub(crate) mod cmd;
mod context;
mod def;
pub(crate) mod device;
mod endpoint;
mod event;
mod quarantine;
pub(crate) mod host;
pub(crate) mod hub;
mod reg;
mod ring;
mod sync;
mod transfer;

pub(crate) use def::*;
pub use reg::Write64Order;
pub use device::Device;
pub use host::Xhci;
use usb_if::host::hub::Speed;

fn parse_default_max_packet_size_from_port_speed(speed: Speed) -> u16 {
    // Default EP0 max packet by speed, as Linux xhci-mem.c:1119-1138 (FS 64, LS 8, HS 64,
    // SS 512, SSP 512).
    match speed {
        Speed::Full => 64,             // Full Speed → 64 bytes
        Speed::Low => 8,               // Low Speed → 8 bytes
        Speed::High => 64,             // High Speed → 64 bytes
        Speed::SuperSpeed => 512,      // SuperSpeed → 512 bytes
        Speed::SuperSpeedPlus => 512,  // SuperSpeedPlus → 512 bytes (Linux xhci-mem.c:1121)
        Speed::Wireless => unimplemented!("Wireless"),
    }
}
