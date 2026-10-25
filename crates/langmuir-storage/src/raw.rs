//! Versioned, bounded archival of application-observed UDP payloads.

use std::fs::{File, OpenOptions};
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV6};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use thiserror::Error;

use crate::{BatchSink, BatchWriter, StorageError, StorageServiceConfig, SubmitError};

pub const RAW_MAGIC: [u8; 8] = *b"LVDSRAW\0";
pub const RAW_VERSION: u16 = 1;
pub const RAW_HEADER_LEN: usize = 128;
pub const RAW_MAX_PAYLOAD: usize = 65_535;
const DATA_HEADER_LEN: usize = 96;
const FOOTER_LEN: usize = 92;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RawOrigin {
    LiveUdp,
    PcapReplay,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RawDestination {
    Captured(SocketAddr),
    SocketBinding(SocketAddr),
    Unknown,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RawHeader {
    pub origin: RawOrigin,
    pub created_at: Duration,
    pub expected_source: SocketAddr,
    pub binding: SocketAddr,
    pub session_id: [u8; 16],
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RawRecord {
    pub id: u64,
    pub timestamp: Duration,
    pub source: SocketAddr,
    pub destination: RawDestination,
    pub source_matches: bool,
    pub origin_packet_index: Option<u64>,
    pub payload: Vec<u8>,
}

#[derive(Clone, Debug)]
pub struct RawConfig {
    pub path: PathBuf,
    pub buffer_capacity: usize,
    pub service: StorageServiceConfig,
}

impl RawConfig {
    #[must_use]
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            buffer_capacity: 64 * 1024,
            service: StorageServiceConfig {
                queue_capacity: 128,
                batch_size: 32,
                flush_interval: Duration::from_millis(250),
                control_timeout: Duration::from_secs(3),
                ..StorageServiceConfig::default()
            },
        }
    }
}

#[derive(Debug, Error)]
pub enum RawError {
    #[error("invalid raw archive: {0}")]
    Invalid(String),
    #[error("unsupported raw archive version {0}")]
    UnsupportedVersion(u16),
    #[error("raw archive I/O failed: {0}")]
    Io(#[from] io::Error),
    #[error("raw archive is incomplete: missing END footer")]
    Incomplete,
    #[error("raw archive truncated at byte {offset} while reading {context}")]
    Truncated { offset: u64, context: &'static str },
    #[error("raw archive worker failed: {0}")]
    Storage(#[from] StorageError),
    #[error("raw archive is closed")]
    Closed,
    #[error("raw archive counter overflow")]
    CounterOverflow,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RawCaptureSnapshot {
    pub observed: u64,
    pub enqueued: u64,
    pub written: u64,
    pub dropped: u64,
    pub payload_bytes: u64,
    pub last_observed_id: u64,
    pub last_written_id: u64,
    pub closed: bool,
    pub complete: bool,
    pub error: Option<String>,
}

#[derive(Default)]
struct AccountingState {
    snapshot: RawCaptureSnapshot,
    finish_requested: bool,
}

/// Reliable state independent of the worker's bounded event channel.
#[derive(Clone, Default)]
pub struct RawCaptureAccounting(Arc<Mutex<AccountingState>>);

impl RawCaptureAccounting {
    #[must_use]
    pub fn snapshot(&self) -> RawCaptureSnapshot {
        self.0.lock().snapshot.clone()
    }

    /// Allocates an observation ID before the queue submission is attempted.
    ///
    /// # Errors
    /// Returns counter overflow; the ID is never wrapped or reused.
    pub fn observe(&self) -> Result<u64, RawError> {
        let mut state = self.0.lock();
        let id = state
            .snapshot
            .observed
            .checked_add(1)
            .ok_or(RawError::CounterOverflow)?;
        state.snapshot.observed = id;
        state.snapshot.last_observed_id = id;
        Ok(id)
    }

    pub fn record_enqueued(&self) {
        self.0.lock().snapshot.enqueued += 1;
    }

    pub fn record_dropped(&self, reason: impl Into<String>) {
        let mut state = self.0.lock();
        state.snapshot.dropped += 1;
        state.snapshot.complete = false;
        state.snapshot.error.get_or_insert_with(|| reason.into());
    }

    pub fn request_finish(&self) {
        self.0.lock().finish_requested = true;
    }

    pub fn mark_failed(&self, error: impl Into<String>) {
        let mut state = self.0.lock();
        state.snapshot.complete = false;
        state.snapshot.error.get_or_insert_with(|| error.into());
    }

    fn abandon(&self) {
        let mut state = self.0.lock();
        state.finish_requested = false;
        state.snapshot.complete = false;
        state
            .snapshot
            .error
            .get_or_insert_with(|| "raw recording was abandoned".to_owned());
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RawRecordReceipt {
    pub id: u64,
    pub queued: bool,
}

/// A raw recorder with its own bounded writer and independent lifecycle.
pub struct RawArchive {
    path: PathBuf,
    writer: Option<BatchWriter<RawRecord>>,
    accounting: RawCaptureAccounting,
}

impl RawArchive {
    /// Exclusively creates a raw file and starts its bounded background worker.
    ///
    /// # Errors
    /// Returns configuration, creation, header write or worker creation errors.
    pub fn start(config: RawConfig, header: RawHeader) -> Result<Self, RawError> {
        let accounting = RawCaptureAccounting::default();
        let sink = RawSink::create(&config, header, accounting.clone())?;
        let writer = BatchWriter::start(sink, config.service)?;
        Ok(Self {
            path: config.path,
            writer: Some(writer),
            accounting,
        })
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    #[must_use]
    pub fn accounting(&self) -> RawCaptureAccounting {
        self.accounting.clone()
    }

    #[must_use]
    pub fn status(&self) -> RawCaptureSnapshot {
        if let Some(writer) = &self.writer {
            let status = writer.status();
            if let Some(error) = status.last_error {
                self.accounting.mark_failed(error.to_string());
            } else if status.panicked {
                self.accounting.mark_failed("raw writer panicked");
            }
        }
        self.accounting.snapshot()
    }

    /// Copies one observed payload without waiting for storage queue capacity.
    /// A full queue returns a receipt with `queued == false`, records a loss,
    /// and permanently prevents a complete recording state.
    ///
    /// # Errors
    /// Returns invalid metadata, size/counter overflow or a closed worker.
    pub fn try_record(
        &mut self,
        timestamp: Duration,
        source: SocketAddr,
        destination: RawDestination,
        source_matches: bool,
        origin_packet_index: Option<u64>,
        payload: &[u8],
    ) -> Result<RawRecordReceipt, RawError> {
        let writer = self.writer.as_ref().ok_or(RawError::Closed)?;
        let id = self.accounting.observe()?;
        if payload.len() > RAW_MAX_PAYLOAD {
            self.accounting
                .record_dropped("raw UDP payload exceeds 65535 bytes");
            return Err(RawError::Invalid("payload exceeds 65535 bytes".to_owned()));
        }
        let record = RawRecord {
            id,
            timestamp,
            source,
            destination,
            source_matches,
            origin_packet_index,
            payload: payload.to_vec(),
        };
        match writer.submit(record) {
            Ok(()) => {
                self.accounting.record_enqueued();
                Ok(RawRecordReceipt { id, queued: true })
            }
            Err(SubmitError::QueueFull(_)) => {
                self.accounting
                    .record_dropped("raw recording queue overflow");
                Ok(RawRecordReceipt { id, queued: false })
            }
            Err(SubmitError::Closed(_) | SubmitError::Timeout { .. }) => {
                self.accounting
                    .record_dropped("raw recording writer is closed");
                Err(RawError::Closed)
            }
        }
    }

    /// Stops acceptance, drains accepted records, and explicitly closes.
    /// Keep an `accounting()` clone to inspect final state after an error.
    ///
    /// # Errors
    /// Returns write/flush/close failures, worker panic or bounded stop timeout.
    pub fn stop(mut self) -> Result<RawCaptureSnapshot, RawError> {
        self.accounting.request_finish();
        let result = self.writer.take().ok_or(RawError::Closed)?.shutdown();
        if let Err(error) = result {
            self.accounting.mark_failed(error.to_string());
            return Err(error.into());
        }
        Ok(self.accounting.snapshot())
    }
}

impl Drop for RawArchive {
    fn drop(&mut self) {
        if self.writer.is_some() {
            self.accounting.abandon();
        }
    }
}

/// Streaming raw encoder; only complete flushed batches advance counters.
pub struct RawSink<W: Write = File> {
    writer: Option<BufWriter<W>>,
    header: RawHeader,
    accounting: RawCaptureAccounting,
    records: u64,
    payload_bytes: u64,
    last_id: u64,
}

impl RawSink<File> {
    /// Creates and flushes the header without replacing an existing path.
    ///
    /// # Errors
    /// Returns configuration, exclusive creation or initialization I/O errors.
    pub fn create(
        config: &RawConfig,
        header: RawHeader,
        accounting: RawCaptureAccounting,
    ) -> Result<Self, RawError> {
        if config.path.as_os_str().is_empty() || config.buffer_capacity == 0 {
            return Err(RawError::Invalid(
                "path and buffer capacity must be nonzero".to_owned(),
            ));
        }
        let _ = encode_header(&header)?;
        if let Some(parent) = config
            .path
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
        {
            std::fs::create_dir_all(parent)?;
        }
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&config.path)?;
        Self::from_writer(file, config.buffer_capacity, header, accounting)
    }
}

impl<W: Write> RawSink<W> {
    fn from_writer(
        writer: W,
        capacity: usize,
        header: RawHeader,
        accounting: RawCaptureAccounting,
    ) -> Result<Self, RawError> {
        if capacity == 0 {
            return Err(RawError::Invalid("buffer capacity is zero".to_owned()));
        }
        let encoded = encode_header(&header)?;
        let mut output = BufWriter::with_capacity(capacity, writer);
        if let Err(error) = output.write_all(&encoded).and_then(|()| output.flush()) {
            accounting.mark_failed(error.to_string());
            drop(output.into_parts());
            return Err(error.into());
        }
        Ok(Self {
            writer: Some(output),
            header,
            accounting,
            records: 0,
            payload_bytes: 0,
            last_id: 0,
        })
    }

    fn fail(&mut self, error: &RawError) {
        self.accounting.mark_failed(error.to_string());
        if let Some(writer) = self.writer.take() {
            drop(writer.into_parts());
        }
    }
}

impl<W: Write + Send + 'static> BatchSink<RawRecord> for RawSink<W> {
    type Error = RawError;

    fn write_batch(&mut self, records: &[RawRecord]) -> Result<(), RawError> {
        if self.writer.is_none() {
            return Err(RawError::Closed);
        }
        let result = (|| {
            let mut last_id = self.last_id;
            let mut payload_bytes = self.payload_bytes;
            let count = u64::try_from(records.len()).map_err(|_| RawError::CounterOverflow)?;
            let total = self
                .records
                .checked_add(count)
                .ok_or(RawError::CounterOverflow)?;
            for record in records {
                validate_record(record, &self.header, last_id)?;
                last_id = record.id;
                payload_bytes = payload_bytes
                    .checked_add(
                        u64::try_from(record.payload.len())
                            .map_err(|_| RawError::CounterOverflow)?,
                    )
                    .ok_or(RawError::CounterOverflow)?;
            }
            let writer = self.writer.as_mut().ok_or(RawError::Closed)?;
            for record in records {
                let encoded = encode_record_header(record)?;
                writer.write_all(&encoded)?;
                writer.write_all(&record.payload)?;
                writer.write_all(&encoded[4..8])?;
            }
            writer.flush()?;
            self.records = total;
            self.payload_bytes = payload_bytes;
            self.last_id = last_id;
            let mut state = self.accounting.0.lock();
            state.snapshot.written = total;
            state.snapshot.payload_bytes = payload_bytes;
            state.snapshot.last_written_id = last_id;
            Ok(())
        })();
        if let Err(error) = &result {
            self.fail(error);
        }
        result
    }

    fn flush(&mut self) -> Result<(), RawError> {
        let result = self
            .writer
            .as_mut()
            .ok_or(RawError::Closed)?
            .flush()
            .map_err(RawError::Io);
        if let Err(error) = &result {
            self.fail(error);
        }
        result
    }

    fn close(&mut self) -> Result<(), RawError> {
        let result = (|| {
            let mut snapshot = self.accounting.snapshot();
            let explicit = self.accounting.0.lock().finish_requested;
            snapshot.complete = explicit && snapshot.dropped == 0 && snapshot.error.is_none();
            validate_summary(
                &snapshot,
                self.records,
                self.payload_bytes,
                self.last_id,
                explicit,
            )?;
            let encoded = encode_footer(&snapshot, explicit);
            let writer = self.writer.as_mut().ok_or(RawError::Closed)?;
            writer.write_all(&encoded)?;
            writer.flush()?;
            let mut state = self.accounting.0.lock();
            state.snapshot.closed = true;
            // A stop deadline can fail while the final flush is in progress.
            // Its reliable error state must survive a later worker completion.
            state.snapshot.complete =
                snapshot.complete && state.finish_requested && state.snapshot.error.is_none();
            Ok(())
        })();
        if let Err(error) = &result {
            self.fail(error);
        }
        if let Some(writer) = self.writer.take() {
            drop(writer.into_parts());
        }
        result
    }
}

impl<W: Write> Drop for RawSink<W> {
    fn drop(&mut self) {
        if let Some(writer) = self.writer.take() {
            drop(writer.into_parts());
        }
    }
}

/// Strict streaming reader. Normal EOF requires a validated END block.
pub struct RawReader<R: Read> {
    reader: R,
    header: RawHeader,
    offset: u64,
    records: u64,
    payload_bytes: u64,
    last_id: u64,
    summary: Option<RawCaptureSnapshot>,
}

impl RawReader<BufReader<File>> {
    /// Opens and strictly checks a version-one raw archive.
    ///
    /// # Errors
    /// Returns open, truncated-header or invalid-header errors.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, RawError> {
        Self::new(BufReader::new(File::open(path)?))
    }
}

impl<R: Read> RawReader<R> {
    /// Reads a fixed header without allocating from untrusted file lengths.
    ///
    /// # Errors
    /// Returns I/O, truncation, unsupported-version or invalid-header errors.
    pub fn new(mut reader: R) -> Result<Self, RawError> {
        let mut bytes = [0; RAW_HEADER_LEN];
        let mut offset = 0;
        read_exact(&mut reader, &mut bytes, &mut offset, "file header")?;
        let header = decode_header(&bytes)?;
        Ok(Self {
            reader,
            header,
            offset,
            records: 0,
            payload_bytes: 0,
            last_id: 0,
            summary: None,
        })
    }

    #[must_use]
    pub const fn header(&self) -> &RawHeader {
        &self.header
    }

    #[must_use]
    pub const fn summary(&self) -> Option<&RawCaptureSnapshot> {
        self.summary.as_ref()
    }

    /// Returns one original observation, or None after the sole valid footer.
    /// Earlier valid records remain available before a later truncation error.
    ///
    /// # Errors
    /// Returns missing-footer, truncated block, malformed metadata or I/O errors.
    pub fn next_record(&mut self) -> Result<Option<RawRecord>, RawError> {
        if self.summary.is_some() {
            return Ok(None);
        }
        let mut prefix = [0; 8];
        let Some(first) = read_byte(&mut self.reader, &mut self.offset)? else {
            return Err(RawError::Incomplete);
        };
        prefix[0] = first;
        read_exact(
            &mut self.reader,
            &mut prefix[1..],
            &mut self.offset,
            "block prefix",
        )?;
        match le16(&prefix, 0) {
            1 => self.read_data(prefix).map(Some),
            2 => {
                self.read_footer(prefix)?;
                Ok(None)
            }
            _ => Err(invalid("unknown block kind")),
        }
    }

    fn read_data(&mut self, prefix: [u8; 8]) -> Result<RawRecord, RawError> {
        let total = le32(&prefix, 4);
        if le16(&prefix, 2) != 96 || !(100..=65_635).contains(&total) {
            return Err(invalid("DATA header or block length"));
        }
        let mut bytes = [0; DATA_HEADER_LEN];
        bytes[..8].copy_from_slice(&prefix);
        read_exact(
            &mut self.reader,
            &mut bytes[8..],
            &mut self.offset,
            "DATA header",
        )?;
        let length = usize::try_from(le32(&bytes, 80)).map_err(|_| invalid("payload length"))?;
        if length > RAW_MAX_PAYLOAD || u32::try_from(length + 100).ok() != Some(total) {
            return Err(invalid("DATA payload length"));
        }
        let flags = le32(&bytes, 28);
        let kind = (flags >> 2) & 3;
        if flags & !15 != 0 || kind == 3 || le32(&bytes, 84) != 0 {
            return Err(invalid("DATA flags or reserved field"));
        }
        let destination = match kind {
            0 => {
                if bytes[56..80] != [0; 24] {
                    return Err(invalid("unknown destination is not zero"));
                }
                RawDestination::Unknown
            }
            1 => RawDestination::Captured(decode_endpoint(&bytes[56..80])?),
            _ => RawDestination::SocketBinding(decode_endpoint(&bytes[56..80])?),
        };
        let origin_packet_index = if flags & 2 != 0 {
            Some(le64(&bytes, 88))
        } else {
            if le64(&bytes, 88) != 0 {
                return Err(invalid("absent packet index is not zero"));
            }
            None
        };
        let mut record = RawRecord {
            id: le64(&bytes, 8),
            timestamp: decode_time(&bytes, 16, 24)?,
            source: decode_endpoint(&bytes[32..56])?,
            destination,
            source_matches: flags & 1 != 0,
            origin_packet_index,
            payload: Vec::new(),
        };
        validate_record(&record, &self.header, self.last_id)?;
        record.payload.resize(length, 0);
        read_exact(
            &mut self.reader,
            &mut record.payload,
            &mut self.offset,
            "UDP payload",
        )?;
        let mut trailer = [0; 4];
        read_exact(
            &mut self.reader,
            &mut trailer,
            &mut self.offset,
            "DATA repeated length",
        )?;
        if le32(&trailer, 0) != total {
            return Err(invalid("DATA repeated length"));
        }
        self.records = self
            .records
            .checked_add(1)
            .ok_or(RawError::CounterOverflow)?;
        self.payload_bytes = self
            .payload_bytes
            .checked_add(u64::try_from(length).map_err(|_| RawError::CounterOverflow)?)
            .ok_or(RawError::CounterOverflow)?;
        self.last_id = record.id;
        Ok(record)
    }

    fn read_footer(&mut self, prefix: [u8; 8]) -> Result<(), RawError> {
        if le16(&prefix, 2) != 88 || le32(&prefix, 4) != 92 {
            return Err(invalid("END length"));
        }
        let mut bytes = [0; FOOTER_LEN];
        bytes[..8].copy_from_slice(&prefix);
        read_exact(
            &mut self.reader,
            &mut bytes[8..],
            &mut self.offset,
            "END footer",
        )?;
        if bytes[72..88] != [0; 16] || le32(&bytes, 88) != 92 {
            return Err(invalid("END reserved or repeated length"));
        }
        let complete = match le32(&bytes, 56) {
            1 => true,
            2 => false,
            _ => return Err(invalid("END state")),
        };
        let explicit = match le32(&bytes, 60) {
            1 => true,
            2 => false,
            _ => return Err(invalid("END stop reason")),
        };
        let snapshot = RawCaptureSnapshot {
            observed: le64(&bytes, 8),
            enqueued: le64(&bytes, 16),
            written: le64(&bytes, 24),
            dropped: le64(&bytes, 32),
            payload_bytes: le64(&bytes, 40),
            last_observed_id: le64(&bytes, 48),
            last_written_id: le64(&bytes, 64),
            closed: true,
            complete,
            error: (!complete).then(|| "raw archive is marked incomplete".to_owned()),
        };
        validate_summary(
            &snapshot,
            self.records,
            self.payload_bytes,
            self.last_id,
            explicit,
        )?;
        if read_byte(&mut self.reader, &mut self.offset)?.is_some() {
            return Err(invalid("bytes after END footer"));
        }
        self.summary = Some(snapshot);
        Ok(())
    }
}

fn invalid(message: &str) -> RawError {
    RawError::Invalid(message.to_owned())
}

fn validate_record(record: &RawRecord, header: &RawHeader, last_id: u64) -> Result<(), RawError> {
    if record.id == 0 || record.id <= last_id {
        return Err(invalid("record IDs are not strictly increasing"));
    }
    if record.payload.len() > RAW_MAX_PAYLOAD {
        return Err(invalid("payload exceeds 65535 bytes"));
    }
    if record.source_matches != (record.source == header.expected_source) {
        return Err(invalid(
            "source match flag disagrees with configured source",
        ));
    }
    let _ = encode_endpoint(record.source)?;
    if let RawDestination::Captured(endpoint) | RawDestination::SocketBinding(endpoint) =
        record.destination
    {
        let _ = encode_endpoint(endpoint)?;
    }
    Ok(())
}

fn validate_summary(
    snapshot: &RawCaptureSnapshot,
    records: u64,
    payload_bytes: u64,
    last_id: u64,
    explicit: bool,
) -> Result<(), RawError> {
    if snapshot.enqueued != records
        || snapshot.written != records
        || snapshot.payload_bytes != payload_bytes
        || snapshot.last_written_id != last_id
        || snapshot.last_observed_id != snapshot.observed
        || snapshot.enqueued.checked_add(snapshot.dropped) != Some(snapshot.observed)
        || last_id > snapshot.last_observed_id
        || (records == 0) != (last_id == 0)
        || (snapshot.complete && (!explicit || snapshot.dropped != 0))
    {
        return Err(invalid("END accounting disagrees with observed records"));
    }
    Ok(())
}

fn encode_header(header: &RawHeader) -> Result<[u8; RAW_HEADER_LEN], RawError> {
    let mut bytes = [0; RAW_HEADER_LEN];
    bytes[..8].copy_from_slice(&RAW_MAGIC);
    bytes[8..10].copy_from_slice(&RAW_VERSION.to_le_bytes());
    bytes[10..12].copy_from_slice(&128_u16.to_le_bytes());
    bytes[12..16].copy_from_slice(&0x0102_0304_u32.to_le_bytes());
    bytes[16..20].copy_from_slice(
        &(match header.origin {
            RawOrigin::LiveUdp => 1_u32,
            RawOrigin::PcapReplay => 2,
        })
        .to_le_bytes(),
    );
    bytes[20..24].copy_from_slice(&65_535_u32.to_le_bytes());
    bytes[24..32].copy_from_slice(&header.created_at.as_secs().to_le_bytes());
    bytes[32..36].copy_from_slice(&header.created_at.subsec_nanos().to_le_bytes());
    bytes[40..56].copy_from_slice(&header.session_id);
    bytes[56..80].copy_from_slice(&encode_endpoint(header.expected_source)?);
    bytes[80..104].copy_from_slice(&encode_endpoint(header.binding)?);
    Ok(bytes)
}

fn decode_header(bytes: &[u8; RAW_HEADER_LEN]) -> Result<RawHeader, RawError> {
    if bytes[..8] != RAW_MAGIC {
        return Err(invalid("file magic"));
    }
    if le16(bytes, 8) != RAW_VERSION {
        return Err(RawError::UnsupportedVersion(le16(bytes, 8)));
    }
    if le16(bytes, 10) != 128
        || le32(bytes, 12) != 0x0102_0304
        || le32(bytes, 20) != 65_535
        || le32(bytes, 36) != 0
        || bytes[104..128] != [0; 24]
    {
        return Err(invalid("file header metadata"));
    }
    let origin = match le32(bytes, 16) {
        1 => RawOrigin::LiveUdp,
        2 => RawOrigin::PcapReplay,
        _ => return Err(invalid("origin")),
    };
    let mut session_id = [0; 16];
    session_id.copy_from_slice(&bytes[40..56]);
    Ok(RawHeader {
        origin,
        created_at: decode_time(bytes, 24, 32)?,
        expected_source: decode_endpoint(&bytes[56..80])?,
        binding: decode_endpoint(&bytes[80..104])?,
        session_id,
    })
}

fn encode_record_header(record: &RawRecord) -> Result<[u8; DATA_HEADER_LEN], RawError> {
    let mut bytes = [0; DATA_HEADER_LEN];
    bytes[..2].copy_from_slice(&1_u16.to_le_bytes());
    bytes[2..4].copy_from_slice(&96_u16.to_le_bytes());
    let total = u32::try_from(record.payload.len() + 100).map_err(|_| RawError::CounterOverflow)?;
    bytes[4..8].copy_from_slice(&total.to_le_bytes());
    bytes[8..16].copy_from_slice(&record.id.to_le_bytes());
    bytes[16..24].copy_from_slice(&record.timestamp.as_secs().to_le_bytes());
    bytes[24..28].copy_from_slice(&record.timestamp.subsec_nanos().to_le_bytes());
    let kind = match record.destination {
        RawDestination::Unknown => 0,
        RawDestination::Captured(_) => 1,
        RawDestination::SocketBinding(_) => 2,
    };
    let flags = u32::from(record.source_matches)
        | (u32::from(record.origin_packet_index.is_some()) << 1)
        | (kind << 2);
    bytes[28..32].copy_from_slice(&flags.to_le_bytes());
    bytes[32..56].copy_from_slice(&encode_endpoint(record.source)?);
    if let RawDestination::Captured(endpoint) | RawDestination::SocketBinding(endpoint) =
        record.destination
    {
        bytes[56..80].copy_from_slice(&encode_endpoint(endpoint)?);
    }
    bytes[80..84].copy_from_slice(
        &u32::try_from(record.payload.len())
            .map_err(|_| RawError::CounterOverflow)?
            .to_le_bytes(),
    );
    bytes[88..96].copy_from_slice(&record.origin_packet_index.unwrap_or(0).to_le_bytes());
    Ok(bytes)
}

fn encode_footer(snapshot: &RawCaptureSnapshot, explicit: bool) -> [u8; FOOTER_LEN] {
    let mut bytes = [0; FOOTER_LEN];
    bytes[..2].copy_from_slice(&2_u16.to_le_bytes());
    bytes[2..4].copy_from_slice(&88_u16.to_le_bytes());
    bytes[4..8].copy_from_slice(&92_u32.to_le_bytes());
    for (offset, value) in [
        (8, snapshot.observed),
        (16, snapshot.enqueued),
        (24, snapshot.written),
        (32, snapshot.dropped),
        (40, snapshot.payload_bytes),
        (48, snapshot.last_observed_id),
        (64, snapshot.last_written_id),
    ] {
        bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
    }
    bytes[56..60].copy_from_slice(&(if snapshot.complete { 1_u32 } else { 2 }).to_le_bytes());
    bytes[60..64].copy_from_slice(&(if explicit { 1_u32 } else { 2 }).to_le_bytes());
    bytes[88..92].copy_from_slice(&92_u32.to_le_bytes());
    bytes
}

fn encode_endpoint(endpoint: SocketAddr) -> Result<[u8; 24], RawError> {
    let mut bytes = [0; 24];
    bytes[2..4].copy_from_slice(&endpoint.port().to_le_bytes());
    match endpoint {
        SocketAddr::V4(address) => {
            bytes[0] = 4;
            bytes[4..8].copy_from_slice(&address.ip().octets());
        }
        SocketAddr::V6(address) => {
            if address.flowinfo() != 0 {
                return Err(invalid("nonzero IPv6 flowinfo is unsupported"));
            }
            bytes[0] = 6;
            bytes[4..20].copy_from_slice(&address.ip().octets());
            bytes[20..24].copy_from_slice(&address.scope_id().to_le_bytes());
        }
    }
    Ok(bytes)
}

fn decode_endpoint(bytes: &[u8]) -> Result<SocketAddr, RawError> {
    if bytes[1] != 0 {
        return Err(invalid("endpoint reserved byte"));
    }
    let port = le16(bytes, 2);
    match bytes[0] {
        4 => {
            if bytes[8..24] != [0; 16] {
                return Err(invalid("IPv4 endpoint padding"));
            }
            Ok(SocketAddr::new(
                IpAddr::V4(Ipv4Addr::new(bytes[4], bytes[5], bytes[6], bytes[7])),
                port,
            ))
        }
        6 => {
            let mut octets = [0; 16];
            octets.copy_from_slice(&bytes[4..20]);
            Ok(SocketAddr::V6(SocketAddrV6::new(
                Ipv6Addr::from(octets),
                port,
                0,
                le32(bytes, 20),
            )))
        }
        _ => Err(invalid("endpoint address family")),
    }
}

fn decode_time(bytes: &[u8], seconds: usize, nanos: usize) -> Result<Duration, RawError> {
    let nanos = le32(bytes, nanos);
    if nanos >= 1_000_000_000 {
        return Err(invalid("timestamp nanoseconds"));
    }
    Ok(Duration::new(le64(bytes, seconds), nanos))
}

fn read_exact<R: Read>(
    reader: &mut R,
    output: &mut [u8],
    offset: &mut u64,
    context: &'static str,
) -> Result<(), RawError> {
    let mut done = 0;
    while done < output.len() {
        match reader.read(&mut output[done..]) {
            Ok(0) => {
                return Err(RawError::Truncated {
                    offset: *offset,
                    context,
                });
            }
            Ok(count) => {
                done += count;
                *offset = offset
                    .checked_add(u64::try_from(count).map_err(|_| RawError::CounterOverflow)?)
                    .ok_or(RawError::CounterOverflow)?;
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn read_byte<R: Read>(reader: &mut R, offset: &mut u64) -> Result<Option<u8>, RawError> {
    let mut byte = [0];
    loop {
        match reader.read(&mut byte) {
            Ok(0) => return Ok(None),
            Ok(_) => {
                *offset = offset.checked_add(1).ok_or(RawError::CounterOverflow)?;
                return Ok(Some(byte[0]));
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error.into()),
        }
    }
}

fn le16(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([bytes[offset], bytes[offset + 1]])
}
fn le32(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
    ])
}
fn le64(bytes: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes([
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
        bytes[offset + 4],
        bytes[offset + 5],
        bytes[offset + 6],
        bytes[offset + 7],
    ])
}

#[cfg(test)]
#[path = "raw_tests.rs"]
mod tests;
