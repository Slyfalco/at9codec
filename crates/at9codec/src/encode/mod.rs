//! ATRAC9 encode (in progress).
//!
//! The encoder is the inverse of [`crate::decode`]: PCM → forward MDCT → scale
//! factors → bit allocation → quantization → Huffman/raw packing → RIFF/WAVE AT9.
//! No open ATRAC9 encoder exists, so the quantization/bit-allocation *decisions*
//! are built from scratch, validated by round-tripping through our (bit-exact)
//! decoder and the reference tool.
//!
//! Landed so far: the forward MDCT ([`crate::decode::mdct::MdctTables::run_mdct`],
//! verified to round-trip with the IMDCT), the [`bitwriter`], the WAV reader
//! ([`wavread`]), ATRAC9 config-byte generation and the AT9 container writer.
//! Still to build: the per-frame encode pipeline and rate control.

pub mod bitwriter;
pub mod encoder;
pub mod huffenc;
pub mod resample;
pub mod wavread;

/// How large the encoded file may be. The codec owns the console limits, so a
/// caller names the target instead of passing a raw byte count. The encoder picks
/// the highest standard bitrate whose file fits, and returns
/// [`Error::ExceedsSizeLimit`] if even the lowest bitrate overflows the cap.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub enum SizeLimit {
    /// No cap (up to the RIFF 4 GiB container) — general game AT9.
    #[default]
    None,
    /// The PS Vita system/theme BGM limit (under 5 MB); the console won't use a
    /// larger theme BGM.
    VitaSystemBgm,
    /// The tighter 2 MiB limit for a theme BGM that must also run on PS4/PS5.
    PsCross,
    /// A custom cap in bytes.
    Bytes(usize),
}

impl SizeLimit {
    /// The cap in bytes (`0` = no cap). The console limits come from the codec's
    /// own constants, so callers never hard-code them.
    pub fn max_bytes(self) -> usize {
        match self {
            SizeLimit::None => 0,
            SizeLimit::VitaSystemBgm => encoder::VITA_BGM_MAX_BYTES,
            SizeLimit::PsCross => encoder::PS_CROSS_BGM_MAX_BYTES,
            SizeLimit::Bytes(n) => n,
        }
    }
}

/// Options for the encoders. Every field defaults to "automatic"/"off", so build
/// one with [`EncodeOptions::default()`] and set only what you need.
#[derive(Default)]
pub struct EncodeOptions<'a> {
    /// Target bitrate in bits/sec. `0` = auto (the highest standard rate that fits
    /// `size_limit`). A non-zero value must be one of the standard rates for the
    /// stream (see [`encoder::standard_bitrates`]), else [`Error::InvalidBitrate`].
    pub bitrate: u32,
    /// How large the output may be. Defaults to [`SizeLimit::None`]; name the
    /// target console with [`SizeLimit::VitaSystemBgm`] / [`SizeLimit::PsCross`]
    /// and the codec applies its own limit.
    pub size_limit: SizeLimit,
    /// Loop start, in decoded-PCM samples: `None` = no loop, `Some(0)` = loop the
    /// whole track, `Some(n)` = loop from sample `n`.
    pub loop_start: Option<u32>,
    /// Loop end — the inclusive last sample of the loop. `None` = to the track end.
    /// Only used when `loop_start` is set.
    pub loop_end: Option<u32>,
    /// Progress callback `(superframes_done, total)`. `None` = no reporting.
    pub progress: Option<crate::ProgressFn<'a>>,
}

/// Encode a WAV buffer to a complete AT9 file. Accepts 8/16/24/32-bit integer and
/// 32/64-bit float PCM WAV (converted to 16-bit, no resampling). See [`EncodeOptions`].
pub fn encode_wav(wav_buf: &[u8], opts: &EncodeOptions) -> Result<Vec<u8>> {
    let pcm = wavread::parse_wav(wav_buf)?;
    encoder::encode(&pcm, opts)
}

/// Encode interleaved 16-bit PCM samples to a complete AT9 file. See [`EncodeOptions`].
pub fn encode_pcm16(
    samples: &[i16],
    channels: u16,
    sample_rate: u32,
    opts: &EncodeOptions,
) -> Result<Vec<u8>> {
    let pcm = wavread::WavPcm {
        channels,
        sample_rate,
        samples: samples.to_vec(),
    };
    encoder::encode(&pcm, opts)
}

use crate::decode::tables::{
    CHANNEL_CONFIGS, SAMPLE_RATES, SAMPLING_RATE_INDEX_TO_FRAME_SAMPLES_POWER,
};
use crate::error::{Error, Result};
use bitwriter::BitWriter;

/// The ATRAC9 SubFormat GUID written into the `fmt ` chunk.
const ATRAC9_GUID: [u8; 16] = [
    0xD2, 0x42, 0xE1, 0x47, 0xBA, 0x36, 0x8D, 0x4D,
    0x88, 0xFC, 0x61, 0x65, 0x4F, 0x8C, 0x83, 0x6C,
];

/// Chosen encoder parameters for a stream.
#[derive(Debug, Clone, Copy)]
pub struct EncoderConfig {
    pub sample_rate_index: usize,
    pub channel_config_index: usize,
    pub superframe_index: u32,
    pub frame_bytes: u32,
    pub sample_rate: u32,
    pub channels: u16,
    pub frame_samples: usize,
    pub frames_per_superframe: usize,
    pub superframe_bytes: u32,
}

impl EncoderConfig {
    /// Pick a configuration for the given format and target bitrate (bits/sec).
    pub fn new(sample_rate: u32, channels: u16, bitrate: u32) -> Result<Self> {
        // Prefer the low-sample-rate variant (index <= 7).
        let sri = SAMPLE_RATES
            .iter()
            .take(8)
            .position(|&r| r == sample_rate)
            .ok_or(Error::NotImplemented("unsupported sample rate"))?;
        let cci = match channels {
            1 => 0usize, // mono
            2 => 2usize, // stereo (single stereo block)
            _ => return Err(Error::NotImplemented("only mono/stereo for now")),
        };
        if CHANNEL_CONFIGS[cci].channel_count as u16 != channels {
            return Err(Error::BadConfig);
        }
        let frame_samples_power = SAMPLING_RATE_INDEX_TO_FRAME_SAMPLES_POWER[sri] as u32;
        let frame_samples = 1usize << frame_samples_power;
        let superframe_index = 2u32; // 4 frames/superframe, as the reference tool uses
        let frames_per_superframe = 1usize << superframe_index;

        // frame_bytes from bitrate: bytes/sec / (frames/sec).
        let frames_per_sec = sample_rate as f64 / frame_samples as f64;
        let frame_bytes = ((bitrate as f64 / 8.0) / frames_per_sec).round() as u32;
        let frame_bytes = frame_bytes.clamp(1, 2048);

        Ok(EncoderConfig {
            sample_rate_index: sri,
            channel_config_index: cci,
            superframe_index,
            frame_bytes,
            sample_rate,
            channels,
            frame_samples,
            frames_per_superframe,
            superframe_bytes: frame_bytes << superframe_index,
        })
    }

    /// The forward-MDCT scale that makes MDCT∘IMDCT the identity (2/frame_samples).
    pub fn mdct_scale(&self) -> f64 {
        2.0 / self.frame_samples as f64
    }

    /// Build the 4-byte ATRAC9 config data (inverse of the decoder's parse).
    pub fn config_bytes(&self) -> [u8; 4] {
        let mut w = BitWriter::new();
        w.write(0xFE, 8); // header
        w.write(self.sample_rate_index as u32, 4);
        w.write(self.channel_config_index as u32, 3);
        w.write(0, 1); // validation bit
        w.write(self.frame_bytes - 1, 11);
        w.write(self.superframe_index, 2);
        w.align(8); // pad the final 3 bits
        let b = w.into_bytes();
        [b[0], b[1], b[2], b[3]]
    }
}

fn push_chunk(out: &mut Vec<u8>, id: &[u8; 4], body: &[u8]) {
    out.extend_from_slice(id);
    out.extend_from_slice(&(body.len() as u32).to_le_bytes());
    out.extend_from_slice(body);
    if body.len() & 1 == 1 {
        out.push(0); // word padding
    }
}

/// Write a complete AT9 RIFF/WAVE file from encoded superframe bytes.
///
/// `total_samples` is the true decoded length; `encoder_delay` the priming (256);
/// `loop_start`/`loop_end` are decoded-WAV sample coords (None = whole-track loop).
pub fn build_at9_container(
    config: &EncoderConfig,
    data: &[u8],
    total_samples: u32,
    encoder_delay: u32,
    loop_start: Option<u32>,
    loop_end: Option<u32>,
) -> Vec<u8> {
    // fmt (WAVE_FORMAT_EXTENSIBLE + ATRAC9): 52-byte body.
    let mut fmt = Vec::with_capacity(52);
    fmt.extend_from_slice(&0xFFFEu16.to_le_bytes()); // wFormatTag
    fmt.extend_from_slice(&config.channels.to_le_bytes());
    fmt.extend_from_slice(&config.sample_rate.to_le_bytes());
    let avg_bytes_per_sec =
        config.sample_rate * config.superframe_bytes / config.superframe_samples();
    fmt.extend_from_slice(&avg_bytes_per_sec.to_le_bytes());
    fmt.extend_from_slice(&(config.superframe_bytes as u16).to_le_bytes()); // nBlockAlign
    fmt.extend_from_slice(&0u16.to_le_bytes()); // wBitsPerSample = 0
    fmt.extend_from_slice(&34u16.to_le_bytes()); // cbSize
    fmt.extend_from_slice(&(config.superframe_samples() as u16).to_le_bytes()); // samples/block
    let channel_mask: u32 = if config.channels == 1 { 0x1 } else { 0x3 }; // FL / FL+FR
    fmt.extend_from_slice(&channel_mask.to_le_bytes()); // dwChannelMask
    fmt.extend_from_slice(&ATRAC9_GUID);
    fmt.extend_from_slice(&1u32.to_le_bytes()); // version
    fmt.extend_from_slice(&config.config_bytes()); // ATRAC9 config
    fmt.extend_from_slice(&0u32.to_le_bytes()); // reserved

    // fact: total samples, encoder delay, (delay again, as seen in real files).
    let mut fact = Vec::with_capacity(12);
    fact.extend_from_slice(&total_samples.to_le_bytes());
    fact.extend_from_slice(&encoder_delay.to_le_bytes());
    fact.extend_from_slice(&encoder_delay.to_le_bytes());

    // smpl: one forward loop in AT9 coords (decoded coord + delay). Only when a
    // loop is requested (the reference tool's own files omit smpl otherwise).
    // The loop END is the INCLUSIVE last sample: default to the track's last
    // sample (total-1) and clamp any custom end to it. An end of `total` (one
    // past the last sample) breaks looping on hardware, so never write that.
    let last = total_samples.saturating_sub(1);
    let start = loop_start.unwrap_or(0) + encoder_delay;
    let end = loop_end.map(|e| e.min(last)).unwrap_or(last) + encoder_delay;
    let mut smpl = Vec::with_capacity(60);
    smpl.extend_from_slice(&0u32.to_le_bytes()); // manufacturer
    smpl.extend_from_slice(&0u32.to_le_bytes()); // product
    let sample_period = (1_000_000_000u64 / config.sample_rate as u64) as u32;
    smpl.extend_from_slice(&sample_period.to_le_bytes());
    smpl.extend_from_slice(&60u32.to_le_bytes()); // MIDI unity note
    smpl.extend_from_slice(&0u32.to_le_bytes()); // MIDI pitch fraction
    smpl.extend_from_slice(&0u32.to_le_bytes()); // SMPTE format
    smpl.extend_from_slice(&0u32.to_le_bytes()); // SMPTE offset
    smpl.extend_from_slice(&1u32.to_le_bytes()); // num loops
    // cbSamplerData: the reference encoder writes the loop-list size here (24
    // bytes per loop record), NOT 0. The hardware uses this field to recognise
    // the loop, so a 0 here means the console ignores the loop and won't loop.
    smpl.extend_from_slice(&24u32.to_le_bytes()); // sampler data = 1 loop * 24 bytes
    smpl.extend_from_slice(&0u32.to_le_bytes()); // cue point id
    smpl.extend_from_slice(&0u32.to_le_bytes()); // loop type (forward)
    smpl.extend_from_slice(&start.to_le_bytes());
    smpl.extend_from_slice(&end.to_le_bytes());
    smpl.extend_from_slice(&0u32.to_le_bytes()); // fraction
    smpl.extend_from_slice(&0u32.to_le_bytes()); // play count (infinite)

    let mut body = Vec::new();
    push_chunk(&mut body, b"fmt ", &fmt);
    push_chunk(&mut body, b"fact", &fact);
    if loop_start.is_some() {
        push_chunk(&mut body, b"smpl", &smpl);
    }
    push_chunk(&mut body, b"data", data);

    let mut out = Vec::with_capacity(12 + body.len());
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&((4 + body.len()) as u32).to_le_bytes());
    out.extend_from_slice(b"WAVE");
    out.extend_from_slice(&body);
    out
}

impl EncoderConfig {
    fn superframe_samples(&self) -> u32 {
        (self.frame_samples * self.frames_per_superframe) as u32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_bytes_match_known_48k_stereo() {
        // 48 kHz stereo at 144 kbps should reproduce the real config FE 74 0B F0.
        let cfg = EncoderConfig::new(48000, 2, 144000).unwrap();
        assert_eq!(cfg.frame_bytes, 96);
        assert_eq!(cfg.config_bytes(), [0xFE, 0x74, 0x0B, 0xF0]);
    }

    #[test]
    fn container_round_trips_through_parser() {
        let cfg = EncoderConfig::new(48000, 2, 144000).unwrap();
        // One superframe of zero data, just to exercise the container.
        let data = vec![0u8; cfg.superframe_bytes as usize];
        let file = build_at9_container(&cfg, &data, 1024, 256, Some(0), None);
        let c = crate::container::parse(&file).unwrap();
        assert_eq!(c.sample_rate, 48000);
        assert_eq!(c.channels, 2);
        assert_eq!(c.total_samples, 1024);
        assert_eq!(c.encoder_delay, 256);
        assert_eq!(c.config.frame_bytes, 96);
        assert_eq!(c.block_align, 384);
        let l = c.loop_info.unwrap();
        assert_eq!(l.start, 0);
        assert!(l.is_whole);
    }
}
