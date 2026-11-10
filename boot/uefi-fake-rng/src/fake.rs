
use core::{
    ffi::c_void,
    ptr::{null, null_mut},
};

type Handle = *mut c_void;
type Status = usize;

const SUCCESS: Status = 0;
const DEVICE_ERROR: Status = (1 << 63) | 7;

/// The real loader, as built.
static LOADER: &[u8] = include_bytes!(env!("FAKE_RNG_LOADER"));
const MODE: &str = env!("FAKE_RNG_MODE");

#[repr(C)]
#[derive(Clone, Copy)]
struct Guid(u32, u16, u16, [u8; 8]);

const RNG_PROTOCOL: Guid = Guid(0x3152_bca5, 0xeade, 0x433d, [0x86, 0x2e, 0xc0, 0x1c, 0xdc, 0x29, 0x1f, 0x44]);
const LOADED_IMAGE: Guid = Guid(0x5b1b_31a1, 0x9562, 0x11d2, [0x8e, 0x3f, 0x00, 0xa0, 0xc9, 0x69, 0x72, 0x3b]);

#[repr(C)]
struct TextOut {
    reset: usize,
    output_string: unsafe extern "efiapi" fn(*mut TextOut, *const u16) -> Status,
}

/// Only the fields in use; the header is 24 bytes.
#[repr(C)]
struct SystemTable {
    header: [u64; 3],
    firmware_vendor: usize,
    firmware_revision: u64,
    console_in_handle: usize,
    console_in: usize,
    console_out_handle: usize,
    console_out: *mut TextOut,
    standard_error_handle: usize,
    standard_error: usize,
    runtime_services: usize,
    boot_services: *const [usize; 40],
}

#[repr(C)]
struct LoadedImage {
    revision: u32,
    parent_handle: Handle,
    system_table: *mut SystemTable,
    device_handle: Handle,
    file_path: *mut c_void,
}

#[repr(C)]
struct RngProtocol {
    get_info: unsafe extern "efiapi" fn(*mut RngProtocol, *mut usize, *mut Guid) -> Status,
    get_rng: unsafe extern "efiapi" fn(*mut RngProtocol, *const Guid, usize, *mut u8) -> Status,
}

static mut SYSTEM_TABLE: *mut SystemTable = null_mut();
static mut PROTOCOL: RngProtocol = RngProtocol { get_info, get_rng };

unsafe extern "efiapi" fn get_info(_: *mut RngProtocol, _: *mut usize, _: *mut Guid) -> Status {
    DEVICE_ERROR
}

fn print(text: &str) {
    let mut wide = [0u16; 128];
    for (slot, c) in wide.iter_mut().zip(text.encode_utf16()) {
        *slot = c;
    }
    // SAFETY: the firmware's table lives while boot services do, and `wide` ends with a NUL.
    unsafe {
        let out = (*SYSTEM_TABLE).console_out;
        ((*out).output_string)(out, wide.as_ptr());
    }
}

/// Boot service by its index in the table, after the three words of the header.
unsafe fn service(index: usize) -> usize {
    unsafe { (*(*SYSTEM_TABLE).boot_services)[3 + index] }
}

unsafe extern "efiapi" fn get_rng(_: *mut RngProtocol, _: *const Guid, len: usize, out: *mut u8) -> Status {
    print("fake-rng: called\r\n");
    match MODE {
        "hang" => loop {
            core::hint::spin_loop();
        },
        // An address nothing maps.
        "fault" => unsafe {
            core::ptr::read_volatile(0x0000_7fff_ffff_f000 as *const u32);
        },
        "error" => return DEVICE_ERROR,
        "slow" => unsafe {
            let stall: unsafe extern "efiapi" fn(usize) -> Status = core::mem::transmute(service(28));
            stall(8_000_000);
        },
        _ => {}
    }
    for i in 0..len {
        // SAFETY: the caller gave `len` bytes.
        unsafe { out.add(i).write((i as u8).wrapping_mul(37).wrapping_add(11)) };
    }
    SUCCESS
}

#[unsafe(no_mangle)]
unsafe extern "efiapi" fn efi_main(image: Handle, st: *mut SystemTable) -> Status {
    unsafe {
        SYSTEM_TABLE = st;
        print("fake-rng: installed\r\n");
        if MODE != "none" {
            let install: unsafe extern "efiapi" fn(*mut Handle, *const Guid, u32, *mut c_void) -> Status =
                core::mem::transmute(service(13));
            let mut handle: Handle = null_mut();
            let s = install(&mut handle, &RNG_PROTOCOL, 0, (&raw mut PROTOCOL).cast());
            if s != SUCCESS {
                print("fake-rng: install failed\r\n");
                return s;
            }
        }
        let load: unsafe extern "efiapi" fn(u8, Handle, *const c_void, *const u8, usize, *mut Handle) -> Status =
            core::mem::transmute(service(22));
        let start: unsafe extern "efiapi" fn(Handle, *mut usize, *mut *mut u16) -> Status =
            core::mem::transmute(service(23));
        let handle_protocol: unsafe extern "efiapi" fn(Handle, *const Guid, *mut *mut c_void) -> Status =
            core::mem::transmute(service(16));

        let mut child: Handle = null_mut();
        let s = load(0, image, null(), LOADER.as_ptr(), LOADER.len(), &mut child);
        if s != SUCCESS {
            print("fake-rng: load failed\r\n");
            return s;
        }
        let (mut own, mut theirs): (*mut c_void, *mut c_void) = (null_mut(), null_mut());
        handle_protocol(image, &LOADED_IMAGE, &mut own);
        handle_protocol(child, &LOADED_IMAGE, &mut theirs);
        let (own, theirs) = (own as *mut LoadedImage, theirs as *mut LoadedImage);
        (*theirs).device_handle = (*own).device_handle;
        (*theirs).file_path = (*own).file_path;
        start(child, null_mut(), null_mut())
    }
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    loop {
        core::hint::spin_loop();
    }
}
