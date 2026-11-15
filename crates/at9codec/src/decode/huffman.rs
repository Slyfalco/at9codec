//! Huffman decoding (port of LibAtrac9 `huffCodes.c`).
//!
//! The large codebook `Bits`/`Codes` arrays and the codebook metadata live in the
//! generated `huffman_tables` module; this file holds the reader, the lookup-table
//! builder and the value decoder.

use super::bitreader::{sign_extend32, BitReader};
use super::huffman_tables as tbl;

/// Static description of one Huffman codebook (no runtime lookup yet).
#[derive(Clone, Copy)]
pub struct CodebookMeta {
    pub bits: &'static [u8],
    pub codes: &'static [u16],
    pub value_count: usize,
    pub value_count_power: u32,
    pub value_bits: u32,
    pub value_max: i32,
    pub max_bit_size: u32,
}

/// A codebook with its decode lookup table built.
pub struct Codebook {
    meta: CodebookMeta,
    /// `1 << max_bit_size` entries mapping a peeked code to a value index.
    lookup: Vec<u8>,
}

impl Codebook {
    fn new(meta: CodebookMeta) -> Self {
        let size = 1usize << meta.max_bit_size;
        let mut lookup = vec![0u8; size];
        for i in 0..meta.bits.len() {
            let b = meta.bits[i] as u32;
            if b == 0 {
                continue;
            }
            let unused = meta.max_bit_size - b;
            let start = (meta.codes[i] as usize) << unused;
            let len = 1usize << unused;
            for slot in lookup.iter_mut().skip(start).take(len) {
                *slot = i as u8;
            }
        }
        Codebook { meta, lookup }
    }

    pub fn value_count(&self) -> usize {
        self.meta.value_count
    }
    pub fn value_count_power(&self) -> u32 {
        self.meta.value_count_power
    }
    pub fn value_max(&self) -> i32 {
        self.meta.value_max
    }

    /// Read one Huffman-coded value, advancing the bit reader.
    pub fn read_value(&self, br: &mut BitReader, is_signed: bool) -> i32 {
        let code = br.peek(self.meta.max_bit_size) as usize;
        let value = self.lookup[code];
        let bits = self.meta.bits[value as usize];
        br.pos += bits as usize;
        if is_signed {
            sign_extend32(value as u32, self.meta.value_bits)
        } else {
            value as i32
        }
    }
}

/// Expand packed Huffman `values` into `spectrum[index..]` (port of
/// `DecodeHuffmanValues`).
pub fn decode_huffman_values(
    spectrum: &mut [i32],
    index: usize,
    band_count: usize,
    cb: &Codebook,
    values: &[i32],
) {
    let value_count = band_count >> cb.meta.value_count_power;
    let mask = (1i32 << cb.meta.value_bits) - 1;
    let mut idx = index;
    for &packed in values.iter().take(value_count) {
        let mut value = packed;
        for _ in 0..cb.meta.value_count {
            spectrum[idx] = sign_extend32((value & mask) as u32, cb.meta.value_bits);
            idx += 1;
            value >>= cb.meta.value_bits;
        }
    }
}

/// All codebooks, lookups built. Construct once per decoder.
pub struct HuffmanTables {
    /// Indexed by scale-factor VLC bit length (slots 0, and 1 for signed, empty).
    pub sf_unsigned: Vec<Option<Codebook>>,
    pub sf_signed: Vec<Option<Codebook>>,
    /// `[codebook_set][precision][codebook_index]`.
    pub spectrum: Vec<Vec<Vec<Option<Codebook>>>>,
}

impl HuffmanTables {
    pub fn new() -> Self {
        let build = |m: Option<CodebookMeta>| m.map(Codebook::new);
        let sf_unsigned = tbl::sf_unsigned().into_iter().map(build).collect();
        let sf_signed = tbl::sf_signed().into_iter().map(build).collect();
        let spectrum = tbl::spectrum()
            .into_iter()
            .map(|set| {
                set.into_iter()
                    .map(|row| row.into_iter().map(build).collect())
                    .collect()
            })
            .collect();
        HuffmanTables {
            sf_unsigned,
            sf_signed,
            spectrum,
        }
    }
}

impl Default for HuffmanTables {
    fn default() -> Self {
        Self::new()
    }
}
