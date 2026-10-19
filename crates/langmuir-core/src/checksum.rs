/// Returns the low eight bits of the sum of all bytes.
#[must_use]
pub fn additive_checksum(data: &[u8]) -> u8 {
    data.iter()
        .fold(0_u8, |checksum, byte| checksum.wrapping_add(*byte))
}

/// Computes CRC16-CCITT exactly as the lower computer's `Calculate_CRC16`:
/// `poly=0x1021`, `init=0xFFFF`, no reflection, and `xorout=0x0000`.
#[must_use]
pub fn crc16_ccitt_false(data: &[u8]) -> u16 {
    let mut crc = 0xFFFF_u16;
    for &byte in data {
        crc ^= u16::from(byte) << 8;
        for _ in 0..8 {
            if crc & 0x8000 != 0 {
                crc = (crc << 1) ^ 0x1021;
            } else {
                crc <<= 1;
            }
        }
    }
    crc
}

#[cfg(test)]
pub(crate) mod test_support {
    /// Test-only translation of the manufacturer's C function. The wider
    /// accumulator and explicit truncation model its `(uint16_t)` casts,
    /// independently of the production u16 shift implementation.
    pub(crate) fn manufacturer_crc16(data: &[u8]) -> u16 {
        let mut crc = 0xFFFF_u32;
        for byte in data {
            crc ^= u32::from(*byte) << 8;
            for _ in 0..8 {
                crc = if crc & 0x8000 != 0 {
                    ((crc << 1) ^ 0x1021) & 0xFFFF
                } else {
                    (crc << 1) & 0xFFFF
                };
            }
        }
        u16::try_from(crc).expect("manufacturer reference masks to 16 bits")
    }
}

#[cfg(test)]
mod tests {
    use super::{additive_checksum, crc16_ccitt_false, test_support::manufacturer_crc16};

    #[test]
    fn additive_checksum_wraps_at_eight_bits() {
        assert_eq!(additive_checksum(&[]), 0);
        assert_eq!(additive_checksum(&[0xFF, 0x02]), 0x01);
        assert_eq!(additive_checksum(&[0x04, 0x11, 0x9B]), 0xB0);
    }

    #[test]
    fn crc_matches_the_lower_computer_ccitt_reference_algorithm() {
        assert_eq!(crc16_ccitt_false(b"123456789"), 0x29B1);
        assert_eq!(crc16_ccitt_false(&[]), 0xFFFF);
    }

    #[test]
    fn crc_matches_independent_manufacturer_c_vectors() {
        // Fixed expectations were produced by compiling the manufacturer's
        // supplied C, rather than by this crate's production CRC function.
        let ascending: Vec<u8> = (0..=u8::MAX).collect();
        let vectors: &[(&[u8], u16)] = &[
            (&[], 0xFFFF),
            (b"123456789", 0x29B1),
            (&[0x00, 0xFF, 0x80, 0x01, 0x55, 0xAA], 0x5B5D),
            (&ascending, 0x3FBD),
        ];
        for &(bytes, expected) in vectors {
            assert_eq!(manufacturer_crc16(bytes), expected);
            assert_eq!(crc16_ccitt_false(bytes), expected);
        }
    }
}
