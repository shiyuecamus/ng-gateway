//! Small FFI utilities used by macro-generated exports.
//!
//! # Goals
//! - Avoid panics across FFI boundaries.
//! - Keep `ng_driver_factory!` / `ng_plugin_factory!` macro bodies readable.
//! - Centralize safety checks for raw pointer outputs.

use tokio::runtime::Runtime;

/// Convert a `&'static str` to a C string safely.
///
/// # Notes
/// - This MUST NOT panic.
/// - If the input contains an interior NUL, it is sanitized to a space (`0x20`).
#[inline]
pub fn cstring_sanitized(input: &'static str) -> ::std::ffi::CString {
    let bytes = input.as_bytes();
    let mut buf: ::std::vec::Vec<u8> = ::std::vec::Vec::with_capacity(bytes.len() + 1);
    for &b in bytes.iter() {
        buf.push(if b == 0 { b' ' } else { b });
    }
    buf.push(0);
    // SAFETY: we ensured there are no interior NULs and we appended a terminator.
    unsafe { ::std::ffi::CString::from_vec_unchecked(buf) }
}

/// Write a slice's pointer/length to out-params.
///
/// # Safety
/// - `out_ptr` and `out_len` must be valid for writes of their respective types.
/// - Caller must ensure the slice outlives the consumer on the other side of FFI.
#[inline]
pub unsafe fn write_slice_ptr_len(out_ptr: *mut *const u8, out_len: *mut usize, bytes: &[u8]) {
    if out_ptr.is_null() || out_len.is_null() {
        return;
    }
    *out_ptr = bytes.as_ptr();
    *out_len = bytes.len();
}

/// Build a Tokio runtime for a `cdylib` library.
///
/// # Notes
/// - Returns `None` on failure instead of panicking.
#[inline]
pub fn build_runtime(thread_name: &'static str) -> Option<Runtime> {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_name(thread_name)
        .build()
        .ok()
}
