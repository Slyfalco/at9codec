//! Minimal RIFF/WAVE reader for the encoder. Accepts 8/16/24/32-bit integer and
//! 32/64-bit float PCM and converts to 16-bit samples. Conversion is bit-depth
//! only — no resampling — so the sample rate, frame count, pitch/tempo and any
//! loop sample positions are preserved exactly.

use crate::error::{Error, Result};

/// Decoded PCM read from a WAV file.
pub struct WavPcm {
    pub channels: u16,
    pub sample_rate: u32,
    /// Interleaved 16-bit samples.
    pub samples: Vec<i16>,
}

fn u16le(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}
fn u32le(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}

const WAVE_FORMAT_PCM: u16 = 1;
const WAVE_FORMAT_FLOAT: u16 = 3;
const WAVE_FORMAT_EXTENSIBLE: u16 = 0xFFFE;

/// Parse a WAV buffer and return interleaved 16-bit samples, converting from any
/// supported bit depth (no resampling).
pub fn parse_wav(buf: &[u8]) -> Result<WavPcm> {
    if buf.len() < 12 || &buf[0..4] != b"RIFF" || &buf[8..12] != b"WAVE" {
        return Err(Error::NotRiff);
    }
    let mut channels = 0u16;
    let mut sample_rate = 0u32;
    let mut bits = 0u16;
    let mut fmt_tag = 0u16;
    let mut data: Option<&[u8]> = None;

    let mut pos = 12usize;
    while pos + 8 <= buf.len() {
        let id = &buf[pos..pos + 4];
        let size = u32le(buf, pos + 4) as usize;
        let body = pos + 8;
        let end = (body + size).min(buf.len());
        match id {
            b"fmt " => {
                let f = &buf[body..end];
                if f.len() >= 16 {
                    fmt_tag = u16le(f, 0);
                    channels = u16le(f, 2);
                    sample_rate = u32le(f, 4);
                    bits = u16le(f, 14);
                    // WAVE_FORMAT_EXTENSIBLE: the real format is the first two
                    // bytes of the SubFormat GUID (offset 24 in the fmt body).
                    if fmt_tag == WAVE_FORMAT_EXTENSIBLE && f.len() >= 26 {
                        fmt_tag = u16le(f, 24);
                    }
                }
            }
            b"data" => data = Some(&buf[body..end]),
            _ => {}
        }
        pos = end + (size & 1);
    }

    let data = data.ok_or(Error::MissingChunk("data"))?;
    if sample_rate == 0 || channels == 0 {
        return Err(Error::MissingChunk("fmt "));
    }

    let to_i16 = |v: i64| v.clamp(-32768, 32767) as i16;
    let samples: Vec<i16> = match (fmt_tag, bits) {
        // Unsigned 8-bit: centre at 128, scale up to 16-bit.
        (WAVE_FORMAT_PCM, 8) => data.iter().map(|&b| to_i16((b as i64 - 128) * 256)).collect(),
        (WAVE_FORMAT_PCM, 16) => data
            .chunks_exact(2)
            .map(|c| i16::from_le_bytes([c[0], c[1]]))
            .collect(),
        // 24-bit signed little-endian → top 16 bits.
        (WAVE_FORMAT_PCM, 24) => data
            .chunks_exact(3)
            .map(|c| {
                let v = ((c[0] as i32) | ((c[1] as i32) << 8) | ((c[2] as i32) << 16)) << 8 >> 8;
                to_i16((v >> 8) as i64)
            })
            .collect(),
        // 32-bit signed → top 16 bits.
        (WAVE_FORMAT_PCM, 32) => data
            .chunks_exact(4)
            .map(|c| to_i16((i32::from_le_bytes([c[0], c[1], c[2], c[3]]) >> 16) as i64))
            .collect(),
        // 32-bit float in [-1, 1].
        (WAVE_FORMAT_FLOAT, 32) => data
            .chunks_exact(4)
            .map(|c| {
                let f = f32::from_le_bytes([c[0], c[1], c[2], c[3]]);
                to_i16((f.clamp(-1.0, 1.0) as f64 * 32767.0).round() as i64)
            })
            .collect(),
        // 64-bit float in [-1, 1].
        (WAVE_FORMAT_FLOAT, 64) => data
            .chunks_exact(8)
            .map(|c| {
                let f = f64::from_le_bytes(c.try_into().unwrap());
                to_i16((f.clamp(-1.0, 1.0) * 32767.0).round() as i64)
            })
            .collect(),
        _ => return Err(Error::NotImplemented("unsupported WAV sample format")),
    };

    Ok(WavPcm {
        channels,
        sample_rate,
        samples,
    })
}
