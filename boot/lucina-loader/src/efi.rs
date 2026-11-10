//! The UEFI calls LucinaLoader makes (UEFI 2.10). Started from LucinaUEFI's `efi.rs` (`boot/lucina-uefi`) and
//! extended with what a boot menu for Linux needs: every FAT volume, the keyboard, GOP modes and Blt, variables and
//! ResetSystem (firmware setup), LoadImage/StartImage, the initrd's LoadFile2 protocol and the device tree
//! configuration table. Only the fields and functions used here are typed; the others are placeholders of the right
//! size. All of the loader's `unsafe` is here.
//!
//! Boot services stay live for the whole life of the loader: the kernel's EFI stub exits them, not the loader.

use core::{
    ffi::c_void,
    fmt,
    ptr::{self, null, null_mut},
    sync::atomic::{AtomicPtr, AtomicUsize, Ordering},
};

pub type Handle = *mut c_void;
pub type Status = usize;
/// LoaderData pages from [`Efi::allocate`], never freed unless given back.
pub type Pages = &'static mut [u8];

pub const SUCCESS: Status = 0;
const ERROR_BIT: usize = 1 << 63;
pub const LOAD_ERROR: Status = ERROR_BIT | 1;
pub const INVALID_PARAMETER: Status = ERROR_BIT | 2;
pub const UNSUPPORTED: Status = ERROR_BIT | 3;
pub const BUFFER_TOO_SMALL: Status = ERROR_BIT | 5;
pub const NOT_FOUND: Status = ERROR_BIT | 14;

pub fn status_name(s: Status) -> &'static str {
    match s & !ERROR_BIT {
        0 => "SUCCESS",
        1 => "LOAD_ERROR",
        2 => "INVALID_PARAMETER",
        3 => "UNSUPPORTED",
        5 => "BUFFER_TOO_SMALL",
        6 => "NOT_READY",
        7 => "DEVICE_ERROR",
        8 => "WRITE_PROTECTED",
        9 => "OUT_OF_RESOURCES",
        14 => "NOT_FOUND",
        15 => "ACCESS_DENIED",
        21 => "ABORTED",
        26 => "SECURITY_VIOLATION",
        _ => "error",
    }
}

#[repr(C)]
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Guid(u32, u16, u16, [u8; 8]);

const LOADED_IMAGE: Guid = Guid(
    0x5b1b_31a1,
    0x9562,
    0x11d2,
    [0x8e, 0x3f, 0x00, 0xa0, 0xc9, 0x69, 0x72, 0x3b],
);
const SIMPLE_FILE_SYSTEM: Guid = Guid(
    0x964e_5b22,
    0x6459,
    0x11d2,
    [0x8e, 0x39, 0x00, 0xa0, 0xc9, 0x69, 0x72, 0x3b],
);
const GRAPHICS_OUTPUT: Guid = Guid(
    0x9042_a9de,
    0x23dc,
    0x4a38,
    [0x96, 0xfb, 0x7a, 0xde, 0xd0, 0x80, 0x51, 0x6a],
);
const BLOCK_IO: Guid = Guid(
    0x964e_5b21,
    0x6459,
    0x11d2,
    [0x8e, 0x39, 0x00, 0xa0, 0xc9, 0x69, 0x72, 0x3b],
);
const DEVICE_PATH: Guid = Guid(
    0x0957_6e91,
    0x6d3f,
    0x11d2,
    [0x8e, 0x39, 0x00, 0xa0, 0xc9, 0x69, 0x72, 0x3b],
);
const LOAD_FILE2: Guid = Guid(
    0x4006_c0c1,
    0xfcb3,
    0x403e,
    [0x99, 0x6d, 0x4a, 0x6c, 0x87, 0x24, 0xe0, 0x6d],
);
/// EFI_GLOBAL_VARIABLE: OsIndications and OsIndicationsSupported.
const GLOBAL_VARIABLE: Guid = Guid(
    0x8be4_df61,
    0x93ca,
    0x11d2,
    [0xaa, 0x0d, 0x00, 0xe0, 0x98, 0x03, 0x2b, 0x8c],
);
/// The flattened device tree configuration table (EFI_DTB_TABLE_GUID).
pub const DTB_TABLE: Guid = Guid(
    0xb1b6_21d5,
    0xf19c,
    0x41a5,
    [0x83, 0x0b, 0xd9, 0x15, 0x2c, 0x69, 0xaa, 0xe0],
);

/// The device path Linux's EFI stub looks up for its initrd (`LINUX_EFI_INITRD_MEDIA_GUID` in
/// `include/linux/efi.h`, `efi_load_initrd_dev_path` in `drivers/firmware/efi/libstub`): one vendor media node
/// (type 4, subtype 3, 20 bytes, GUID 5568e427-68fc-4f3d-ac74-ca555231cc68) and the end node.
static INITRD_PATH: [u8; 24] = [
    0x04, 0x03, 20, 0, 0x27, 0xe4, 0x68, 0x55, 0xfc, 0x68, 0x3d, 0x4f, 0xac, 0x74, 0xca, 0x55,
    0x52, 0x31, 0xcc, 0x68, 0x7f, 0xff, 0x04, 0x00,
];

#[repr(C)]
struct TableHeader {
    signature: u64,
    revision: u32,
    header_size: u32,
    crc32: u32,
    reserved: u32,
}

#[repr(C)]
pub struct SystemTable {
    hdr: TableHeader,
    firmware_vendor: *const u16,
    firmware_revision: u32,
    console_in_handle: Handle,
    con_in: *mut TextInput,
    console_out_handle: Handle,
    con_out: *mut TextOutput,
    standard_error_handle: Handle,
    std_err: *mut TextOutput,
    runtime_services: *mut RuntimeServices,
    boot_services: *mut BootServices,
    number_of_table_entries: usize,
    configuration_table: *const ConfigurationTable,
}

#[repr(C)]
struct ConfigurationTable {
    vendor_guid: Guid,
    vendor_table: *const c_void,
}

#[repr(C)]
struct TextInput {
    reset: unsafe extern "efiapi" fn(*mut TextInput, u8) -> Status,
    read_key_stroke: unsafe extern "efiapi" fn(*mut TextInput, *mut InputKey) -> Status,
    wait_for_key: *mut c_void,
}

#[repr(C)]
#[derive(Default)]
struct InputKey {
    scan_code: u16,
    unicode_char: u16,
}

#[repr(C)]
struct TextOutput {
    reset: usize,
    output_string: unsafe extern "efiapi" fn(*mut TextOutput, *const u16) -> Status,
    test_string: usize,
    query_mode: usize,
    set_mode: usize,
    set_attribute: usize,
    clear_screen: unsafe extern "efiapi" fn(*mut TextOutput) -> Status,
    set_cursor_position: usize,
    enable_cursor: unsafe extern "efiapi" fn(*mut TextOutput, u8) -> Status,
    mode: usize,
}

#[repr(C)]
struct RuntimeServices {
    hdr: TableHeader,
    // get_time, set_time, get_wakeup_time, set_wakeup_time, set_virtual_address_map, convert_pointer
    time_and_virtual: [usize; 6],
    get_variable: unsafe extern "efiapi" fn(
        *const u16,
        *const Guid,
        *mut u32,
        *mut usize,
        *mut c_void,
    ) -> Status,
    get_next_variable_name: usize,
    set_variable:
        unsafe extern "efiapi" fn(*const u16, *const Guid, u32, usize, *const c_void) -> Status,
    get_next_high_monotonic_count: usize,
    reset_system: unsafe extern "efiapi" fn(u32, Status, usize, *const c_void) -> !,
}

#[repr(C)]
struct BootServices {
    hdr: TableHeader,
    raise_tpl: usize,
    restore_tpl: usize,
    allocate_pages: unsafe extern "efiapi" fn(u32, u32, usize, *mut u64) -> Status,
    free_pages: unsafe extern "efiapi" fn(u64, usize) -> Status,
    get_memory_map: usize,
    allocate_pool: usize,
    free_pool: unsafe extern "efiapi" fn(*mut c_void) -> Status,
    // create_event, set_timer, wait_for_event, signal_event, close_event, check_event
    events: [usize; 6],
    install_protocol_interface:
        unsafe extern "efiapi" fn(*mut Handle, *const Guid, u32, *mut c_void) -> Status,
    reinstall_protocol_interface: usize,
    uninstall_protocol_interface:
        unsafe extern "efiapi" fn(Handle, *const Guid, *mut c_void) -> Status,
    handle_protocol: unsafe extern "efiapi" fn(Handle, *const Guid, *mut *mut c_void) -> Status,
    reserved: usize,
    register_protocol_notify: usize,
    locate_handle: usize,
    locate_device_path: usize,
    install_configuration_table: unsafe extern "efiapi" fn(*const Guid, *const c_void) -> Status,
    load_image: unsafe extern "efiapi" fn(
        u8,
        Handle,
        *const c_void,
        *const u8,
        usize,
        *mut Handle,
    ) -> Status,
    start_image: unsafe extern "efiapi" fn(Handle, *mut usize, *mut *mut u16) -> Status,
    exit: usize,
    unload_image: unsafe extern "efiapi" fn(Handle) -> Status,
    exit_boot_services: usize,
    get_next_monotonic_count: usize,
    stall: unsafe extern "efiapi" fn(usize) -> Status,
    set_watchdog_timer: unsafe extern "efiapi" fn(usize, u64, usize, *const u16) -> Status,
    // connect_controller, disconnect_controller, open_protocol, close_protocol, open_protocol_information,
    // protocols_per_handle
    drivers: [usize; 6],
    locate_handle_buffer: unsafe extern "efiapi" fn(
        u32,
        *const Guid,
        *const c_void,
        *mut usize,
        *mut *mut Handle,
    ) -> Status,
    locate_protocol:
        unsafe extern "efiapi" fn(*const Guid, *mut c_void, *mut *mut c_void) -> Status,
}

#[repr(C)]
struct LoadedImage {
    revision: u32,
    parent_handle: Handle,
    system_table: *mut SystemTable,
    device_handle: Handle,
    file_path: *mut c_void,
    reserved: *mut c_void,
    load_options_size: u32,
    load_options: *mut c_void,
    image_base: *mut c_void,
    image_size: u64,
    image_code_type: u32,
    image_data_type: u32,
    unload: usize,
}

#[repr(C)]
struct SimpleFileSystem {
    revision: u64,
    open_volume: unsafe extern "efiapi" fn(*mut SimpleFileSystem, *mut *mut FileProtocol) -> Status,
}

#[repr(C)]
struct FileProtocol {
    revision: u64,
    open: unsafe extern "efiapi" fn(
        *mut FileProtocol,
        *mut *mut FileProtocol,
        *const u16,
        u64,
        u64,
    ) -> Status,
    close: unsafe extern "efiapi" fn(*mut FileProtocol) -> Status,
    delete: usize,
    read: unsafe extern "efiapi" fn(*mut FileProtocol, *mut usize, *mut u8) -> Status,
    write: unsafe extern "efiapi" fn(*mut FileProtocol, *mut usize, *const u8) -> Status,
    get_position: unsafe extern "efiapi" fn(*mut FileProtocol, *mut u64) -> Status,
    set_position: unsafe extern "efiapi" fn(*mut FileProtocol, u64) -> Status,
    get_info: usize,
    set_info: usize,
    flush: unsafe extern "efiapi" fn(*mut FileProtocol) -> Status,
}

#[repr(C)]
struct BlockMedia {
    media_id: u32,
    removable: u8,
    present: u8,
    logical_partition: u8,
    read_only: u8,
    write_caching: u8,
    block_size: u32,
}

#[repr(C)]
struct BlockIo {
    revision: u64,
    media: *const BlockMedia,
    reset: usize,
    read_blocks: unsafe extern "efiapi" fn(*mut BlockIo, u32, u64, usize, *mut u8) -> Status,
}

const FILE_READ: u64 = 0x1;
const FILE_WRITE: u64 = 0x2;

#[repr(C)]
struct GraphicsOutput {
    query_mode: unsafe extern "efiapi" fn(
        *mut GraphicsOutput,
        u32,
        *mut usize,
        *mut *mut GopModeInfo,
    ) -> Status,
    set_mode: unsafe extern "efiapi" fn(*mut GraphicsOutput, u32) -> Status,
    blt: unsafe extern "efiapi" fn(
        *mut GraphicsOutput,
        *mut u32,
        u32,
        usize,
        usize,
        usize,
        usize,
        usize,
        usize,
        usize,
    ) -> Status,
    mode: *const GopMode,
}

#[repr(C)]
struct GopMode {
    max_mode: u32,
    mode: u32,
    info: *const GopModeInfo,
    size_of_info: usize,
    frame_buffer_base: u64,
    frame_buffer_size: usize,
}

#[repr(C)]
struct GopModeInfo {
    version: u32,
    horizontal_resolution: u32,
    vertical_resolution: u32,
    pixel_format: u32,
    masks: [u32; 4],
    pixels_per_scan_line: u32,
}

/// EfiBltBufferToVideo.
const BLT_BUFFER_TO_VIDEO: u32 = 2;
const ALLOCATE_ANY_PAGES: u32 = 0;
const LOADER_DATA: u32 = 2;
/// LocateHandleBuffer's ByProtocol.
const BY_PROTOCOL: u32 = 2;
/// EFI_NATIVE_INTERFACE.
const NATIVE_INTERFACE: u32 = 0;
/// Non-volatile, boot service and runtime access: how OsIndications is written.
const VAR_NV_BS_RT: u32 = 0x7;
/// EfiResetCold.
const RESET_COLD: u32 = 0;

/// The console for the panic handler.
static PANIC_CONSOLE: AtomicPtr<TextOutput> = AtomicPtr::new(null_mut());

/// The initrd LoadFile2 hands out (one at a time).
static INITRD_DATA: AtomicPtr<u8> = AtomicPtr::new(null_mut());
static INITRD_LEN: AtomicUsize = AtomicUsize::new(0);

#[repr(C)]
struct LoadFile2 {
    load_file: unsafe extern "efiapi" fn(
        *mut LoadFile2,
        *mut c_void,
        u8,
        *mut usize,
        *mut c_void,
    ) -> Status,
}

static INITRD_LOAD_FILE: LoadFile2 = LoadFile2 {
    load_file: load_initrd,
};

/// LoadFile2.LoadFile for the initrd device path: the size first (BUFFER_TOO_SMALL), then the bytes. This is the
/// whole protocol the EFI stub uses (`efi_load_initrd_dev_path`).
unsafe extern "efiapi" fn load_initrd(
    _this: *mut LoadFile2,
    _path: *mut c_void,
    boot_policy: u8,
    size: *mut usize,
    buffer: *mut c_void,
) -> Status {
    if size.is_null() {
        return INVALID_PARAMETER;
    }
    if boot_policy != 0 {
        return UNSUPPORTED;
    }
    let data = INITRD_DATA.load(Ordering::Relaxed);
    let len = INITRD_LEN.load(Ordering::Relaxed);
    if data.is_null() {
        return NOT_FOUND;
    }
    // SAFETY: the caller passes a valid size pointer and, when non-null, a buffer of *size bytes; `data` holds `len`
    // bytes of LoaderData pages the loader never frees while the protocol is installed.
    unsafe {
        if buffer.is_null() || *size < len {
            *size = len;
            return BUFFER_TOO_SMALL;
        }
        ptr::copy_nonoverlapping(data, buffer as *mut u8, len);
        *size = len;
    }
    SUCCESS
}

/// Boot services: live for as long as the loader runs.
pub struct Efi {
    image: Handle,
    st: *mut SystemTable,
}

/// A key press: the UEFI scan code and the character.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct KeyPress {
    pub scan: u16,
    pub ch: u16,
}

impl Efi {
    /// # Safety
    ///
    /// `image` and `st` must be what the firmware passed to `efi_main`, and boot services must not have been
    /// exited.
    pub unsafe fn new(image: Handle, st: *mut SystemTable) -> Efi {
        // SAFETY: by the caller, st is the live system table.
        PANIC_CONSOLE.store(unsafe { (*st).con_out }, Ordering::Relaxed);
        Efi { image, st }
    }

    fn bs(&self) -> &BootServices {
        // SAFETY: boot services are live while the loader runs (see the type's documentation).
        unsafe { &*(*self.st).boot_services }
    }

    fn rt(&self) -> &RuntimeServices {
        // SAFETY: runtime services outlive boot services.
        unsafe { &*(*self.st).runtime_services }
    }

    /// Writes `s` to the firmware console (screen and, on QEMU, the serial port), `\n` as CR LF.
    pub fn print(&self, s: &str) {
        // SAFETY: con_out is live while boot services are.
        unsafe { output((*self.st).con_out, s) }
    }

    /// Clears the firmware console and hides its cursor.
    pub fn clear_console(&self) {
        // SAFETY: con_out is live while boot services are; both calls only change the console.
        unsafe {
            let out = (*self.st).con_out;
            if !out.is_null() {
                ((*out).clear_screen)(out);
                ((*out).enable_cursor)(out, 0);
            }
        }
    }

    pub fn disable_watchdog(&self) {
        // SAFETY: a zero timeout disables the watchdog the boot manager armed; no data pointer.
        unsafe { (self.bs().set_watchdog_timer)(0, 0, 0, ptr::null()) };
    }

    pub fn stall_ms(&self, ms: u64) {
        // SAFETY: Stall only waits.
        unsafe { (self.bs().stall)(ms as usize * 1000) };
    }

    /// The next key press waiting, if any.
    pub fn read_key(&self) -> Option<KeyPress> {
        let mut k = InputKey::default();
        // SAFETY: con_in is live while boot services are; ReadKeyStroke writes one InputKey.
        let s = unsafe {
            let cin = (*self.st).con_in;
            if cin.is_null() {
                return None;
            }
            ((*cin).read_key_stroke)(cin, &mut k)
        };
        (s == SUCCESS).then_some(KeyPress {
            scan: k.scan_code,
            ch: k.unicode_char,
        })
    }

    /// `pages` pages of `LoaderData`, zeroed.
    pub fn allocate_pages(&self, pages: usize) -> Result<&'static mut [u8], Status> {
        let mut addr = 0u64;
        // SAFETY: AllocatePages writes the address of the new pages into `addr`.
        let s = unsafe {
            (self.bs().allocate_pages)(ALLOCATE_ANY_PAGES, LOADER_DATA, pages, &mut addr)
        };
        if s != SUCCESS {
            return Err(s);
        }
        let len = pages * 4096;
        // SAFETY: the firmware just gave us these pages; nothing else uses them, and the identity mapping UEFI
        // runs with makes the physical address a valid pointer.
        let buf = unsafe { core::slice::from_raw_parts_mut(addr as usize as *mut u8, len) };
        buf.fill(0);
        Ok(buf)
    }

    /// At least `bytes` bytes of `LoaderData` pages.
    pub fn allocate(&self, bytes: usize) -> Result<&'static mut [u8], Status> {
        self.allocate_pages(bytes.div_ceil(4096).max(1))
    }

    /// `n` pixels (0x00RRGGBB) in LoaderData pages.
    pub fn allocate_pixels(&self, n: usize) -> Result<&'static mut [u32], Status> {
        let b = self.allocate(n * 4)?;
        // SAFETY: the pages are 4096-aligned, hold at least n * 4 bytes and are ours alone; any bits are a u32.
        Ok(unsafe { core::slice::from_raw_parts_mut(b.as_mut_ptr() as *mut u32, n) })
    }

    /// `n` UCS-2 units in LoaderData pages, zeroed.
    pub fn allocate_u16(&self, n: usize) -> Result<&'static mut [u16], Status> {
        let b = self.allocate(n * 2)?;
        // SAFETY: as for allocate_pixels, with 2-byte units.
        Ok(unsafe { core::slice::from_raw_parts_mut(b.as_mut_ptr() as *mut u16, n) })
    }

    /// Gives back pages from [`Efi::allocate`] (or a slice of their start); `buf` must not be used afterwards.
    pub fn free(&self, buf: &'static mut [u8]) {
        let (addr, pages) = (buf.as_ptr() as u64, buf.len().div_ceil(4096).max(1));
        // SAFETY: by the caller, these are whole pages from AllocatePages that nothing refers to any more.
        unsafe { (self.bs().free_pages)(addr, pages) };
    }

    /// The configuration table with `guid`.
    pub fn config_table(&self, guid: &Guid) -> Option<*const c_void> {
        // SAFETY: the system table holds `number_of_table_entries` entries.
        unsafe {
            let st = &*self.st;
            (0..st.number_of_table_entries)
                .map(|i| &*st.configuration_table.add(i))
                .find(|t| t.vendor_guid == *guid)
                .map(|t| t.vendor_table)
        }
    }

    /// Installs (or, with null, removes) a configuration table. The table must stay in memory: LoaderData pages
    /// from [`Efi::allocate`] that are never freed.
    pub fn install_config_table(&self, guid: &Guid, table: *const c_void) -> Result<(), Status> {
        // SAFETY: the firmware copies the GUID and keeps the pointer; by the caller the table outlives the loader.
        let s = unsafe { (self.bs().install_configuration_table)(guid, table) };
        if s == SUCCESS { Ok(()) } else { Err(s) }
    }

    fn loaded_image(&self, handle: Handle) -> Option<*mut LoadedImage> {
        let mut iface: *mut c_void = null_mut();
        // SAFETY: HandleProtocol on an image handle; the protocol outlives this call.
        let s = unsafe { (self.bs().handle_protocol)(handle, &LOADED_IMAGE, &mut iface) };
        (s == SUCCESS && !iface.is_null()).then_some(iface as *mut LoadedImage)
    }

    /// Handles of the FAT volumes: the one the loader was started from first, then the others in the firmware's
    /// order. Returns how many were written into `out`.
    pub fn volumes(&self, out: &mut [Handle]) -> usize {
        let mut n = 0;
        if let Some(li) = self.loaded_image(self.image) {
            // SAFETY: our own loaded image protocol.
            let dev = unsafe { (*li).device_handle };
            if !dev.is_null() && !out.is_empty() {
                out[0] = dev;
                n = 1;
            }
        }
        let (mut count, mut buf): (usize, *mut Handle) = (0, null_mut());
        // SAFETY: LocateHandleBuffer allocates `buf` (count handles) from pool; we free it below.
        unsafe {
            let s = (self.bs().locate_handle_buffer)(
                BY_PROTOCOL,
                &SIMPLE_FILE_SYSTEM,
                null(),
                &mut count,
                &mut buf,
            );
            if s != SUCCESS || buf.is_null() {
                return n;
            }
            for &h in core::slice::from_raw_parts(buf, count) {
                if n < out.len() && !out[..n].contains(&h) {
                    out[n] = h;
                    n += 1;
                }
            }
            (self.bs().free_pool)(buf as *mut c_void);
        }
        n
    }

    /// The first block of a volume (its boot sector), in LoaderData pages the caller gives back.
    pub fn first_block(&self, handle: Handle) -> Result<Pages, Status> {
        let mut iface: *mut c_void = null_mut();
        // SAFETY: HandleProtocol writes the interface; the media structure lives as long as the protocol, and
        // ReadBlocks writes one block (block_size bytes) into the buffer, whose page alignment suits any device.
        unsafe {
            let s = (self.bs().handle_protocol)(handle, &BLOCK_IO, &mut iface);
            if s != SUCCESS || iface.is_null() {
                return Err(if s == SUCCESS { NOT_FOUND } else { s });
            }
            let io = iface as *mut BlockIo;
            let media = (*io).media.as_ref().ok_or(NOT_FOUND)?;
            let size = media.block_size as usize;
            let buf = self.allocate(size)?;
            let s = ((*io).read_blocks)(io, media.media_id, 0, size, buf.as_mut_ptr());
            if s != SUCCESS {
                self.free(buf);
                return Err(s);
            }
            Ok(&mut buf[..size])
        }
    }

    /// The root directory of a FAT volume.
    pub fn open_volume(&self, handle: Handle) -> Result<Dir<'_>, Status> {
        let mut sfs: *mut c_void = null_mut();
        let mut root: *mut FileProtocol = null_mut();
        // SAFETY: the handle comes from the firmware; OpenVolume writes a file handle.
        unsafe {
            let s = (self.bs().handle_protocol)(handle, &SIMPLE_FILE_SYSTEM, &mut sfs);
            if s != SUCCESS || sfs.is_null() {
                return Err(if s == SUCCESS { NOT_FOUND } else { s });
            }
            let sfs = sfs as *mut SimpleFileSystem;
            let s = ((*sfs).open_volume)(sfs, &mut root);
            if s != SUCCESS || root.is_null() {
                return Err(if s == SUCCESS { NOT_FOUND } else { s });
            }
        }
        Ok(Dir { root, _efi: self })
    }

    /// The firmware's graphics output, switched to the first of `modes` (width, height) it offers; without a
    /// match the current mode stays. `None` when there is no GOP.
    pub fn display(&self, modes: impl Iterator<Item = (u32, u32)>) -> Option<Display<'_>> {
        let mut iface: *mut c_void = null_mut();
        // SAFETY: LocateProtocol writes an interface pointer; the GOP structures are firmware-owned and live while
        // boot services are. QueryMode allocates its info from pool, which we free.
        unsafe {
            if (self.bs().locate_protocol)(&GRAPHICS_OUTPUT, null_mut(), &mut iface) != SUCCESS
                || iface.is_null()
            {
                return None;
            }
            let gop = iface as *mut GraphicsOutput;
            let max = (*gop).mode.as_ref()?.max_mode;
            'want: for (w, h) in modes {
                for m in 0..max {
                    let (mut size, mut info) = (0usize, null_mut::<GopModeInfo>());
                    if ((*gop).query_mode)(gop, m, &mut size, &mut info) != SUCCESS
                        || info.is_null()
                    {
                        continue;
                    }
                    let fits =
                        (*info).horizontal_resolution == w && (*info).vertical_resolution == h;
                    (self.bs().free_pool)(info as *mut c_void);
                    if fits {
                        if (*(*gop).mode).mode != m {
                            ((*gop).set_mode)(gop, m);
                        }
                        break 'want;
                    }
                }
            }
            let info = (*gop).mode.as_ref()?.info.as_ref()?;
            let (width, height) = (info.horizontal_resolution, info.vertical_resolution);
            (width > 0 && height > 0).then_some(Display {
                gop,
                width,
                height,
                _efi: self,
            })
        }
    }

    /// Reads a UEFI global variable of up to 8 bytes as a little-endian number.
    pub fn global_u64(&self, name: &str) -> Option<u64> {
        let mut n = [0u16; 64];
        ucs2(name, &mut n).ok()?;
        let mut bytes = [0u8; 8];
        let mut size = bytes.len();
        // SAFETY: GetVariable writes at most `size` bytes into `bytes`.
        let s = unsafe {
            (self.rt().get_variable)(
                n.as_ptr(),
                &GLOBAL_VARIABLE,
                null_mut(),
                &mut size,
                bytes.as_mut_ptr().cast(),
            )
        };
        (s == SUCCESS).then(|| u64::from_le_bytes(bytes))
    }

    /// Writes a UEFI global variable as 8 bytes (non-volatile, boot and runtime access).
    pub fn set_global_u64(&self, name: &str, value: u64) -> Result<(), Status> {
        let mut n = [0u16; 64];
        ucs2(name, &mut n)?;
        let bytes = value.to_le_bytes();
        // SAFETY: SetVariable reads `bytes.len()` bytes.
        let s = unsafe {
            (self.rt().set_variable)(
                n.as_ptr(),
                &GLOBAL_VARIABLE,
                VAR_NV_BS_RT,
                bytes.len(),
                bytes.as_ptr().cast(),
            )
        };
        if s == SUCCESS { Ok(()) } else { Err(s) }
    }

    /// Restarts the machine (cold).
    pub fn reset_cold(&self) -> ! {
        // SAFETY: ResetSystem does not return.
        unsafe { (self.rt().reset_system)(RESET_COLD, SUCCESS, 0, null()) }
    }

    /// LoadImage from memory (no device path, as from a buffer): the firmware checks and relocates the PE image.
    pub fn load_image(&self, data: &[u8]) -> Result<Handle, Status> {
        let mut child: Handle = null_mut();
        // SAFETY: LoadImage reads `data.len()` bytes and writes the new image handle.
        let s = unsafe {
            (self.bs().load_image)(0, self.image, null(), data.as_ptr(), data.len(), &mut child)
        };
        if s == SUCCESS && !child.is_null() {
            Ok(child)
        } else {
            Err(s)
        }
    }

    /// Gives a loaded image its load options (the kernel command line, UCS-2 with a NUL). `options` must stay in
    /// memory until the image has read it.
    pub fn set_load_options(&self, child: Handle, options: &'static [u16]) -> Result<(), Status> {
        let li = self.loaded_image(child).ok_or(NOT_FOUND)?;
        // SAFETY: the child's loaded image protocol is ours to fill in before StartImage.
        unsafe {
            (*li).load_options = options.as_ptr() as *mut c_void;
            (*li).load_options_size = (options.len() * 2) as u32;
        }
        Ok(())
    }

    /// StartImage: returns only when the image exits (a kernel that boots never does).
    pub fn start_image(&self, child: Handle) -> Status {
        // SAFETY: a handle from LoadImage; no exit data wanted.
        unsafe { (self.bs().start_image)(child, null_mut(), null_mut()) }
    }

    pub fn unload_image(&self, child: Handle) {
        // SAFETY: a handle from LoadImage that did not start or has returned.
        unsafe { (self.bs().unload_image)(child) };
    }

    /// Publishes `data` as the initrd: a new handle with the initrd device path and LoadFile2. The pages stay the
    /// initrd's until [`Efi::uninstall_initrd`]; they come back either way.
    pub fn install_initrd(&self, data: Pages) -> Result<(Handle, Pages), (Status, Pages)> {
        INITRD_DATA.store(data.as_mut_ptr(), Ordering::Relaxed);
        INITRD_LEN.store(data.len(), Ordering::Relaxed);
        let mut h: Handle = null_mut();
        // SAFETY: both interfaces are statics; a null handle asks for a new one.
        let s = unsafe {
            let path = INITRD_PATH.as_ptr() as *mut c_void;
            let s = (self.bs().install_protocol_interface)(
                &mut h,
                &DEVICE_PATH,
                NATIVE_INTERFACE,
                path,
            );
            if s == SUCCESS {
                let lf2 = &INITRD_LOAD_FILE as *const LoadFile2 as *mut c_void;
                let s2 = (self.bs().install_protocol_interface)(
                    &mut h,
                    &LOAD_FILE2,
                    NATIVE_INTERFACE,
                    lf2,
                );
                if s2 != SUCCESS {
                    (self.bs().uninstall_protocol_interface)(h, &DEVICE_PATH, path);
                }
                s2
            } else {
                s
            }
        };
        if s == SUCCESS {
            Ok((h, data))
        } else {
            INITRD_DATA.store(null_mut(), Ordering::Relaxed);
            INITRD_LEN.store(0, Ordering::Relaxed);
            Err((s, data))
        }
    }

    pub fn uninstall_initrd(&self, h: Handle) {
        // SAFETY: the interfaces [`Efi::install_initrd`] installed on `h`.
        unsafe {
            let lf2 = &INITRD_LOAD_FILE as *const LoadFile2 as *mut c_void;
            (self.bs().uninstall_protocol_interface)(h, &LOAD_FILE2, lf2);
            (self.bs().uninstall_protocol_interface)(
                h,
                &DEVICE_PATH,
                INITRD_PATH.as_ptr() as *mut c_void,
            );
        }
        INITRD_DATA.store(null_mut(), Ordering::Relaxed);
        INITRD_LEN.store(0, Ordering::Relaxed);
    }
}

/// The GOP in the mode [`Efi::display`] chose.
pub struct Display<'e> {
    gop: *mut GraphicsOutput,
    pub width: u32,
    pub height: u32,
    _efi: &'e Efi,
}

impl Display<'_> {
    /// Copies a whole screen of pixels (0x00RRGGBB, `width * height`) to the display with Blt, which works whatever
    /// the framebuffer's pixel format (and on BLT-only GOPs).
    pub fn show(&self, px: &mut [u32]) {
        let (w, h) = (self.width as usize, self.height as usize);
        if px.len() < w * h {
            return;
        }
        // SAFETY: Blt reads w*h pixels of 4 bytes (EFI_GRAPHICS_OUTPUT_BLT_PIXEL: blue, green, red, reserved, which
        // is 0x00RRGGBB little-endian) with a row stride of w*4 bytes.
        unsafe {
            ((*self.gop).blt)(
                self.gop,
                px.as_mut_ptr(),
                BLT_BUFFER_TO_VIDEO,
                0,
                0,
                0,
                0,
                w,
                h,
                w * 4,
            );
        }
    }
}

/// UCS-2 with a NUL; `/` becomes `\` (UEFI paths). Non-ASCII is refused.
fn ucs2(s: &str, out: &mut [u16]) -> Result<(), Status> {
    if s.len() >= out.len() || !s.is_ascii() {
        return Err(NOT_FOUND);
    }
    for (o, b) in out.iter_mut().zip(s.bytes()) {
        *o = if b == b'/' {
            u16::from(b'\\')
        } else {
            u16::from(b)
        };
    }
    out[s.len()] = 0;
    Ok(())
}

/// A volume's root directory; files are opened by absolute path (`/lucinaos/boot.toml`).
pub struct Dir<'e> {
    root: *mut FileProtocol,
    _efi: &'e Efi,
}

/// An open file.
pub struct File<'d> {
    f: *mut FileProtocol,
    _dir: &'d Dir<'d>,
}

impl Dir<'_> {
    fn open_mode(&self, path: &str, mode: u64) -> Result<File<'_>, Status> {
        let mut name = [0u16; 260];
        ucs2(path, &mut name)?;
        let mut f: *mut FileProtocol = null_mut();
        // SAFETY: root is an open directory; Open writes a new handle.
        let s = unsafe { ((*self.root).open)(self.root, &mut f, name.as_ptr(), mode, 0) };
        if s != SUCCESS || f.is_null() {
            return Err(if s == SUCCESS { NOT_FOUND } else { s });
        }
        Ok(File { f, _dir: self })
    }

    pub fn open(&self, path: &str) -> Result<File<'_>, Status> {
        self.open_mode(path, FILE_READ)
    }

    /// Writes `data` over the start of an existing file and flushes it; the file keeps its size when `data` is as
    /// long (GRUB's `save_env` rewrites its 1 KiB environment block the same way, without touching the FAT).
    pub fn overwrite(&self, path: &str, data: &[u8]) -> Result<(), Status> {
        let file = self.open_mode(path, FILE_READ | FILE_WRITE)?;
        let mut len = data.len();
        // SAFETY: an open file handle; Write reads `len` bytes from data.
        unsafe {
            let s = ((*file.f).set_position)(file.f, 0);
            if s != SUCCESS {
                return Err(s);
            }
            let s = ((*file.f).write)(file.f, &mut len, data.as_ptr());
            if s != SUCCESS {
                return Err(s);
            }
            if len != data.len() {
                return Err(LOAD_ERROR);
            }
            let s = ((*file.f).flush)(file.f);
            if s != SUCCESS { Err(s) } else { Ok(()) }
        }
    }

    /// GRUB's `[ -f path ]`: the file can be opened.
    pub fn exists(&self, path: &str) -> bool {
        self.open(path).is_ok()
    }
}

impl Drop for Dir<'_> {
    fn drop(&mut self) {
        // SAFETY: closing our own handle once.
        unsafe { ((*self.root).close)(self.root) };
    }
}

impl File<'_> {
    pub fn size(&self) -> Result<u64, Status> {
        let mut pos = 0u64;
        // SAFETY: SetPosition(u64::MAX) moves to the end of a file; GetPosition reads it back.
        unsafe {
            let s = ((*self.f).set_position)(self.f, u64::MAX);
            if s != SUCCESS {
                return Err(s);
            }
            let s = ((*self.f).get_position)(self.f, &mut pos);
            if s != SUCCESS {
                return Err(s);
            }
            let s = ((*self.f).set_position)(self.f, 0);
            if s != SUCCESS {
                return Err(s);
            }
        }
        Ok(pos)
    }

    /// Fills `buf` from the current position; fails if the file ends first.
    pub fn read_exact(&self, buf: &mut [u8]) -> Result<(), Status> {
        let mut done = 0;
        while done < buf.len() {
            let mut len = buf.len() - done;
            // SAFETY: Read writes at most `len` bytes at the pointer.
            let s = unsafe { ((*self.f).read)(self.f, &mut len, buf[done..].as_mut_ptr()) };
            if s != SUCCESS {
                return Err(s);
            }
            if len == 0 {
                return Err(LOAD_ERROR);
            }
            done += len;
        }
        Ok(())
    }
}

impl Drop for File<'_> {
    fn drop(&mut self) {
        // SAFETY: closing our own handle once.
        unsafe { ((*self.f).close)(self.f) };
    }
}

/// Writes `s` to a firmware console in chunks of UCS-2, `\n` as CR LF.
///
/// # Safety
///
/// `out` must be null or the live console of running boot services.
unsafe fn output(out: *mut TextOutput, s: &str) {
    if out.is_null() {
        return;
    }
    let mut buf = [0u16; 129];
    let mut n = 0;
    let mut chars = s.chars().peekable();
    while chars.peek().is_some() {
        while n + 2 < 128 {
            let Some(c) = chars.next() else { break };
            if c == '\n' {
                buf[n] = u16::from(b'\r');
                n += 1;
            }
            buf[n] = if (c as u32) < 0xffff {
                c as u16
            } else {
                u16::from(b'?')
            };
            n += 1;
        }
        buf[n] = 0;
        // SAFETY: by the caller, out is live; buf is NUL-terminated.
        unsafe { ((*out).output_string)(out, buf.as_ptr()) };
        n = 0;
    }
}

/// For the panic handler: prints to the firmware console.
pub struct PanicConsole;

impl fmt::Write for PanicConsole {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        // SAFETY: the console stays live while the loader runs (it never exits boot services).
        unsafe { output(PANIC_CONSOLE.load(Ordering::Relaxed), s) };
        Ok(())
    }
}
