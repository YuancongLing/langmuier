use std::io::{self, Read, Write};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crossbeam_channel::{Receiver, Sender, bounded};
use langmuir_core::{Command, GNSS_PRESETS, Rs422StreamDecoder};
use langmuir_transport::{ReaderConfig, ReaderWorker, Transport, TransportError};

use super::{Controller, star_simulator::StarComputerSimulator};
use crate::bridge::{BackendStateSnapshot, LinkKind, UiRequest};
use crate::star_simulator::StarSimulatorConfig;

fn config() -> StarSimulatorConfig {
    StarSimulatorConfig {
        enabled: [true; 3],
        ..Default::default()
    }
}

#[test]
fn scheduler_defaults_off_and_skips_missed_deadlines_without_catchup() {
    let now = Instant::now();
    let mut simulator = StarComputerSimulator::default();
    assert_eq!(simulator.take_due(now), Vec::new());
    simulator.start(config(), now).unwrap();
    assert_eq!(simulator.take_due(now).len(), 3);
    assert_eq!(simulator.take_due(now), Vec::new());
    assert_eq!(
        simulator.take_due(now + Duration::from_millis(3_500)).len(),
        3
    );
    assert_eq!(simulator.status.skipped, [2; 3]);
    assert_eq!(
        simulator.take_due(now + Duration::from_millis(3_500)),
        Vec::new()
    );
    simulator.stop();
    assert_eq!(
        simulator.take_due(now + Duration::from_secs(60)),
        Vec::new()
    );
}

#[test]
fn scheduler_validates_all_periods_before_activating() {
    for interval in [0, 99, 60_001, u32::MAX] {
        let mut simulator = StarComputerSimulator::default();
        let mut settings = config();
        settings.interval_millis[1] = interval;
        assert!(simulator.start(settings, Instant::now()).is_err());
        assert!(!simulator.status.active);
    }
}

struct ControlledSerial {
    written: Arc<Mutex<Vec<u8>>>,
    gate: Option<Receiver<()>>,
    disconnected: Arc<std::sync::atomic::AtomicBool>,
    captures: Arc<Mutex<Vec<serde_json::Value>>>,
    frame: Vec<u8>,
    started_at: Option<SystemTime>,
}

impl Read for ControlledSerial {
    fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
        if let Some(gate) = self.gate.take() {
            gate.recv_timeout(Duration::from_secs(2)).unwrap();
        }
        if self.disconnected.load(std::sync::atomic::Ordering::Acquire) {
            return Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "controlled disconnect",
            ));
        }
        std::thread::sleep(Duration::from_millis(1));
        Err(io::ErrorKind::TimedOut.into())
    }
}

impl Write for ControlledSerial {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        // Force write_all to exercise several physical writes per frame.
        let count = bytes.len().min(7);
        self.started_at.get_or_insert_with(SystemTime::now);
        self.frame.extend_from_slice(&bytes[..count]);
        self.written
            .lock()
            .unwrap()
            .extend_from_slice(&bytes[..count]);
        Ok(count)
    }

    fn flush(&mut self) -> io::Result<()> {
        let complete = SystemTime::now();
        let hex = super::hex_bytes(&self.frame);
        self.captures.lock().unwrap().push(serde_json::json!({
            "hex": hex,
            "source": "CONTROLLED_TRANSPORT_SYNTHETIC",
            "started_unix_micros": u64::try_from(self.started_at.take().unwrap().duration_since(UNIX_EPOCH).unwrap().as_micros()).unwrap(),
            "completed_unix_micros": u64::try_from(complete.duration_since(UNIX_EPOCH).unwrap().as_micros()).unwrap(),
        }));
        self.frame.clear();
        Ok(())
    }
}

impl Transport for ControlledSerial {
    fn endpoint(&self) -> &'static str {
        "controlled://RS422-SYNTHETIC"
    }
    fn set_read_timeout(&mut self, _: Duration) -> Result<(), TransportError> {
        Ok(())
    }
}

struct TestSerial {
    controller: Controller,
    written: Arc<Mutex<Vec<u8>>>,
    disconnected: Arc<std::sync::atomic::AtomicBool>,
    release: Sender<()>,
    captures: Arc<Mutex<Vec<serde_json::Value>>>,
}

fn controlled_serial() -> TestSerial {
    let (critical, critical_rx) = bounded(128);
    let (display, _) = bounded(16);
    let mut controller = Controller::new(
        critical,
        critical_rx,
        display,
        Arc::default(),
        Arc::new(Mutex::new(BackendStateSnapshot::default())),
    );
    let written = Arc::new(Mutex::new(Vec::new()));
    let disconnected = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let captures = Arc::new(Mutex::new(Vec::new()));
    let (release, gate) = bounded(1);
    controller.rs422 = Some(
        ReaderWorker::spawn(
            Box::new(ControlledSerial {
                written: Arc::clone(&written),
                gate: Some(gate),
                disconnected: Arc::clone(&disconnected),
                captures: Arc::clone(&captures),
                frame: Vec::new(),
                started_at: None,
            }),
            Rs422StreamDecoder::new(),
            ReaderConfig::default(),
        )
        .unwrap(),
    );
    controller.rs422_connected = true;
    TestSerial {
        controller,
        written,
        disconnected,
        release,
        captures,
    }
}

fn unix_micros() -> u64 {
    u64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_micros(),
    )
    .unwrap()
}

fn save_capture(serial: &TestSerial, lifecycle: bool, evidence: &serde_json::Value) {
    let Some(path) = std::env::var_os("STAR_SIMULATOR_CAPTURE_PATH") else {
        return;
    };
    let mut path = std::path::PathBuf::from(path);
    if lifecycle {
        path.set_file_name("controlled-lifecycle.json");
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    let status = &serial.controller.star_simulator.status;
    let capture = serde_json::json!({
        "source": "CONTROLLED_TRANSPORT_SYNTHETIC",
        "real_com_port": false,
        "frame_write_chunk_limit": 7,
        "frames": &*serial.captures.lock().unwrap(),
        "wire_hex":super::hex_bytes(&serial.written.lock().unwrap()),
        "markers": evidence["markers"],
        "status": { "active":status.active, "sent":status.sent, "skipped":status.skipped,
            "failed":status.failed,"queue_depth":status.queue_depth,
            "timing":status.timing.iter().map(|timing| serde_json::json!({
                "samples":timing.samples,"last_interval_micros":timing.last_interval_micros,
                "max_lateness_micros":timing.max_lateness_micros,
                "total_lateness_micros":timing.total_lateness_micros,
            })).collect::<Vec<_>>(),
        },
        "evidence":evidence,
    });
    std::fs::write(path, serde_json::to_string_pretty(&capture).unwrap()).unwrap();
}

fn pump(controller: &mut Controller, duration: Duration) {
    let until = Instant::now() + duration;
    while Instant::now() < until {
        controller.drain_rs422();
        controller.poll_reader_statuses();
        controller.poll_telemetry_request(Instant::now());
        controller.poll_star_simulator(Instant::now());
        controller.drain_command_acks();
        std::thread::sleep(Duration::from_millis(1));
    }
}

fn frames(wire: &[u8]) -> Vec<&[u8]> {
    let mut output = Vec::new();
    let mut offset = 0;
    while offset < wire.len() {
        assert_eq!(&wire[offset..offset + 2], &[0xEB, 0x90]);
        let length = usize::from(u16::from_be_bytes([wire[offset + 4], wire[offset + 5]])) + 6;
        let frame = &wire[offset..offset + length];
        assert_eq!(
            frame[frame.len() - 1],
            frame[6..frame.len() - 1]
                .iter()
                .fold(0u8, |sum, b| sum.wrapping_add(*b))
        );
        output.push(frame);
        offset += length;
    }
    output
}

#[test]
fn controlled_single_writer_sends_three_periodic_types_with_polling_and_control() {
    let mut serial = controlled_serial();
    serial.release.send(()).unwrap();
    serial
        .controller
        .handle_request(UiRequest::StartStarSimulator(config()));
    assert!(serial.controller.telemetry_polling.is_some());
    serial.controller.handle_request(UiRequest::SendCommand(
        Command::ConfigureDc(1000).encode().into_bytes(),
    ));
    pump(&mut serial.controller, Duration::from_millis(6_250));
    let status = serial.controller.star_simulator.status.clone();
    assert!(status.sent.iter().all(|sent| *sent >= 7));
    assert_eq!(status.failed, [0; 3]);
    assert!(status.timing.iter().all(|timing| timing.samples >= 6));
    let wire = serial.written.lock().unwrap().clone();
    let decoded = frames(&wire);
    assert!(decoded.iter().filter(|frame| frame[6] == 0x11).count() >= 2);
    assert!(decoded.iter().any(|frame| frame[6] == 0x21));
    for frame in decoded.iter().filter(|frame| frame[6] == 0xFF) {
        assert!([14, 67, 122].contains(&frame.len()));
        if frame.len() == 122 {
            assert_eq!(
                frame[120],
                frame[15..120]
                    .iter()
                    .fold(0u8, |sum, b| sum.wrapping_add(*b))
            );
        }
    }
    let stop_requested = unix_micros();
    serial
        .controller
        .handle_request(UiRequest::StopStarSimulator);
    let stop_completed = unix_micros();
    let sent_before = serial.controller.star_simulator.status.sent;
    pump(&mut serial.controller, Duration::from_millis(120));
    assert_eq!(serial.controller.star_simulator.status.sent, sent_before);
    let captured = serial.captures.lock().unwrap().clone();
    for pair in captured.windows(2) {
        assert!(
            pair[1]["started_unix_micros"].as_u64().unwrap()
                > pair[0]["completed_unix_micros"].as_u64().unwrap()
        );
    }
    save_capture(
        &serial,
        false,
        &serde_json::json!({
            "broadcast_intervals_ms":[1000,1000,1000],"telemetry_interval_ms":5000,
            "duration_ms":6250,"stop_no_new_broadcast":true,
            "control_dc_1000_written":true,"sent_after_stop":sent_before,
            "markers":{"stop_requested_unix_micros":stop_requested,"stop_unix_micros":stop_completed,
                "observed_until_unix_micros":unix_micros()},
        }),
    );
    serial.controller.disconnect(LinkKind::Rs422);
}

#[test]
fn controlled_point_switch_preserves_enqueued_snapshot_disconnect_never_resumes() {
    let mut serial = controlled_serial();
    let mut settings = config();
    settings.enabled = [false, false, true];
    settings.interval_millis[2] = 100;
    serial
        .controller
        .handle_request(UiRequest::StartStarSimulator(settings.clone()));
    settings.point = GNSS_PRESETS[2];
    let point_change = unix_micros();
    serial
        .controller
        .handle_request(UiRequest::ConfigureStarSimulator(settings));
    serial.release.send(()).unwrap();
    pump(&mut serial.controller, Duration::from_millis(250));
    let wire = serial.written.lock().unwrap().clone();
    let decoded = frames(&wire);
    let gnss: Vec<_> = decoded
        .into_iter()
        .filter(|frame| frame.len() == 122)
        .collect();
    assert!(gnss.len() >= 2);
    assert_ne!(&gnss[0][37..49], &gnss[1][37..49]);
    let disconnected_at = unix_micros();
    serial
        .disconnected
        .store(true, std::sync::atomic::Ordering::Release);
    pump(&mut serial.controller, Duration::from_millis(30));
    assert!(!serial.controller.star_simulator.status.active);
    assert!(serial.controller.telemetry_polling.is_none());
    serial.controller.disconnect(LinkKind::Rs422);
    let before_reconnect = serial.captures.lock().unwrap().len();
    let reconnected_at = unix_micros();
    serial.controller.rs422 = Some(
        ReaderWorker::spawn(
            Box::new(ControlledSerial {
                written: Arc::clone(&serial.written),
                captures: Arc::clone(&serial.captures),
                disconnected: Arc::new(std::sync::atomic::AtomicBool::new(false)),
                gate: None,
                frame: Vec::new(),
                started_at: None,
            }),
            Rs422StreamDecoder::new(),
            ReaderConfig::default(),
        )
        .unwrap(),
    );
    pump(&mut serial.controller, Duration::from_millis(120));
    assert!(serial.controller.rs422_connected);
    assert!(!serial.controller.star_simulator.status.active);
    assert!(serial.controller.telemetry_polling.is_none());
    assert_eq!(serial.captures.lock().unwrap().len(), before_reconnect);
    assert_eq!(
        serial
            .controller
            .star_simulator
            .take_due(Instant::now() + Duration::from_secs(10)),
        Vec::new()
    );
    save_capture(
        &serial,
        true,
        &serde_json::json!({
            "initial_point":"SYNTHETIC A","following_point":"SYNTHETIC C",
            "queued_point_snapshot_preserved":true,"disconnect_stops_scheduler":true,
            "disconnect_stops_polling":true,"reconnected_state_does_not_resume":true,
            "markers":{"point_change_unix_micros":point_change,"disconnect_unix_micros":disconnected_at,
                "reconnect_unix_micros":reconnected_at,"observed_until_unix_micros":unix_micros()},
        }),
    );
    serial.controller.disconnect(LinkKind::Rs422);
}

#[test]
fn control_congestion_stops_broadcast_when_keepalive_poll_cannot_queue() {
    let mut serial = controlled_serial();
    serial
        .controller
        .handle_request(UiRequest::StartStarSimulator(config()));
    let request = Command::RequestTelemetry.encode().into_bytes().to_vec();
    while serial
        .controller
        .rs422
        .as_ref()
        .unwrap()
        .try_send_bytes(request.clone())
        .is_ok()
    {}
    let later = Instant::now() + Duration::from_secs(6);
    serial.controller.poll_telemetry_request(later);
    assert!(serial.controller.telemetry_polling.is_none());
    serial.controller.poll_star_simulator(later);
    let active = serial.controller.star_simulator.status.active;
    let error = serial.controller.star_simulator.status.last_error.clone();
    serial.release.send(()).unwrap();
    assert!(
        !active,
        "broadcast must stop when its required telemetry polling stops"
    );
    assert!(error.is_some());
    serial.controller.disconnect(LinkKind::Rs422);
}

#[test]
fn restarting_broadcast_session_does_not_count_idle_time_as_period() {
    let mut serial = controlled_serial();
    serial.release.send(()).unwrap();
    serial
        .controller
        .handle_request(UiRequest::StartStarSimulator(config()));
    pump(&mut serial.controller, Duration::from_millis(70));
    assert_eq!(serial.controller.star_simulator.status.timing[0].samples, 0);
    serial
        .controller
        .handle_request(UiRequest::StopStarSimulator);
    serial
        .controller
        .handle_request(UiRequest::StartStarSimulator(config()));
    pump(&mut serial.controller, Duration::from_millis(70));
    let samples = serial.controller.star_simulator.status.timing[0].samples;
    serial.controller.disconnect(LinkKind::Rs422);
    assert_eq!(
        samples, 0,
        "a new session has no measured interval until its second frame"
    );
}

#[test]
fn disabling_all_types_stops_scheduler_and_publishes_inactive_state() {
    let mut serial = controlled_serial();
    serial.release.send(()).unwrap();
    serial
        .controller
        .handle_request(UiRequest::StartStarSimulator(config()));
    let mut disabled = config();
    disabled.enabled = [false; 3];
    serial
        .controller
        .handle_request(UiRequest::ConfigureStarSimulator(disabled));
    let active = serial.controller.star_simulator.status.active;
    serial.controller.disconnect(LinkKind::Rs422);
    assert!(!active);
}
