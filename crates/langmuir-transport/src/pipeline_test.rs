use std::fs::File;
use std::io::Read;
use std::path::Path;
use std::time::Duration;

use langmuir_core::{LVDS_CRC_FIELD_OFFSET, LvdsStreamDecoder, SAMPLE_COUNT};
use langmuir_storage::{
    BatchWriter, BinaryConfig, BinaryV3Reader, BinaryV3Sink, StorageServiceConfig, V3_FRAME_LEN,
    V3_HEADER_LEN, V3_MAGIC,
};
use tempfile::tempdir;

use crate::mock::{lvds_packet, memory_transport};
use crate::{BackpressurePolicy, ReaderConfig, ReaderEvent, ReaderWorker, StopReason};

fn packet_at_50ms_interval(frame_index: u32) -> Vec<u8> {
    let mut packet = lvds_packet(frame_index, 1_700_000_000);
    packet[11..15].copy_from_slice(&(frame_index * 50_000).to_be_bytes());
    packet
}

#[test]
fn fragmented_decodable_lvds_reaches_storage_with_unchecked_crc_and_rejects_bad_structure() {
    const FRAME_COUNT: u32 = 4;
    let (transport, mut input) = memory_transport(256, Duration::from_millis(5)).unwrap();
    let chunk_pattern = [1_usize, 7, 31, 257, 1_024];
    for frame_index in 0..FRAME_COUNT {
        let mut packet = packet_at_50ms_interval(frame_index);
        if frame_index == 1 {
            let mut invalid_structure = packet.clone();
            invalid_structure[2] = 0;
            input.feed(invalid_structure).unwrap();
            packet[LVDS_CRC_FIELD_OFFSET] ^= 0x01;
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
    let sink = BinaryV3Sink::create(BinaryConfig::new(&capture_path)).unwrap();
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
            // Started, every frame, and Stopped must fit even if the test
            // consumer has not been scheduled; auxiliary events use try_send.
            event_capacity: usize::try_from(FRAME_COUNT).unwrap() + 2,
            read_buffer_bytes: 333,
            backpressure: BackpressurePolicy::Wait(Duration::from_secs(1)),
            ..ReaderConfig::default()
        },
    )
    .unwrap();

    assert!(matches!(
        reader
            .events()
            .recv_timeout(Duration::from_secs(1))
            .unwrap(),
        ReaderEvent::Started { .. }
    ));
    // Finish the finite producer before draining frames so this also exercises
    // the slow-consumer schedule that previously lost the Stopped event.
    let reader_exit = reader.join().unwrap();
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
    let writer_exit = writer.shutdown().unwrap();
    assert_eq!(received, FRAME_COUNT);
    assert_eq!(reader_exit.metrics.frames_decoded, u64::from(FRAME_COUNT));
    assert_eq!(reader_exit.metrics.rejected_frames, 1);
    assert_eq!(reader_exit.metrics.checksum_failures, 0);
    assert_eq!(reader_exit.metrics.events_dropped, 0);
    assert_eq!(reader_exit.metrics.queue_overflows, 0);
    assert_eq!(writer_exit.metrics.written_items, u64::from(FRAME_COUNT));
    assert_eq!(writer_exit.metrics.rejected_items, 0);

    assert_v3_capture(&capture_path, FRAME_COUNT);
}

fn assert_v3_capture(capture_path: &Path, frame_count: u32) {
    let mut bytes = Vec::new();
    File::open(capture_path)
        .unwrap()
        .read_to_end(&mut bytes)
        .unwrap();
    assert_eq!(&bytes[..8], &V3_MAGIC);
    assert_eq!(
        bytes.len(),
        usize::from(V3_HEADER_LEN)
            + usize::try_from(frame_count).unwrap() * usize::try_from(V3_FRAME_LEN).unwrap()
    );
    let mut stored = BinaryV3Reader::open(capture_path).unwrap();
    for frame_index in 0..frame_count {
        let frame = stored.next_frame().unwrap().unwrap();
        assert_eq!(frame.timestamp.utc_seconds, 1_700_000_000);
        assert_eq!(frame.timestamp.microseconds, frame_index * 50_000);
        assert_eq!(frame.frame_seq_u8, u8::try_from(frame_index).unwrap());
        assert_eq!(frame.channel_1.len(), 100);
        assert_eq!(frame.channel_2.len(), 100);
        let phase = u16::try_from(frame_index).unwrap();
        for sample in 0..SAMPLE_COUNT {
            let offset = u16::try_from(sample).unwrap();
            assert_eq!(frame.channel_1[sample], 512 + phase + offset);
            assert_eq!(frame.channel_2[sample], 1_023 - phase * 2 - offset);
        }
    }
    assert!(stored.next_frame().unwrap().is_none());
}
