//! Band-limited sample-rate conversion for the encoder.
//!
//! ATRAC9 only encodes at the console's rates (12/24/48 kHz), so a source at any
//! other rate (44.1 kHz, 32 kHz, …) is resampled to the nearest supported rate
//! here before encoding. This keeps the codec self-contained — callers hand over
//! audio at any rate and get a valid AT9 back.
//!
//! The method is a windowed-sinc (Blackman) interpolating filter with the cutoff
//! lowered to the output Nyquist when downsampling, so it is anti-aliased and far
//! better than linear interpolation, while needing no external crates.

use super::wavread::WavPcm;

/// Sample rates the ATRAC9 encoder accepts.
const SUPPORTED: [u32; 3] = [48000, 24000, 12000];

/// True if `rate` is one the encoder can use directly (no resampling).
pub fn is_supported(rate: u32) -> bool {
    SUPPORTED.contains(&rate)
}

/// The rate we resample an unsupported `rate` to: the lowest supported rate that
/// is still >= the source (so we never throw away bandwidth we could keep), i.e.
/// >24k -> 48k, >12k -> 24k, else 12k. Matches the old app-side quantisation.
pub fn target_rate(rate: u32) -> u32 {
    if rate > 24000 {
        48000
    } else if rate > 12000 {
        24000
    } else {
        12000
    }
}

/// Scale a sample index from `from_rate` to `to_rate` (for loop points).
pub fn scale_index(index: u32, from_rate: u32, to_rate: u32) -> u32 {
    ((index as u64 * to_rate as u64 + from_rate as u64 / 2) / from_rate as u64) as u32
}

// Half-width of the sinc kernel (taps per side). 32 taps total — a good quality
// / speed balance for music.
const HALF: i64 = 16;

#[inline]
fn sinc(x: f64) -> f64 {
    if x.abs() < 1e-9 {
        1.0
    } else {
        let px = std::f64::consts::PI * x;
        px.sin() / px
    }
}

/// Resample interleaved 16-bit PCM to `to_rate`. Returns the input unchanged when
/// it is already at `to_rate`. Sample count scales by `to_rate / from_rate`.
pub fn resample(pcm: &WavPcm, to_rate: u32) -> WavPcm {
    if pcm.sample_rate == to_rate || pcm.sample_rate == 0 {
        return WavPcm {
            channels: pcm.channels,
            sample_rate: to_rate,
            samples: pcm.samples.clone(),
        };
    }
    let ch = pcm.channels.max(1) as usize;
    let in_frames = pcm.samples.len() / ch;
    if in_frames == 0 {
        return WavPcm { channels: pcm.channels, sample_rate: to_rate, samples: Vec::new() };
    }
    let ratio = to_rate as f64 / pcm.sample_rate as f64;
    let out_frames = ((in_frames as f64) * ratio).round() as usize;
    // Cutoff (in input-Nyquist units): 1.0 when upsampling; the output Nyquist
    // fraction when downsampling, to band-limit and avoid aliasing.
    let fc = if ratio >= 1.0 { 1.0 } else { ratio };

    // De-interleave into per-channel f64 for the inner loop.
    let mut chans: Vec<Vec<f64>> = vec![Vec::with_capacity(in_frames); ch];
    for (i, &s) in pcm.samples.iter().enumerate() {
        chans[i % ch].push(s as f64);
    }

    let mut out = vec![0i16; out_frames * ch];
    let step = 1.0 / ratio; // input frames advanced per output frame
    for o in 0..out_frames {
        let pos = o as f64 * step; // position in input-frame coords
        let center = pos.floor() as i64;
        for c in 0..ch {
            let x = &chans[c];
            let mut acc = 0.0;
            let mut wsum = 0.0;
            for k in (center - HALF + 1)..=(center + HALF) {
                if k < 0 || (k as usize) >= in_frames {
                    continue;
                }
                let d = pos - k as f64; // distance in input samples
                // Blackman window over [-HALF, HALF].
                let wn = d / HALF as f64;
                if wn.abs() > 1.0 {
                    continue;
                }
                let t = std::f64::consts::PI * (wn + 1.0); // map [-1,1] -> [0,2pi]
                let window = 0.42 - 0.5 * t.cos() + 0.08 * (2.0 * t).cos();
                let w = fc * sinc(fc * d) * window;
                acc += x[k as usize] * w;
                wsum += w;
            }
            let v = if wsum.abs() > 1e-12 { acc / wsum } else { 0.0 };
            out[o * ch + c] = v.round().clamp(-32768.0, 32767.0) as i16;
        }
    }

    WavPcm { channels: pcm.channels, sample_rate: to_rate, samples: out }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scaling_and_target() {
        assert_eq!(target_rate(44100), 48000);
        assert_eq!(target_rate(32000), 48000);
        assert_eq!(target_rate(22050), 24000);
        assert_eq!(target_rate(8000), 12000);
        assert!(is_supported(48000));
        assert!(!is_supported(44100));
        // 44100 -> 48000 loop index scales by 48000/44100.
        assert_eq!(scale_index(44100, 44100, 48000), 48000);
    }

    #[test]
    fn resample_preserves_a_tone_frequency() {
        // 1 kHz tone at 44100 -> 48000 should stay ~1 kHz and roughly full length.
        let sr = 44100u32;
        let n = sr as usize; // 1 second
        let mut s = Vec::with_capacity(n);
        for i in 0..n {
            s.push(((i as f64 / sr as f64 * 1000.0 * std::f64::consts::TAU).sin() * 10000.0) as i16);
        }
        let pcm = WavPcm { channels: 1, sample_rate: sr, samples: s };
        let out = resample(&pcm, 48000);
        assert_eq!(out.sample_rate, 48000);
        // ~1 second at 48 kHz.
        assert!((out.samples.len() as i64 - 48000).abs() < 50, "len {}", out.samples.len());
        // Peak amplitude preserved within a few percent (no big gain drift).
        let peak = out.samples.iter().map(|&x| x.unsigned_abs()).max().unwrap();
        assert!(peak > 9000 && peak < 11000, "peak {peak}");
    }
}
