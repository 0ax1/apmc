//! Region mode API for marking code sections to measure.
//!
//! When the apmc inject dylib is loaded (`apmc stat --region`), [`start`] and
//! [`stop`] control counter measurement for the calling thread. Multiple
//! start/stop pairs accumulate. Results are summed across all threads.
//!
//! When the dylib is **not** loaded, both functions are no-ops.
//!
//! # Example
//! ```no_run
//! apmc::region::start();
//! // ... hot code ...
//! apmc::region::stop();
//! ```

use std::sync::OnceLock;

type Fn = unsafe extern "C" fn();

fn resolve(name: &[u8]) -> Option<Fn> {
    extern "C" {
        fn dlsym(
            handle: *mut std::ffi::c_void,
            symbol: *const std::ffi::c_char,
        ) -> *mut std::ffi::c_void;
    }
    const RTLD_DEFAULT: *mut std::ffi::c_void = -2isize as *mut std::ffi::c_void;
    let ptr = unsafe { dlsym(RTLD_DEFAULT, name.as_ptr().cast()) };
    if ptr.is_null() {
        None
    } else {
        Some(unsafe { std::mem::transmute::<*mut std::ffi::c_void, Fn>(ptr) })
    }
}

static START_FN: OnceLock<Option<Fn>> = OnceLock::new();
static STOP_FN: OnceLock<Option<Fn>> = OnceLock::new();

/// Start measuring hardware counters for the current thread.
///
/// No-op if the apmc inject dylib is not loaded.
#[inline]
pub fn start() {
    if let Some(f) = START_FN.get_or_init(|| resolve(b"apmc_start_impl\0")) {
        unsafe { f() };
    }
}

/// Stop measuring and accumulate the delta since the last [`start`] call.
///
/// No-op if the apmc inject dylib is not loaded.
#[inline]
pub fn stop() {
    if let Some(f) = STOP_FN.get_or_init(|| resolve(b"apmc_stop_impl\0")) {
        unsafe { f() };
    }
}
