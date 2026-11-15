//! Inverse MDCT and its trig/window/shuffle tables.
//!
//! Ported from LibAtrac9 `imdct.c` + the table generation in `decinit.c`. The
//! IMDCT is built on a fast DCT-IV (recursive butterfly) plus a windowed
//! overlap-add against the previous frame.

use core::f64::consts::PI;

const MAX_FRAME_SAMPLES: usize = 256;

/// Precomputed tables for the IMDCT at a fixed frame-size power (6, 7 or 8).
pub struct MdctTables {
    /// Frame-size power (log2 of the frame size).
    pub bits: u32,
    sin: [[f64; MAX_FRAME_SAMPLES]; 9],
    cos: [[f64; MAX_FRAME_SAMPLES]; 9],
    shuffle: [[usize; MAX_FRAME_SAMPLES]; 9],
    mdct_window: [f64; MAX_FRAME_SAMPLES],
    imdct_window: [f64; MAX_FRAME_SAMPLES],
}

fn bit_reverse32(value: u32, bits: u32) -> u32 {
    if bits == 0 {
        0
    } else {
        value.reverse_bits() >> (32 - bits)
    }
}

impl MdctTables {
    pub fn new(frame_size_power: u32) -> Self {
        let mut t = MdctTables {
            bits: frame_size_power,
            sin: [[0.0; MAX_FRAME_SAMPLES]; 9],
            cos: [[0.0; MAX_FRAME_SAMPLES]; 9],
            shuffle: [[0; MAX_FRAME_SAMPLES]; 9],
            mdct_window: [0.0; MAX_FRAME_SAMPLES],
            imdct_window: [0.0; MAX_FRAME_SAMPLES],
        };
        for size_bits in 0..9u32 {
            let size = 1usize << size_bits;
            for i in 0..size {
                let value = PI * (4 * i + 1) as f64 / (4 * size) as f64;
                t.sin[size_bits as usize][i] = value.sin();
                t.cos[size_bits as usize][i] = value.cos();
                t.shuffle[size_bits as usize][i] =
                    bit_reverse32((i ^ (i / 2)) as u32, size_bits) as usize;
            }
        }
        let frame_size = 1usize << frame_size_power;
        for i in 0..frame_size {
            t.mdct_window[i] =
                ((((i as f64 + 0.5) / frame_size as f64 - 0.5) * PI).sin() + 1.0) * 0.5;
        }
        for i in 0..frame_size {
            let a = t.mdct_window[frame_size - 1 - i];
            let b = t.mdct_window[i];
            t.imdct_window[i] = t.mdct_window[i] / (a * a + b * b);
        }
        t
    }

    /// Run the inverse MDCT for one frame. `prev` is the per-channel overlap
    /// buffer (persists between frames); `input` is the spectrum (`frame_size`
    /// values); `output` receives `frame_size` PCM samples.
    pub fn run_imdct(&self, prev: &mut [f64], input: &[f64], output: &mut [f64]) {
        let size = 1usize << self.bits;
        let half = size / 2;
        let mut dct_out = [0.0f64; MAX_FRAME_SAMPLES];
        self.dct4(input, &mut dct_out);
        let window = &self.imdct_window;

        for i in 0..half {
            output[i] = window[i] * dct_out[i + half] + prev[i];
            output[i + half] = window[i + half] * -dct_out[size - 1 - i] - prev[i + half];
            prev[i] = window[size - 1 - i] * -dct_out[half - i - 1];
            prev[i + half] = window[half - i - 1] * dct_out[i];
        }
    }

    /// Fast DCT-IV (LibAtrac9 factorization).
    fn dct4(&self, input: &[f64], output: &mut [f64]) {
        let mdct_bits = self.bits as usize;
        let mdct_size = 1usize << mdct_bits;
        let mut tmp = [0.0f64; MAX_FRAME_SAMPLES];

        let size = mdct_size;
        let last_index = size - 1;
        let half_size = size / 2;

        {
            let sin = &self.sin[mdct_bits];
            let cos = &self.cos[mdct_bits];
            for i in 0..half_size {
                let i2 = i * 2;
                let a = input[i2];
                let b = input[last_index - i2];
                let s = sin[i];
                let c = cos[i];
                tmp[i2] = a * c + b * s;
                tmp[i2 + 1] = a * s - b * c;
            }
        }

        let stage_count = mdct_bits - 1;
        for stage in 0..stage_count {
            let block_count = 1usize << stage;
            let block_size_bits = stage_count - stage;
            let block_half_size_bits = block_size_bits - 1;
            let block_size = 1usize << block_size_bits;
            let block_half_size = 1usize << block_half_size_bits;
            let sin = &self.sin[block_half_size_bits];
            let cos = &self.cos[block_half_size_bits];

            for block in 0..block_count {
                for i in 0..block_half_size {
                    let front_pos = (block * block_size + i) * 2;
                    let back_pos = front_pos + block_size;
                    let a = tmp[front_pos] - tmp[back_pos];
                    let b = tmp[front_pos + 1] - tmp[back_pos + 1];
                    let s = sin[i];
                    let c = cos[i];
                    tmp[front_pos] += tmp[back_pos];
                    tmp[front_pos + 1] += tmp[back_pos + 1];
                    tmp[back_pos] = a * c + b * s;
                    tmp[back_pos + 1] = a * s - b * c;
                }
            }
        }

        let shuffle = &self.shuffle[mdct_bits];
        for i in 0..mdct_size {
            output[i] = tmp[shuffle[i]];
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Naive DCT-IV for cross-checking the fast transform.
    fn naive_dct4(input: &[f64]) -> Vec<f64> {
        let n = input.len();
        let mut out = vec![0.0; n];
        for k in 0..n {
            let mut acc = 0.0;
            for (i, &x) in input.iter().enumerate() {
                acc += x
                    * (PI / n as f64 * (i as f64 + 0.5) * (k as f64 + 0.5)).cos();
            }
            out[k] = acc;
        }
        out
    }

    #[test]
    fn dct4_matches_naive() {
        for bits in [6u32, 7, 8] {
            let n = 1usize << bits;
            let t = MdctTables::new(bits);
            // Deterministic pseudo-random input.
            let input: Vec<f64> = (0..n)
                .map(|i| ((i as f64 * 12.9898).sin() * 43758.5453).fract() - 0.5)
                .collect();
            let mut got = vec![0.0; n];
            t.dct4(&input, &mut got);
            let want = naive_dct4(&input);
            for i in 0..n {
                assert!(
                    (got[i] - want[i]).abs() < 1e-6,
                    "bits={bits} i={i} got={} want={}",
                    got[i],
                    want[i]
                );
            }
        }
    }

    #[test]
    fn imdct_window_is_finite() {
        let t = MdctTables::new(8);
        for i in 0..256 {
            assert!(t.imdct_window[i].is_finite());
        }
    }
}
