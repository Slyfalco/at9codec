//! AT9 file (RIFF/WAVE) container parsing.
//!
//! An `.at9` file is a RIFF/WAVE container holding ATRAC9 data. All the metadata
//! needed to decode — true length, encoder delay, loop points and the 4-byte
//! ATRAC9 config — lives in standard chunks and is readable without touching the
//! compressed stream:
//!
//! * `fmt ` — WAVE_FORMAT_EXTENSIBLE; its "extra" bytes carry the ATRAC9 GUID and
//!   the 4-byte ATRAC9 config data.
//! * `fact` — field[0] = total decoded samples (true length); field[1] = encoder
//!   delay (priming samples, typically 256).
//! * `smpl` — sample loops (start/end in ATRAC9 sample coords, which include the
//!   encoder delay).
//! * `data` — the ATRAC9 superframes.
//!
//! This module is intentionally decoder-independent: it is the same container
//! layer the final native codec and any host app share.

use crate::error::{Error, Result};

/// The ATRAC9 SubFormat GUID as it appears in the `fmt ` extra data.
const ATRAC9_GUID: [u8; 16] = [
    0xD2, 0x42, 0xE1, 0x47, 0xBA, 0x36, 0x8D, 0x4D,
    0x88, 0xFC, 0x61, 0x65, 0x4F, 0x8C, 0x83, 0x6C,
];

/// ATRAC9 sample-rate table, indexed by the 4-bit config field.
const SAMPLE_RATES: [u32; 16] = [
    11025, 12000, 16000, 22050, 24000, 32000, 44100, 48000,
    44100, 48000, 64000, 88200, 96000, 128000, 176400, 192000,
];

/// Decoded ATRAC9 configuration (parsed from the 4-byte config in `fmt `).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Atrac9Config {
    /// The raw 4 config bytes (header 0xFE + packed fields).
    pub raw: [u8; 4],
    pub sample_rate_index: u8,
    pub sample_rate: u32,
    pub channel_config_index: u8,
    /// Bytes in one ATRAC9 frame.
    pub frame_bytes: u16,
    /// log2 of the number of frames per superframe.
    pub superframe_index: u8,
    /// Number of frames per superframe (`1 << superframe_index`).
    pub frames_per_superframe: u8,
    /// Bytes in one superframe (`frame_bytes * frames_per_superframe`).
    pub superframe_bytes: u32,
    /// Samples produced by one superframe (from `fmt`'s samples-per-block).
    pub superframe_samples: u32,
    /// Samples produced by one frame.
    pub frame_samples: u32,
}

/// A forward loop, expressed in DECODED-WAV sample coordinates (delay removed).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LoopInfo {
    pub start: u32,
    pub end: u32,
    pub is_whole: bool,
}

/// Everything parsed from an AT9 container.
#[derive(Debug, Clone)]
pub struct Container {
    pub sample_rate: u32,
    pub channels: u16,
    pub block_align: u16,
    pub avg_bytes_per_sec: u32,
    /// True decoded length in samples (`fact` field[0]).
    pub total_samples: u32,
    /// Encoder priming delay in samples (`fact` field[1]).
    pub encoder_delay: u32,
    pub config: Atrac9Config,
    /// Loop in decoded-WAV coordinates, if the file defines one.
    pub loop_info: Option<LoopInfo>,
    /// Byte range of the `data` chunk within the input buffer.
    pub data_offset: usize,
    pub data_len: usize,
}

impl Container {
    /// Borrow the ATRAC9 `data` bytes from the original buffer.
    pub fn data<'a>(&self, buf: &'a [u8]) -> &'a [u8] {
        &buf[self.data_offset..self.data_offset + self.data_len]
    }

    /// Number of complete superframes in the data chunk.
    pub fn superframe_count(&self) -> u32 {
        if self.config.superframe_bytes == 0 {
            0
        } else {
            self.data_len as u32 / self.config.superframe_bytes
        }
    }
}

/// MSB-first bit reader over a byte slice (ATRAC9 packs fields big-endian-bitwise).
struct BitReader<'a> {
    bytes: &'a [u8],
    bit_pos: usize,
}

impl<'a> BitReader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, bit_pos: 0 }
    }
    fn read(&mut self, n: usize) -> u32 {
        let mut v = 0u32;
        for _ in 0..n {
            let byte = self.bytes.get(self.bit_pos >> 3).copied().unwrap_or(0);
            let bit = (byte >> (7 - (self.bit_pos & 7))) & 1;
            v = (v << 1) | bit as u32;
            self.bit_pos += 1;
        }
        v
    }
}

fn read_u16(b: &[u8], off: usize) -> u16 {
    u16::from_le_bytes([b[off], b[off + 1]])
}
fn read_u32(b: &[u8], off: usize) -> u32 {
    u32::from_le_bytes([b[off], b[off + 1], b[off + 2], b[off + 3]])
}

/// Parse the 4-byte ATRAC9 config data. `samples_per_block` comes from the `fmt`
/// extensible header and is used to derive the per-frame sample count.
fn parse_config(raw: [u8; 4], samples_per_block: u32) -> Result<Atrac9Config> {
    // Byte 0 must be the ATRAC9 config header/sync.
    if raw[0] != 0xFE {
        return Err(Error::BadConfig);
    }
    let mut br = BitReader::new(&raw[1..]);
    let sample_rate_index = br.read(4) as u8;
    let channel_config_index = br.read(3) as u8;
    let _validation = br.read(1); // must be 0 for a valid stream
    let frame_bytes = (br.read(11) + 1) as u16;
    let superframe_index = br.read(2) as u8;

    let frames_per_superframe = 1u8 << superframe_index;
    let superframe_bytes = frame_bytes as u32 * frames_per_superframe as u32;
    let sample_rate = *SAMPLE_RATES
        .get(sample_rate_index as usize)
        .ok_or(Error::BadConfig)?;
    let superframe_samples = samples_per_block;
    let frame_samples = if frames_per_superframe > 0 {
        samples_per_block / frames_per_superframe as u32
    } else {
        0
    };

    Ok(Atrac9Config {
        raw,
        sample_rate_index,
        sample_rate,
        channel_config_index,
        frame_bytes,
        superframe_index,
        frames_per_superframe,
        superframe_bytes,
        superframe_samples,
        frame_samples,
    })
}

/// Parse an AT9 container from a complete in-memory file buffer.
pub fn parse(buf: &[u8]) -> Result<Container> {
    if buf.len() < 12 || &buf[0..4] != b"RIFF" || &buf[8..12] != b"WAVE" {
        return Err(Error::NotRiff);
    }

    let mut sample_rate = 0u32;
    let mut channels = 0u16;
    let mut block_align = 0u16;
    let mut avg_bytes_per_sec = 0u32;
    let mut samples_per_block = 0u32;
    let mut config_raw: Option<[u8; 4]> = None;
    let mut is_atrac9 = false;

    let mut total_samples = 0u32;
    let mut encoder_delay = 0u32;
    let mut have_fact = false;

    let mut smpl_start: Option<u32> = None;
    let mut smpl_end: Option<u32> = None;

    let mut data_offset = 0usize;
    let mut data_len = 0usize;
    let mut have_data = false;

    let mut pos = 12usize;
    while pos + 8 <= buf.len() {
        let id = &buf[pos..pos + 4];
        let size = read_u32(buf, pos + 4) as usize;
        let body = pos + 8;
        let end = body.saturating_add(size);
        if end > buf.len() && id != b"data" {
            // A truncated non-data chunk: stop parsing cleanly.
            break;
        }

        match id {
            b"fmt " => {
                if size < 16 {
                    return Err(Error::Truncated("fmt "));
                }
                let f = &buf[body..body + size.min(buf.len() - body)];
                channels = read_u16(f, 2);
                sample_rate = read_u32(f, 4);
                avg_bytes_per_sec = read_u32(f, 8);
                block_align = read_u16(f, 12);
                // WAVE_FORMAT_EXTENSIBLE extra: wSamplesPerBlock(2) mask(4) GUID(16)
                // then 12 ATRAC9 bytes: version(4) config(4) reserved(4).
                if size >= 18 {
                    let cb = read_u16(f, 16) as usize;
                    if cb >= 22 && size >= 18 + cb {
                        samples_per_block = read_u16(f, 18) as u32;
                        let guid = &f[24..40];
                        is_atrac9 = guid == ATRAC9_GUID;
                        // config data sits 4 bytes into the 12 ATRAC9 extra bytes
                        // (after the 4-byte version), i.e. offset 44 in fmt body.
                        if size >= 48 {
                            config_raw =
                                Some([f[44], f[45], f[46], f[47]]);
                        }
                    }
                }
            }
            b"fact" => {
                let f = &buf[body..end];
                if f.len() >= 4 {
                    total_samples = read_u32(f, 0);
                    have_fact = true;
                }
                if f.len() >= 8 {
                    encoder_delay = read_u32(f, 4);
                }
            }
            b"smpl" => {
                let f = &buf[body..end];
                if f.len() >= 36 {
                    let num_loops = read_u32(f, 28);
                    if num_loops >= 1 && f.len() >= 36 + 24 {
                        // loop: cueId(4) type(4) start(4) end(4) frac(4) count(4)
                        smpl_start = Some(read_u32(f, 36 + 8));
                        smpl_end = Some(read_u32(f, 36 + 12));
                    }
                }
            }
            b"data" => {
                data_offset = body;
                data_len = size.min(buf.len() - body);
                have_data = true;
            }
            _ => {}
        }

        pos = end + (size & 1); // chunks are word-aligned
    }

    if sample_rate == 0 {
        return Err(Error::MissingChunk("fmt "));
    }
    if !is_atrac9 {
        return Err(Error::NotAtrac9);
    }
    if !have_data {
        return Err(Error::MissingChunk("data"));
    }
    if !have_fact {
        return Err(Error::MissingChunk("fact"));
    }
    let config_raw = config_raw.ok_or(Error::NotAtrac9)?;
    let config = parse_config(config_raw, samples_per_block)?;

    // Convert AT9-coord loop points to decoded-WAV coords (strip the delay).
    let loop_info = match (smpl_start, smpl_end) {
        (Some(s), Some(_e)) => {
            let start = s.saturating_sub(encoder_delay);
            // The loop always ends at the true end of the audio.
            let end = total_samples;
            Some(LoopInfo {
                start,
                end,
                is_whole: start == 0,
            })
        }
        _ => None,
    };

    Ok(Container {
        sample_rate,
        channels,
        block_align,
        avg_bytes_per_sec,
        total_samples,
        encoder_delay,
        config,
        loop_info,
        data_offset,
        data_len,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_fe740bf0_matches_known_values() {
        // The config data seen in real 48 kHz stereo AT9 BGM files.
        // samples-per-block (superframe samples) is 1024 for these.
        let cfg = parse_config([0xFE, 0x74, 0x0B, 0xF0], 1024).unwrap();
        assert_eq!(cfg.sample_rate_index, 7);
        assert_eq!(cfg.sample_rate, 48000);
        assert_eq!(cfg.channel_config_index, 2);
        assert_eq!(cfg.frame_bytes, 96);
        assert_eq!(cfg.superframe_index, 2);
        assert_eq!(cfg.frames_per_superframe, 4);
        assert_eq!(cfg.superframe_bytes, 384); // == block_align
        assert_eq!(cfg.frame_samples, 256);
    }

    #[test]
    fn rejects_bad_config_header() {
        assert_eq!(parse_config([0x00, 0, 0, 0], 1024), Err(Error::BadConfig));
    }

    #[test]
    fn rejects_non_riff() {
        assert!(matches!(parse(b"not a riff file at all"), Err(Error::NotRiff)));
    }
}
