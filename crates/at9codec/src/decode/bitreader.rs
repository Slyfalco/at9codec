//! MSB-first bit reader, mirroring LibAtrac9's `BitReaderCxt`.

/// Reads big-endian-packed fields from a byte buffer, tracking a bit position.
pub struct BitReader<'a> {
    buf: &'a [u8],
    /// Current read position, in bits from the start of the buffer.
    pub pos: usize,
}

impl<'a> BitReader<'a> {
    pub fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    /// Bits remaining in the buffer.
    pub fn remaining(&self) -> usize {
        (self.buf.len() * 8).saturating_sub(self.pos)
    }

    /// Read `bits` bits (0..=32) as an unsigned value, advancing the position.
    pub fn read(&mut self, bits: u32) -> u32 {
        let v = self.peek(bits);
        self.pos += bits as usize;
        v
    }

    /// Peek `bits` bits without advancing. MSB-first; out-of-range bytes read 0.
    pub fn peek(&self, bits: u32) -> u32 {
        let mut v = 0u32;
        for i in 0..bits as usize {
            let p = self.pos + i;
            let byte = self.buf.get(p >> 3).copied().unwrap_or(0);
            let bit = (byte >> (7 - (p & 7))) & 1;
            v = (v << 1) | bit as u32;
        }
        v
    }

    /// Read `bits` bits and sign-extend from that width.
    pub fn read_signed(&mut self, bits: u32) -> i32 {
        let v = self.read(bits);
        sign_extend32(v, bits)
    }

    /// Read `bits` bits interpreted as offset-binary (value - 2^(bits-1)).
    pub fn read_offset_binary(&mut self, bits: u32) -> i32 {
        let offset = 1i32 << (bits - 1);
        self.read(bits) as i32 - offset
    }

    /// Advance the position to the next multiple of `multiple` bits.
    pub fn align(&mut self, multiple: usize) {
        let rem = self.pos % multiple;
        if rem != 0 {
            self.pos += multiple - rem;
        }
    }
}

/// Sign-extend the low `bits` bits of `value` to a full i32.
pub fn sign_extend32(value: u32, bits: u32) -> i32 {
    let shift = 32 - bits;
    ((value << shift) as i32) >> shift
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_msb_first() {
        // 0b1011_0010, 0b1100_0000
        let data = [0b1011_0010u8, 0b1100_0000];
        let mut br = BitReader::new(&data);
        assert_eq!(br.read(3), 0b101);
        assert_eq!(br.read(5), 0b10010);
        assert_eq!(br.read(2), 0b11);
        assert_eq!(br.pos, 10);
    }

    #[test]
    fn signed_and_offset() {
        assert_eq!(sign_extend32(0b111, 3), -1);
        assert_eq!(sign_extend32(0b011, 3), 3);
        let data = [0b000_111_00u8];
        let mut br = BitReader::new(&data);
        br.read(3);
        assert_eq!(br.read_offset_binary(3), 3); // 0b111=7, -4 => 3
    }

    #[test]
    fn align_works() {
        let data = [0u8; 4];
        let mut br = BitReader::new(&data);
        br.read(3);
        br.align(8);
        assert_eq!(br.pos, 8);
        br.align(8);
        assert_eq!(br.pos, 8);
    }
}
