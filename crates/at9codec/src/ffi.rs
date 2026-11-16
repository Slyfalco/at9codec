//! C ABI surface for the `cdylib` (`.dll`/`.so`/`.dylib`).
//!
//! All functions are `#[no_mangle] extern "C"` so any language can bind them.
//! Buffers returned by the library are heap-allocated here and must be released
//! with the matching `at9_free_*` function (do not free them with the caller's
//! allocator). See `include/at9codec.h` and `docs/USING_THE_LIBRARY.md`.

use crate::container;
use std::os::raw::{c_char, c_void};

/// Status codes returned across the ABI.
pub const AT9_OK: i32 = 0;
pub const AT9_ERR_NULL: i32 = -1;
pub const AT9_ERR_PARSE: i32 = -2;
pub const AT9_ERR_DECODE: i32 = -3;
pub const AT9_ERR_ENCODE: i32 = -4;

/// Progress callback: `(user, units_done, units_total)`. One unit = one
/// superframe. Invoked from a single thread (a dedicated monitor thread for
/// encode; the decode thread for decode); never concurrently.
pub type At9ProgressCb = unsafe extern "C" fn(*mut c_void, u32, u32);

/// A C callback + its opaque user pointer, bundled so it can cross into the
/// codec's monitor thread. The pointer is only ever handed back to the caller's
/// own callback, so we assert Send/Sync for it.
struct CProgress {
    cb: At9ProgressCb,
    user: *mut c_void,
}
unsafe impl Send for CProgress {}
unsafe impl Sync for CProgress {}

/// `loop_start` convention shared by the encode `_ex` functions: negative = no
/// loop, 0 = whole-track loop, positive = loop from that sample to the end.
fn loop_opt(loop_start: i32) -> Option<u32> {
    if loop_start < 0 {
        None
    } else {
        Some(loop_start as u32)
    }
}

fn leak_u8(v: Vec<u8>) -> (*mut u8, usize) {
    let b = v.into_boxed_slice();
    let len = b.len();
    (Box::into_raw(b) as *mut u8, len)
}

fn leak_i16(v: Vec<i16>) -> (*mut i16, usize) {
    let b = v.into_boxed_slice();
    let len = b.len();
    (Box::into_raw(b) as *mut i16, len)
}

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

/// Decode an AT9 file to interleaved 16-bit PCM.
///
/// On success writes an allocated sample buffer to `*out_pcm`, its length (number
/// of `int16_t` samples, i.e. frames × channels) to `*out_count`, and the format
/// to `*out_channels` / `*out_sample_rate`, and returns [`AT9_OK`]. Release the
/// buffer with [`at9_free_pcm`].
///
/// # Safety
/// `at9` must point to `at9_len` readable bytes; the `out_*` pointers must be
/// valid and writable.
#[no_mangle]
pub unsafe extern "C" fn at9_decode(
    at9: *const u8,
    at9_len: usize,
    out_pcm: *mut *mut i16,
    out_count: *mut usize,
    out_channels: *mut u16,
    out_sample_rate: *mut u32,
) -> i32 {
    if at9.is_null() || out_pcm.is_null() || out_count.is_null()
        || out_channels.is_null() || out_sample_rate.is_null()
    {
        return AT9_ERR_NULL;
    }
    let buf = std::slice::from_raw_parts(at9, at9_len);
    match crate::decode_to_pcm16(buf) {
        Ok(pcm) => {
            *out_channels = pcm.channels;
            *out_sample_rate = pcm.sample_rate;
            let (ptr, len) = leak_i16(pcm.samples);
            *out_pcm = ptr;
            *out_count = len;
            AT9_OK
        }
        Err(_) => AT9_ERR_DECODE,
    }
}

/// Like [`at9_decode`], but reports progress through `cb`/`user` (one call per
/// superframe). Pass `cb = NULL` for no reporting (identical to [`at9_decode`]).
///
/// # Safety
/// As [`at9_decode`]; additionally `cb`, when non-null, must be a valid function
/// pointer callable as [`At9ProgressCb`].
#[no_mangle]
pub unsafe extern "C" fn at9_decode_ex(
    at9: *const u8,
    at9_len: usize,
    cb: Option<At9ProgressCb>,
    user: *mut c_void,
    out_pcm: *mut *mut i16,
    out_count: *mut usize,
    out_channels: *mut u16,
    out_sample_rate: *mut u32,
) -> i32 {
    if at9.is_null() || out_pcm.is_null() || out_count.is_null()
        || out_channels.is_null() || out_sample_rate.is_null()
    {
        return AT9_ERR_NULL;
    }
    let buf = std::slice::from_raw_parts(at9, at9_len);
    let ctx = cb.map(|cb| CProgress { cb, user });
    let closure = ctx
        .as_ref()
        .map(|c| move |d, t| unsafe { (c.cb)(c.user, d, t) });
    let progress: Option<crate::ProgressFn> = closure
        .as_ref()
        .map(|f| f as &(dyn Fn(u32, u32) + Sync));
    match crate::decode_to_pcm16_progress(buf, progress) {
        Ok(pcm) => {
            *out_channels = pcm.channels;
            *out_sample_rate = pcm.sample_rate;
            let (ptr, len) = leak_i16(pcm.samples);
            *out_pcm = ptr;
            *out_count = len;
            AT9_OK
        }
        Err(_) => AT9_ERR_DECODE,
    }
}

/// Free a PCM buffer returned by [`at9_decode`]. `count` must be the value
/// written to `out_count`.
///
/// # Safety
/// `pcm`/`count` must be exactly what [`at9_decode`] returned (or `pcm` null).
#[no_mangle]
pub unsafe extern "C" fn at9_free_pcm(pcm: *mut i16, count: usize) {
    if !pcm.is_null() {
        drop(Box::from_raw(std::slice::from_raw_parts_mut(pcm, count)));
    }
}

/// Encode interleaved 16-bit PCM to an AT9 file (RIFF/WAVE). `bitrate` is in
/// bits/sec (0 = a sensible default for the channel count).
///
/// On success writes an allocated AT9 byte buffer to `*out_at9` and its length to
/// `*out_len`, and returns [`AT9_OK`]. Release the buffer with [`at9_free_at9`].
///
/// # Safety
/// `pcm` must point to `sample_count` readable `int16_t`s; the `out_*` pointers
/// must be valid and writable.
#[no_mangle]
pub unsafe extern "C" fn at9_encode(
    pcm: *const i16,
    sample_count: usize,
    channels: u16,
    sample_rate: u32,
    bitrate: u32,
    out_at9: *mut *mut u8,
    out_len: *mut usize,
) -> i32 {
    if pcm.is_null() || out_at9.is_null() || out_len.is_null() {
        return AT9_ERR_NULL;
    }
    let samples = std::slice::from_raw_parts(pcm, sample_count);
    match crate::encode::encode_pcm16(samples, channels, sample_rate, bitrate) {
        Ok(bytes) => {
            let (ptr, len) = leak_u8(bytes);
            *out_at9 = ptr;
            *out_len = len;
            AT9_OK
        }
        Err(_) => AT9_ERR_ENCODE,
    }
}

/// Encode a 16-bit PCM WAV file buffer to an AT9 file. Convenience over
/// [`at9_encode`] when you already have a WAV container (8/16/24/32-bit int or
/// 32/64-bit float input is accepted and converted to 16-bit internally).
///
/// # Safety
/// `wav` must point to `wav_len` readable bytes; `out_*` must be valid/writable.
#[no_mangle]
pub unsafe extern "C" fn at9_encode_wav(
    wav: *const u8,
    wav_len: usize,
    bitrate: u32,
    out_at9: *mut *mut u8,
    out_len: *mut usize,
) -> i32 {
    if wav.is_null() || out_at9.is_null() || out_len.is_null() {
        return AT9_ERR_NULL;
    }
    let buf = std::slice::from_raw_parts(wav, wav_len);
    match crate::encode::encode_wav(buf, bitrate) {
        Ok(bytes) => {
            let (ptr, len) = leak_u8(bytes);
            *out_at9 = ptr;
            *out_len = len;
            AT9_OK
        }
        Err(_) => AT9_ERR_ENCODE,
    }
}

/// Like [`at9_encode`], with loop points and progress reporting. `loop_start`:
/// negative = no loop, 0 = whole-track loop, positive = loop from that sample to
/// the end (decoded-WAV coords). `cb`/`user` as in [`at9_decode_ex`] (NULL = none).
///
/// # Safety
/// As [`at9_encode`]; `cb`, when non-null, must be a valid [`At9ProgressCb`].
#[no_mangle]
pub unsafe extern "C" fn at9_encode_ex(
    pcm: *const i16,
    sample_count: usize,
    channels: u16,
    sample_rate: u32,
    bitrate: u32,
    loop_start: i32,
    cb: Option<At9ProgressCb>,
    user: *mut c_void,
    out_at9: *mut *mut u8,
    out_len: *mut usize,
) -> i32 {
    if pcm.is_null() || out_at9.is_null() || out_len.is_null() {
        return AT9_ERR_NULL;
    }
    let samples = std::slice::from_raw_parts(pcm, sample_count);
    let ctx = cb.map(|cb| CProgress { cb, user });
    let closure = ctx
        .as_ref()
        .map(|c| move |d, t| unsafe { (c.cb)(c.user, d, t) });
    let progress: Option<crate::ProgressFn> = closure
        .as_ref()
        .map(|f| f as &(dyn Fn(u32, u32) + Sync));
    match crate::encode::encode_pcm16_ex(
        samples, channels, sample_rate, bitrate, loop_opt(loop_start), progress,
    ) {
        Ok(bytes) => {
            let (ptr, len) = leak_u8(bytes);
            *out_at9 = ptr;
            *out_len = len;
            AT9_OK
        }
        Err(_) => AT9_ERR_ENCODE,
    }
}

/// Like [`at9_encode_wav`], with loop points and progress reporting. See
/// [`at9_encode_ex`] for `loop_start` and `cb`/`user`.
///
/// # Safety
/// As [`at9_encode_wav`]; `cb`, when non-null, must be a valid [`At9ProgressCb`].
#[no_mangle]
pub unsafe extern "C" fn at9_encode_wav_ex(
    wav: *const u8,
    wav_len: usize,
    bitrate: u32,
    loop_start: i32,
    cb: Option<At9ProgressCb>,
    user: *mut c_void,
    out_at9: *mut *mut u8,
    out_len: *mut usize,
) -> i32 {
    if wav.is_null() || out_at9.is_null() || out_len.is_null() {
        return AT9_ERR_NULL;
    }
    let buf = std::slice::from_raw_parts(wav, wav_len);
    let ctx = cb.map(|cb| CProgress { cb, user });
    let closure = ctx
        .as_ref()
        .map(|c| move |d, t| unsafe { (c.cb)(c.user, d, t) });
    let progress: Option<crate::ProgressFn> = closure
        .as_ref()
        .map(|f| f as &(dyn Fn(u32, u32) + Sync));
    match crate::encode::encode_wav_ex(buf, bitrate, loop_opt(loop_start), progress) {
        Ok(bytes) => {
            let (ptr, len) = leak_u8(bytes);
            *out_at9 = ptr;
            *out_len = len;
            AT9_OK
        }
        Err(_) => AT9_ERR_ENCODE,
    }
}

/// Free an AT9 byte buffer returned by [`at9_encode`] / [`at9_encode_wav`].
/// `len` must be the value written to `out_len`.
///
/// # Safety
/// `at9`/`len` must be exactly what the encode function returned (or `at9` null).
#[no_mangle]
pub unsafe extern "C" fn at9_free_at9(at9: *mut u8, len: usize) {
    if !at9.is_null() {
        drop(Box::from_raw(std::slice::from_raw_parts_mut(at9, len)));
    }
}
