//! ATRAC9 decode.
//!
//! Clean-room port of LibAtrac9's decoder, in progress. Foundation landed so far:
//! the bit reader ([`bitreader`]), constant tables ([`tables`]), the IMDCT and its
//! tables ([`mdct`]), and the decoder config/layout init ([`DecoderConfig`]).
//!
//! Still to port (the bitstream path): frame unpack + Huffman codebooks,
//! dequantization, scale factors, intensity stereo and band extension. Those
//! turn [`crate::decode_to_pcm16`] from a stub into a working decoder.

pub mod bitreader;
pub mod mdct;
pub mod tables;

use crate::container::Atrac9Config;
use crate::error::{Error, Result};
use tables::ChannelConfig;

/// Fully-derived decoder configuration (mirrors LibAtrac9's `ConfigData`).
#[derive(Debug, Clone)]
pub struct DecoderConfig {
    pub sample_rate_index: usize,
    pub channel_config_index: usize,
    pub frame_bytes: usize,
    pub superframe_index: u32,

    pub channel_config: ChannelConfig,
    pub channel_count: usize,
    pub sample_rate: u32,
    pub high_sample_rate: bool,
    pub frames_per_superframe: usize,
    pub frame_samples_power: u32,
    pub frame_samples: usize,
    pub superframe_bytes: usize,
    pub superframe_samples: usize,
}

impl DecoderConfig {
    /// Derive the full decoder layout from the parsed 4-byte ATRAC9 config.
    pub fn new(cfg: &Atrac9Config) -> Result<Self> {
        let sri = cfg.sample_rate_index as usize;
        let cci = cfg.channel_config_index as usize;
        if sri >= tables::SAMPLE_RATES.len() || cci >= tables::CHANNEL_CONFIGS.len() {
            return Err(Error::BadConfig);
        }
        let channel_config = tables::CHANNEL_CONFIGS[cci];
        let frames_per_superframe = 1usize << cfg.superframe_index;
        let frame_samples_power =
            tables::SAMPLING_RATE_INDEX_TO_FRAME_SAMPLES_POWER[sri] as u32;
        let frame_samples = 1usize << frame_samples_power;
        Ok(DecoderConfig {
            sample_rate_index: sri,
            channel_config_index: cci,
            frame_bytes: cfg.frame_bytes as usize,
            superframe_index: cfg.superframe_index as u32,
            channel_config,
            channel_count: channel_config.channel_count as usize,
            sample_rate: tables::SAMPLE_RATES[sri],
            high_sample_rate: sri > 7,
            frames_per_superframe,
            frame_samples_power,
            frame_samples,
            superframe_bytes: cfg.frame_bytes as usize * frames_per_superframe,
            superframe_samples: frame_samples * frames_per_superframe,
        })
    }

    /// Build the IMDCT tables for this configuration's frame size.
    pub fn mdct_tables(&self) -> mdct::MdctTables {
        mdct::MdctTables::new(self.frame_samples_power)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::container;

    #[test]
    fn derives_known_48k_stereo_layout() {
        let cfg = container::parse_config_for_test([0xFE, 0x74, 0x0B, 0xF0]);
        let d = DecoderConfig::new(&cfg).unwrap();
        assert_eq!(d.sample_rate, 48000);
        assert_eq!(d.channel_count, 2);
        assert_eq!(d.channel_config.block_count, 1);
        assert_eq!(d.frames_per_superframe, 4);
        assert_eq!(d.frame_samples_power, 8);
        assert_eq!(d.frame_samples, 256);
        assert_eq!(d.superframe_samples, 1024);
        assert_eq!(d.superframe_bytes, 384);
        assert!(!d.high_sample_rate);
    }
}
