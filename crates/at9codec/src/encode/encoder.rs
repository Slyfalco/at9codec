//! First-generation ATRAC9 frame encoder.
//!
//! A deliberately simple but **valid** encoder: it emits a bitstream any ATRAC9
//! decoder (ours and the reference tool) reads back. Strategy:
//! * dual-mono block layout for stereo (each channel is channel-0 of its block,
//!   so scale factors use the simple CLC mode — no joint-stereo Huffman),
//! * no band extension,
//! * **raw** spectral coding (precision ≥ 7), which has no Huffman codebook-fit
//!   constraints, so any quantized value in range is encodable.
//!
//! Quantization uses the exact `QuantizerStepSize * SpectrumScale` the decoder
//! uses, so encode→decode reconstructs the forward-MDCT spectrum faithfully.
//! Huffman coding (smaller files) and joint stereo are later optimisations.

use super::bitwriter::BitWriter;
use super::wavread::WavPcm;
use super::{build_at9_container, EncoderConfig};
use crate::decode::mdct::MdctTables;
use crate::decode::tables::*;
use crate::error::{Error, Result};

const FRAME_SAMPLES: usize = 256; // 48/44.1 kHz low-rate configs
const ENCODER_DELAY: u32 = 256;

/// Per-unit quantization choice.
struct UnitQuant {
    sf: usize,
    precision: usize,      // QuantizerStepSize index (== precisions[i])
    precision_fine: usize, // QuantizerFineStepSize index (== precisions_fine[i])
}

/// Pick the smallest scale factor (>=7, so coding stays raw) that keeps the
/// coarse quantized values in range for this unit's coefficients.
fn choose_unit_quant(coeffs: &[f64]) -> UnitQuant {
    let cmax = coeffs.iter().fold(0.0f64, |m, &c| m.max(c.abs()));
    for sf in 7..=30usize {
        let precision = sf.min(15);
        let cstep = QUANTIZER_STEP_SIZE[precision] * SPECTRUM_SCALE[sf];
        let qmax = (cmax / cstep).round() as i64;
        let limit = (1i64 << precision) - 1;
        if qmax <= limit {
            return UnitQuant {
                sf,
                precision,
                precision_fine: sf.saturating_sub(15),
            };
        }
    }
    UnitQuant { sf: 30, precision: 15, precision_fine: 15 }
}

fn clamp_signed(v: i64, bits: usize) -> i32 {
    let hi = (1i64 << (bits - 1)) - 1;
    let lo = -(1i64 << (bits - 1));
    v.clamp(lo, hi) as i32
}

/// Encode one mono block (one channel's spectrum for one frame) into `w`.
fn encode_mono_block(w: &mut BitWriter, spectra: &[f64; 256], first_in_superframe: bool) {
    let min_band_count = MIN_BAND_COUNT[0] as u32; // low sample rate
    let band_count = MAX_BAND_COUNT[7] as u32; // 18 for 48 kHz low config
    let quant_unit_count = BAND_TO_QUANT_UNIT_COUNT[band_count as usize] as usize; // 30

    // Block header.
    w.write(if first_in_superframe { 0 } else { 1 }, 1); // !FirstInSuperframe
    w.write(0, 1); // ReuseBandParams = 0

    // Band params (Mono: band_count, no stereo band, band_extension_enabled=0).
    w.write(band_count - min_band_count, 4);
    w.write(0, 1); // band_extension_enabled

    // Gradient params: mode 0, flat gradient value 0 over all units.
    w.write(0, 2); // gradient mode
    w.write(0, 6); // start unit
    w.write(quant_unit_count as u32 - 1, 6); // end unit (decoder adds 1)
    w.write(0, 5); // start value
    w.write(0, 5); // end value
    w.write(0, 4); // gradient boundary

    // (Mono: no stereo params.)
    // Extension params: no band extension, no extension data.
    w.write(0, 1); // HasExtensionData = 0

    // Choose per-unit quantization from the spectrum.
    let mut units = Vec::with_capacity(quant_unit_count);
    for i in 0..quant_unit_count {
        let lo = QUANT_UNIT_TO_COEFF_INDEX[i] as usize;
        let hi = QUANT_UNIT_TO_COEFF_INDEX[i + 1] as usize;
        units.push(choose_unit_quant(&spectra[lo..hi]));
    }

    // Scale factors via CLC (coding mode 1), fixed 5-bit length (covers 0..31).
    w.write(1, 2); // scale factor coding mode = CLC
    w.write(5 - 2, 2); // bitLength = read(2)+2 = 5  (so no baseValue is sent)
    for u in &units {
        w.write(u.sf as u32, 5);
    }

    // Spectra: raw signed coding (precision+1 bits each), since precision >= 7.
    for (i, u) in units.iter().enumerate() {
        let lo = QUANT_UNIT_TO_COEFF_INDEX[i] as usize;
        let hi = QUANT_UNIT_TO_COEFF_INDEX[i + 1] as usize;
        let cstep = QUANTIZER_STEP_SIZE[u.precision] * SPECTRUM_SCALE[u.sf];
        let bits = (u.precision + 1) as u32;
        for &c in &spectra[lo..hi] {
            let q = clamp_signed((c / cstep).round() as i64, u.precision + 1);
            w.write_signed(q, bits);
        }
    }

    // Fine residual coding where precision_fine > 0.
    for (i, u) in units.iter().enumerate() {
        if u.precision_fine == 0 {
            continue;
        }
        let lo = QUANT_UNIT_TO_COEFF_INDEX[i] as usize;
        let hi = QUANT_UNIT_TO_COEFF_INDEX[i + 1] as usize;
        let cstep = QUANTIZER_STEP_SIZE[u.precision] * SPECTRUM_SCALE[u.sf];
        let fstep = QUANTIZER_FINE_STEP_SIZE[u.precision_fine] * SPECTRUM_SCALE[u.sf];
        let bits = (u.precision_fine + 1) as u32;
        for &c in &spectra[lo..hi] {
            let q = clamp_signed((c / cstep).round() as i64, u.precision + 1);
            let residual = c - q as f64 * cstep;
            let qf = clamp_signed((residual / fstep).round() as i64, u.precision_fine + 1);
            w.write_signed(qf, bits);
        }
    }

    w.align(8);
}

/// Encode 16-bit PCM to a complete AT9 file. `bitrate` is advisory; the frame
/// size is sized to fit the content (raw coding is not yet rate-controlled).
pub fn encode_to_at9(wav: &WavPcm, _bitrate: u32) -> Result<Vec<u8>> {
    if wav.sample_rate == 0 || wav.channels == 0 {
        return Err(Error::BadConfig);
    }
    let channels = wav.channels as usize;
    let total_samples = (wav.samples.len() / channels) as u32;

    // Config: mono or dual-mono (channel_config 0 / 1).
    let sri = SAMPLE_RATES
        .iter()
        .take(8)
        .position(|&r| r == wav.sample_rate)
        .ok_or(Error::NotImplemented("unsupported sample rate"))?;
    let cci = match channels {
        1 => 0usize,
        2 => 1usize, // dual mono
        _ => return Err(Error::NotImplemented("only mono/stereo")),
    };
    let frames_per_superframe = 4usize;

    // De-interleave into per-channel PCM (f64).
    let frame_samples = FRAME_SAMPLES;
    let samples_per_ch = wav.samples.len() / channels;
    let mut ch_pcm: Vec<Vec<f64>> = vec![Vec::with_capacity(samples_per_ch); channels];
    for (i, &s) in wav.samples.iter().enumerate() {
        ch_pcm[i % channels].push(s as f64);
    }

    // Frames to encode: cover all input + a flush frame (MDCT one-frame latency),
    // rounded up to a whole superframe.
    let real_frames = samples_per_ch.div_ceil(frame_samples);
    let mut num_frames = real_frames + 1;
    num_frames = num_frames.div_ceil(frames_per_superframe) * frames_per_superframe;

    // Forward MDCT per channel → spectra per frame.
    let mdct = MdctTables::new(8);
    let scale = 2.0 / frame_samples as f64;
    let mut spectra: Vec<Vec<[f64; 256]>> = vec![Vec::with_capacity(num_frames); channels];
    for c in 0..channels {
        let mut prev = [0.0f64; 256];
        let pcm = &ch_pcm[c];
        for f in 0..num_frames {
            let mut input = [0.0f64; 256];
            for i in 0..frame_samples {
                let idx = f * frame_samples + i;
                input[i] = pcm.get(idx).copied().unwrap_or(0.0);
            }
            let mut out = [0.0f64; 256];
            mdct.run_mdct(&mut prev, &input, &mut out, scale);
            spectra[c].push(out);
        }
    }

    // Pass 1: pack every frame, find the largest (sets frame_bytes).
    let mut frame_bytes_list: Vec<Vec<u8>> = Vec::with_capacity(num_frames);
    for f in 0..num_frames {
        let mut w = BitWriter::new();
        let first = f % frames_per_superframe == 0;
        for c in 0..channels {
            encode_mono_block(&mut w, &spectra[c][f], first);
        }
        frame_bytes_list.push(w.into_bytes());
    }
    let max_frame = frame_bytes_list.iter().map(|b| b.len()).max().unwrap_or(1);
    let frame_bytes = max_frame.clamp(1, 2048) as u32;
    let superframe_bytes = (frame_bytes as usize) * frames_per_superframe;

    // Assemble superframes, padding each to superframe_bytes.
    let mut data = Vec::with_capacity(num_frames / frames_per_superframe * superframe_bytes);
    for sf in 0..(num_frames / frames_per_superframe) {
        let sf_start = data.len();
        for fi in 0..frames_per_superframe {
            data.extend_from_slice(&frame_bytes_list[sf * frames_per_superframe + fi]);
        }
        data.resize(sf_start + superframe_bytes, 0);
    }

    let config = EncoderConfig {
        sample_rate_index: sri,
        channel_config_index: cci,
        superframe_index: 2,
        frame_bytes,
        sample_rate: wav.sample_rate,
        channels: wav.channels,
        frame_samples,
        frames_per_superframe,
        superframe_bytes: superframe_bytes as u32,
    };

    Ok(build_at9_container(&config, &data, total_samples, ENCODER_DELAY, None))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_decode_round_trip_is_accurate() {
        // Stereo tone: encode with our encoder, decode with our (bit-exact)
        // decoder, and check the reconstruction is close to the original.
        let sr = 48000u32;
        let n = 4096usize; // samples per channel
        let mut samples = Vec::with_capacity(n * 2);
        for i in 0..n {
            let t = i as f64 / sr as f64;
            let l = ((t * 440.0 * std::f64::consts::TAU).sin() * 8000.0) as i16;
            let r = ((t * 660.0 * std::f64::consts::TAU).sin() * 6000.0) as i16;
            samples.push(l);
            samples.push(r);
        }
        let wav = WavPcm { channels: 2, sample_rate: sr, samples: samples.clone() };

        let at9 = encode_to_at9(&wav, 0).unwrap();
        let pcm = crate::decode_to_pcm16(&at9).unwrap();

        assert_eq!(pcm.channels, 2);
        assert_eq!(pcm.sample_rate, sr);
        assert_eq!(pcm.samples.len(), samples.len());

        // Skip the first frame (MDCT warm-up) and compare the steady state.
        let skip = 512 * 2;
        let mut err = 0.0f64;
        let mut energy = 0.0f64;
        for i in skip..samples.len() {
            let d = samples[i] as f64 - pcm.samples[i] as f64;
            err += d * d;
            energy += (samples[i] as f64).powi(2);
        }
        let snr = 10.0 * (energy / err).log10();
        assert!(snr > 40.0, "round-trip SNR too low: {snr:.1} dB");
    }
}
