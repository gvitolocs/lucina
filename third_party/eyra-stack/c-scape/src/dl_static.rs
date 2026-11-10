//! LucinaOS: what static musl answers for dynamic loading, since every Juno program is static. musl in a static build
//! (`src/ldso/dlopen.c`, `dlerror.c`) returns NULL from `dlopen` and `dlsym`, and `dlerror` then says why, once. Upstream
//! c-scape panicked here instead (`todo!`, `unimplemented!`), which ended a program that only asked whether a symbol exists.
//! musl keeps the message per thread; this keeps one for the process, which is enough for "is it there?" questions.

use core::ffi::CStr;
use core::ptr::null_mut;
use core::sync::atomic::{AtomicPtr, Ordering};
use libc::{c_char, c_int, c_void};

static PENDING: AtomicPtr<c_char> = AtomicPtr::new(null_mut());

const NO_LOADING: &CStr = c"Dynamic loading not supported";
const NO_SYMBOL: &CStr = c"Symbol not found";
const BAD_HANDLE: &CStr = c"Invalid library handle";

/// Records the message the next `dlerror` returns.
fn set_dl_error(msg: &'static CStr) {
    PENDING.store(msg.as_ptr().cast_mut(), Ordering::Release);
}

/// What `dlsym` answers for a symbol it does not know: NULL, and a message for `dlerror`.
pub(crate) fn symbol_not_found() -> *mut c_void {
    set_dl_error(NO_SYMBOL);
    null_mut()
}

#[cfg(feature = "take-charge")]
#[no_mangle]
unsafe extern "C" fn dlopen(file: *const c_char, mode: c_int) -> *mut c_void {
    libc!(libc::dlopen(file, mode));
    let _ = (file, mode);
    set_dl_error(NO_LOADING);
    null_mut()
}

#[cfg(feature = "take-charge")]
#[no_mangle]
unsafe extern "C" fn dlerror() -> *mut c_char {
    libc!(libc::dlerror());
    PENDING.swap(null_mut(), Ordering::AcqRel)
}

#[cfg(feature = "take-charge")]
#[no_mangle]
unsafe extern "C" fn dlclose(handle: *mut c_void) -> c_int {
    libc!(libc::dlclose(handle));
    let _ = handle;
    set_dl_error(BAD_HANDLE);
    1
}

#[cfg(feature = "take-charge")]
#[no_mangle]
unsafe extern "C" fn dladdr(addr: *const c_void, info: *mut libc::Dl_info) -> c_int {
    libc!(libc::dladdr(addr, info));
    let _ = (addr, info);
    0
}
