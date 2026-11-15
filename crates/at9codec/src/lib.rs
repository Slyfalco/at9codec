//! # at9codec — clean-room ATRAC9 (AT9) codec
//!
//! A from-scratch, clean-room implementation of the PlayStation ATRAC9 ("AT9")
//! audio codec, with **no PlayStation code**. The goal is a small, portable
//! native library (`.dll`/`.so`/`.dylib`) plus a CLI, so apps can decode and
//! (eventually) encode AT9 without a Windows-only reference AT9 tool or a bundled
//! Wine.
//!
//! ## Status
//! * **Container parsing** — done ([`container`]): reads `fmt`/`fact`/`smpl`/`data`,
//!   the ATRAC9 config, true length, encoder delay and loop points.
//! * **Decode** — in progress ([`decode_to_pcm16`] returns `NotImplemented`).
//!   Based on the clean-room LibAtrac9/VGAudio reference implementations.
//! * **Encode** — not started (the hard part: no open ATRAC9 encoder exists).
//!
//! ## Clean-room note
//! Derived from the open, MIT-licensed clean-room references (LibAtrac9 for
//! decode, VGAudio for encode structure) plus public DSP technique — never from
//! the reference tool, which is used only as a black-box validation oracle.

pub mod container;
pub mod decode;
pub mod encode;
pub mod error;
pub mod wav;

mod ffi;

pub use container::{Atrac9Config, Container, LoopInfo};
pub use error::{Error, Result};

/// Library version string (from Cargo).
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Parse an AT9 file buffer into its [`Container`] metadata.
pub fn parse(buf: &[u8]) -> Result<Container> {
    container::parse(buf)
}

/// Decoded PCM audio: interleaved 16-bit samples plus format.
#[derive(Debug, Clone)]
pub struct Pcm {
    pub channels: u16,
    pub sample_rate: u32,
    /// Interleaved 16-bit samples, trimmed to the true length.
    pub samples: Vec<i16>,
}

impl Pcm {
    /// Number of sample frames (per-channel samples).
    pub fn frames(&self) -> usize {
        if self.channels == 0 {
            0
        } else {
            self.samples.len() / self.channels as usize
        }
    }
}

/// Decode an AT9 file buffer to interleaved 16-bit PCM, stripping the encoder
/// delay and trimming to the true (un-looped) length from the `fact` chunk.
pub fn decode_to_pcm16(buf: &[u8]) -> Result<Pcm> {
    let container = parse(buf)?;
    decode::decode_container(&container, buf)
}
