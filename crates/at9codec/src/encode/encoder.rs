//! ATRAC9 frame encoder with Huffman coding + bit allocation.
//!
//! Targets a standard ATRAC9 bitrate (so the reference tool and hardware decoders accept
//! the output). Strategy, mirroring the reference tool's observed scheme:
//! * dual-mono block layout (each channel is channel-0 of its block → simple CLC
//!   scale factors, no joint-stereo Huffman),
//! * band-limit each block to where the spectrum has energy,
//! * scale factors from the spectral envelope; a flat gradient sets precision,
//!   searched per frame for the best quality that fits the byte budget,
//! * Huffman-code the quantized spectra (raw only where precision forces it),
//! * per-unit "bump precision until encodable" so every symbol exists in the
//!   decoder-selected codebook (some low-precision codebooks lack e.g. all-zero).
//!
//! Quantization uses the decoder's exact `step * scale`, and all derived state
//! (codebook set, precisions) is recomputed exactly as the decoder does, so the
//! transmitted scale factors + gradient fully determine decoding.

use super::bitwriter::BitWriter;
use super::huffenc;
use super::wavread::WavPcm;
use super::{build_at9_container, EncoderConfig};
use crate::decode::huffman::HuffmanTables;
use crate::decode::mdct::MdctTables;
use crate::decode::tables::*;
use crate::error::{Error, Result};

const FRAME_SAMPLES: usize = 256;
const ENCODER_DELAY: u32 = 256;
const FRAMES_PER_SUPERFRAME: usize = 4;

/// One block's chosen parameters + quantized data, ready to pack.
struct BlockPlan {
    band_count: usize,
    quant_unit_count: usize,
    grad_start_unit: i32,
    grad_start_value: i32,
    grad_boundary: i32,
    scale_factors: [i32; 31],
    precisions: [i32; 30],
    precisions_fine: [i32; 30],
    codebook_set: [i32; 30],
    quantized: [i32; 256],
    quantized_fine: [i32; 256],
}

fn ceil_log2(x: f64) -> i32 {
    if x <= 0.0 {
        return i32::MIN / 2;
    }
    x.log2().ceil() as i32
}

/// Scale factor per unit: scale[sf] must be >= the unit's largest coefficient.
fn scale_factors_from_spectrum(spectra: &[f64; 256], quc: usize) -> [i32; 31] {
    let mut sf = [0i32; 31];
    for i in 0..quc {
        let lo = QUANT_UNIT_TO_COEFF_INDEX[i] as usize;
        let hi = QUANT_UNIT_TO_COEFF_INDEX[i + 1] as usize;
        let cmax = spectra[lo..hi].iter().fold(0.0f64, |m, &c| m.max(c.abs()));
        sf[i] = (15 + ceil_log2(cmax)).clamp(0, 31);
    }
    sf
}

/// Highest quant unit with energy → smallest band_count covering it.
fn choose_band_count(spectra: &[f64; 256], sri: usize) -> usize {
    // the reference tool never emits the top band (18); cap at 16 to match the reference encoder.
    let max_band = (MAX_BAND_COUNT[sri] as usize).min(16);
    let min_band = MIN_BAND_COUNT[0] as usize;
    let max_quc = BAND_TO_QUANT_UNIT_COUNT[max_band] as usize;
    let gmax = spectra.iter().fold(0.0f64, |m, &c| m.max(c.abs()));
    let thresh = gmax * 1e-5 + 1e-9;
    let mut needed = 1usize;
    for i in 0..max_quc {
        let lo = QUANT_UNIT_TO_COEFF_INDEX[i] as usize;
        let hi = QUANT_UNIT_TO_COEFF_INDEX[i + 1] as usize;
        if spectra[lo..hi].iter().any(|&c| c.abs() > thresh) {
            needed = i + 1;
        }
    }
    // Smallest band whose unit count covers `needed`.
    for b in min_band..=max_band {
        if BAND_TO_QUANT_UNIT_COUNT[b] as usize >= needed {
            return b;
        }
    }
    max_band
}

/// Replicates the decoder's CalculateSpectrumCodebookIndex (low sample rate).
fn calc_codebook_set(sf: &[i32; 31], coded_quant_units: usize) -> [i32; 30] {
    let mut cb = [0i32; 30];
    let qu = coded_quant_units;
    if qu <= 1 {
        return cb;
    }
    let mut s = *sf;
    let orig = s[qu];
    s[qu] = s[qu - 1];
    let mut avg = 0i32;
    if qu > 12 {
        for &v in s.iter().take(12) {
            avg += v;
        }
        avg = (avg + 6) / 12;
    }
    for i in 8..qu {
        let prev = s[i - 1];
        let next = s[i + 1];
        let min_sf = prev.min(next);
        if s[i] - min_sf >= 3 || s[i] - prev + s[i] - next >= 3 {
            cb[i] = 1;
        }
    }
    for i in 12..qu {
        if cb[i] == 0 {
            let min_sf = s[i - 1].min(s[i + 1]);
            let bias = if QUANT_UNIT_TO_COEFF_COUNT[i] == 16 { 1 } else { 0 };
            if s[i] - min_sf >= 2 && s[i] >= avg - bias {
                cb[i] = 1;
            }
        }
    }
    let _ = orig;
    cb
}

const BASE_CURVE: [u8; 48] = [
    1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 4, 4, 5, 5, 6, 7, 8, 9, 10, 11, 12, 13,
    15, 16, 18, 19, 20, 21, 22, 23, 24, 25, 26, 26, 27, 27, 28, 28, 28, 29, 29,
    29, 29, 30, 30, 30, 30,
];

fn gen_gradient_curves() -> [[u8; 48]; 48] {
    let base_len = BASE_CURVE.len();
    let mut curves = [[0u8; 48]; 48];
    for length in 1..=base_len {
        for i in 0..length {
            curves[length - 1][i] = BASE_CURVE[i * base_len / length];
        }
    }
    curves
}

/// Build the gradient array for mode 2 (end unit/value fixed at 31), replicating
/// the decoder's CreateGradient for a rising curve.
fn create_gradient(gsu: i32, gsv: i32, quc: usize, curves: &[[u8; 48]; 48]) -> [i32; 31] {
    let geu = 31i32;
    let gev = 31i32;
    let mut g = [0i32; 31];
    let value_count = gev - gsv;
    let unit_count = geu - gsu;
    for i in 0..geu as usize {
        g[i] = gsv;
    }
    // (units >= geu would be gev, but quc <= 30 < 31 so they are unused.)
    if unit_count <= 0 || value_count == 0 {
        return g;
    }
    let curve = &curves[(unit_count - 1) as usize];
    let scale = (value_count - 1) as f64 / 31.0;
    let base_val = gsv + 1;
    for i in gsu..geu {
        let c = curve[(i - gsu) as usize] as f64;
        if (i as usize) < 31 {
            g[i as usize] = base_val + (c * scale) as i32;
        }
    }
    let _ = quc;
    g
}

/// Replicates the decoder's CalculateMask.
fn calc_mask(sf: &[i32; 31], quc: usize) -> [i32; 30] {
    let mut mask = [0i32; 30];
    for i in 1..quc {
        let delta = sf[i] - sf[i - 1];
        if delta > 1 {
            mask[i] += (delta - 1).min(5);
        } else if delta < -1 {
            mask[i - 1] += (-delta - 1).min(5);
        }
    }
    mask
}

/// Replicates the decoder's CalculatePrecisions for gradient mode 2.
fn calc_precisions(
    sf: &[i32; 31],
    mask: &[i32; 30],
    gradient: &[i32; 31],
    boundary: i32,
    quc: usize,
) -> ([i32; 30], [i32; 30]) {
    let mut prec = [0i32; 30];
    let mut fine = [0i32; 30];
    for i in 0..quc {
        let mut p = sf[i] + mask[i] - gradient[i];
        if p > 0 {
            p = 3 * p / 8; // gradient mode 2
        }
        prec[i] = p;
    }
    for i in 0..quc {
        if prec[i] < 1 {
            prec[i] = 1;
        }
    }
    for i in 0..(boundary as usize).min(quc) {
        prec[i] += 1;
    }
    for i in 0..quc {
        if prec[i] > 15 {
            fine[i] = prec[i] - 15;
            prec[i] = 15;
        }
    }
    (prec, fine)
}

fn clamp_signed(v: i64, bits: i32) -> i32 {
    let hi = (1i64 << (bits - 1)) - 1;
    let lo = -(1i64 << (bits - 1));
    v.clamp(lo, hi) as i32
}

/// Plan one block for a given gradient start-value (the rate knob). Codebook-fit
/// is handled at pack time by symbol snapping, so no precision bumping is needed.
/// Scale-factor context for a block: everything that depends only on the
/// spectrum/band/boost (not the gradient), computed once and reused across the
/// gradient search.
struct SfCtx {
    sf: [i32; 31],
    quc: usize,
    band_count: usize,
    mask: [i32; 30],
    codebook_set: [i32; 30],
}

fn block_sf(spectra: &[f64; 256], boost: i32, band_override: Option<usize>, sri: usize) -> SfCtx {
    let band_count = band_override.unwrap_or_else(|| choose_band_count(spectra, sri));
    let quc = BAND_TO_QUANT_UNIT_COUNT[band_count] as usize;
    let mut sf = scale_factors_from_spectrum(spectra, quc);
    if boost > 0 {
        for s in sf.iter_mut().take(quc) {
            *s = (*s + boost).min(31);
        }
    }
    let mask = calc_mask(&sf, quc);
    let codebook_set = calc_codebook_set(&sf, quc);
    SfCtx { sf, quc, band_count, mask, codebook_set }
}

#[allow(clippy::needless_range_loop)]
fn plan_from_sf(ctx: &SfCtx, spectra: &[f64; 256], gsv: i32, gb: i32, curves: &[[u8; 48]; 48]) -> BlockPlan {
    let quc = ctx.quc;
    let sf = ctx.sf;
    // Gradient mode 2 curve near the top band; boundary lifts the low units.
    let gsu = ((quc as i32) - 1).clamp(1, 30);
    let gb = gb.clamp(0, (quc as i32).min(15));
    let gradient = create_gradient(gsu, gsv, quc, curves);
    let (precisions, precisions_fine) = calc_precisions(&sf, &ctx.mask, &gradient, gb, quc);
    let codebook_set = ctx.codebook_set;

    let mut quantized = [0i32; 256];
    let mut quantized_fine = [0i32; 256];
    for i in 0..quc {
        let lo = QUANT_UNIT_TO_COEFF_INDEX[i] as usize;
        let hi = QUANT_UNIT_TO_COEFF_INDEX[i + 1] as usize;
        let p = precisions[i];
        let cstep = QUANTIZER_STEP_SIZE[p as usize] * SPECTRUM_SCALE[sf[i] as usize];
        for k in lo..hi {
            quantized[k] = clamp_signed((spectra[k] / cstep).round() as i64, p + 1);
        }
        if precisions_fine[i] > 0 {
            let pf = precisions_fine[i];
            let fstep = QUANTIZER_FINE_STEP_SIZE[pf as usize] * SPECTRUM_SCALE[sf[i] as usize];
            for k in lo..hi {
                let residual = spectra[k] - quantized[k] as f64 * cstep;
                quantized_fine[k] = clamp_signed((residual / fstep).round() as i64, pf + 1);
            }
        }
    }

    BlockPlan {
        band_count: ctx.band_count,
        quant_unit_count: quc,
        grad_start_unit: gsu,
        grad_start_value: gsv,
        grad_boundary: gb,
        scale_factors: sf,
        precisions,
        precisions_fine,
        codebook_set,
        quantized,
        quantized_fine,
    }
}

fn plan_block(
    spectra: &[f64; 256],
    gsv: i32,
    gb: i32,
    boost: i32,
    band_override: Option<usize>,
    sri: usize,
    curves: &[[u8; 48]; 48],
) -> BlockPlan {
    let ctx = block_sf(spectra, boost, band_override, sri);
    plan_from_sf(&ctx, spectra, gsv, gb, curves)
}

/// Encode scale factors with VLC delta-offset (coding mode 0), matching the reference tool.
/// Reconstructs as: sf[i] = r[i] + baseValue - weights[i], where r[0] is sent
/// raw and r[i] = (r[i-1] + delta) & (valueMax-1) with Huffman-coded deltas.
fn pack_scale_factors_vlc(w: &mut BitWriter, sf: &[i32; 31], ext_unit: usize, huff: &HuffmanTables) {
    let weight_index = 0usize;
    let weights = &SCALE_FACTOR_WEIGHTS[weight_index];
    let adj = |i: usize| sf[i] + weights[i] as i32;

    let base = (0..ext_unit).map(adj).min().unwrap_or(0).clamp(0, 31);
    let maxr = (0..ext_unit).map(|i| adj(i) - base).max().unwrap_or(0);
    let mut bitlen = 3u32;
    while ((1i32 << bitlen) - 1) < maxr && bitlen < 6 {
        bitlen += 1;
    }
    let cb = huff.sf_unsigned[bitlen as usize].as_ref().unwrap();
    let value_max = cb.value_max();

    w.write(0, 2); // coding mode = VLC delta offset
    w.write(weight_index as u32, 3);
    w.write(base as u32, 5);
    w.write(bitlen - 3, 2);

    let r0 = adj(0) - base;
    w.write(r0 as u32, bitlen);
    let mut prev = r0;
    for i in 1..ext_unit {
        let ri = adj(i) - base;
        let delta = (ri - prev) & (value_max - 1);
        huffenc::encode_scale_value(w, cb, delta);
        prev = ri;
    }
}

/// Pack a planned mono block into `w`. `filler_bits` (0..=31), when Some, emits a
/// has-extension-data section the decoder skips — used to pad a block to an exact
/// byte count so superframes fill exactly (the reference decoder reads frames continuously).
fn pack_block(
    w: &mut BitWriter,
    plan: &BlockPlan,
    first_in_superframe: bool,
    huff: &HuffmanTables,
    filler_bits: Option<u32>,
) {
    let min_band_count = MIN_BAND_COUNT[0] as u32;

    w.write(if first_in_superframe { 0 } else { 1 }, 1); // !FirstInSuperframe
    w.write(0, 1); // ReuseBandParams

    // Band params (Mono).
    w.write(plan.band_count as u32 - min_band_count, 4);
    w.write(0, 1); // band_extension_enabled

    // Gradient: mode 2 (decoder fixes end unit/value at 31).
    w.write(2, 2); // mode
    w.write(plan.grad_start_unit as u32, 5); // start unit
    w.write(plan.grad_start_value as u32, 5); // start value
    w.write(plan.grad_boundary as u32, 4); // boundary

    // Extension params: optional skippable filler (band extension disabled).
    match filler_bits {
        Some(n) => {
            w.write(1, 1); // HasExtensionData
            w.write(0, 2); // bex mode (ignored; band extension disabled)
            w.write(n, 5); // bex data length (bits to skip)
            for _ in 0..n {
                w.write(0, 1);
            }
        }
        None => w.write(0, 1), // HasExtensionData = 0
    }

    // One channel's data (scale factors + spectra + fine).
    pack_channel(w, plan, huff, None);
    // Note: caller aligns to a byte boundary (so the pre-align bit count can be
    // read for exact-fill sizing).
}

/// Bits to code channel-1 scale factors as signed distances from channel 0's
/// (coding mode 1). Returns `(bits, bit_length)`, or None if no codebook can
/// represent all distances. Cheaper than independent coding when channels are
/// similar — frees bits for precision, as the reference encoder does.
fn sf_vlc_relative_bits(
    sf1: &[i32; 31],
    sf0: &[i32; 31],
    ext_unit: usize,
    huff: &HuffmanTables,
) -> Option<(usize, u32)> {
    for bitlen in 2..=5u32 {
        let Some(cb) = huff.sf_signed[bitlen as usize].as_ref() else {
            continue;
        };
        let lo = -(1i32 << (bitlen - 1));
        let hi = (1i32 << (bitlen - 1)) - 1;
        let mut bits = 2 + 2; // coding mode + bit-length field
        let mut ok = true;
        for i in 0..ext_unit {
            let d = sf1[i] - sf0[i];
            if d < lo || d > hi {
                ok = false;
                break;
            }
            let sl = huffenc::scale_value_bits(cb, d);
            if sl == 0 {
                ok = false; // distance absent from this codebook
                break;
            }
            bits += sl;
        }
        if ok {
            return Some((bits, bitlen));
        }
    }
    None
}

/// Pack channel-1 scale factors relative to channel 0 (coding mode 1).
fn pack_scale_factors_relative(
    w: &mut BitWriter,
    sf1: &[i32; 31],
    sf0: &[i32; 31],
    ext_unit: usize,
    huff: &HuffmanTables,
    bitlen: u32,
) {
    let cb = huff.sf_signed[bitlen as usize].as_ref().unwrap();
    w.write(1, 2); // coding mode 1 (distance to baseline)
    w.write(bitlen - 2, 2);
    for i in 0..ext_unit {
        huffenc::encode_scale_value(w, cb, sf1[i] - sf0[i]);
    }
}

/// Pack one channel's payload: scale factors (independent, or relative to a
/// baseline for channel 1), then spectra (Huffman/raw) and fine residuals.
fn pack_channel(
    w: &mut BitWriter,
    plan: &BlockPlan,
    huff: &HuffmanTables,
    relative: Option<(&[i32; 31], u32)>,
) {
    let max_huff = MAX_HUFF_PRECISION[0] as i32;
    let quc = plan.quant_unit_count;
    match relative {
        Some((baseline, bitlen)) => {
            pack_scale_factors_relative(w, &plan.scale_factors, baseline, quc, huff, bitlen)
        }
        None => pack_scale_factors_vlc(w, &plan.scale_factors, quc, huff),
    }
    for i in 0..quc {
        let lo = QUANT_UNIT_TO_COEFF_INDEX[i] as usize;
        let hi = QUANT_UNIT_TO_COEFF_INDEX[i + 1] as usize;
        let p = plan.precisions[i];
        if p + 1 <= max_huff {
            let cbidx = QUANT_UNIT_TO_CODEBOOK_INDEX[i] as usize;
            let cb = huff.spectrum[plan.codebook_set[i] as usize][(p + 1) as usize][cbidx]
                .as_ref()
                .unwrap();
            huffenc::encode_spectra_snapped(w, cb, &plan.quantized[lo..hi]);
        } else {
            for k in lo..hi {
                w.write_signed(plan.quantized[k], (p + 1) as u32);
            }
        }
    }
    for i in 0..quc {
        if plan.precisions_fine[i] > 0 {
            let lo = QUANT_UNIT_TO_COEFF_INDEX[i] as usize;
            let hi = QUANT_UNIT_TO_COEFF_INDEX[i + 1] as usize;
            let bits = (plan.precisions_fine[i] + 1) as u32;
            for k in lo..hi {
                w.write_signed(plan.quantized_fine[k], bits);
            }
        }
    }
}

/// Pack a stereo block: one shared header (band params, gradient, stereo params)
/// then both channels. `stereo_band == band_count` means no intensity stereo
/// (both channels fully coded), which is what the reference encoder emits at
/// these bitrates. `plan0`/`plan1` must share band_count and gradient params.
///
/// Retained for the config-2 stereo path; currently dual-mono gives better
/// quality until the shared-gradient allocation is tuned.
fn pack_stereo_block(
    w: &mut BitWriter,
    plan0: &BlockPlan,
    plan1: &BlockPlan,
    first_in_superframe: bool,
    huff: &HuffmanTables,
    filler_bits: Option<u32>,
) {
    let min_band_count = MIN_BAND_COUNT[0] as u32;
    w.write(if first_in_superframe { 0 } else { 1 }, 1); // !FirstInSuperframe
    w.write(0, 1); // ReuseBandParams

    // Band params (Stereo): band_count, stereo_band (= band_count → no intensity).
    w.write(plan0.band_count as u32 - min_band_count, 4);
    w.write(plan0.band_count as u32 - min_band_count, 4);
    w.write(0, 1); // band_extension_enabled

    // Shared gradient (mode 2).
    w.write(2, 2);
    w.write(plan0.grad_start_unit as u32, 5);
    w.write(plan0.grad_start_value as u32, 5);
    w.write(plan0.grad_boundary as u32, 4);

    // Stereo params: primary channel 0, no joint-stereo signs.
    w.write(0, 1); // primary_channel_index
    w.write(0, 1); // has_joint_stereo_signs

    // Extension params: optional skippable filler.
    match filler_bits {
        Some(n) => {
            w.write(1, 1);
            w.write(0, 2);
            w.write(n, 5);
            for _ in 0..n {
                w.write(0, 1);
            }
        }
        None => w.write(0, 1),
    }

    pack_channel(w, plan0, huff, None);
    // Channel 1: code scale factors relative to channel 0 when that is cheaper.
    let quc = plan1.quant_unit_count;
    let rel = sf_vlc_relative_bits(&plan1.scale_factors, &plan0.scale_factors, quc, huff);
    let mode0 = sf_vlc_bits(&plan1.scale_factors, quc, huff);
    match rel {
        Some((relbits, bitlen)) if relbits < mode0 => {
            pack_channel(w, plan1, huff, Some((&plan0.scale_factors, bitlen)))
        }
        _ => pack_channel(w, plan1, huff, None),
    }
}

fn pack_stereo_to_vec(
    plan0: &BlockPlan,
    plan1: &BlockPlan,
    first: bool,
    huff: &HuffmanTables,
    filler: Option<u32>,
) -> Vec<u8> {
    let mut w = BitWriter::new();
    pack_stereo_block(&mut w, plan0, plan1, first, huff, filler);
    w.align(8);
    w.into_bytes()
}

fn pack_block_to_vec(
    plan: &BlockPlan,
    first: bool,
    huff: &HuffmanTables,
    filler: Option<u32>,
) -> Vec<u8> {
    let mut w = BitWriter::new();
    pack_block(&mut w, plan, first, huff, filler);
    w.align(8);
    w.into_bytes()
}

/// Exact bit length of the VLC delta-offset scale factors (mirrors
/// `pack_scale_factors_vlc`), used by the bit model.
fn sf_vlc_bits(sf: &[i32; 31], ext_unit: usize, huff: &HuffmanTables) -> usize {
    let weights = &SCALE_FACTOR_WEIGHTS[0];
    let adj = |i: usize| sf[i] + weights[i] as i32;
    let base = (0..ext_unit).map(adj).min().unwrap_or(0).clamp(0, 31);
    let maxr = (0..ext_unit).map(|i| adj(i) - base).max().unwrap_or(0);
    let mut bitlen = 3u32;
    while ((1i32 << bitlen) - 1) < maxr && bitlen < 6 {
        bitlen += 1;
    }
    let cb = huff.sf_unsigned[bitlen as usize].as_ref().unwrap();
    let value_max = cb.value_max();
    let mut bits = 2 + 3 + 5 + 2 + bitlen as usize; // mode + weight + base + bitlen field + r0
    let mut prev = adj(0) - base;
    for i in 1..ext_unit {
        let ri = adj(i) - base;
        let delta = (ri - prev) & (value_max - 1);
        bits += huffenc::scale_value_bits(cb, delta);
        prev = ri;
    }
    bits
}

/// Exact pre-align bit length of a block for a given gradient, computed in a
/// single fused quantize+count pass with no BlockPlan allocation (the ATRAC9
/// analogue of HCA's CalculateUsedBits — exact per-value cost, not an estimate).
/// Equals `block_bits(plan_from_sf(ctx, .., gsv, gb, ..), None)`.
fn used_bits(
    ctx: &SfCtx,
    spectra: &[f64; 256],
    gsv: i32,
    gb: i32,
    curves: &[[u8; 48]; 48],
    huff: &HuffmanTables,
    sf_bits: usize,
) -> usize {
    let quc = ctx.quc;
    let gsu = ((quc as i32) - 1).clamp(1, 30);
    let gb = gb.clamp(0, (quc as i32).min(15));
    let gradient = create_gradient(gsu, gsv, quc, curves);
    let (prec, pfine) = calc_precisions(&ctx.sf, &ctx.mask, &gradient, gb, quc);
    // block header(2) + band params(5, mono) + gradient mode 2(16) + has_ext=0(1)
    // + the (precomputed, gradient-independent) VLC scale-factor bits.
    let mut bits = 2 + 5 + 16 + 1 + sf_bits;
    let max_huff = MAX_HUFF_PRECISION[0] as i32;
    let mut coeffs = [0i32; 16];
    for i in 0..quc {
        let lo = QUANT_UNIT_TO_COEFF_INDEX[i] as usize;
        let hi = QUANT_UNIT_TO_COEFF_INDEX[i + 1] as usize;
        let p = prec[i];
        let inv = 1.0 / (QUANTIZER_STEP_SIZE[p as usize] * SPECTRUM_SCALE[ctx.sf[i] as usize]);
        let cnt = hi - lo;
        for k in 0..cnt {
            coeffs[k] = clamp_signed((spectra[lo + k] * inv).round() as i64, p + 1);
        }
        if p + 1 <= max_huff {
            let cb = huff.spectrum[ctx.codebook_set[i] as usize][(p + 1) as usize]
                [QUANT_UNIT_TO_CODEBOOK_INDEX[i] as usize]
                .as_ref()
                .unwrap();
            bits += huffenc::spectra_bits(cb, &coeffs[..cnt]);
        } else {
            bits += cnt * (p as usize + 1);
        }
        if pfine[i] > 0 {
            bits += cnt * (pfine[i] as usize + 1);
        }
    }
    bits
}

/// Pack one block to EXACTLY `target_bytes`, best quality first. Searches the
/// gradient start-value and boundary for the largest size <= target, then pads
/// with skippable filler (0..31 bits) to hit the exact byte count. Falls back to
/// coding the full band (and finally a scale-factor boost) so that even quiet /
/// silent frames can reach the target. None if truly unreachable.
fn pack_block_exact(
    spectra: &[f64; 256],
    first: bool,
    sri: usize,
    curves: &[[u8; 48]; 48],
    huff: &HuffmanTables,
    target_bytes: usize,
) -> Option<Vec<u8>> {
    let natural = choose_band_count(spectra, sri);
    let max_band = (MAX_BAND_COUNT[sri] as usize).min(16);
    // Try the natural band first (best quality); widen the band, then boost scale
    // factors, only as needed to make quiet frames large enough to fill.
    let mut attempts: Vec<(usize, i32)> = vec![(natural, 0)];
    if max_band != natural {
        attempts.push((max_band, 0));
    }
    for b in 1..=24 {
        attempts.push((max_band, b)); // scale-factor boost for near-silent frames
    }

    // Filler maths: a block packed with no extension data is `bits` bits long
    // (pre-align); enabling the skippable extension replaces that 1 bit with
    // 8 + N bits, so total = bits + 7 + N. Choosing N = target*8 - bits - 7 makes
    // the block exactly `target_bytes`, valid when N is in 0..=31.
    let target_bits = target_bytes as i64 * 8;
    for (band, boost) in attempts {
        // Scale factors / mask / codebook set depend only on band+boost, so
        // compute them once and reuse across the gradient search.
        let ctx = block_sf(spectra, boost, Some(band), sri);
        // VLC scale-factor bits are gradient-independent: compute once per attempt.
        let sf_bits = sf_vlc_bits(&ctx.sf, ctx.quc, huff);
        // Block size decreases as gsv rises; binary-search the smallest gsv whose
        // size leaves room for filler (bits <= target*8 - 7) using the fast bit
        // model, then scan a few gradient/boundary combos around it for an exact
        // fit and pack only the winner.
        let (mut lo, mut hi, mut start) = (0i32, 31i32, 32i32);
        while lo <= hi {
            let mid = (lo + hi) / 2;
            if used_bits(&ctx, spectra, mid, 15, curves, huff, sf_bits) as i64 <= target_bits - 7 {
                start = mid;
                hi = mid - 1;
            } else {
                lo = mid + 1;
            }
        }
        // For each gradient near the fit, pick the highest boundary that still
        // fits (most low-frequency precision, least filler), then take the overall
        // least-filler combo and pack only it.
        let order = [start, start - 1, start + 1, start + 2, start - 2];
        let mut best: Option<(i32, i32, i64)> = None; // gsv, gb, n
        for gsv in order.into_iter().filter(|&g| (0..=31).contains(&g)) {
            let (mut lo2, mut hi2, mut bg) = (0i32, 15i32, -1i32);
            while lo2 <= hi2 {
                let mid = (lo2 + hi2) / 2;
                if used_bits(&ctx, spectra, gsv, mid, curves, huff, sf_bits) as i64 <= target_bits - 7
                {
                    bg = mid;
                    lo2 = mid + 1;
                } else {
                    hi2 = mid - 1;
                }
            }
            if bg >= 0 {
                let bits = used_bits(&ctx, spectra, gsv, bg, curves, huff, sf_bits) as i64;
                let n = target_bits - bits - 7;
                if (0..=31).contains(&n) && best.map_or(true, |(_, _, bn)| n < bn) {
                    best = Some((gsv, bg, n));
                }
            }
        }
        if let Some((gsv, gb, n)) = best {
            let plan = plan_from_sf(&ctx, spectra, gsv, gb, curves);
            let v = pack_block_to_vec(&plan, first, huff, Some(n as u32));
            if v.len() == target_bytes {
                return Some(v);
            }
        }
    }
    None
}

fn encode_frame(
    frame_spectra: &[[f64; 256]],
    first: bool,
    sri: usize,
    frame_bytes: usize,
    huff: &HuffmanTables,
    curves: &[[u8; 48]; 48],
) -> Vec<u8> {
    // Each frame must be EXACTLY frame_bytes so superframes fill without gaps
    // (the reference decoder reads frames continuously and rejects trailing padding).
    if frame_spectra.len() == 1 {
        if let Some(v) = pack_block_exact(&frame_spectra[0], first, sri, curves, huff, frame_bytes) {
            return v;
        }
        // Fallback (rare): smallest frame we can make, padded to frame_bytes.
        let plan = plan_block(&frame_spectra[0], 31, 0, 0, None, sri, curves);
        let mut v = pack_block_to_vec(&plan, first, huff, None);
        v.resize(frame_bytes, 0);
        return v;
    }

    // Stereo input → one config-2 stereo block (the standard stereo AT9 layout,
    // matching the reference encoder), with channel-1 scale factors coded
    // relative to channel 0 to free bits for precision.
    encode_stereo_frame(&frame_spectra[0], &frame_spectra[1], first, sri, frame_bytes, huff, curves)
}

/// Exact-fill a stereo block (both channels, shared band + gradient) to
/// `frame_bytes`. Both channels use the same gradient; the budget is shared.
fn encode_stereo_frame(
    spec0: &[f64; 256],
    spec1: &[f64; 256],
    first: bool,
    sri: usize,
    frame_bytes: usize,
    huff: &HuffmanTables,
    curves: &[[u8; 48]; 48],
) -> Vec<u8> {
    let target_bits = frame_bytes as i64 * 8;
    let natural = choose_band_count(spec0, sri).max(choose_band_count(spec1, sri));
    let max_band = (MAX_BAND_COUNT[sri] as usize).min(16);
    let mut attempts: Vec<(usize, i32)> = vec![(natural, 0)];
    if max_band != natural {
        attempts.push((max_band, 0));
    }
    for b in 1..=24 {
        attempts.push((max_band, b));
    }
    for (band, boost) in attempts {
        let ctx0 = block_sf(spec0, boost, Some(band), sri);
        let ctx1 = block_sf(spec1, boost, Some(band), sri);
        let sf0 = sf_vlc_bits(&ctx0.sf, ctx0.quc, huff);
        // Channel 1: size with the cheaper of independent / relative-to-ch0 coding
        // (relative frees bits for precision, which `pack_stereo_block` then uses).
        let sf1_indep = sf_vlc_bits(&ctx1.sf, ctx1.quc, huff);
        let sf1 = sf_vlc_relative_bits(&ctx1.sf, &ctx0.sf, ctx1.quc, huff)
            .map(|(b, _)| b.min(sf1_indep))
            .unwrap_or(sf1_indep);
        // Stereo block bits = stereo header(30) + both channels' content.
        // used_bits is mono-header(24)+content, so subtract 2*24 and add 30 = -18.
        let bits_at = |gsv: i32, gb: i32| -> i64 {
            (used_bits(&ctx0, spec0, gsv, gb, curves, huff, sf0)
                + used_bits(&ctx1, spec1, gsv, gb, curves, huff, sf1)) as i64
                - 18
        };
        let (mut lo, mut hi, mut start) = (0i32, 31i32, 32i32);
        while lo <= hi {
            let mid = (lo + hi) / 2;
            if bits_at(mid, 15) <= target_bits - 7 {
                start = mid;
                hi = mid - 1;
            } else {
                lo = mid + 1;
            }
        }
        // For each gradient near the fit, pick the highest boundary that still
        // fits (most low-frequency precision and least filler), then choose the
        // overall combo with the least filler (most content). Pack only once.
        let order = [start, start - 1, start + 1, start + 2, start - 2];
        let mut best: Option<(i32, i32, i64)> = None; // gsv, gb, n
        for gsv in order.into_iter().filter(|&g| (0..=31).contains(&g)) {
            let (mut lo2, mut hi2, mut bg) = (0i32, 15i32, -1i32);
            while lo2 <= hi2 {
                let mid = (lo2 + hi2) / 2;
                if bits_at(gsv, mid) <= target_bits - 7 {
                    bg = mid;
                    lo2 = mid + 1;
                } else {
                    hi2 = mid - 1;
                }
            }
            if bg >= 0 {
                let n = target_bits - bits_at(gsv, bg) - 7;
                if (0..=31).contains(&n) && best.map_or(true, |(_, _, bn)| n < bn) {
                    best = Some((gsv, bg, n));
                }
            }
        }
        if let Some((gsv, gb, n)) = best {
            let plan0 = plan_from_sf(&ctx0, spec0, gsv, gb, curves);
            let plan1 = plan_from_sf(&ctx1, spec1, gsv, gb, curves);
            let v = pack_stereo_to_vec(&plan0, &plan1, first, huff, Some(n as u32));
            if v.len() == frame_bytes {
                return v;
            }
        }
    }
    // Fallback (rare): max band, smallest frame, padded to frame_bytes.
    let ctx0 = block_sf(spec0, 0, Some(max_band), sri);
    let ctx1 = block_sf(spec1, 0, Some(max_band), sri);
    let plan0 = plan_from_sf(&ctx0, spec0, 31, 0, curves);
    let plan1 = plan_from_sf(&ctx1, spec1, 31, 0, curves);
    let mut v = pack_stereo_to_vec(&plan0, &plan1, first, huff, None);
    v.resize(frame_bytes, 0);
    v
}

/// PS Vita theme-BGM size limit (under 5 MB): the console won't use a theme AT9
/// larger than this. Pass as `EncodeOptions::max_bytes` for Vita theme BGM.
pub const VITA_BGM_MAX_BYTES: usize = 5 * 1024 * 1024;

/// Tighter 2 MiB cap for a theme BGM that must also work on PS4/PS5 (cross-console
/// support). Pass as `max_bytes` instead of [`VITA_BGM_MAX_BYTES`] for that case.
pub const PS_CROSS_BGM_MAX_BYTES: usize = 2 * 1024 * 1024;

/// Encode PCM to a complete AT9 file. All behaviour (bitrate, size cap, loop,
/// progress) is driven by [`EncodeOptions`]; `&EncodeOptions::default()` gives a
/// best-quality, no-loop, uncapped encode.
pub fn encode(wav: &WavPcm, opts: &super::EncodeOptions) -> Result<Vec<u8>> {
    let bitrate = opts.bitrate;
    let max_bytes = opts.max_bytes;
    let progress = opts.progress;
    if wav.sample_rate == 0 || wav.channels == 0 {
        return Err(Error::BadConfig);
    }
    // Resample to a supported Vita rate (12/24/48 kHz) if the source isn't one,
    // scaling any loop points to the new rate so they stay at the same position.
    let src_rate = wav.sample_rate;
    let owned;
    let (wav, loop_start, loop_end) = if super::resample::is_supported(src_rate) {
        (wav, opts.loop_start, opts.loop_end)
    } else {
        let tr = super::resample::target_rate(src_rate);
        owned = super::resample::resample(wav, tr);
        (
            &owned,
            opts.loop_start.map(|s| super::resample::scale_index(s, src_rate, tr)),
            opts.loop_end.map(|e| super::resample::scale_index(e, src_rate, tr)),
        )
    };
    let channels = wav.channels as usize;
    let total_samples = (wav.samples.len() / channels) as u32;
    let sri = SAMPLE_RATES
        .iter()
        .take(8)
        .position(|&r| r == wav.sample_rate)
        .ok_or(Error::NotImplemented("unsupported sample rate"))?;
    let cci = match channels {
        1 => 0usize,
        2 => 2usize, // stereo block (config 2) — the standard for stereo AT9
        _ => return Err(Error::NotImplemented("only mono/stereo")),
    };

    // De-interleave.
    let samples_per_ch = wav.samples.len() / channels;
    let mut ch_pcm: Vec<Vec<f64>> = vec![Vec::with_capacity(samples_per_ch); channels];
    for (i, &s) in wav.samples.iter().enumerate() {
        ch_pcm[i % channels].push(s as f64);
    }
    let real_frames = samples_per_ch.div_ceil(FRAME_SAMPLES);
    let num_frames = (real_frames + 1).div_ceil(FRAMES_PER_SUPERFRAME) * FRAMES_PER_SUPERFRAME;

    // Choose the frame size (bytes/frame). The hardware only accepts the standard
    // AT9 frame sizes the reference the reference tool emits — determined empirically against
    // the reference tool (any other size is rejected "Not Supported Param"), at 48 kHz:
    //   mono   24,32,40,48,56,64 bytes    = 36,48,60,72,84,96 kbps
    //   stereo 48,64,80,96,112,128 bytes  = 72,96,120,144,168,192 kbps
    // (bitrate = frame_bytes * 1500 at 48 kHz). First pick the target size from
    // the bitrate (0 = highest standard = best quality; explicit = snap DOWN to a
    // standard size), then, if a `max_bytes` cap is set (e.g. VITA_BGM_MAX_BYTES
    // for theme BGM), reduce to the highest standard size that fits the cap. Each
    // frame is filled to exactly frame_bytes, so the size is exact:
    // num_frames * frame_bytes + a small container header.
    const CONTAINER_OVERHEAD: usize = 256;
    let valid_fb: &[usize] = if channels == 1 {
        &[24, 32, 40, 48, 56, 64]
    } else {
        &[48, 64, 80, 96, 112, 128]
    };
    let file_size = |fb: usize| num_frames * fb + CONTAINER_OVERHEAD;
    let want_fb = if bitrate == 0 {
        *valid_fb.last().unwrap()
    } else {
        let frames_per_sec = wav.sample_rate as f64 / FRAME_SAMPLES as f64;
        let want = ((bitrate as f64 / 8.0) / frames_per_sec).round() as usize;
        valid_fb.iter().rev().copied().find(|&fb| fb <= want).unwrap_or(valid_fb[0])
    };
    let frame_bytes = if max_bytes > 0 {
        valid_fb
            .iter()
            .rev()
            .copied()
            .find(|&fb| fb <= want_fb && file_size(fb) <= max_bytes)
            .unwrap_or(valid_fb[0])
    } else {
        want_fb
    };

    // Forward MDCT per channel → spectra per frame.
    let mdct = MdctTables::new(8);
    let scale = 2.0 / FRAME_SAMPLES as f64;
    let mut spectra: Vec<Vec<[f64; 256]>> = vec![Vec::with_capacity(num_frames); channels];
    for c in 0..channels {
        let mut prev = [0.0f64; 256];
        for f in 0..num_frames {
            let mut input = [0.0f64; 256];
            for i in 0..FRAME_SAMPLES {
                input[i] = ch_pcm[c].get(f * FRAME_SAMPLES + i).copied().unwrap_or(0.0);
            }
            let mut out = [0.0f64; 256];
            mdct.run_mdct(&mut prev, &input, &mut out, scale);
            spectra[c].push(out);
        }
    }

    let huff = HuffmanTables::new();
    let curves = gen_gradient_curves();

    // Encode superframes. They are independent (spectra are precomputed), so
    // encode them across threads and concatenate in order. Uses only std threads
    // to keep the library dependency-free.
    let superframe_bytes = frame_bytes * FRAMES_PER_SUPERFRAME;
    let total_sf = num_frames / FRAMES_PER_SUPERFRAME;
    let mut results: Vec<Vec<u8>> = vec![Vec::new(); total_sf];
    let nthreads = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .clamp(1, total_sf.max(1));

    // Progress is tracked with a single shared atomic the workers bump per
    // superframe (one relaxed add — effectively free). When a callback is given,
    // a dedicated monitor thread polls it and invokes the callback, so the encode
    // workers never call into foreign code. With no callback, no monitor is spawned.
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    let done = AtomicUsize::new(0);
    let stop = AtomicBool::new(false);
    let total_sf_u = total_sf as u32;

    let encode_sf = |sf_idx: usize, slot: &mut Vec<u8>, done: &AtomicUsize| {
        let mut buf = Vec::with_capacity(superframe_bytes);
        for fi in 0..FRAMES_PER_SUPERFRAME {
            let f = sf_idx * FRAMES_PER_SUPERFRAME + fi;
            let frame_spectra: Vec<[f64; 256]> = (0..channels).map(|c| spectra[c][f]).collect();
            buf.extend_from_slice(&encode_frame(
                &frame_spectra, fi == 0, sri, frame_bytes, &huff, &curves,
            ));
        }
        buf.resize(superframe_bytes, 0);
        *slot = buf;
        done.fetch_add(1, Ordering::Relaxed);
    };

    std::thread::scope(|s| {
        if let Some(cb) = progress {
            let done = &done;
            let stop = &stop;
            s.spawn(move || {
                cb(0, total_sf_u);
                while !stop.load(Ordering::Acquire) {
                    let d = (done.load(Ordering::Relaxed) as u32).min(total_sf_u);
                    cb(d, total_sf_u);
                    std::thread::sleep(std::time::Duration::from_millis(30));
                }
                cb(total_sf_u, total_sf_u);
            });
        }

        if nthreads <= 1 {
            for (sf_idx, slot) in results.iter_mut().enumerate() {
                encode_sf(sf_idx, slot, &done);
            }
        } else {
            let chunk = total_sf.div_ceil(nthreads);
            let spectra = &spectra;
            let huff = &huff;
            let curves = &curves;
            let done = &done;
            for (ti, slots) in results.chunks_mut(chunk).enumerate() {
                let base = ti * chunk;
                s.spawn(move || {
                    for (j, slot) in slots.iter_mut().enumerate() {
                        let sf_idx = base + j;
                        let mut buf = Vec::with_capacity(superframe_bytes);
                        for fi in 0..FRAMES_PER_SUPERFRAME {
                            let f = sf_idx * FRAMES_PER_SUPERFRAME + fi;
                            let frame_spectra: Vec<[f64; 256]> =
                                (0..channels).map(|c| spectra[c][f]).collect();
                            buf.extend_from_slice(&encode_frame(
                                &frame_spectra, fi == 0, sri, frame_bytes, huff, curves,
                            ));
                        }
                        buf.resize(superframe_bytes, 0);
                        *slot = buf;
                        done.fetch_add(1, Ordering::Relaxed);
                    }
                });
            }
        }
        stop.store(true, Ordering::Release);
    });
    let mut data = Vec::with_capacity(total_sf * superframe_bytes);
    for sf in results {
        data.extend_from_slice(&sf);
    }

    let config = EncoderConfig {
        sample_rate_index: sri,
        channel_config_index: cci,
        superframe_index: 2,
        frame_bytes: frame_bytes as u32,
        sample_rate: wav.sample_rate,
        channels: wav.channels,
        frame_samples: FRAME_SAMPLES,
        frames_per_superframe: FRAMES_PER_SUPERFRAME,
        superframe_bytes: superframe_bytes as u32,
    };
    Ok(build_at9_container(&config, &data, total_samples, ENCODER_DELAY, loop_start, loop_end))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_decode_round_trip_is_reasonable() {
        let sr = 48000u32;
        let n = 8192usize;
        let mut samples = Vec::with_capacity(n * 2);
        for i in 0..n {
            let t = i as f64 / sr as f64;
            let l = ((t * 440.0 * std::f64::consts::TAU).sin() * 8000.0) as i16;
            let r = ((t * 660.0 * std::f64::consts::TAU).sin() * 6000.0) as i16;
            samples.push(l);
            samples.push(r);
        }
        let wav = WavPcm { channels: 2, sample_rate: sr, samples: samples.clone() };
        let at9 = encode(&wav, &crate::encode::EncodeOptions::default()).unwrap();

        // Must decode with our decoder and stay at a standard bitrate.
        let c = crate::container::parse(&at9).unwrap();
        assert!(c.block_align <= 512, "block_align {} too big", c.block_align);
        let pcm = crate::decode_to_pcm16(&at9).unwrap();
        assert_eq!(pcm.samples.len(), samples.len());

        let skip = 1024 * 2;
        let mut err = 0.0f64;
        let mut energy = 0.0f64;
        for i in skip..samples.len() {
            let d = samples[i] as f64 - pcm.samples[i] as f64;
            err += d * d;
            energy += (samples[i] as f64).powi(2);
        }
        let snr = 10.0 * (energy / err.max(1e-9)).log10();
        assert!(snr > 20.0, "round-trip SNR too low: {snr:.1} dB");
    }
}
