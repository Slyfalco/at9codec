//! ATRAC9 frame decoder: the bitstream path.
//!
//! Clean-room port of LibAtrac9's `unpack.c`, `scale_factors.c`, `quantization.c`,
//! `bit_allocation.c` and the per-frame orchestration in `decoder.c`. Persistent
//! per-channel/per-block state (IMDCT overlap, previous scale factors, reused band
//! params, RNG) is kept in [`Decoder`], reused across frames as in the reference.

use super::bex::{self, Rng};
use super::bitreader::BitReader;
use super::huffman::{decode_huffman_values, HuffmanTables};
use super::mdct::MdctTables;
use super::tables::*;
use super::DecoderConfig;
use crate::error::{Error, Result};

const MAX_FRAME_SAMPLES: usize = 256;
const MAX_QUANT_UNITS: usize = 30;

fn round16(x: f64) -> i16 {
    let v = (x + 0.5).floor() as i32; // LibAtrac9 Round == floor(x + 0.5)
    v.clamp(-32768, 32767) as i16
}

#[derive(Clone)]
struct Channel {
    channel_index: usize,
    coded_quant_units: usize,
    scale_factor_coding_mode: i32,
    scale_factors: [i32; 31],
    scale_factors_prev: [i32; 31],
    precisions: [i32; MAX_QUANT_UNITS],
    precisions_fine: [i32; MAX_QUANT_UNITS],
    precision_mask: [i32; MAX_QUANT_UNITS],
    codebook_set: [i32; MAX_QUANT_UNITS],
    quantized_spectra: [i32; MAX_FRAME_SAMPLES],
    quantized_spectra_fine: [i32; MAX_FRAME_SAMPLES],
    spectra: [f64; MAX_FRAME_SAMPLES],
    pcm: [f64; MAX_FRAME_SAMPLES],
    imdct_prev: [f64; MAX_FRAME_SAMPLES],
    bex_mode: i32,
    bex_value_count: i32,
    bex_values: [i32; 4],
    rng: Rng,
}

impl Channel {
    fn new(channel_index: usize) -> Self {
        Channel {
            channel_index,
            coded_quant_units: 0,
            scale_factor_coding_mode: 0,
            scale_factors: [0; 31],
            scale_factors_prev: [0; 31],
            precisions: [0; MAX_QUANT_UNITS],
            precisions_fine: [0; MAX_QUANT_UNITS],
            precision_mask: [0; MAX_QUANT_UNITS],
            codebook_set: [0; MAX_QUANT_UNITS],
            quantized_spectra: [0; MAX_FRAME_SAMPLES],
            quantized_spectra_fine: [0; MAX_FRAME_SAMPLES],
            spectra: [0.0; MAX_FRAME_SAMPLES],
            pcm: [0.0; MAX_FRAME_SAMPLES],
            imdct_prev: [0.0; MAX_FRAME_SAMPLES],
            bex_mode: 0,
            bex_value_count: 0,
            bex_values: [0; 4],
            rng: Rng::default(),
        }
    }
}

#[derive(Clone)]
struct Block {
    block_type: BlockType,
    channel_count: usize,
    channels: Vec<Channel>,
    first_in_superframe: bool,
    reuse_band_params: bool,
    band_count: i32,
    stereo_band: i32,
    extension_band: i32,
    quantization_unit_count: i32,
    stereo_quantization_unit: i32,
    extension_unit: i32,
    quantization_units_prev: i32,
    gradient: [i32; 31],
    gradient_mode: i32,
    gradient_start_unit: i32,
    gradient_start_value: i32,
    gradient_end_unit: i32,
    gradient_end_value: i32,
    gradient_boundary: i32,
    primary_channel_index: usize,
    has_joint_stereo_signs: bool,
    joint_stereo_signs: [i32; MAX_QUANT_UNITS],
    band_extension_enabled: bool,
    has_extension_data: bool,
    bex_data_length: i32,
    bex_mode: i32,
}

impl Block {
    fn new(block_type: BlockType) -> Self {
        let channel_count = block_type.channel_count();
        Block {
            block_type,
            channel_count,
            channels: (0..channel_count).map(Channel::new).collect(),
            first_in_superframe: true,
            reuse_band_params: false,
            band_count: 0,
            stereo_band: 0,
            extension_band: 0,
            quantization_unit_count: 0,
            stereo_quantization_unit: 0,
            extension_unit: 0,
            quantization_units_prev: 0,
            gradient: [0; 31],
            gradient_mode: 0,
            gradient_start_unit: 0,
            gradient_start_value: 0,
            gradient_end_unit: 0,
            gradient_end_value: 0,
            gradient_boundary: 0,
            primary_channel_index: 0,
            has_joint_stereo_signs: false,
            joint_stereo_signs: [0; MAX_QUANT_UNITS],
            band_extension_enabled: false,
            has_extension_data: false,
            bex_data_length: 0,
            bex_mode: 0,
        }
    }
}

/// A reusable ATRAC9 decoder bound to one stream configuration.
pub struct Decoder {
    config: DecoderConfig,
    huffman: HuffmanTables,
    mdct: MdctTables,
    gradient_curves: [[u8; 48]; 48],
    blocks: Vec<Block>,
    index_in_superframe: usize,
    /// (block_index, channel_in_block) in output channel order.
    channel_order: Vec<(usize, usize)>,
}

const BASE_CURVE: [u8; 48] = [
    1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 4, 4, 5, 5, 6, 7, 8, 9, 10, 11, 12, 13,
    15, 16, 18, 19, 20, 21, 22, 23, 24, 25, 26, 26, 27, 27, 28, 28, 28, 29, 29,
    29, 29, 30, 30, 30, 30,
];

fn generate_gradient_curves() -> [[u8; 48]; 48] {
    let base_len = BASE_CURVE.len();
    let mut curves = [[0u8; 48]; 48];
    for length in 1..=base_len {
        for i in 0..length {
            curves[length - 1][i] = BASE_CURVE[i * base_len / length];
        }
    }
    curves
}

impl Decoder {
    pub fn new(config: DecoderConfig) -> Self {
        let mdct = config.mdct_tables();
        let cc = config.channel_config;
        let mut blocks = Vec::new();
        let mut channel_order = Vec::new();
        for b in 0..cc.block_count as usize {
            let block = Block::new(cc.types[b]);
            for c in 0..block.channel_count {
                channel_order.push((b, c));
            }
            blocks.push(block);
        }
        Decoder {
            config,
            huffman: HuffmanTables::new(),
            mdct,
            gradient_curves: generate_gradient_curves(),
            blocks,
            index_in_superframe: 0,
            channel_order,
        }
    }

    pub fn channel_count(&self) -> usize {
        self.config.channel_count
    }
    pub fn sample_rate(&self) -> u32 {
        self.config.sample_rate
    }
    pub fn frame_samples(&self) -> usize {
        self.config.frame_samples
    }

    /// Decode one frame from `br`, filling each channel's PCM. Appends the frame's
    /// interleaved 16-bit samples to `out`.
    pub fn decode_frame(&mut self, br: &mut BitReader, out: &mut Vec<i16>) -> Result<()> {
        let block_count = self.config.channel_config.block_count as usize;
        for i in 0..block_count {
            self.unpack_block(i, br)?;
            if self.blocks[i].first_in_superframe && self.index_in_superframe != 0 {
                return Err(Error::NotImplemented("superframe flag invalid"));
            }
        }
        self.index_in_superframe += 1;
        if self.index_in_superframe == self.config.frames_per_superframe {
            self.index_in_superframe = 0;
        }

        // Spectrum -> PCM pipeline per block.
        for i in 0..block_count {
            self.dequantize_spectra(i);
            self.apply_intensity_stereo(i);
            self.scale_spectrum_block(i);
            self.apply_band_extension(i);
            self.imdct_block(i);
        }

        self.write_frame_pcm(out);
        Ok(())
    }

    fn write_frame_pcm(&self, out: &mut Vec<i16>) {
        let sample_count = self.config.frame_samples;
        for smpl in 0..sample_count {
            for &(b, c) in &self.channel_order {
                out.push(round16(self.blocks[b].channels[c].pcm[smpl]));
            }
        }
    }

    // ----- unpack -----

    fn unpack_block(&mut self, bi: usize, br: &mut BitReader) -> Result<()> {
        // header
        self.blocks[bi].first_in_superframe = br.read(1) == 0;
        self.blocks[bi].reuse_band_params = br.read(1) == 1;
        if self.blocks[bi].first_in_superframe
            && self.blocks[bi].reuse_band_params
            && self.blocks[bi].block_type != BlockType::Lfe
        {
            return Err(Error::NotImplemented("reuse band params invalid"));
        }

        if self.blocks[bi].block_type == BlockType::Lfe {
            self.unpack_lfe_block(bi, br)?;
        } else {
            self.unpack_standard_block(bi, br)?;
        }
        br.align(8);
        Ok(())
    }

    fn unpack_standard_block(&mut self, bi: usize, br: &mut BitReader) -> Result<()> {
        if !self.blocks[bi].reuse_band_params {
            self.read_band_params(bi, br)?;
        }
        self.read_gradient_params(bi, br)?;
        self.create_gradient(bi);
        self.read_stereo_params(bi, br)?;
        self.read_extension_params(bi, br)?;

        // Snapshot block-level values used inside the per-channel loop.
        let primary = self.blocks[bi].primary_channel_index;
        let quant_unit_count = self.blocks[bi].quantization_unit_count;
        let stereo_quant = self.blocks[bi].stereo_quantization_unit;
        let extension_unit = self.blocks[bi].extension_unit;
        let first = self.blocks[bi].first_in_superframe;
        let qu_prev = self.blocks[bi].quantization_units_prev;
        let gradient = self.blocks[bi].gradient;
        let gradient_mode = self.blocks[bi].gradient_mode;
        let gradient_boundary = self.blocks[bi].gradient_boundary;
        let channel_count = self.blocks[bi].channel_count;
        let high = self.config.high_sample_rate;

        for i in 0..channel_count {
            let coded = if primary == i { quant_unit_count } else { stereo_quant };
            self.blocks[bi].channels[i].coded_quant_units = coded as usize;

            // Channel 1's scale-factor baseline is channel 0's freshly-decoded
            // scale factors (channel 0 is processed first), not a stale snapshot.
            let ch0_scale_factors = if i == 1 {
                self.blocks[bi].channels[0].scale_factors
            } else {
                [0i32; 31]
            };
            {
                let ch = &mut self.blocks[bi].channels[i];
                read_scale_factors(
                    ch,
                    i,
                    first,
                    qu_prev,
                    extension_unit,
                    &ch0_scale_factors,
                    &self.huffman,
                    br,
                )?;
            }
            calculate_mask(&mut self.blocks[bi].channels[i], quant_unit_count);
            calculate_precisions(
                &mut self.blocks[bi].channels[i],
                &gradient,
                gradient_mode,
                gradient_boundary,
                quant_unit_count,
            );
            calculate_spectrum_codebook_index(&mut self.blocks[bi].channels[i], high);

            read_spectra(&mut self.blocks[bi].channels[i], br, &self.huffman, high);
            read_spectra_fine(&mut self.blocks[bi].channels[i], br);
        }

        self.blocks[bi].quantization_units_prev = if self.blocks[bi].band_extension_enabled {
            self.blocks[bi].extension_unit
        } else {
            self.blocks[bi].quantization_unit_count
        };
        Ok(())
    }

    fn read_band_params(&mut self, bi: usize, br: &mut BitReader) -> Result<()> {
        let high = self.config.high_sample_rate as usize;
        let min_band_count = MIN_BAND_COUNT[high] as i32;
        let max_extension_band = MAX_EXTENSION_BAND[high] as i32;
        let block = &mut self.blocks[bi];
        block.band_count = br.read(4) as i32 + min_band_count;
        block.quantization_unit_count = BAND_TO_QUANT_UNIT_COUNT[block.band_count as usize] as i32;

        if block.band_count > MAX_BAND_COUNT[self.config.sample_rate_index] as i32 {
            return Err(Error::NotImplemented("band params invalid"));
        }
        if block.block_type == BlockType::Stereo {
            block.stereo_band = br.read(4) as i32 + min_band_count;
            block.stereo_quantization_unit =
                BAND_TO_QUANT_UNIT_COUNT[block.stereo_band as usize] as i32;
        } else {
            block.stereo_band = block.band_count;
        }
        if block.stereo_band > block.band_count {
            return Err(Error::NotImplemented("band params invalid"));
        }

        block.band_extension_enabled = br.read(1) == 1;
        if block.band_extension_enabled {
            block.extension_band = br.read(4) as i32 + min_band_count;
            if block.extension_band < block.band_count || block.extension_band > max_extension_band {
                return Err(Error::NotImplemented("band params invalid"));
            }
            block.extension_unit = BAND_TO_QUANT_UNIT_COUNT[block.extension_band as usize] as i32;
        } else {
            block.extension_band = block.band_count;
            block.extension_unit = block.quantization_unit_count;
        }
        Ok(())
    }

    fn read_gradient_params(&mut self, bi: usize, br: &mut BitReader) -> Result<()> {
        let block = &mut self.blocks[bi];
        block.gradient_mode = br.read(2) as i32;
        if block.gradient_mode > 0 {
            block.gradient_end_unit = 31;
            block.gradient_end_value = 31;
            block.gradient_start_unit = br.read(5) as i32;
            block.gradient_start_value = br.read(5) as i32;
        } else {
            block.gradient_start_unit = br.read(6) as i32;
            block.gradient_end_unit = br.read(6) as i32 + 1;
            block.gradient_start_value = br.read(5) as i32;
            block.gradient_end_value = br.read(5) as i32;
        }
        block.gradient_boundary = br.read(4) as i32;

        if block.gradient_boundary > block.quantization_unit_count
            || !(0..48).contains(&block.gradient_start_unit)
            || !(0..48).contains(&block.gradient_end_unit)
            || block.gradient_start_unit > block.gradient_end_unit
            || !(0..32).contains(&block.gradient_start_value)
            || !(0..32).contains(&block.gradient_end_value)
        {
            return Err(Error::NotImplemented("gradient params invalid"));
        }
        Ok(())
    }

    fn create_gradient(&mut self, bi: usize) {
        let block = &mut self.blocks[bi];
        let value_count = block.gradient_end_value - block.gradient_start_value;
        let unit_count = block.gradient_end_unit - block.gradient_start_unit;

        for i in 0..block.gradient_end_unit as usize {
            block.gradient[i] = block.gradient_start_value;
        }
        for i in block.gradient_end_unit..=block.quantization_unit_count {
            block.gradient[i as usize] = block.gradient_end_value;
        }
        if unit_count <= 0 || value_count == 0 {
            return;
        }
        let curve = &self.gradient_curves[(unit_count - 1) as usize];
        if value_count <= 0 {
            let scale = (-value_count - 1) as f64 / 31.0;
            let base_val = block.gradient_start_value - 1;
            for i in block.gradient_start_unit..block.gradient_end_unit {
                let c = curve[(i - block.gradient_start_unit) as usize] as f64;
                block.gradient[i as usize] = base_val - (c * scale) as i32;
            }
        } else {
            let scale = (value_count - 1) as f64 / 31.0;
            let base_val = block.gradient_start_value + 1;
            for i in block.gradient_start_unit..block.gradient_end_unit {
                let c = curve[(i - block.gradient_start_unit) as usize] as f64;
                block.gradient[i as usize] = base_val + (c * scale) as i32;
            }
        }
    }

    fn read_stereo_params(&mut self, bi: usize, br: &mut BitReader) -> Result<()> {
        let block = &mut self.blocks[bi];
        if block.block_type != BlockType::Stereo {
            return Ok(());
        }
        block.primary_channel_index = br.read(1) as usize;
        block.has_joint_stereo_signs = br.read(1) == 1;
        if block.has_joint_stereo_signs {
            for i in block.stereo_quantization_unit..block.quantization_unit_count {
                block.joint_stereo_signs[i as usize] = br.read(1) as i32;
            }
        } else {
            block.joint_stereo_signs = [0; MAX_QUANT_UNITS];
        }
        Ok(())
    }

    fn read_extension_params(&mut self, bi: usize, br: &mut BitReader) -> Result<()> {
        let mut bex_band = 0usize;
        let is_stereo = self.blocks[bi].block_type == BlockType::Stereo;
        if self.blocks[bi].band_extension_enabled {
            let quc = self.blocks[bi].quantization_unit_count as usize;
            bex_band = bex::BEX_GROUP_INFO[quc - 13].band_count as usize;
            if is_stereo {
                bex_read_header(&mut self.blocks[bi].channels[1], br, bex_band);
            } else {
                br.pos += 1;
            }
        }
        self.blocks[bi].has_extension_data = br.read(1) == 1;

        if !self.blocks[bi].has_extension_data {
            return Ok(());
        }
        if !self.blocks[bi].band_extension_enabled {
            self.blocks[bi].bex_mode = br.read(2) as i32;
            self.blocks[bi].bex_data_length = br.read(5) as i32;
            br.pos += self.blocks[bi].bex_data_length as usize;
            return Ok(());
        }

        bex_read_header(&mut self.blocks[bi].channels[0], br, bex_band);
        self.blocks[bi].bex_data_length = br.read(5) as i32;
        if self.blocks[bi].bex_data_length == 0 {
            return Ok(());
        }
        let bex_data_end = br.pos + self.blocks[bi].bex_data_length as usize;
        bex_read_data(&mut self.blocks[bi].channels[0], br, bex_band);
        if is_stereo {
            bex_read_data(&mut self.blocks[bi].channels[1], br, bex_band);
        }
        if br.pos > bex_data_end {
            return Err(Error::NotImplemented("extension data invalid"));
        }
        Ok(())
    }

    fn unpack_lfe_block(&mut self, bi: usize, br: &mut BitReader) -> Result<()> {
        self.blocks[bi].quantization_unit_count = 2;
        let reuse = self.blocks[bi].reuse_band_params;
        let ch = &mut self.blocks[bi].channels[0];
        ch.scale_factors = [0; 31];
        for i in 0..2 {
            ch.scale_factors[i] = br.read(5) as i32;
        }
        let precision = if reuse { 8 } else { 4 };
        for i in 0..2 {
            ch.precisions[i] = precision;
            ch.precisions_fine[i] = 0;
        }
        ch.coded_quant_units = 2;
        ch.quantized_spectra = [0; MAX_FRAME_SAMPLES];
        for i in 0..ch.coded_quant_units {
            if ch.precisions[i] <= 0 {
                continue;
            }
            let prec = (ch.precisions[i] + 1) as u32;
            for j in QUANT_UNIT_TO_COEFF_INDEX[i]..QUANT_UNIT_TO_COEFF_INDEX[i + 1] {
                ch.quantized_spectra[j as usize] = br.read_signed(prec);
            }
        }
        Ok(())
    }

    // ----- dequantize / stereo / scale / band ext / imdct -----

    fn dequantize_spectra(&mut self, bi: usize) {
        let block = &mut self.blocks[bi];
        for ch in block.channels.iter_mut() {
            ch.spectra = [0.0; MAX_FRAME_SAMPLES];
            for band in 0..ch.coded_quant_units {
                let sub_index = QUANT_UNIT_TO_COEFF_INDEX[band] as usize;
                let sub_count = QUANT_UNIT_TO_COEFF_COUNT[band] as usize;
                let step = QUANTIZER_STEP_SIZE[ch.precisions[band] as usize];
                let step_fine = QUANTIZER_FINE_STEP_SIZE[ch.precisions_fine[band] as usize];
                for sb in 0..sub_count {
                    let coarse = ch.quantized_spectra[sub_index + sb] as f64 * step;
                    let fine = ch.quantized_spectra_fine[sub_index + sb] as f64 * step_fine;
                    ch.spectra[sub_index + sb] = coarse + fine;
                }
            }
        }
    }

    fn apply_intensity_stereo(&mut self, bi: usize) {
        let block = &mut self.blocks[bi];
        if block.block_type != BlockType::Stereo {
            return;
        }
        let total_units = block.quantization_unit_count;
        let stereo_units = block.stereo_quantization_unit;
        if stereo_units >= total_units {
            return;
        }
        let signs = block.joint_stereo_signs;
        let primary0 = block.primary_channel_index == 0;
        let (a, b) = block.channels.split_at_mut(1);
        // source = primary channel, dest = the other channel.
        let (source, dest) = if primary0 {
            (&a[0], &mut b[0])
        } else {
            (&b[0], &mut a[0])
        };
        for i in stereo_units..total_units {
            let sign = signs[i as usize];
            for sb in QUANT_UNIT_TO_COEFF_INDEX[i as usize]..QUANT_UNIT_TO_COEFF_INDEX[i as usize + 1] {
                let sbi = sb as usize;
                dest.spectra[sbi] = if sign > 0 {
                    -source.spectra[sbi]
                } else {
                    source.spectra[sbi]
                };
            }
        }
    }

    fn scale_spectrum_block(&mut self, bi: usize) {
        let block = &mut self.blocks[bi];
        let quant_unit_count = block.quantization_unit_count;
        for ch in block.channels.iter_mut() {
            for i in 0..quant_unit_count as usize {
                let scale = SPECTRUM_SCALE[ch.scale_factors[i] as usize];
                for sb in QUANT_UNIT_TO_COEFF_INDEX[i]..QUANT_UNIT_TO_COEFF_INDEX[i + 1] {
                    ch.spectra[sb as usize] *= scale;
                }
            }
        }
    }

    fn apply_band_extension(&mut self, bi: usize) {
        let block = &mut self.blocks[bi];
        if !block.band_extension_enabled || !block.has_extension_data {
            return;
        }
        let quant_unit_count = block.quantization_unit_count as usize;
        for ch in block.channels.iter_mut() {
            bex::apply_band_extension_channel(
                &mut ch.spectra,
                &ch.scale_factors,
                &ch.bex_values,
                ch.bex_mode,
                &mut ch.rng,
                quant_unit_count,
            );
        }
    }

    fn imdct_block(&mut self, bi: usize) {
        let mdct = &self.mdct;
        for ch in self.blocks[bi].channels.iter_mut() {
            mdct.run_imdct(&mut ch.imdct_prev, &ch.spectra, &mut ch.pcm);
        }
    }
}

// ----- band extension header/data readers (operate on one channel) -----

fn bex_read_header(ch: &mut Channel, br: &mut BitReader, bex_band: usize) {
    let bex_mode = br.read(2) as i32;
    ch.bex_mode = if bex_band > 2 { bex_mode } else { 4 };
    ch.bex_value_count = bex::BEX_ENCODED_VALUE_COUNTS[ch.bex_mode as usize][bex_band] as i32;
}

fn bex_read_data(ch: &mut Channel, br: &mut BitReader, bex_band: usize) {
    for i in 0..ch.bex_value_count as usize {
        let data_length = bex::BEX_DATA_LENGTHS[ch.bex_mode as usize][bex_band][i] as u32;
        ch.bex_values[i] = br.read(data_length) as i32;
    }
}

// ----- scale factors (port of scale_factors.c) -----

#[allow(clippy::too_many_arguments)]
fn read_scale_factors(
    ch: &mut Channel,
    channel_index: usize,
    first_in_superframe: bool,
    quant_units_prev: i32,
    extension_unit: i32,
    ch0_scale_factors: &[i32; 31],
    huffman: &HuffmanTables,
    br: &mut BitReader,
) -> Result<()> {
    ch.scale_factors = [0; 31];
    ch.scale_factor_coding_mode = br.read(2) as i32;
    let ext = extension_unit;
    if channel_index == 0 {
        match ch.scale_factor_coding_mode {
            0 => read_vlc_delta_offset(&mut ch.scale_factors, br, ext, huffman),
            1 => read_clc_offset(&mut ch.scale_factors, br, ext),
            2 => {
                if first_in_superframe {
                    return Err(Error::NotImplemented("scale factor mode invalid"));
                }
                let base = ch.scale_factors_prev;
                read_vlc_distance_to_baseline(&mut ch.scale_factors, br, &base, quant_units_prev, ext, huffman);
            }
            3 => {
                if first_in_superframe {
                    return Err(Error::NotImplemented("scale factor mode invalid"));
                }
                let base = ch.scale_factors_prev;
                read_vlc_delta_offset_with_baseline(&mut ch.scale_factors, br, &base, quant_units_prev, ext, huffman);
            }
            _ => {}
        }
    } else {
        match ch.scale_factor_coding_mode {
            0 => read_vlc_delta_offset(&mut ch.scale_factors, br, ext, huffman),
            1 => read_vlc_distance_to_baseline(&mut ch.scale_factors, br, ch0_scale_factors, ext, ext, huffman),
            2 => read_vlc_delta_offset_with_baseline(&mut ch.scale_factors, br, ch0_scale_factors, ext, ext, huffman),
            3 => {
                if first_in_superframe {
                    return Err(Error::NotImplemented("scale factor mode invalid"));
                }
                let base = ch.scale_factors_prev;
                read_vlc_distance_to_baseline(&mut ch.scale_factors, br, &base, quant_units_prev, ext, huffman);
            }
            _ => {}
        }
    }

    for i in 0..ext as usize {
        if ch.scale_factors[i] < 0 || ch.scale_factors[i] > 31 {
            return Err(Error::NotImplemented("scale factor out of bounds"));
        }
    }
    ch.scale_factors_prev = ch.scale_factors;
    Ok(())
}

fn read_clc_offset(sf: &mut [i32; 31], br: &mut BitReader, extension_unit: i32) {
    let max_bits = 5u32;
    let bit_length = br.read(2) + 2;
    let base_value = if bit_length < max_bits { br.read(max_bits) as i32 } else { 0 };
    for i in 0..extension_unit as usize {
        sf[i] = br.read(bit_length) as i32 + base_value;
    }
}

fn read_vlc_delta_offset(sf: &mut [i32; 31], br: &mut BitReader, extension_unit: i32, huffman: &HuffmanTables) {
    let weight_index = br.read(3) as usize;
    let weights = &SCALE_FACTOR_WEIGHTS[weight_index];
    let base_value = br.read(5) as i32;
    let bit_length = br.read(2) + 3;
    let codebook = huffman.sf_unsigned[bit_length as usize].as_ref().unwrap();

    sf[0] = br.read(bit_length) as i32;
    let value_max = codebook.value_max();
    for i in 1..extension_unit as usize {
        let delta = codebook.read_value(br, false);
        sf[i] = (sf[i - 1] + delta) & (value_max - 1);
    }
    for i in 0..extension_unit as usize {
        sf[i] += base_value - weights[i] as i32;
    }
}

fn read_vlc_distance_to_baseline(
    sf: &mut [i32; 31],
    br: &mut BitReader,
    baseline: &[i32],
    baseline_length: i32,
    extension_unit: i32,
    huffman: &HuffmanTables,
) {
    let bit_length = br.read(2) + 2;
    let codebook = huffman.sf_signed[bit_length as usize].as_ref().unwrap();
    let unit_count = extension_unit.min(baseline_length);
    for i in 0..unit_count as usize {
        let distance = codebook.read_value(br, true);
        sf[i] = (baseline[i] + distance) & 31;
    }
    for i in unit_count as usize..extension_unit as usize {
        sf[i] = br.read(5) as i32;
    }
}

fn read_vlc_delta_offset_with_baseline(
    sf: &mut [i32; 31],
    br: &mut BitReader,
    baseline: &[i32],
    baseline_length: i32,
    extension_unit: i32,
    huffman: &HuffmanTables,
) {
    let base_value = br.read_offset_binary(5);
    let bit_length = br.read(2) + 1;
    let codebook = huffman.sf_unsigned[bit_length as usize].as_ref().unwrap();
    let unit_count = extension_unit.min(baseline_length);

    sf[0] = br.read(bit_length) as i32;
    let value_max = codebook.value_max();
    for i in 1..unit_count as usize {
        let delta = codebook.read_value(br, false);
        sf[i] = (sf[i - 1] + delta) & (value_max - 1);
    }
    for i in 0..unit_count as usize {
        sf[i] += base_value + baseline[i];
    }
    for i in unit_count as usize..extension_unit as usize {
        sf[i] = br.read(5) as i32;
    }
}

// ----- bit allocation (port of bit_allocation.c) -----

fn calculate_mask(ch: &mut Channel, quant_unit_count: i32) {
    ch.precision_mask = [0; MAX_QUANT_UNITS];
    for i in 1..quant_unit_count as usize {
        let delta = ch.scale_factors[i] - ch.scale_factors[i - 1];
        if delta > 1 {
            ch.precision_mask[i] += (delta - 1).min(5);
        } else if delta < -1 {
            ch.precision_mask[i - 1] += (-delta - 1).min(5);
        }
    }
}

fn calculate_precisions(
    ch: &mut Channel,
    gradient: &[i32; 31],
    gradient_mode: i32,
    gradient_boundary: i32,
    quant_unit_count: i32,
) {
    let quc = quant_unit_count as usize;
    if gradient_mode != 0 {
        for i in 0..quc {
            ch.precisions[i] = ch.scale_factors[i] + ch.precision_mask[i] - gradient[i];
            if ch.precisions[i] > 0 {
                match gradient_mode {
                    1 => ch.precisions[i] /= 2,
                    2 => ch.precisions[i] = 3 * ch.precisions[i] / 8,
                    3 => ch.precisions[i] /= 4,
                    _ => {}
                }
            }
        }
    } else {
        for i in 0..quc {
            ch.precisions[i] = ch.scale_factors[i] - gradient[i];
        }
    }

    for i in 0..quc {
        if ch.precisions[i] < 1 {
            ch.precisions[i] = 1;
        }
    }
    for i in 0..gradient_boundary as usize {
        ch.precisions[i] += 1;
    }
    for i in 0..quc {
        ch.precisions_fine[i] = 0;
        if ch.precisions[i] > 15 {
            ch.precisions_fine[i] = ch.precisions[i] - 15;
            ch.precisions[i] = 15;
        }
    }
}

fn calculate_spectrum_codebook_index(ch: &mut Channel, high_sample_rate: bool) {
    ch.codebook_set = [0; MAX_QUANT_UNITS];
    let quant_units = ch.coded_quant_units;
    if quant_units <= 1 || high_sample_rate {
        return;
    }
    let sf = &mut ch.scale_factors;
    let original = sf[quant_units];
    sf[quant_units] = sf[quant_units - 1];

    let mut avg = 0i32;
    if quant_units > 12 {
        for &v in sf.iter().take(12) {
            avg += v;
        }
        avg = (avg + 6) / 12;
    }
    for i in 8..quant_units {
        let prev_sf = sf[i - 1];
        let next_sf = sf[i + 1];
        let min_sf = prev_sf.min(next_sf);
        if sf[i] - min_sf >= 3 || sf[i] - prev_sf + sf[i] - next_sf >= 3 {
            ch.codebook_set[i] = 1;
        }
    }
    for i in 12..quant_units {
        if ch.codebook_set[i] == 0 {
            let min_sf = sf[i - 1].min(sf[i + 1]);
            let bias = if QUANT_UNIT_TO_COEFF_COUNT[i] == 16 { 1 } else { 0 };
            if sf[i] - min_sf >= 2 && sf[i] >= avg - bias {
                ch.codebook_set[i] = 1;
            }
        }
    }
    ch.scale_factors[quant_units] = original;
}

// ----- spectra (port of unpack.c ReadSpectra/ReadSpectraFine) -----

fn read_spectra(ch: &mut Channel, br: &mut BitReader, huffman: &HuffmanTables, high_sample_rate: bool) {
    let mut values = [0i32; 16];
    ch.quantized_spectra = [0; MAX_FRAME_SAMPLES];
    let max_huff_precision = MAX_HUFF_PRECISION[high_sample_rate as usize] as i32;

    for i in 0..ch.coded_quant_units {
        let subband_count = QUANT_UNIT_TO_COEFF_COUNT[i] as usize;
        let precision = ch.precisions[i] + 1;
        if precision <= max_huff_precision {
            let cb = huffman.spectrum[ch.codebook_set[i] as usize][precision as usize]
                [QUANT_UNIT_TO_CODEBOOK_INDEX[i] as usize]
                .as_ref()
                .unwrap();
            let group_count = subband_count >> cb.value_count_power();
            for v in values.iter_mut().take(group_count) {
                *v = cb.read_value(br, false);
            }
            decode_huffman_values(
                &mut ch.quantized_spectra,
                QUANT_UNIT_TO_COEFF_INDEX[i] as usize,
                subband_count,
                cb,
                &values,
            );
        } else {
            let start = QUANT_UNIT_TO_COEFF_INDEX[i];
            let end = QUANT_UNIT_TO_COEFF_INDEX[i + 1];
            for j in start..end {
                ch.quantized_spectra[j as usize] = br.read_signed(precision as u32);
            }
        }
    }
}

fn read_spectra_fine(ch: &mut Channel, br: &mut BitReader) {
    ch.quantized_spectra_fine = [0; MAX_FRAME_SAMPLES];
    for i in 0..ch.coded_quant_units {
        if ch.precisions_fine[i] > 0 {
            let overflow_bits = (ch.precisions_fine[i] + 1) as u32;
            let start = QUANT_UNIT_TO_COEFF_INDEX[i];
            let end = QUANT_UNIT_TO_COEFF_INDEX[i + 1];
            for j in start..end {
                ch.quantized_spectra_fine[j as usize] = br.read_signed(overflow_bits);
            }
        }
    }
}
