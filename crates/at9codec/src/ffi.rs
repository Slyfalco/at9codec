//! C ABI surface for the `cdylib` (`.dll`/`.so`/`.dylib`).
//!
//! Kept deliberately small and `#[no_mangle] extern "C"` so any language can bind
//! it. Decode/encode entry points are added here as those land; for now it
//! exposes the version and container-info parsing.

use crate::container;
use std::os::raw::c_char;

/// Status codes returned across the ABI.
pub const AT9_OK: i32 = 0;
pub const AT9_ERR_NULL: i32 = -1;
pub const AT9_ERR_PARSE: i32 = -2;

/// Container metadata, C-layout. Mirrors [`crate::container::Container`]'s
/// scalar fields (loop presence is flattened for a flat ABI).
#[repr(C)]
#[derive(Clone, Copy)]
pub struct At9Info {
    pub sample_rate: u32,
    pub channels: u16,
    pub _pad: u16,
    pub total_samples: u32,
    pub encoder_delay: u32,
    pub frame_bytes: u16,
    pub frames_per_superframe: u16,
    pub superframe_count: u32,
    /// 1 if a loop is defined, else 0.
    pub has_loop: u32,
    /// Loop start/end in decoded-WAV sample coords (valid when `has_loop`).
    pub loop_start: u32,
    pub loop_end: u32,
    pub is_whole_loop: u32,
}

/// Null-terminated library version string. Valid for the life of the library.
#[no_mangle]
pub extern "C" fn at9_version() -> *const c_char {
    // Static with an embedded NUL so we can hand out a stable pointer.
    concat!(env!("CARGO_PKG_VERSION"), "\0").as_ptr() as *const c_char
}

/// Parse an AT9 buffer and fill `out`. Returns [`AT9_OK`] or a negative error.
///
/// # Safety
/// `data` must point to `len` readable bytes; `out` must be a valid, writable
/// pointer to an [`At9Info`].
#[no_mangle]
pub unsafe extern "C" fn at9_parse_info(
    data: *const u8,
    len: usize,
    out: *mut At9Info,
) -> i32 {
    if data.is_null() || out.is_null() {
        return AT9_ERR_NULL;
    }
    let buf = std::slice::from_raw_parts(data, len);
    match container::parse(buf) {
        Ok(c) => {
            let (has_loop, ls, le, whole) = match c.loop_info {
                Some(l) => (1u32, l.start, l.end, l.is_whole as u32),
                None => (0, 0, 0, 0),
            };
            *out = At9Info {
                sample_rate: c.sample_rate,
                channels: c.channels,
                _pad: 0,
                total_samples: c.total_samples,
                encoder_delay: c.encoder_delay,
                frame_bytes: c.config.frame_bytes,
                frames_per_superframe: c.config.frames_per_superframe as u16,
                superframe_count: c.superframe_count(),
                has_loop,
                loop_start: ls,
                loop_end: le,
                is_whole_loop: whole,
            };
            AT9_OK
        }
        Err(_) => AT9_ERR_PARSE,
    }
}
