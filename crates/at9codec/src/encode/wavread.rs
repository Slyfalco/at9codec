//! Minimal RIFF/WAVE reader for 16-bit PCM input to the encoder.

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

/// Parse a canonical PCM WAV buffer. Only 16-bit PCM is supported (the encoder's
/// input contract); other formats should be converted to 16-bit PCM first.
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
    // Accept PCM (1) or WAVE_FORMAT_EXTENSIBLE (0xFFFE) with 16-bit samples.
    if (fmt_tag != 1 && fmt_tag != 0xFFFE) || bits != 16 {
        return Err(Error::NotImplemented("only 16-bit PCM WAV input"));
    }
    let samples = data
        .chunks_exact(2)
        .map(|c| i16::from_le_bytes([c[0], c[1]]))
        .collect();
    Ok(WavPcm {
        channels,
        sample_rate,
        samples,
    })
}
