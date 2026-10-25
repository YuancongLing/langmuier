//! Raw recording observes received datagrams before any science admission.

use crate::bridge::RawRecordingStatus;
use langmuir_storage::{RawArchive, RawConfig, RawDestination, RawHeader, RawRecordReceipt};
use std::{net::SocketAddr, path::Path, time::Duration};

pub(super) struct RawRecording {
    archive: RawArchive,
}

impl RawRecording {
    pub(super) fn start(path: &Path, header: RawHeader) -> std::io::Result<Self> {
        RawArchive::start(RawConfig::new(path), header)
            .map(|archive| Self { archive })
            .map_err(std::io::Error::other)
    }

    pub(super) fn record(
        &mut self,
        timestamp: Duration,
        source: SocketAddr,
        destination: RawDestination,
        source_matches: bool,
        packet_index: Option<u64>,
        payload: &[u8],
    ) -> Option<RawRecordReceipt> {
        self.archive
            .try_record(
                timestamp,
                source,
                destination,
                source_matches,
                packet_index,
                payload,
            )
            .ok()
    }

    pub(super) fn status(&self) -> RawRecordingStatus {
        status(self.archive.path(), self.archive.status(), true)
    }

    fn fail(&self, error: impl Into<String>) {
        self.archive.accounting().mark_failed(error);
    }

    pub(super) fn stop(self) -> RawRecordingStatus {
        let path = self.archive.path().to_owned();
        let accounting = self.archive.accounting();
        let stopped = self.archive.stop();
        let mut snapshot = accounting.snapshot();
        if let Err(error) = stopped {
            snapshot.complete = false;
            snapshot.error = Some(error.to_string());
        }
        status(&path, snapshot, false)
    }
}

fn status(
    path: &Path,
    value: langmuir_storage::RawCaptureSnapshot,
    active: bool,
) -> RawRecordingStatus {
    RawRecordingStatus {
        active,
        path: Some(path.to_string_lossy().into_owned()),
        received: value.observed,
        enqueued: value.enqueued,
        written: value.written,
        dropped: value.dropped,
        closed: value.closed,
        complete: value.complete,
        error: value.error,
    }
}

/// Full PCAP-to-raw copy, independent of display playhead and science storage.
pub(super) struct PcapRawTask {
    cancel: crossbeam_channel::Sender<()>,
    state: std::sync::Arc<std::sync::Mutex<RawRecordingStatus>>,
    done: crossbeam_channel::Receiver<()>,
    join: Option<std::thread::JoinHandle<()>>,
}
impl PcapRawTask {
    // One background task owns the complete PCAP/raw/audit lifecycle.
    #[allow(clippy::too_many_lines)]
    pub(super) fn start(
        input: &Path,
        output: &Path,
        header: RawHeader,
        recovery: bool,
    ) -> std::io::Result<Self> {
        let expected_source = header.expected_source;
        let session_id = header.session_id;
        let mut recorder = RawRecording::start(output, header)?;
        let file = std::fs::File::open(input)?;
        let mut audit_path = output.as_os_str().to_owned();
        audit_path.push(".audit.jsonl");
        let mut audit = super::recovery_audit::RecoveryAudit::start(Path::new(&audit_path))?;
        let mut ingress = super::UdpLvdsIngress::new(expected_source);
        ingress.recovery_enabled = recovery;
        ingress.audit_context["source_file"] = serde_json::json!(input.to_string_lossy());
        ingress.audit_context["raw_archive"] = serde_json::json!(output.to_string_lossy());
        ingress.audit_context["raw_session_id"] = serde_json::json!(super::hex_bytes(&session_id));
        let (cancel, cancelled) = crossbeam_channel::bounded(1);
        let (finished, done) = crossbeam_channel::bounded(1);
        let state = std::sync::Arc::new(std::sync::Mutex::new(recorder.status()));
        let worker_state = state.clone();
        let join = std::thread::Builder::new()
            .name("lvds-pcap-raw-export".to_owned())
            .spawn(move || {
                let mut reader =
                    langmuir_transport::PcapNgReader::new(std::io::BufReader::new(file));
                let mut packet_index = 0_u64;
                let mut error = None;
                let clock = std::time::Instant::now();
                let mut epoch = None;
                while cancelled.is_empty() {
                    // File export can apply backpressure before observing another
                    // record. Live socket recording always remains nonblocking.
                    while recorder
                        .status()
                        .enqueued
                        .saturating_sub(recorder.status().written)
                        >= 96
                        && cancelled.is_empty()
                    {
                        if let Some(value) = recorder.status().error {
                            error = Some(value);
                            break;
                        }
                        if cancelled.recv_timeout(Duration::from_millis(10)).is_ok() {
                            error = Some("PCAP raw export cancelled before EOF".to_owned());
                            break;
                        }
                    }
                    if error.is_some() || !cancelled.is_empty() {
                        break;
                    }
                    match reader.next_packet() {
                        Ok(Some(packet)) => {
                            if let Some(udp) = packet.udp {
                                let receipt = recorder.record(
                                    packet.timestamp,
                                    udp.source,
                                    RawDestination::Captured(udp.destination),
                                    udp.source == expected_source,
                                    Some(packet_index),
                                    &udp.payload,
                                );
                                if let Some(receipt) = receipt {
                                    let first = *epoch.get_or_insert(packet.timestamp);
                                    if let Some(at) =
                                        clock.checked_add(packet.timestamp.saturating_sub(first))
                                    {
                                        ingress.push_observed_at(
                                            udp.source,
                                            RawDestination::Captured(udp.destination),
                                            &udp.payload,
                                            at,
                                            receipt.id,
                                            |_| {},
                                        );
                                        for mut row in ingress.audit_rows.drain(..) {
                                            row["origin_packet_index"] =
                                                serde_json::json!(packet_index);
                                            if !audit.record_wait(row, &cancelled) {
                                                error = Some(
                                                    "PCAP provenance audit cancelled or failed"
                                                        .to_owned(),
                                                );
                                                break;
                                            }
                                        }
                                        ingress.observations.clear();
                                    } else {
                                        error = Some("PCAP timestamp overflow".to_owned());
                                    }
                                } else {
                                    error = Some(
                                        "PCAP raw writer failed to observe a UDP record".to_owned(),
                                    );
                                }
                            }
                            packet_index = packet_index.saturating_add(1);
                            *worker_state
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner) =
                                recorder.status();
                        }
                        Ok(None) => break,
                        Err(value) => {
                            error = Some(value.to_string());
                            break;
                        }
                    }
                }
                if !cancelled.is_empty() {
                    error = Some("PCAP raw export cancelled before EOF".to_owned());
                }
                if let Some(value) = &error {
                    recorder.fail(value.clone());
                    audit.mark_failed(value.clone());
                }
                let audit_status = audit.stop();
                if let Some(value) = audit_status.error {
                    recorder.fail(format!("PCAP audit: {value}"));
                }
                let mut final_status = recorder.stop();
                if let Some(error) = error {
                    final_status.complete = false;
                    final_status.error = Some(error);
                }
                *worker_state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = final_status;
                let _ = finished.try_send(());
            })?;
        Ok(Self {
            cancel,
            state,
            done,
            join: Some(join),
        })
    }
    pub(super) fn status(&self) -> RawRecordingStatus {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
    pub(super) fn stop(mut self) -> RawRecordingStatus {
        let _ = self.cancel.try_send(());
        if self.done.recv_timeout(Duration::from_secs(3)).is_ok()
            && let Some(join) = self.join.take()
        {
            let _ = join.join();
        }
        let mut state = self.status();
        if self.join.is_some() {
            state.active = false;
            state.complete = false;
            state.error = Some("PCAP raw export close timed out".to_owned());
        }
        state
    }
}
impl Drop for PcapRawTask {
    fn drop(&mut self) {
        let _ = self.cancel.try_send(());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use langmuir_storage::{RawDestination, RawHeader, RawOrigin, RawReader};
    use std::{net::SocketAddr, time::Duration};

    #[test]
    fn malformed_pcap_marks_both_raw_and_audit_incomplete() {
        let directory = tempfile::tempdir().unwrap();
        let input = directory.path().join("SYNTHETIC-broken.pcapng");
        let output = directory.path().join("SYNTHETIC-broken.lvdsraw");
        std::fs::write(&input, [1, 2, 3]).unwrap();
        let task = PcapRawTask::start(
            &input,
            &output,
            RawHeader {
                origin: RawOrigin::PcapReplay,
                created_at: Duration::ZERO,
                expected_source: "192.168.1.128:1534".parse().unwrap(),
                binding: "0.0.0.0:1534".parse().unwrap(),
                session_id: [3; 16],
            },
            false,
        )
        .unwrap();
        task.done.recv_timeout(Duration::from_secs(5)).unwrap();
        let status = task.status();
        assert!(!status.complete);
        assert!(status.error.is_some());
        let text = std::fs::read_to_string(output.with_extension("lvdsraw.audit.jsonl")).unwrap();
        let footer: serde_json::Value = serde_json::from_str(text.lines().last().unwrap()).unwrap();
        assert_eq!(
            footer["complete"], false,
            "a truncated PCAP cannot have a complete audit"
        );
    }

    #[test]
    fn raw_recording_keeps_invalid_duplicate_short_and_non_target_payloads() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("SYNTHETIC-all-input.lvdsraw");
        let source: SocketAddr = "127.0.0.1:1234".parse().unwrap();
        let expected: SocketAddr = "192.168.1.128:1534".parse().unwrap();
        let binding = "127.0.0.1:1534".parse().unwrap();
        let header = RawHeader {
            origin: RawOrigin::LiveUdp,
            created_at: Duration::ZERO,
            expected_source: expected,
            binding,
            session_id: [1; 16],
        };
        let mut recorder = RawRecording::start(&path, header).unwrap();
        let inputs = [
            vec![0_u8; 24],
            vec![0_u8; 268],
            vec![0_u8; 268],
            vec![0xFF; 293],
        ];
        for (index, payload) in inputs.iter().enumerate() {
            recorder.record(
                Duration::from_millis(u64::try_from(index).unwrap()),
                source,
                RawDestination::SocketBinding(binding),
                false,
                None,
                payload,
            );
        }
        let status = recorder.stop();
        assert!(status.complete && status.closed);
        assert_eq!((status.received, status.written, status.dropped), (4, 4, 0));
        let mut reader = RawReader::open(&path).unwrap();
        for (index, bytes) in inputs.iter().enumerate() {
            let record = reader.next_record().unwrap().unwrap();
            assert_eq!(record.id, u64::try_from(index).unwrap() + 1);
            assert_eq!(record.payload, *bytes);
            assert_eq!(record.source, source);
            assert!(!record.source_matches);
        }
        assert!(reader.next_record().unwrap().is_none());
    }
}
