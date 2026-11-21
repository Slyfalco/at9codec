//! C ABI surface for the `cdylib` (`.dll`/`.so`/`.dylib`).
//!
//! All functions are `#[no_mangle] extern "C"` so any language can bind them.
//! Optional behaviour is passed through small options structs (zero-initialize and
//! set only what you need; pass `NULL` for all defaults), so the API stays stable
//! as features are added. Buffers returned by the library are heap-allocated here
//! and must be released with the matching `at9_free_*` function (do not free them
//! with the caller's allocator). See `include/at9codec.h` and
//! `docs/USING_THE_LIBRARY.md`.

use crate::container;
use std::os::raw::{c_char, c_void};

/// Status codes returned across the ABI.
pub const AT9_OK: i32 = 0;
pub const AT9_ERR_NULL: i32 = -1;
pub const AT9_ERR_PARSE: i32 = -2;
pub const AT9_ERR_DECODE: i32 = -3;
pub const AT9_ERR_ENCODE: i32 = -4;
/// The requested encode bitrate is not a standard ATRAC9 rate for the stream's
/// channel count and sample rate. Use 0 in `At9EncodeOptions::bitrate` for auto
/// (the encoder picks the best standard rate that fits any size cap).
pub const AT9_ERR_BITRATE: i32 = -5;
/// The audio is too long to fit the requested size limit even at the lowest
/// standard bitrate — shorten the track or relax the limit.
pub const AT9_ERR_TOO_LARGE: i32 = -6;

/// `At9EncodeOptions::size_profile` values. Naming a target makes the codec apply
/// its own byte cap for that console (and ignore `max_bytes`). `0` = use `max_bytes`.
pub const AT9_SIZE_NONE: u32 = 0;
/// PS Vita system/theme BGM limit (under 5 MB).
pub const AT9_SIZE_VITA_BGM: u32 = 1;
/// Tighter 2 MiB limit for a theme BGM that must also run on PS4/PS5.
pub const AT9_SIZE_PS_CROSS: u32 = 2;

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

/// Options for [`at9_decode`]. Zero-initialize and set only what you need; pass
/// `NULL` for all defaults. `struct_size` should be `sizeof(At9DecodeOptions)` for
/// forward compatibility.
#[repr(C)]
pub struct At9DecodeOptions {
    pub struct_size: u32,
    /// Progress callback `(user, done, total)` in superframes; `NULL` = none.
    pub progress: Option<At9ProgressCb>,
    pub user: *mut c_void,
}

/// Options for [`at9_encode`] / [`at9_encode_wav`]. Zero-initialize and set only
/// what you need; pass `NULL` for all defaults. `struct_size` should be
/// `sizeof(At9EncodeOptions)`.
#[repr(C)]
pub struct At9EncodeOptions {
    pub struct_size: u32,
    /// Target bitrate (bits/sec); `0` = best standard default for the channels.
    pub bitrate: u32,
    /// Custom size cap in bytes; `0` = no cap. Ignored when `size_profile` names a
    /// console target. Prefer `size_profile` for the console limits.
    pub max_bytes: usize,
    /// Loop start in decoded-PCM samples: `< 0` = no loop, `0` = whole track,
    /// `> 0` = loop from that sample.
    pub loop_start: i32,
    /// Loop end — the inclusive last sample of the loop; `<= 0` = to the track end.
    /// Only used when `loop_start >= 0`.
    pub loop_end: i32,
    /// Progress callback `(user, done, total)` in superframes; `NULL` = none.
    pub progress: Option<At9ProgressCb>,
    pub user: *mut c_void,
    /// Named size limit (`AT9_SIZE_*`). When non-zero the codec applies its own byte
    /// cap for that console and ignores `max_bytes`; `0` = use `max_bytes`. Trailing
    /// field: a caller with an older/smaller `struct_size` that omits it gets `0`.
    pub size_profile: u32,
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
pub unsafe extern "C" fn at9_parse_info(data: *const u8, len: usize, out: *mut At9Info) -> i32 {
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
/// buffer with [`at9_free_pcm`]. `opts` may be `NULL` for defaults.
///
/// # Safety
/// `at9` must point to `at9_len` readable bytes; the `out_*` pointers must be
/// valid and writable; `opts`, if non-null, must point to a valid
/// [`At9DecodeOptions`] whose `progress` (if set) is a valid [`At9ProgressCb`].
#[no_mangle]
pub unsafe extern "C" fn at9_decode(
    at9: *const u8,
    at9_len: usize,
    opts: *const At9DecodeOptions,
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

    // Progress: build a keep-alive closure if a callback was supplied.
    let ctx = if opts.is_null() {
        None
    } else {
        (*opts).progress.map(|cb| CProgress { cb, user: (*opts).user })
    };
    let closure = ctx.as_ref().map(|c| move |d, t| unsafe { (c.cb)(c.user, d, t) });
    let progress: Option<crate::ProgressFn> =
        closure.as_ref().map(|f| f as &(dyn Fn(u32, u32) + Sync));

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

/// Free a PCM buffer returned by [`at9_decode`]. `count` must be `*out_count`.
///
/// # Safety
/// `pcm`/`count` must be exactly what [`at9_decode`] returned (or `pcm` null).
#[no_mangle]
pub unsafe extern "C" fn at9_free_pcm(pcm: *mut i16, count: usize) {
    if !pcm.is_null() {
        drop(Box::from_raw(std::slice::from_raw_parts_mut(pcm, count)));
    }
}

/// Build a Rust [`crate::encode::EncodeOptions`] from the C struct (minus the
/// progress closure, which borrows and must be built in the calling frame).
unsafe fn enc_opts_scalars(
    opts: *const At9EncodeOptions,
) -> (u32, crate::encode::SizeLimit, Option<u32>, Option<u32>) {
    use crate::encode::SizeLimit;
    if opts.is_null() {
        return (0, SizeLimit::None, None, None);
    }
    let o = &*opts;
    let loop_start = if o.loop_start < 0 { None } else { Some(o.loop_start as u32) };
    let loop_end = if loop_start.is_some() && o.loop_end > 0 {
        Some(o.loop_end as u32)
    } else {
        None
    };
    // size_profile is a trailing field; a caller with an older, smaller struct_size
    // that doesn't include it is treated as AT9_SIZE_NONE.
    let covers_profile = (o.struct_size as usize)
        >= core::mem::offset_of!(At9EncodeOptions, size_profile) + core::mem::size_of::<u32>();
    let profile = if covers_profile { o.size_profile } else { AT9_SIZE_NONE };
    let size_limit = match profile {
        AT9_SIZE_VITA_BGM => SizeLimit::VitaSystemBgm,
        AT9_SIZE_PS_CROSS => SizeLimit::PsCross,
        _ if o.max_bytes > 0 => SizeLimit::Bytes(o.max_bytes),
        _ => SizeLimit::None,
    };
    (o.bitrate, size_limit, loop_start, loop_end)
}

/// The codec's byte cap for a size profile (`AT9_SIZE_*`); `0` = no cap / unknown.
/// Lets non-C callers read the console limits from the codec instead of hard-coding
/// them (e.g. to size-check a pre-made AT9 before using it).
#[no_mangle]
pub extern "C" fn at9_profile_max_bytes(size_profile: u32) -> usize {
    use crate::encode::SizeLimit;
    match size_profile {
        AT9_SIZE_VITA_BGM => SizeLimit::VitaSystemBgm.max_bytes(),
        AT9_SIZE_PS_CROSS => SizeLimit::PsCross.max_bytes(),
        _ => 0,
    }
}

/// Encode interleaved 16-bit PCM to an AT9 file (RIFF/WAVE). `opts` may be `NULL`
/// for defaults. On success writes an allocated buffer to `*out_at9` / `*out_len`;
/// free it with [`at9_free_at9`].
///
/// # Safety
/// `pcm` must point to `sample_count` readable `int16_t`s; `out_*` must be valid
/// and writable; `opts`, if non-null, must be a valid [`At9EncodeOptions`].
#[no_mangle]
pub unsafe extern "C" fn at9_encode(
    pcm: *const i16,
    sample_count: usize,
    channels: u16,
    sample_rate: u32,
    opts: *const At9EncodeOptions,
    out_at9: *mut *mut u8,
    out_len: *mut usize,
) -> i32 {
    if pcm.is_null() || out_at9.is_null() || out_len.is_null() {
        return AT9_ERR_NULL;
    }
    let samples = std::slice::from_raw_parts(pcm, sample_count);
    let (bitrate, size_limit, loop_start, loop_end) = enc_opts_scalars(opts);
    let ctx = if opts.is_null() {
        None
    } else {
        (*opts).progress.map(|cb| CProgress { cb, user: (*opts).user })
    };
    let closure = ctx.as_ref().map(|c| move |d, t| unsafe { (c.cb)(c.user, d, t) });
    let eopts = crate::encode::EncodeOptions {
        bitrate,
        size_limit,
        loop_start,
        loop_end,
        progress: closure.as_ref().map(|f| f as &(dyn Fn(u32, u32) + Sync)),
    };
    match crate::encode::encode_pcm16(samples, channels, sample_rate, &eopts) {
        Ok(bytes) => {
            let (ptr, len) = leak_u8(bytes);
            *out_at9 = ptr;
            *out_len = len;
            AT9_OK
        }
        Err(crate::error::Error::InvalidBitrate) => AT9_ERR_BITRATE,
        Err(crate::error::Error::ExceedsSizeLimit) => AT9_ERR_TOO_LARGE,
        Err(_) => AT9_ERR_ENCODE,
    }
}

/// Encode a WAV file buffer to an AT9 file. Accepts 8/16/24/32-bit integer and
/// 32/64-bit float PCM WAV (converted to 16-bit, no resampling). `opts` may be
/// `NULL`. Free the result with [`at9_free_at9`].
///
/// # Safety
/// `wav` must point to `wav_len` readable bytes; `out_*` must be valid/writable;
/// `opts`, if non-null, must be a valid [`At9EncodeOptions`].
#[no_mangle]
pub unsafe extern "C" fn at9_encode_wav(
    wav: *const u8,
    wav_len: usize,
    opts: *const At9EncodeOptions,
    out_at9: *mut *mut u8,
    out_len: *mut usize,
) -> i32 {
    if wav.is_null() || out_at9.is_null() || out_len.is_null() {
        return AT9_ERR_NULL;
    }
    let buf = std::slice::from_raw_parts(wav, wav_len);
    let (bitrate, size_limit, loop_start, loop_end) = enc_opts_scalars(opts);
    let ctx = if opts.is_null() {
        None
    } else {
        (*opts).progress.map(|cb| CProgress { cb, user: (*opts).user })
    };
    let closure = ctx.as_ref().map(|c| move |d, t| unsafe { (c.cb)(c.user, d, t) });
    let eopts = crate::encode::EncodeOptions {
        bitrate,
        size_limit,
        loop_start,
        loop_end,
        progress: closure.as_ref().map(|f| f as &(dyn Fn(u32, u32) + Sync)),
    };
    match crate::encode::encode_wav(buf, &eopts) {
        Ok(bytes) => {
            let (ptr, len) = leak_u8(bytes);
            *out_at9 = ptr;
            *out_len = len;
            AT9_OK
        }
        Err(crate::error::Error::InvalidBitrate) => AT9_ERR_BITRATE,
        Err(crate::error::Error::ExceedsSizeLimit) => AT9_ERR_TOO_LARGE,
        Err(_) => AT9_ERR_ENCODE,
    }
}

/// Free an AT9 byte buffer returned by [`at9_encode`] / [`at9_encode_wav`].
/// `len` must be `*out_len`.
///
/// # Safety
/// `at9`/`len` must be exactly what the encode function returned (or `at9` null).
#[no_mangle]
pub unsafe extern "C" fn at9_free_at9(at9: *mut u8, len: usize) {
    if !at9.is_null() {
        drop(Box::from_raw(std::slice::from_raw_parts_mut(at9, len)));
    }
}
