//! MSB-first bit writer — the inverse of [`crate::decode::bitreader::BitReader`].

/// Accumulates bits MSB-first into a byte buffer. In `count_only` mode it tracks
/// only the bit length (no buffer) — used to size a block without packing it.
#[derive(Default)]
pub struct BitWriter {
    buf: Vec<u8>,
    /// Number of bits written so far.
    pub bit_pos: usize,
    count_only: bool,
}

impl BitWriter {
    pub fn new() -> Self {
        Self::default()
    }

    /// A writer that only measures bit length (produces no bytes).
    pub fn counting() -> Self {
        Self { count_only: true, ..Self::default() }
    }

    /// Write the low `bits` bits of `value`, MSB first.
    pub fn write(&mut self, value: u32, bits: u32) {
        if self.count_only {
            self.bit_pos += bits as usize;
            return;
        }
        if bits == 0 {
            return;
        }
        let mut remaining = bits;
        // Write in chunks, filling the current byte before moving on.
        while remaining > 0 {
            let byte_idx = self.bit_pos >> 3;
            if byte_idx >= self.buf.len() {
                self.buf.push(0);
            }
            let bit_off = (self.bit_pos & 7) as u32;
            let avail = 8 - bit_off; // free bits left in the current byte
            let take = remaining.min(avail);
            // The `take` most-significant of the still-unwritten bits.
            let chunk = (value >> (remaining - take)) & ((1u32 << take) - 1);
            self.buf[byte_idx] |= (chunk << (avail - take)) as u8;
            self.bit_pos += take as usize;
            remaining -= take;
        }
    }

    /// Write a signed value in `bits` bits (two's complement, low bits), matching
    /// `BitReader::read_signed`.
    pub fn write_signed(&mut self, value: i32, bits: u32) {
        let mask = if bits >= 32 { u32::MAX } else { (1u32 << bits) - 1 };
        self.write(value as u32 & mask, bits);
    }

    /// Write a value in offset-binary (`value + 2^(bits-1)`), matching
    /// `BitReader::read_offset_binary`.
    pub fn write_offset_binary(&mut self, value: i32, bits: u32) {
        let offset = 1i32 << (bits - 1);
        self.write((value + offset) as u32, bits);
    }

    /// Pad with zero bits up to the next multiple of `multiple` bits.
    pub fn align(&mut self, multiple: usize) {
        let rem = self.bit_pos % multiple;
        if rem != 0 {
            let pad = multiple - rem;
            if self.count_only {
                self.bit_pos += pad;
                return;
            }
            for _ in 0..pad {
                let byte_idx = self.bit_pos >> 3;
                if byte_idx >= self.buf.len() {
                    self.buf.push(0);
                }
                self.bit_pos += 1;
            }
        }
    }

    /// Bytes written (the final partial byte is zero-padded).
    pub fn into_bytes(self) -> Vec<u8> {
        self.buf
    }

    pub fn len_bytes(&self) -> usize {
        self.buf.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decode::bitreader::BitReader;

    #[test]
    fn round_trips_with_bitreader() {
        let mut w = BitWriter::new();
        w.write(0b101, 3);
        w.write(0b10010, 5);
        w.write(1234, 11);
        w.write_signed(-1, 3);
        w.write_offset_binary(3, 5);
        w.write_signed(-57, 8);
        w.align(8);
        w.write(0xABCD, 16);
        let bytes = w.into_bytes();

        let mut r = BitReader::new(&bytes);
        assert_eq!(r.read(3), 0b101);
        assert_eq!(r.read(5), 0b10010);
        assert_eq!(r.read(11), 1234);
        assert_eq!(r.read_signed(3), -1);
        assert_eq!(r.read_offset_binary(5), 3);
        assert_eq!(r.read_signed(8), -57);
        r.align(8);
        assert_eq!(r.read(16), 0xABCD);
    }

    #[test]
    fn align_pads_to_byte() {
        let mut w = BitWriter::new();
        w.write(0b1, 1);
        w.align(8);
        assert_eq!(w.bit_pos, 8);
        assert_eq!(w.len_bytes(), 1);
    }
}
