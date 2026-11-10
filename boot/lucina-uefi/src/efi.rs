//! The UEFI calls LucinaUEFI makes (UEFI 2.10): console output, pages, the memory map, ExitBootServices, the
//! configuration tables, GOP and the FAT volume the loader was started from. Only the fields and functions used
//! here are typed; the others are placeholders of the right size. All of the loader's firmware `unsafe` is here.
//!
//! [`Efi`] is consumed by [`Efi::exit_boot_services`], so no boot service can be called after a successful exit.
//! The pages it hands out are `LoaderData` and stay valid after the exit: they are what the kernel is given.

use core::{
    ffi::c_void,
    fmt,
    ptr::{self, null_mut},
    sync::atomic::{AtomicPtr, Ordering},
};

use lucina_boot::uefi::{self, Gop, MemDesc};

pub type Handle = *mut c_void;
pub type Status = usize;

pub const SUCCESS: Status = 0;
const ERROR_BIT: usize = 1 << 63;
pub const LOAD_ERROR: Status = ERROR_BIT | 1;
pub const NOT_FOUND: Status = ERROR_BIT | 14;
const UNSUPPORTED: Status = ERROR_BIT | 3;

pub fn status_name(s: Status) -> &'static str {
    match s & !ERROR_BIT {
        0 => "SUCCESS",
        1 => "LOAD_ERROR",
        2 => "INVALID_PARAMETER",
        3 => "UNSUPPORTED",
        5 => "BUFFER_TOO_SMALL",
        7 => "DEVICE_ERROR",
        8 => "WRITE_PROTECTED",
        9 => "OUT_OF_RESOURCES",
        14 => "NOT_FOUND",
        15 => "ACCESS_DENIED",
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
const RNG_PROTOCOL: Guid = Guid(
    0x3152_bca5,
    0xeade,
    0x433d,
    [0x86, 0x2e, 0xc0, 0x1c, 0xdc, 0x29, 0x1f, 0x44],
);
/// `EFI_RNG_ALGORITHM_RAW`: the noise source itself, if the firmware will say so.
const RNG_ALGORITHM_RAW: Guid = Guid(
    0xe431_76d7,
    0xb6e8,
    0x4827,
    [0xb7, 0x84, 0x7f, 0xfd, 0xc4, 0xb6, 0x85, 0x61],
);
/// The flattened device tree configuration table (EFI_DTB_TABLE_GUID).
pub const DTB_TABLE: Guid = Guid(
    0xb1b6_21d5,
    0xf19c,
    0x41a5,
    [0x83, 0x0b, 0xd9, 0x15, 0x2c, 0x69, 0xaa, 0xe0],
);
/// ACPI 2.0+ RSDP (EFI_ACPI_20_TABLE_GUID).
pub const ACPI20_TABLE: Guid = Guid(
    0x8868_e871,
    0xe4f1,
    0x11d3,
    [0xbc, 0x22, 0x00, 0x80, 0xc7, 0x3c, 0x88, 0x81],
);
/// SMBIOS 3 entry point (SMBIOS3_TABLE_GUID).
pub const SMBIOS3_TABLE: Guid = Guid(
    0xf2fd_1544,
    0x9794,
    0x4a2c,
    [0x99, 0x2e, 0xe5, 0xbb, 0xcf, 0x20, 0xe3, 0x94],
);

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
    con_in: *mut c_void,
    console_out_handle: Handle,
    con_out: *mut TextOutput,
    standard_error_handle: Handle,
    std_err: *mut TextOutput,
    runtime_services: *mut c_void,
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
struct TextOutput {
    reset: usize,
    output_string: unsafe extern "efiapi" fn(*mut TextOutput, *const u16) -> Status,
    // test_string, query_mode, set_mode, set_attribute, clear_screen, set_cursor_position, enable_cursor, mode
    rest: [usize; 8],
}

#[repr(C)]
struct BootServices {
    hdr: TableHeader,
    raise_tpl: usize,
    restore_tpl: usize,
    allocate_pages: unsafe extern "efiapi" fn(u32, u32, usize, *mut u64) -> Status,
    free_pages: unsafe extern "efiapi" fn(u64, usize) -> Status,
    get_memory_map:
        unsafe extern "efiapi" fn(*mut usize, *mut u8, *mut usize, *mut usize, *mut u32) -> Status,
    allocate_pool: usize,
    free_pool: usize,
    // create_event .. uninstall_protocol_interface
    events_and_protocols: [usize; 9],
    handle_protocol: unsafe extern "efiapi" fn(Handle, *const Guid, *mut *mut c_void) -> Status,
    // reserved, register_protocol_notify, locate_handle, locate_device_path, install_configuration_table,
    // load_image, start_image, exit, unload_image
    images: [usize; 9],
    exit_boot_services: unsafe extern "efiapi" fn(Handle, usize) -> Status,
    get_next_monotonic_count: usize,
    stall: unsafe extern "efiapi" fn(usize) -> Status,
    set_watchdog_timer: unsafe extern "efiapi" fn(usize, u64, usize, *const u16) -> Status,
    // connect_controller .. protocols_per_handle
    drivers: [usize; 6],
    locate_handle_buffer: unsafe extern "efiapi" fn(
        u32,
        *const Guid,
        *mut c_void,
        *mut usize,
        *mut *mut Handle,
    ) -> Status,
    locate_protocol:
        unsafe extern "efiapi" fn(*const Guid, *mut c_void, *mut *mut c_void) -> Status,
}

#[repr(C)]
struct RngProtocol {
    get_info: usize,
    get_rng: unsafe extern "efiapi" fn(*mut RngProtocol, *const Guid, usize, *mut u8) -> Status,
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
    delete: unsafe extern "efiapi" fn(*mut FileProtocol) -> Status,
    read: unsafe extern "efiapi" fn(*mut FileProtocol, *mut usize, *mut u8) -> Status,
    write: unsafe extern "efiapi" fn(*mut FileProtocol, *mut usize, *const u8) -> Status,
    get_position: unsafe extern "efiapi" fn(*mut FileProtocol, *mut u64) -> Status,
    set_position: unsafe extern "efiapi" fn(*mut FileProtocol, u64) -> Status,
    get_info: usize,
    set_info: usize,
    flush: unsafe extern "efiapi" fn(*mut FileProtocol) -> Status,
}

const FILE_READ: u64 = 0x1;
const FILE_WRITE: u64 = 0x2;
const FILE_CREATE: u64 = 0x8000_0000_0000_0000;

#[repr(C)]
struct GraphicsOutput {
    query_mode: usize,
    set_mode: usize,
    blt: usize,
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

const ALLOCATE_ANY_PAGES: u32 = 0;
const ALLOCATE_MAX_ADDRESS: u32 = 1;
const ALLOCATE_ADDRESS: u32 = 2;
const LOADER_DATA: u32 = 2;

/// Where [`Efi::allocate_pages`] may put the pages.
#[derive(Clone, Copy)]
pub enum Place {
    Anywhere,
    Below(u64),
    At(u64),
}

/// The console for the panic handler; null once boot services are gone.
static PANIC_CONSOLE: AtomicPtr<TextOutput> = AtomicPtr::new(null_mut());

/// Boot services, until [`Efi::exit_boot_services`].
pub struct Efi {
    image: Handle,
    st: *mut SystemTable,
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
        // SAFETY: boot services are live while self exists (see the type's documentation).
        unsafe { &*(*self.st).boot_services }
    }

    /// Writes `s` to the firmware console (screen and, on QEMU, the serial port), `\n` as CR LF.
    pub fn print(&self, s: &str) {
        // SAFETY: con_out is live while boot services are.
        unsafe { output((*self.st).con_out, s) }
    }

    /// The firmware vendor string, ASCII only (anything else as `?`), and the firmware revision.
    pub fn vendor(&self, out: &mut [u8]) -> (usize, u32) {
        // SAFETY: the system table and its vendor string (NUL-terminated UCS-2) live as long as boot services.
        unsafe {
            let st = &*self.st;
            let mut n = 0;
            let mut p = st.firmware_vendor;
            while !p.is_null() && *p != 0 && n < out.len() {
                let c = *p;
                out[n] = if (0x20..0x7f).contains(&c) {
                    c as u8
                } else {
                    b'?'
                };
                n += 1;
                p = p.add(1);
            }
            (n, st.firmware_revision)
        }
    }

    pub fn disable_watchdog(&self) {
        // SAFETY: a zero timeout disables the watchdog the boot manager armed; no data pointer.
        unsafe { (self.bs().set_watchdog_timer)(0, 0, 0, ptr::null()) };
    }

    /// Whether the firmware has `EFI_RNG_PROTOCOL` at all.
    pub fn has_random_protocol(&self) -> bool {
        let mut iface: *mut c_void = null_mut();
        // SAFETY: LocateProtocol writes an interface pointer.
        unsafe { (self.bs().locate_protocol)(&RNG_PROTOCOL, null_mut(), &mut iface) == SUCCESS && !iface.is_null() }
    }

    /// Restarts the machine after this long, unless disabled or armed again before.
    pub fn arm_watchdog(&self, seconds: usize) {
        // SAFETY: arms the firmware's watchdog; no data pointer.
        unsafe { (self.bs().set_watchdog_timer)(seconds, 0, 0, ptr::null()) };
    }

    /// Random bytes from `EFI_RNG_PROTOCOL`, as `efi_random_get_seed` of Linux's EFI stub asks: the raw algorithm, then the
    /// default one. Returns whether it was raw. The firmware's own read loop has no time limit: the caller keeps the firmware's
    /// watchdog running around this, and leaves a mark on the disk first (`rng_marker`).
    pub fn random_bytes(&self, out: &mut [u8]) -> Result<bool, Status> {
        let mut iface: *mut c_void = null_mut();
        // SAFETY: LocateProtocol writes an interface pointer; GetRNG writes `out.len()` bytes to `out`.
        unsafe {
            let status = (self.bs().locate_protocol)(&RNG_PROTOCOL, null_mut(), &mut iface);
            if status != SUCCESS {
                return Err(status);
            }
            if iface.is_null() {
                return Err(NOT_FOUND);
            }
            let rng = iface as *mut RngProtocol;
            let mut raw = true;
            let mut status = ((*rng).get_rng)(rng, &RNG_ALGORITHM_RAW, out.len(), out.as_mut_ptr());
            if status == UNSUPPORTED {
                raw = false;
                status = ((*rng).get_rng)(rng, ptr::null(), out.len(), out.as_mut_ptr());
            }
            if status == SUCCESS { Ok(raw) } else { Err(status) }
        }
    }

    pub fn stall_ms(&self, ms: usize) {
        // SAFETY: Stall only waits.
        unsafe { (self.bs().stall)(ms * 1000) };
    }

    /// `pages` pages of `LoaderData`, zeroed.
    pub fn allocate_pages(&self, place: Place, pages: usize) -> Result<&'static mut [u8], Status> {
        let (kind, mut addr) = match place {
            Place::Anywhere => (ALLOCATE_ANY_PAGES, 0),
            Place::Below(max) => (ALLOCATE_MAX_ADDRESS, max),
            Place::At(a) => (ALLOCATE_ADDRESS, a),
        };
        // SAFETY: AllocatePages writes the address of the new pages into `addr`.
        let s = unsafe { (self.bs().allocate_pages)(kind, LOADER_DATA, pages, &mut addr) };
        if s != SUCCESS {
            return Err(s);
        }
        let len = pages * 4096;
        // SAFETY: the firmware just gave us these pages; nothing else uses them, and the identity mapping UEFI
        // runs with makes the physical address a valid pointer. LoaderData stays ours after ExitBootServices.
        let buf = unsafe { core::slice::from_raw_parts_mut(addr as usize as *mut u8, len) };
        buf.fill(0);
        Ok(buf)
    }

    /// Reserves these pages as the loader's without touching them: unlike [`Efi::allocate_pages`], which clears them, for
    /// memory whose contents survive from the last boot.
    pub fn reserve_pages(&self, at: u64, pages: usize) -> Result<(), Status> {
        let mut addr = at;
        // SAFETY: AllocatePages writes the address of the new pages into `addr`.
        let s =
            unsafe { (self.bs().allocate_pages)(ALLOCATE_ADDRESS, LOADER_DATA, pages, &mut addr) };
        if s == SUCCESS { Ok(()) } else { Err(s) }
    }

    /// Reads the memory map into `buf`; returns (bytes used, map key, descriptor size).
    pub fn memory_map(&self, buf: &mut [u8]) -> Result<(usize, usize, usize), (Status, usize)> {
        let (mut size, mut key, mut desc_size, mut version) = (buf.len(), 0usize, 0usize, 0u32);
        // SAFETY: GetMemoryMap writes at most `size` bytes into buf and the three outputs.
        let s = unsafe {
            (self.bs().get_memory_map)(
                &mut size,
                buf.as_mut_ptr(),
                &mut key,
                &mut desc_size,
                &mut version,
            )
        };
        if s != SUCCESS || desc_size < 40 {
            return Err((s, size));
        }
        Ok((size, key, desc_size))
    }

    /// The memory map size the firmware needs right now.
    pub fn memory_map_size(&self) -> usize {
        match self.memory_map(&mut []) {
            Ok((size, _, _)) | Err((_, size)) => size,
        }
    }

    /// Exits boot services. On failure the firmware may have shut parts of them down already: the caller may
    /// then only read the memory map again and retry.
    pub fn exit_boot_services(self, key: usize) -> Result<(), (Efi, Status)> {
        // SAFETY: our own image handle and a map key from GetMemoryMap.
        let s = unsafe { (self.bs().exit_boot_services)(self.image, key) };
        if s == SUCCESS {
            PANIC_CONSOLE.store(null_mut(), Ordering::Relaxed);
            Ok(())
        } else {
            Err((self, s))
        }
    }

    /// The configuration table with `guid`.
    pub fn config_table(&self, guid: &Guid) -> Option<u64> {
        // SAFETY: the system table holds `number_of_table_entries` entries.
        unsafe {
            let st = &*self.st;
            (0..st.number_of_table_entries)
                .map(|i| &*st.configuration_table.add(i))
                .find(|t| t.vendor_guid == *guid)
                .map(|t| t.vendor_table as u64)
        }
    }

    /// The bytes of a flattened device tree at `addr` (its `totalsize`, at most `limit`).
    pub fn device_tree_at(&self, addr: u64, limit: usize) -> Option<&'static [u8]> {
        // SAFETY: a DTB configuration table points at a device tree in memory the firmware keeps (boot services
        // data at least, which we do not free); the header is read before trusting totalsize.
        unsafe {
            let head = core::slice::from_raw_parts(addr as usize as *const u8, 8);
            if head[..4] != lucina_fdt::MAGIC.to_be_bytes() {
                return None;
            }
            let total = u32::from_be_bytes([head[4], head[5], head[6], head[7]]) as usize;
            (total <= limit).then(|| core::slice::from_raw_parts(addr as usize as *const u8, total))
        }
    }

    /// The current GOP mode, if the firmware has a graphics console.
    pub fn gop(&self) -> Option<Gop> {
        let mut iface: *mut c_void = null_mut();
        // SAFETY: LocateProtocol writes an interface pointer; the GOP structures are firmware-owned and live
        // while boot services are.
        unsafe {
            if (self.bs().locate_protocol)(&GRAPHICS_OUTPUT, null_mut(), &mut iface) != SUCCESS
                || iface.is_null()
            {
                return None;
            }
            let gop = &*(iface as *const GraphicsOutput);
            let mode = gop.mode.as_ref()?;
            let info = mode.info.as_ref()?;
            Some(Gop {
                width: info.horizontal_resolution,
                height: info.vertical_resolution,
                pixels_per_line: info.pixels_per_scan_line,
                format: info.pixel_format,
                masks: [info.masks[0], info.masks[1], info.masks[2]],
                base: mode.frame_buffer_base,
                size: mode.frame_buffer_size as u64,
            })
        }
    }

    /// Where the loader itself is in memory, to clean it to the point of coherency before the MMU goes off.
    pub fn own_image(&self) -> Option<(u64, u64)> {
        let li = self.loaded_image()?;
        Some((li.image_base as u64, li.image_size))
    }

    /// The path of the loader's own EFI file on its volume (`\EFI\BOOT\BOOTAA64.EFI`, `\asterinas\BOOTAA64.EFI`
    /// when a boot manager chainloads it), from the loaded image's file device path.
    pub fn own_path<'b>(&self, out: &'b mut [u8; uefi::MAX_PATH]) -> Option<&'b str> {
        let dp = self.loaded_image()?.file_path as *const u8;
        if dp.is_null() {
            return None;
        }
        // Walk the node headers to the end node; the list is bounded so a broken one cannot run away.
        let mut len = 0usize;
        loop {
            // SAFETY: each header read lies inside a node the previous header (or the start) vouched for.
            let (typ, sub, node) = unsafe {
                let h = dp.add(len);
                (
                    *h,
                    *h.add(1),
                    usize::from(u16::from_le_bytes([*h.add(2), *h.add(3)])),
                )
            };
            if node < 4 || len + node > 4096 {
                return None;
            }
            len += node;
            if (typ, sub) == (0x7f, 0xff) {
                break;
            }
        }
        // SAFETY: the walk above found `len` bytes of nodes ending with the end node.
        let bytes = unsafe { core::slice::from_raw_parts(dp, len) };
        uefi::device_path_file(bytes, out)
    }

    /// The loader's UEFI load options, raw (UCS-2 text when a boot manager passes arguments).
    pub fn load_options(&self) -> &[u8] {
        let Some(li) = self.loaded_image() else {
            return &[];
        };
        if li.load_options.is_null() {
            return &[];
        }
        let len = (li.load_options_size as usize).min(4096);
        // SAFETY: the firmware vouches for LoadOptionsSize bytes at LoadOptions while the image runs.
        unsafe { core::slice::from_raw_parts(li.load_options as *const u8, len) }
    }

    fn loaded_image(&self) -> Option<&LoadedImage> {
        let mut iface: *mut c_void = null_mut();
        // SAFETY: HandleProtocol on our own image handle; the protocol outlives this call.
        unsafe {
            if (self.bs().handle_protocol)(self.image, &LOADED_IMAGE, &mut iface) != SUCCESS {
                return None;
            }
            (iface as *const LoadedImage).as_ref()
        }
    }

    /// The device the loader was started from, if the firmware or boot manager gave one (a boot manager that
    /// LoadImages from memory, as LucinaLoader does, gives none).
    pub fn own_device(&self) -> Option<Handle> {
        let li = self.loaded_image()?;
        (!li.device_handle.is_null()).then_some(li.device_handle)
    }

    /// Every handle with a simple file system (FAT volume). The buffer is never freed: it is small, and boot
    /// services memory is the kernel's after ExitBootServices anyway.
    pub fn file_systems(&self) -> &'static [Handle] {
        const BY_PROTOCOL: u32 = 2;
        let (mut n, mut buf): (usize, *mut Handle) = (0, null_mut());
        // SAFETY: LocateHandleBuffer allocates the array and writes its length and address.
        let s = unsafe {
            (self.bs().locate_handle_buffer)(
                BY_PROTOCOL,
                &SIMPLE_FILE_SYSTEM,
                null_mut(),
                &mut n,
                &mut buf,
            )
        };
        if s != SUCCESS || buf.is_null() {
            return &[];
        }
        // SAFETY: the firmware wrote `n` handles at `buf`, in pool memory that is never freed.
        unsafe { core::slice::from_raw_parts(buf, n) }
    }

    /// The root directory of the FAT volume on `device`.
    pub fn volume_dir(&self, device: Handle) -> Result<Dir<'_>, Status> {
        let mut sfs: *mut c_void = null_mut();
        let mut root: *mut FileProtocol = null_mut();
        // SAFETY: the device handle comes from the firmware; OpenVolume writes a file handle.
        unsafe {
            let s = (self.bs().handle_protocol)(device, &SIMPLE_FILE_SYSTEM, &mut sfs);
            if s != SUCCESS || sfs.is_null() {
                return Err(if s == SUCCESS { NOT_FOUND } else { s });
            }
            let sfs = sfs as *mut SimpleFileSystem;
            let s = ((*sfs).open_volume)(sfs, &mut root);
            if s != SUCCESS {
                return Err(s);
            }
        }
        Ok(Dir { root, _efi: self })
    }
}

/// UCS-2 path buffer: `/` becomes `\`, non-ASCII is refused.
fn ucs2_path(path: &str, out: &mut [u16; 260]) -> Result<(), Status> {
    if path.len() >= out.len() || !path.is_ascii() {
        return Err(NOT_FOUND);
    }
    for (o, b) in out.iter_mut().zip(path.bytes()) {
        *o = if b == b'/' {
            u16::from(b'\\')
        } else {
            u16::from(b)
        };
    }
    out[path.len()] = 0;
    Ok(())
}

/// The volume's root directory; files are opened by absolute path.
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
        ucs2_path(path, &mut name)?;
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

    /// Overwrites `path` from its start, in place: for a file of that size no cluster or length changes.
    pub fn overwrite(&self, path: &str, data: &[u8]) -> Result<(), Status> {
        let file = self.open_mode(path, FILE_READ | FILE_WRITE)?;
        let mut len = data.len();
        // SAFETY: Write reads `len` bytes from data.
        let s = unsafe { ((*file.f).write)(file.f, &mut len, data.as_ptr()) };
        if s != SUCCESS {
            return Err(s);
        }
        // SAFETY: flushing an open handle.
        let s = unsafe { ((*file.f).flush)(file.f) };
        if s != SUCCESS { Err(s) } else { Ok(()) }
    }

    /// Replaces `path` with `data` (best effort: the caller only logs failures).
    pub fn write_file(&self, path: &str, data: &[u8]) -> Result<(), Status> {
        if let Ok(old) = self.open_mode(path, FILE_READ | FILE_WRITE) {
            // Delete closes the handle; a stale longer file would otherwise keep its tail.
            let f = old.f;
            core::mem::forget(old);
            // SAFETY: f is an open file handle; Delete closes it whatever it returns.
            unsafe { ((*f).delete)(f) };
        }
        let file = self.open_mode(path, FILE_READ | FILE_WRITE | FILE_CREATE)?;
        let mut len = data.len();
        // SAFETY: Write reads `len` bytes from data.
        let s = unsafe { ((*file.f).write)(file.f, &mut len, data.as_ptr()) };
        if s != SUCCESS {
            return Err(s);
        }
        // SAFETY: flushing an open handle.
        let s = unsafe { ((*file.f).flush)(file.f) };
        if s != SUCCESS { Err(s) } else { Ok(()) }
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

    pub fn rewind(&self) -> Result<(), Status> {
        // SAFETY: moving the position of an open file.
        let s = unsafe { ((*self.f).set_position)(self.f, 0) };
        if s != SUCCESS { Err(s) } else { Ok(()) }
    }
}

impl Drop for File<'_> {
    fn drop(&mut self) {
        // SAFETY: closing our own handle once.
        unsafe { ((*self.f).close)(self.f) };
    }
}

/// Iterates the descriptors of a memory map read with [`Efi::memory_map`].
pub fn descriptors(map: &[u8], desc_size: usize) -> impl Iterator<Item = MemDesc> + '_ {
    map.chunks_exact(desc_size).filter_map(MemDesc::parse)
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

/// For the panic handler: prints to the firmware console while boot services are live, else does nothing.
pub struct PanicConsole;

impl fmt::Write for PanicConsole {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        // SAFETY: PANIC_CONSOLE is non-null only while boot services are live.
        unsafe { output(PANIC_CONSOLE.load(Ordering::Relaxed), s) };
        Ok(())
    }
}
