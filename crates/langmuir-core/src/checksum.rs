/// Returns the low eight bits of the sum of all bytes.
#[must_use]
pub fn additive_checksum(data: &[u8]) -> u8 {
    data.iter()
        .fold(0_u8, |checksum, byte| checksum.wrapping_add(*byte))
}

/// Computes CRC16-CCITT-FALSE (`poly=0x1021`, `init=0xFFFF`, no reflection,
/// `xorout=0x0000`).
#[must_use]
pub fn crc16_ccitt_false(data: &[u8]) -> u16 {
    let mut crc = 0xFFFF_u16;
    for byte in data {
        crc ^= u16::from(*byte) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 != 0 {
                (crc << 1) ^ 0x1021
            } else {
                crc << 1
            };
        }
    }
    crc
}

#[cfg(test)]
mod tests {
    use super::{additive_checksum, crc16_ccitt_false};

    #[test]
    fn additive_checksum_wraps_at_eight_bits() {
        assert_eq!(additive_checksum(&[]), 0);
        assert_eq!(additive_checksum(&[0xFF, 0x02]), 0x01);
        assert_eq!(additive_checksum(&[0x04, 0x11, 0x9B]), 0xB0);
    }

    #[test]
    fn crc_matches_standard_check_vector() {
        assert_eq!(crc16_ccitt_false(b"123456789"), 0x29B1);
        assert_eq!(crc16_ccitt_false(&[]), 0xFFFF);
    }
}
