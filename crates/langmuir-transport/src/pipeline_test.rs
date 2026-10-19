use std::fs::File;
use std::io::Read;
use std::time::Duration;

use langmuir_core::{
    LVDS_CRC_CALC_END, LVDS_CRC_CALC_START, LVDS_CRC_FIELD_OFFSET, LvdsStreamDecoder, SAMPLE_COUNT,
    crc16_ccitt_false,
};
use langmuir_storage::{
    BINARY_FRAME_LEN, BINARY_HEADER_LEN, BINARY_MAGIC, BatchWriter, BinaryConfig, BinarySink,
    StorageServiceConfig,
};
use tempfile::tempdir;

use crate::mock::{lvds_packet, memory_transport};
use crate::{BackpressurePolicy, ReaderConfig, ReaderEvent, ReaderWorker, StopReason};

fn le_u32(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().expect("four bytes"))
}

fn packet_at_50ms_interval(frame_index: u32) -> Vec<u8> {
    let mut packet = lvds_packet(frame_index, 1_700_000_000);
    packet[11..15].copy_from_slice(&(frame_index * 50_000).to_be_bytes());
    let checksum = crc16_ccitt_false(&packet[LVDS_CRC_CALC_START..LVDS_CRC_CALC_END]);
    packet[LVDS_CRC_FIELD_OFFSET..].copy_from_slice(&checksum.to_be_bytes());
    packet
}

#[test]
fn fragmented_valid_lvds_reaches_storage_while_bad_crc_is_excluded() {
    const FRAME_COUNT: u32 = 4;
    let (transport, mut input) = memory_transport(256, Duration::from_millis(5)).unwrap();
    let chunk_pattern = [1_usize, 7, 31, 257, 1_024];
    for frame_index in 0..FRAME_COUNT {
        let packet = packet_at_50ms_interval(frame_index);
        if frame_index == 1 {
            let mut corrupt = packet.clone();
            corrupt[LVDS_CRC_FIELD_OFFSET] ^= 0x01;
            input.feed(corrupt).unwrap();
        }
        let mut offset = 0;
        let mut pattern_index = 0;
        while offset < packet.len() {
            let end =
                (offset + chunk_pattern[pattern_index % chunk_pattern.len()]).min(packet.len());
            input.feed(packet[offset..end].to_vec()).unwrap();
            offset = end;
            pattern_index += 1;
        }
    }
    input.close();

    let directory = tempdir().unwrap();
    let capture_path = directory.path().join("pipeline.lpd");
    let sink = BinarySink::create(BinaryConfig::new(&capture_path)).unwrap();
    let writer = BatchWriter::start(
        sink,
        StorageServiceConfig {
            queue_capacity: 2,
            batch_size: 2,
            ..StorageServiceConfig::default()
        },
    )
    .unwrap();
    let mut reader = ReaderWorker::spawn(
        Box::new(transport),
        LvdsStreamDecoder::new(),
        ReaderConfig {
            event_capacity: 4,
            read_buffer_bytes: 333,
            backpressure: BackpressurePolicy::Wait(Duration::from_secs(1)),
            ..ReaderConfig::default()
        },
    )
    .unwrap();

    let mut received = 0_u32;
    loop {
        match reader
            .events()
            .recv_timeout(Duration::from_secs(1))
            .unwrap()
        {
            ReaderEvent::Frame(frame) => {
                writer
                    .submit_timeout(frame, Duration::from_secs(1))
                    .unwrap();
                received += 1;
            }
            ReaderEvent::Stopped { reason } => {
                assert_eq!(reason, StopReason::EndOfStream);
                break;
            }
            ReaderEvent::Fault(fault) | ReaderEvent::WriteFailed { fault, .. } => {
                panic!("unexpected pipeline fault: {fault:?}");
            }
            ReaderEvent::Started { .. } | ReaderEvent::BytesWritten { .. } => {}
        }
    }
    let reader_exit = reader.join().unwrap();
    let writer_exit = writer.shutdown().unwrap();
    assert_eq!(received, FRAME_COUNT);
    assert_eq!(reader_exit.metrics.frames_decoded, u64::from(FRAME_COUNT));
    assert_eq!(reader_exit.metrics.rejected_frames, 1);
    assert_eq!(reader_exit.metrics.checksum_failures, 1);
    assert_eq!(reader_exit.metrics.events_dropped, 0);
    assert_eq!(reader_exit.metrics.queue_overflows, 0);
    assert_eq!(writer_exit.metrics.written_items, u64::from(FRAME_COUNT));
    assert_eq!(writer_exit.metrics.rejected_items, 0);

    let mut bytes = Vec::new();
    File::open(capture_path)
        .unwrap()
        .read_to_end(&mut bytes)
        .unwrap();
    assert_eq!(&bytes[..8], &BINARY_MAGIC);
    assert_eq!(
        bytes.len(),
        usize::from(BINARY_HEADER_LEN)
            + usize::try_from(FRAME_COUNT).unwrap() * usize::try_from(BINARY_FRAME_LEN).unwrap()
    );
    for frame_index in 0..FRAME_COUNT {
        let frame_offset = usize::from(BINARY_HEADER_LEN)
            + usize::try_from(frame_index).unwrap() * usize::try_from(BINARY_FRAME_LEN).unwrap();
        assert_eq!(le_u32(&bytes, frame_offset), 1_700_000_000);
        assert_eq!(le_u32(&bytes, frame_offset + 4), frame_index * 50_000);
        assert_eq!(le_u32(&bytes, frame_offset + 10), 2_048 + frame_index % 100);
        assert_eq!(
            le_u32(&bytes, frame_offset + 10 + SAMPLE_COUNT * 4),
            20_000 + (frame_index % 100) * 2
        );
    }
}
