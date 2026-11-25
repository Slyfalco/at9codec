//! On-demand (pull) ATRAC9 decoding, for native playback without pre-decoding
//! the whole file or writing a temporary WAV.
//!
//! [`StreamingDecoder`] decodes one superframe at a time as the caller pulls PCM
//! — the same model an Ogg/Vorbis player uses (pull a packet, decode it, feed the
//! sink). It strips the encoder delay and trims to the true length exactly like
//! [`crate::decode_to_pcm16`], and [`StreamingDecoder::seek`] repositions for
//! sample-accurate looping.
//!
//! Seeking is bit-exact: because the IMDCT overlap-add reaches back only one
//! frame, decoding the single preceding superframe before the target restores
//! the exact overlap state, so a loop seam has no artefact.

use super::bitreader::BitReader;
use super::decoder::Decoder;
use super::DecoderConfig;
use crate::container::{self, Container};
use crate::error::{Error, Result};

/// A pull-based ATRAC9 decoder: call [`read`](Self::read) to decode the next PCM
/// on demand, and [`seek`](Self::seek) to reposition (e.g. to a loop point).
pub struct StreamingDecoder {
    config: DecoderConfig,
    decoder: Decoder,
    /// The raw ATRAC9 superframe data (owned, so the decoder is self-contained).
    data: Vec<u8>,
    superframe_count: usize,
    /// Index of the next superframe to decode.
    next_superframe: usize,
    /// Decoded-but-not-yet-returned interleaved samples, and the read cursor.
    staging: Vec<i16>,
    staging_read: usize,
    /// Per-channel frames still to drop from the front (encoder delay, or the
    /// intra-superframe offset after a seek).
    skip: u64,
    /// Output position in per-channel frames (0..`total_frames`).
    out_pos: u64,

    channels: u16,
    sample_rate: u32,
    total_frames: u64,
    encoder_delay: u64,
    loop_region: Option<(u64, u64)>,
}

impl StreamingDecoder {
    /// Build a streaming decoder from a complete AT9 file buffer.
    pub fn new(buf: &[u8]) -> Result<Self> {
        let container = container::parse(buf)?;
        Self::from_container(&container, buf)
    }

    /// Build from an already-parsed container (avoids re-parsing).
    pub fn from_container(container: &Container, buf: &[u8]) -> Result<Self> {
        let config = DecoderConfig::new(&container.config)?;
        if config.superframe_bytes == 0 {
            return Err(Error::BadConfig);
        }
        let data = container.data(buf).to_vec();
        let superframe_count = data.len() / config.superframe_bytes;
        let decoder = Decoder::new(config.clone());
        let loop_region = container.loop_info.as_ref().and_then(|l| if l.is_whole { None } else { Some((l.start as u64, l.end as u64)) });
        Ok(StreamingDecoder {
            channels: config.channel_count as u16,
            sample_rate: config.sample_rate,
            total_frames: container.total_samples as u64,
            encoder_delay: container.encoder_delay as u64,
            loop_region,
            config,
            decoder,
            data,
            superframe_count,
            next_superframe: 0,
            staging: Vec::new(),
            staging_read: 0,
            skip: container.encoder_delay as u64,
            out_pos: 0,
        })
    }

    pub fn channels(&self) -> u16 {
        self.channels
    }
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }
    /// Total playable length in per-channel frames (the un-looped length).
    pub fn total_frames(&self) -> u64 {
        self.total_frames
    }
    /// The file's loop region in output frames `(start, end_inclusive)`, if any.
    pub fn loop_region(&self) -> Option<(u64, u64)> {
        self.loop_region
    }
    /// Current playback position in per-channel frames.
    pub fn position(&self) -> u64 {
        self.out_pos
    }

    /// Decode the next superframe into `self.staging` (interleaved), applying any
    /// pending `skip`. Returns false at end of data.
    fn fill_staging(&mut self) -> Result<bool> {
        let ch = self.channels as usize;
        loop {
            if self.next_superframe >= self.superframe_count {
                return Ok(false);
            }
            let raw = self.decode_raw_superframe()?;
            let frames = raw.len() / ch; // per-channel frames in this superframe
            if self.skip >= frames as u64 {
                self.skip -= frames as u64; // whole superframe skipped (priming)
                continue;
            }
            let skip_samples = self.skip as usize * ch;
            self.skip = 0;
            self.staging = raw;
            self.staging_read = skip_samples;
            return Ok(true);
        }
    }

    /// Decode one superframe to interleaved PCM and advance the decoder state.
    fn decode_raw_superframe(&mut self) -> Result<Vec<i16>> {
        let base = self.next_superframe * self.config.superframe_bytes;
        let sf = &self.data[base..base + self.config.superframe_bytes];
        let mut out = Vec::with_capacity(self.config.superframe_samples * self.channels as usize);
        let mut off = 0usize;
        for _ in 0..self.config.frames_per_superframe {
            let mut br = BitReader::new(&sf[off..]);
            self.decoder.decode_frame(&mut br, &mut out)?;
            off += br.pos.div_ceil(8);
        }
        self.next_superframe += 1;
        Ok(out)
    }

    /// Fill `out` with up to `out.len()` interleaved 16-bit samples, decoding on
    /// demand. Returns the number written (less than `out.len()` only at the end
    /// of the stream). `out.len()` should be a multiple of the channel count.
    pub fn read(&mut self, out: &mut [i16]) -> usize {
        let ch = self.channels as usize;
        let mut written = 0usize;
        while written < out.len() {
            if self.staging_read >= self.staging.len() {
                if self.out_pos >= self.total_frames {
                    break; // reached the true (trimmed) end
                }
                match self.fill_staging() {
                    Ok(true) => {}
                    _ => break,
                }
                continue;
            }
            let avail = self.staging.len() - self.staging_read;
            let remaining = (self.total_frames - self.out_pos) as usize * ch;
            let n = avail.min(remaining).min(out.len() - written);
            if n == 0 {
                break;
            }
            out[written..written + n].copy_from_slice(&self.staging[self.staging_read..self.staging_read + n]);
            self.staging_read += n;
            self.out_pos += (n / ch) as u64;
            written += n;
        }
        written
    }

    /// Reposition to output frame `frame` (sample-accurate). The single preceding
    /// superframe is re-decoded to restore the IMDCT overlap, so the result is
    /// bit-identical to a full decode from the start.
    pub fn seek(&mut self, frame: u64) {
        let frame = frame.min(self.total_frames);
        self.decoder = Decoder::new(self.config.clone());
        self.staging.clear();
        self.staging_read = 0;
        self.out_pos = frame;

        let abs = self.encoder_delay + frame; // sample index in the raw (un-trimmed) stream
        let sf_samples = self.config.superframe_samples as u64;
        let target = (abs / sf_samples) as usize;
        let prime = target.saturating_sub(1);
        self.next_superframe = prime;
        // Decode the priming superframe(s) up to the target, discarding output but
        // advancing the overlap state.
        while self.next_superframe < target && self.next_superframe < self.superframe_count {
            let _ = self.decode_raw_superframe();
        }
        // Drop the remaining intra-superframe offset when the next read fills.
        self.skip = abs - target as u64 * sf_samples;
    }

    /// Reset to the start of the stream.
    pub fn rewind(&mut self) {
        self.seek(0);
        self.skip = self.encoder_delay;
        self.next_superframe = 0;
        self.decoder = Decoder::new(self.config.clone());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::encode::{encode_pcm16, EncodeOptions};

    /// Make a short stereo AT9 from a sine sweep for round-trip tests.
    fn make_at9(frames: usize) -> Vec<u8> {
        let mut pcm = Vec::with_capacity(frames * 2);
        for i in 0..frames {
            let t = i as f32 / 48000.0;
            let v = (f32::sin(t * 440.0 * std::f32::consts::TAU) * 12000.0) as i16;
            pcm.push(v);
            pcm.push((v as i32 * 3 / 4) as i16);
        }
        encode_pcm16(&pcm, 2, 48000, &EncodeOptions::default()).unwrap()
    }

    #[test]
    fn streaming_matches_whole_file_decode() {
        let at9 = make_at9(20_000);
        let whole = crate::decode_to_pcm16(&at9).unwrap();

        // Pull in small, odd-sized chunks to exercise staging boundaries.
        let mut sd = StreamingDecoder::new(&at9).unwrap();
        assert_eq!(sd.channels(), 2);
        assert_eq!(sd.sample_rate(), 48000);
        let mut streamed = Vec::new();
        let mut buf = vec![0i16; 700]; // 350 frames, not a superframe multiple
        loop {
            let n = sd.read(&mut buf);
            if n == 0 {
                break;
            }
            streamed.extend_from_slice(&buf[..n]);
        }
        assert_eq!(streamed.len(), whole.samples.len());
        assert!(streamed == whole.samples, "streamed output differs from decode_to_pcm16");
    }

    #[test]
    fn seek_is_bit_exact() {
        let at9 = make_at9(20_000);
        let whole = crate::decode_to_pcm16(&at9).unwrap();
        let ch = whole.channels as usize;

        let mut sd = StreamingDecoder::new(&at9).unwrap();
        // Seek to a frame mid-stream and read the rest; it must match the tail of
        // the full decode exactly (proves seamless loop seeks).
        let seek_frame = 9_001u64;
        sd.seek(seek_frame);
        let mut tail = Vec::new();
        let mut buf = vec![0i16; 1024];
        loop {
            let n = sd.read(&mut buf);
            if n == 0 {
                break;
            }
            tail.extend_from_slice(&buf[..n]);
        }
        let expected = &whole.samples[seek_frame as usize * ch..];
        assert_eq!(tail.len(), expected.len());
        assert!(tail == expected, "seek output differs from the full decode's tail");
    }
}
