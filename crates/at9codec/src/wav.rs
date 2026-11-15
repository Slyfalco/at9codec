//! Minimal 16-bit PCM WAV writer, used by the CLI to emit decoded audio.

use std::io::{self, Write};

/// Write interleaved 16-bit PCM samples as a canonical WAV file.
pub fn write_pcm16<W: Write>(
    mut w: W,
    channels: u16,
    sample_rate: u32,
    samples: &[i16],
) -> io::Result<()> {
    let bytes_per_sample = 2u16;
    let block_align = channels * bytes_per_sample;
    let byte_rate = sample_rate * block_align as u32;
    let data_bytes = (samples.len() * 2) as u32;
    let riff_size = 36 + data_bytes;

    w.write_all(b"RIFF")?;
    w.write_all(&riff_size.to_le_bytes())?;
    w.write_all(b"WAVE")?;

    w.write_all(b"fmt ")?;
    w.write_all(&16u32.to_le_bytes())?;
    w.write_all(&1u16.to_le_bytes())?; // PCM
    w.write_all(&channels.to_le_bytes())?;
    w.write_all(&sample_rate.to_le_bytes())?;
    w.write_all(&byte_rate.to_le_bytes())?;
    w.write_all(&block_align.to_le_bytes())?;
    w.write_all(&(bytes_per_sample * 8).to_le_bytes())?;

    w.write_all(b"data")?;
    w.write_all(&data_bytes.to_le_bytes())?;
    let mut buf = Vec::with_capacity(samples.len() * 2);
    for s in samples {
        buf.extend_from_slice(&s.to_le_bytes());
    }
    w.write_all(&buf)?;
    Ok(())
}
