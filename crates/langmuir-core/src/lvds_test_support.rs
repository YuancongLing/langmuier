use super::{LVDS_CRC_CALC_END, LVDS_CRC_CALC_START, LVDS_CRC_FIELD_OFFSET, LVDS_FRAME_LEN};
use crate::checksum::test_support::manufacturer_crc16;

pub(crate) fn refresh_crc16(frame: &mut [u8; LVDS_FRAME_LEN]) {
    let crc = manufacturer_crc16(&frame[LVDS_CRC_CALC_START..LVDS_CRC_CALC_END]);
    frame[LVDS_CRC_FIELD_OFFSET..LVDS_CRC_FIELD_OFFSET + 2].copy_from_slice(&crc.to_be_bytes());
}

/// Independently specified manufacturer vectors, not encoded by the decoder.
pub(crate) fn golden_frame() -> [u8; LVDS_FRAME_LEN] {
    let mut frame = [0_u8; LVDS_FRAME_LEN];
    frame[..15].copy_from_slice(&[
        0xEB, 0x90, 0x55, 0xAA, 0x01, 0x07, 0xAA, 0x65, 0, 0, 0, 0, 0x0F, 0x41, 0x78,
    ]);
    for group in frame[15..140].as_chunks_mut::<5>().0 {
        group.copy_from_slice(&[0xFF, 0xC0, 0x08, 0, 1]);
    }
    for group in frame[140..265].as_chunks_mut::<5>().0 {
        group.copy_from_slice(&[0, 0x3F, 0xF0, 3, 0xFF]);
    }
    frame[265] = 255;
    frame[267..269].copy_from_slice(&[0x12, 0x34]);
    frame
}

pub(crate) fn valid_frame() -> Box<[u8; LVDS_FRAME_LEN]> {
    let mut frame = Box::new(golden_frame());
    frame[7..11].copy_from_slice(&1_700_000_000_u32.to_be_bytes());
    frame[11..15].copy_from_slice(&123_456_u32.to_be_bytes());
    refresh_crc16(&mut frame);
    frame
}
