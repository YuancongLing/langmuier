use std::error::Error;
use std::thread;
use std::time::{Duration, Instant};

use langmuir_core::{LVDS_FRAME_LEN, LvdsStreamDecoder};
use langmuir_storage::{
    BatchWriter, BinaryConfig, BinarySink, StorageServiceConfig, WriterStopReason,
};
use langmuir_transport::mock::{MemoryFeedError, lvds_packet, memory_transport};
use langmuir_transport::{BackpressurePolicy, ReaderConfig, ReaderEvent, ReaderWorker, StopReason};
use tempfile::tempdir;

// Kept as one linear orchestration path so benchmark timing and teardown are
// visible together instead of being hidden behind helpers.
#[allow(clippy::too_many_lines)]
fn main() -> Result<(), Box<dyn Error>> {
    let frame_count = argument(1, 10_000)?;
    let fragment_bytes = argument(2, 257)?;
    if frame_count == 0 || fragment_bytes == 0 {
        return Err("frame count and fragment size must be non-zero".into());
    }
    let frame_count_u32 = u32::try_from(frame_count)
        .map_err(|_| "frame count must fit in a 32-bit replay sequence")?;

    let directory = tempdir()?;
    let path = directory.path().join("replay-benchmark.lpd");
    let sink = BinarySink::create(BinaryConfig::new(&path))?;
    let writer = BatchWriter::start(
        sink,
        StorageServiceConfig {
            queue_capacity: 1_024,
            batch_size: 100,
            flush_interval: Duration::from_secs(2),
            ..StorageServiceConfig::default()
        },
    )?;
    let (transport, mut input) = memory_transport(256, Duration::from_millis(5))?;
    let mut reader = ReaderWorker::spawn(
        Box::new(transport),
        LvdsStreamDecoder::new(),
        ReaderConfig {
            event_capacity: 1_024,
            read_buffer_bytes: 64 * 1_024,
            backpressure: BackpressurePolicy::Wait(Duration::from_secs(1)),
            ..ReaderConfig::default()
        },
    )?;

    let started = Instant::now();
    let producer = thread::spawn(move || -> Result<(), String> {
        for frame_index in 0..frame_count_u32 {
            let packet = lvds_packet(frame_index, 1_700_000_000);
            for fragment in packet.chunks(fragment_bytes) {
                let mut pending = fragment.to_vec();
                loop {
                    match input.feed(pending) {
                        Ok(()) => break,
                        Err(MemoryFeedError::Full(bytes)) => {
                            pending = bytes;
                            thread::yield_now();
                        }
                        Err(MemoryFeedError::Closed(_)) => {
                            return Err("memory transport closed before replay finished".to_owned());
                        }
                        Err(MemoryFeedError::Empty) => {
                            return Err("replay generated an empty fragment".to_owned());
                        }
                    }
                }
            }
        }
        input.close();
        Ok(())
    });

    let mut stored = 0_usize;
    loop {
        match reader.events().recv_timeout(Duration::from_secs(5))? {
            ReaderEvent::Frame(frame) => {
                writer.submit_timeout(frame, Duration::from_secs(1))?;
                stored += 1;
            }
            ReaderEvent::Stopped { reason } => {
                if reason != StopReason::EndOfStream {
                    return Err(format!("reader stopped early: {reason:?}").into());
                }
                break;
            }
            ReaderEvent::Fault(fault) | ReaderEvent::WriteFailed { fault, .. } => {
                return Err(format!("reader fault: {fault:?}").into());
            }
            ReaderEvent::Started { .. } | ReaderEvent::BytesWritten { .. } => {}
        }
    }
    producer.join().map_err(|_| "replay producer panicked")??;
    let reader_exit = reader.join()?;
    let writer_exit = writer.shutdown()?;
    let elapsed = started.elapsed();

    if stored != frame_count || writer_exit.metrics.written_items != u64::from(frame_count_u32) {
        return Err(format!(
            "frame mismatch: requested={frame_count}, received={stored}, written={}",
            writer_exit.metrics.written_items
        )
        .into());
    }
    if writer_exit.reason != WriterStopReason::Requested {
        return Err(format!("storage stopped unexpectedly: {:?}", writer_exit.reason).into());
    }

    let seconds = elapsed.as_secs_f64();
    let frame_length = u32::try_from(LVDS_FRAME_LEN).expect("LVDS frame length fits u32");
    let wire_mib = f64::from(frame_count_u32) * f64::from(frame_length) / (1024.0 * 1024.0);
    println!("frames={frame_count} elapsed={seconds:.3}s");
    println!(
        "throughput={:.1} frames/s, {:.2} MiB/s wire",
        f64::from(frame_count_u32) / seconds,
        wire_mib / seconds
    );
    println!(
        "reader_queue_high_water={} storage_queue_high_water={}",
        reader_exit.metrics.max_event_queue_depth, writer_exit.metrics.max_queue_depth
    );
    println!(
        "dropped={} overflows={} rejected={} checksum_failures={} length_failures={}",
        reader_exit.metrics.events_dropped,
        reader_exit.metrics.queue_overflows,
        writer_exit.metrics.rejected_items,
        reader_exit.metrics.checksum_failures,
        reader_exit.metrics.length_failures
    );
    println!("capture_bytes={}", std::fs::metadata(path)?.len());
    Ok(())
}

fn argument(index: usize, default: usize) -> Result<usize, Box<dyn Error>> {
    match std::env::args().nth(index) {
        Some(value) => Ok(value.parse()?),
        None => Ok(default),
    }
}
