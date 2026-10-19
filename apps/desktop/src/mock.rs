use std::{
    collections::VecDeque,
    f64::consts::TAU,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use langmuir_core::Command;

use crate::bridge::{
    BackendBridge, BackendEvent, BridgeError, LinkKind, LogLevel, LvdsStatusSnapshot,
    TELEMETRY_POLL_INTERVAL_MAX_MILLIS, TELEMETRY_POLL_INTERVAL_MIN_MILLIS, TelemetrySnapshot,
    UiRequest,
};

const MOCK_FRAME_INTERVAL: Duration = Duration::from_millis(50);
const MOCK_SAMPLES_PER_FRAME: u32 = 125;
const SAMPLE_INTERVAL_SECONDS: f64 = 0.0004;

pub struct MockBridge {
    events: VecDeque<BackendEvent>,
    rs422_connected: bool,
    lvds_connected: bool,
    acquisition_running: bool,
    telemetry_poll_interval: Option<Duration>,
    next_telemetry_request: Instant,
    next_frame: Instant,
    next_frame_time_seconds: f64,
    sequence: u64,
    accepted_commands: u8,
    rejected_commands: u8,
}

impl MockBridge {
    #[must_use]
    pub fn new() -> Self {
        let mut bridge = Self {
            events: VecDeque::from([BackendEvent::Log {
                level: LogLevel::Warning,
                message: "显式 MOCK 模式已启动；不会访问真实硬件".to_owned(),
            }]),
            rs422_connected: false,
            lvds_connected: false,
            acquisition_running: false,
            telemetry_poll_interval: None,
            next_telemetry_request: Instant::now(),
            next_frame: Instant::now(),
            next_frame_time_seconds: unix_time_seconds(),
            sequence: 0,
            accepted_commands: 0,
            rejected_commands: 0,
        };
        bridge.queue_lvds_status();
        bridge
    }

    fn queue_link(&mut self, link: LinkKind, connected: bool, detail: String) {
        self.events.push_back(BackendEvent::LinkChanged {
            link,
            connected,
            detail,
        });
        if link == LinkKind::Lvds {
            self.queue_lvds_status();
        }
    }

    fn queue_lvds_status(&mut self) {
        self.events
            .push_back(BackendEvent::LvdsStatus(Box::new(LvdsStatusSnapshot {
                source: "MOCK 合成数据（未经过真实 UDP 解析）".to_owned(),
                receive_state: if self.acquisition_running {
                    "MOCK 合成波形运行中；不写科学文件"
                } else if self.lvds_connected {
                    "MOCK 已连接；未生成波形"
                } else {
                    "MOCK 未连接"
                }
                .to_owned(),
                ..LvdsStatusSnapshot::default()
            })));
    }

    fn generate_frame(&mut self) {
        let now = self.next_frame_time_seconds;
        let sequence_phase =
            u32::try_from(self.sequence % 1_000_000).expect("the modulo result always fits in u32");
        let phase = f64::from(sequence_phase) * 0.17;
        let sample_capacity =
            usize::try_from(MOCK_SAMPLES_PER_FRAME).expect("the mock sample count fits in usize");
        let mut channel_1 = Vec::with_capacity(sample_capacity);
        let mut channel_2 = Vec::with_capacity(sample_capacity);
        for index in 0..MOCK_SAMPLES_PER_FRAME {
            let x = phase + f64::from(index) / f64::from(MOCK_SAMPLES_PER_FRAME) * TAU;
            channel_1.push(32_768.0 + 18_000.0 * x.sin());
            channel_2.push(32_768.0 + 12_000.0 * (x * 1.7).cos());
        }
        self.events.push_back(BackendEvent::ScientificSamples {
            channel_1_start_seconds: now,
            channel_2_start_seconds: now,
            sample_interval_seconds: SAMPLE_INTERVAL_SECONDS,
            channel_1,
            channel_2,
        });

        if self.sequence.is_multiple_of(10) {
            self.events
                .push_back(BackendEvent::Telemetry(self.telemetry_snapshot(now)));
        }
        self.sequence = self.sequence.wrapping_add(1);
        self.next_frame = self
            .next_frame
            .checked_add(MOCK_FRAME_INTERVAL)
            .unwrap_or_else(Instant::now);
        self.next_frame_time_seconds += SAMPLE_INTERVAL_SECONDS * f64::from(MOCK_SAMPLES_PER_FRAME);
    }

    fn telemetry_snapshot(&self, now: f64) -> TelemetrySnapshot {
        let seconds = Duration::try_from_secs_f64(now).map_or(u32::MAX, |duration| {
            u32::try_from(duration.as_secs()).unwrap_or(u32::MAX)
        });
        TelemetrySnapshot {
            absolute_time_seconds: seconds,
            local_time_seconds: seconds % 86_400,
            cpu_temperature_celsius: 42 + (self.sequence % 7) as u8,
            telemetry_request_count: u8::try_from(self.sequence).unwrap_or(u8::MAX),
            accepted_commands: self.accepted_commands,
            rejected_commands: self.rejected_commands,
            work_mode: 0x55,
            scan_frequency: 100,
            scan_voltage: 5_000,
            ad9833_status: 1,
            dac8563_status: 1,
            adc_1_status: 1,
            adc_2_status: 1,
            lvds_status: 1,
            last_injected_command: [0x21, 0x55, 0x64, 0x9B, 0x9B],
        }
    }

    fn queue_one_due_frame(&mut self, now: Instant) -> bool {
        if self.acquisition_running && now >= self.next_frame {
            self.generate_frame();
            true
        } else {
            false
        }
    }

    fn queue_due_telemetry_request(&mut self, now: Instant) -> bool {
        let Some(interval) = self.telemetry_poll_interval else {
            return false;
        };
        if !self.rs422_connected {
            self.telemetry_poll_interval = None;
            self.events
                .push_back(BackendEvent::TelemetryPollingChanged {
                    active: false,
                    interval_millis: u32::try_from(interval.as_millis()).unwrap_or(u32::MAX),
                });
            return true;
        }
        if now < self.next_telemetry_request {
            return false;
        }
        self.accepted_commands = self.accepted_commands.wrapping_add(1);
        self.events.push_back(BackendEvent::CommandSent(
            Command::RequestTelemetry.encode().into_bytes(),
        ));
        self.events.push_back(BackendEvent::Telemetry(
            self.telemetry_snapshot(unix_time_seconds()),
        ));
        self.next_telemetry_request = now.checked_add(interval).unwrap_or(now);
        true
    }

    fn set_telemetry_polling(&mut self, enabled: bool, interval_millis: u32) {
        if !enabled {
            self.telemetry_poll_interval = None;
            self.events
                .push_back(BackendEvent::TelemetryPollingChanged {
                    active: false,
                    interval_millis,
                });
            return;
        }
        if !(TELEMETRY_POLL_INTERVAL_MIN_MILLIS..=TELEMETRY_POLL_INTERVAL_MAX_MILLIS)
            .contains(&interval_millis)
        {
            self.events.push_back(BackendEvent::Fault {
                source: "循环遥测".to_owned(),
                message: "循环周期超出允许范围".to_owned(),
            });
            return;
        }
        if !self.rs422_connected {
            self.events.push_back(BackendEvent::Fault {
                source: "循环遥测".to_owned(),
                message: "RS422 未连接，不能启动循环请求".to_owned(),
            });
            return;
        }
        let interval = Duration::from_millis(u64::from(interval_millis));
        self.telemetry_poll_interval = Some(interval);
        self.next_telemetry_request = Instant::now();
        self.events
            .push_back(BackendEvent::TelemetryPollingChanged {
                active: true,
                interval_millis,
            });
        self.queue_due_telemetry_request(Instant::now());
    }

    fn reject_pcap_replay(&mut self) {
        self.events.push_back(BackendEvent::Fault {
            source: "PCAPNG 回放".to_owned(),
            message: "MOCK 模式只生成合成波形；请不带 --mock 启动应用，以使用真实只读 PCAPNG 回放"
                .to_owned(),
        });
        self.queue_lvds_status();
    }

    fn stop_pcap_replay(&mut self) {
        self.events.push_back(BackendEvent::Log {
            level: LogLevel::Info,
            message: "MOCK 模式没有运行 PCAPNG 回放".to_owned(),
        });
        self.queue_lvds_status();
    }
}

impl BackendBridge for MockBridge {
    fn request(&mut self, request: UiRequest) -> Result<(), BridgeError> {
        match request {
            UiRequest::RefreshPorts => {
                self.events.push_back(BackendEvent::PortsUpdated {
                    rs422: vec!["MOCK-RS422".to_owned()],
                });
            }
            UiRequest::Connect { link, port, baud } => {
                match link {
                    LinkKind::Rs422 => self.rs422_connected = true,
                    LinkKind::Lvds => self.lvds_connected = true,
                }
                self.queue_link(link, true, format!("MOCK 合成 {port} @ {baud}"));
            }
            UiRequest::BindLvdsUdp { bind_address, port } => {
                self.lvds_connected = true;
                self.queue_link(
                    LinkKind::Lvds,
                    true,
                    format!("MOCK 合成（未绑定 UDP）{bind_address}:{port}"),
                );
            }
            UiRequest::Disconnect { link } => {
                match link {
                    LinkKind::Rs422 => {
                        self.rs422_connected = false;
                        if let Some(interval) = self.telemetry_poll_interval.take() {
                            self.events
                                .push_back(BackendEvent::TelemetryPollingChanged {
                                    active: false,
                                    interval_millis: u32::try_from(interval.as_millis())
                                        .unwrap_or(u32::MAX),
                                });
                        }
                    }
                    LinkKind::Lvds => {
                        self.lvds_connected = false;
                        if self.acquisition_running {
                            self.acquisition_running = false;
                            self.events
                                .push_back(BackendEvent::AcquisitionChanged { running: false });
                        }
                    }
                }
                self.queue_link(link, false, "用户断开".to_owned());
            }
            UiRequest::StartAcquisition => {
                if !self.lvds_connected {
                    self.events.push_back(BackendEvent::Fault {
                        source: "采集".to_owned(),
                        message: "请先连接 MOCK-LVDS".to_owned(),
                    });
                    return Ok(());
                }
                self.acquisition_running = true;
                self.next_frame = Instant::now();
                self.next_frame_time_seconds = unix_time_seconds();
                self.events
                    .push_back(BackendEvent::AcquisitionChanged { running: true });
                self.queue_lvds_status();
            }
            UiRequest::StopAcquisition => {
                self.acquisition_running = false;
                self.events
                    .push_back(BackendEvent::AcquisitionChanged { running: false });
                self.queue_lvds_status();
            }
            UiRequest::StartPcapReplay { .. } => self.reject_pcap_replay(),
            UiRequest::StopPcapReplay => self.stop_pcap_replay(),
            UiRequest::SetTelemetryPolling {
                enabled,
                interval_millis,
            } => self.set_telemetry_polling(enabled, interval_millis),
            UiRequest::SendCommand(frame) => {
                if !self.rs422_connected {
                    self.rejected_commands = self.rejected_commands.wrapping_add(1);
                    self.events.push_back(BackendEvent::Fault {
                        source: "RS422".to_owned(),
                        message: "未连接，模拟指令未发送".to_owned(),
                    });
                    return Ok(());
                }
                self.accepted_commands = self.accepted_commands.wrapping_add(1);
                self.events.push_back(BackendEvent::CommandSent(frame));
            }
            UiRequest::ExportCsv { suggested_name } => {
                self.events.push_back(BackendEvent::CsvExported {
                    path: format!("mock://{suggested_name}"),
                });
            }
            UiRequest::InjectMockFault => {
                self.rejected_commands = self.rejected_commands.wrapping_add(1);
                self.events.push_back(BackendEvent::Fault {
                    source: "MOCK".to_owned(),
                    message: "用户注入的可恢复测试异常".to_owned(),
                });
            }
        }
        Ok(())
    }

    fn try_next_event(&mut self) -> Option<BackendEvent> {
        if let Some(event) = self.events.pop_front() {
            return Some(event);
        }
        if self.queue_one_due_frame(Instant::now()) {
            return self.events.pop_front();
        }
        if self.queue_due_telemetry_request(Instant::now()) {
            return self.events.pop_front();
        }
        None
    }
}

fn unix_time_seconds() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_secs_f64()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overdue_mock_clock_catches_up_at_twenty_hertz() {
        let mut bridge = MockBridge::new();
        bridge.events.clear();
        bridge.acquisition_running = true;
        let now = Instant::now();
        bridge.next_frame = now
            .checked_sub(Duration::from_millis(175))
            .expect("the test instant supports a short subtraction");

        let mut scientific_frames = 0;
        while bridge.queue_one_due_frame(now) {
            while let Some(event) = bridge.events.pop_front() {
                if matches!(event, BackendEvent::ScientificSamples { .. }) {
                    scientific_frames += 1;
                }
            }
        }
        assert_eq!(scientific_frames, 4);
        assert!(bridge.next_frame > now);
    }

    #[test]
    fn telemetry_polling_returns_a_snapshot_without_science_capture() {
        let mut bridge = MockBridge::new();
        bridge.rs422_connected = true;

        bridge.set_telemetry_polling(true, 1_000);

        assert!(
            bridge
                .events
                .iter()
                .any(|event| matches!(event, BackendEvent::Telemetry(_)))
        );
        assert!(!bridge.acquisition_running);
    }

    #[test]
    fn mock_send_command_emits_the_exact_submitted_frame() {
        let mut bridge = MockBridge::new();
        bridge
            .request(UiRequest::Connect {
                link: LinkKind::Rs422,
                port: "MOCK-RS422".to_owned(),
                baud: 115_200,
            })
            .unwrap();
        let frame = Command::ConfigureDc(0x1234).encode().into_bytes();
        bridge.request(UiRequest::SendCommand(frame)).unwrap();

        assert!(
            bridge.events.iter().any(|event| {
                matches!(event, BackendEvent::CommandSent(sent) if *sent == frame)
            })
        );
    }

    #[test]
    fn mock_status_never_fabricates_real_packets_or_storage() {
        let bridge = MockBridge::new();
        let status = bridge
            .events
            .iter()
            .find_map(|event| match event {
                BackendEvent::LvdsStatus(status) => Some(status),
                _ => None,
            })
            .unwrap();
        assert!(status.source.contains("MOCK 合成"));
        assert_eq!(status.stats, crate::bridge::LvdsReceiveStats::default());
        assert!(status.replay.is_none());
        assert_eq!(
            status.storage,
            crate::bridge::ScientificStorageStatus::default()
        );
    }

    #[test]
    fn mock_replay_request_is_explicitly_rejected_without_fake_packets() {
        let mut bridge = MockBridge::new();
        bridge.events.clear();
        bridge
            .request(UiRequest::StartPcapReplay {
                path: "capture.pcapng".to_owned(),
            })
            .unwrap();
        assert!(bridge.events.iter().any(|event| matches!(
            event, BackendEvent::Fault { source, message }
                if source == "PCAPNG 回放" && message.contains("不带 --mock")
        )));
        assert!(
            !bridge
                .events
                .iter()
                .any(|event| matches!(event, BackendEvent::ScientificSamples { .. }))
        );
        assert!(!bridge.lvds_connected);
        assert!(!bridge.acquisition_running);
    }
}
