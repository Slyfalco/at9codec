//! Huffman encoding — the inverse of the decoder's `ReadHuffmanValue` /
//! `DecodeHuffmanValues`.
//!
//! Spectrum codebooks pack `value_count` coefficients into one symbol
//! (`symbol = c0 | c1<<vb | ...`, each coeff masked to `value_bits`), then emit
//! that symbol's Huffman code. Some symbols are absent from a codebook
//! (`symbol_len == 0`); [`can_encode_spectra`] lets the caller detect that and
//! raise the precision so the stream stays valid.

use super::bitwriter::BitWriter;
use crate::decode::huffman::Codebook;

fn mask(value_bits: u32) -> i32 {
    (1i32 << value_bits) - 1
}

/// Pack `value_count` coefficients (starting at `off`) into a codebook symbol.
fn symbol_at(cb: &Codebook, coeffs: &[i32], off: usize) -> usize {
    let vb = cb.value_bits();
    let m = mask(vb);
    let mut symbol = 0u32;
    for k in 0..cb.value_count() {
        symbol |= ((coeffs[off + k] & m) as u32) << (k as u32 * vb);
    }
    symbol as usize
}

/// True if every group of `coeffs` has a code in this codebook.
pub fn can_encode_spectra(cb: &Codebook, coeffs: &[i32]) -> bool {
    let vc = cb.value_count();
    if vc == 0 || coeffs.len() % vc != 0 {
        return false;
    }
    (0..coeffs.len())
        .step_by(vc)
        .all(|off| cb.symbol_len(symbol_at(cb, coeffs, off)) > 0)
}

/// Encode `coeffs` (a whole quant unit) with a spectrum codebook. Caller must
/// have checked [`can_encode_spectra`]. Returns false if a symbol was absent.
pub fn encode_spectra(w: &mut BitWriter, cb: &Codebook, coeffs: &[i32]) -> bool {
    let vc = cb.value_count();
    let mut off = 0;
    while off < coeffs.len() {
        let symbol = symbol_at(cb, coeffs, off);
        let len = cb.symbol_len(symbol);
        if len == 0 {
            return false;
        }
        w.write(cb.symbol_code(symbol), len);
        off += vc;
    }
    true
}

/// Encode one value with a scale-factor codebook (value_count 1). `value` is the
/// signed delta/distance; it is masked to `value_bits` to find the symbol.
pub fn encode_scale_value(w: &mut BitWriter, cb: &Codebook, value: i32) -> bool {
    let symbol = (value & mask(cb.value_bits())) as usize;
    let len = cb.symbol_len(symbol);
    if len == 0 {
        return false;
    }
    w.write(cb.symbol_code(symbol), len);
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decode::bitreader::BitReader;
    use crate::decode::huffman::{decode_huffman_values, HuffmanTables};

    #[test]
    fn spectrum_huffman_round_trips() {
        let tables = HuffmanTables::new();
        // A21: set 0, precision index 2, codebook index 0 (value_count 2, vb 2).
        let cb = tables.spectrum[0][2][0].as_ref().unwrap();
        // Coeffs in {-1,0,1}, no all-zero pair (A21's representable set).
        let coeffs = [1, 0, -1, 1, 0, -1, 1, -1];
        assert!(can_encode_spectra(cb, &coeffs));

        let mut w = BitWriter::new();
        assert!(encode_spectra(&mut w, cb, &coeffs));
        let bytes = w.into_bytes();

        // Decode back with the reader + DecodeHuffmanValues.
        let mut r = BitReader::new(&bytes);
        let groups = coeffs.len() / cb.value_count();
        let mut syms = vec![0i32; groups];
        for s in syms.iter_mut() {
            *s = cb.read_value(&mut r, false);
        }
        let mut out = vec![0i32; coeffs.len()];
        decode_huffman_values(&mut out, 0, coeffs.len(), cb, &syms);
        assert_eq!(out, coeffs);
    }

    #[test]
    fn detects_absent_symbol() {
        let tables = HuffmanTables::new();
        let cb = tables.spectrum[0][2][0].as_ref().unwrap();
        // (0,0) is absent from A21.
        assert!(!can_encode_spectra(cb, &[0, 0]));
    }
}
