use std::{
    collections::VecDeque,
    f64::consts::TAU,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use crate::bridge::{
    BackendBridge, BackendEvent, BridgeError, LinkKind, LogLevel, TelemetrySnapshot, UiRequest,
};

const MOCK_FRAME_INTERVAL: Duration = Duration::from_millis(10);
const MOCK_SAMPLES_PER_FRAME: u32 = 1_000;
const SAMPLE_INTERVAL_SECONDS: f64 = 0.000_01;

pub struct MockBridge {
    events: VecDeque<BackendEvent>,
    rs422_connected: bool,
    lvds_connected: bool,
    acquisition_running: bool,
    next_frame: Instant,
    next_frame_time_seconds: f64,
    sequence: u64,
    accepted_commands: u8,
    rejected_commands: u8,
}

impl MockBridge {
    #[must_use]
    pub fn new() -> Self {
        Self {
            events: VecDeque::from([BackendEvent::Log {
                level: LogLevel::Warning,
                message: "显式 MOCK 模式已启动；不会访问真实硬件".to_owned(),
            }]),
            rs422_connected: false,
            lvds_connected: false,
            acquisition_running: false,
            next_frame: Instant::now(),
            next_frame_time_seconds: unix_time_seconds(),
            sequence: 0,
            accepted_commands: 0,
            rejected_commands: 0,
        }
    }

    fn queue_link(&mut self, link: LinkKind, connected: bool, detail: String) {
        self.events.push_back(BackendEvent::LinkChanged {
            link,
            connected,
            detail,
        });
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
            let seconds = Duration::try_from_secs_f64(now).map_or(u32::MAX, |duration| {
                u32::try_from(duration.as_secs()).unwrap_or(u32::MAX)
            });
            self.events
                .push_back(BackendEvent::Telemetry(TelemetrySnapshot {
                    absolute_time_seconds: seconds,
                    local_time_seconds: seconds % 86_400,
                    cpu_temperature_celsius: 42 + (self.sequence % 7) as u8,
                    pcb_temperature_celsius: 37 + (self.sequence % 5) as u8,
                    magnetic_x: 1_200 + (self.sequence % 100) as u16,
                    magnetic_y: 2_300 + (self.sequence % 80) as u16,
                    magnetic_z: 3_400 + (self.sequence % 60) as u16,
                    accepted_commands: self.accepted_commands,
                    rejected_commands: self.rejected_commands,
                    dds1_status: 1,
                    dds2_status: 1,
                    dac_status: 1,
                    auxiliary_status: 1,
                    adc1_status: 1,
                    adc2_status: 1,
                }));
        }
        self.sequence = self.sequence.wrapping_add(1);
        self.next_frame = self
            .next_frame
            .checked_add(MOCK_FRAME_INTERVAL)
            .unwrap_or_else(Instant::now);
        self.next_frame_time_seconds += SAMPLE_INTERVAL_SECONDS * f64::from(MOCK_SAMPLES_PER_FRAME);
    }

    fn queue_one_due_frame(&mut self, now: Instant) -> bool {
        if self.acquisition_running && now >= self.next_frame {
            self.generate_frame();
            true
        } else {
            false
        }
    }
}

impl BackendBridge for MockBridge {
    fn request(&mut self, request: UiRequest) -> Result<(), BridgeError> {
        match request {
            UiRequest::RefreshPorts => {
                self.events.push_back(BackendEvent::PortsUpdated {
                    rs422: vec!["MOCK-RS422".to_owned()],
                    lvds: vec!["MOCK-LVDS".to_owned()],
                });
            }
            UiRequest::Connect { link, port, baud } => {
                match link {
                    LinkKind::Rs422 => self.rs422_connected = true,
                    LinkKind::Lvds => self.lvds_connected = true,
                }
                self.queue_link(link, true, format!("{port} @ {baud}"));
            }
            UiRequest::Disconnect { link } => {
                match link {
                    LinkKind::Rs422 => self.rs422_connected = false,
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
            }
            UiRequest::StopAcquisition => {
                self.acquisition_running = false;
                self.events
                    .push_back(BackendEvent::AcquisitionChanged { running: false });
            }
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
    fn overdue_mock_clock_catches_up_at_one_hundred_hertz() {
        let mut bridge = MockBridge::new();
        bridge.events.clear();
        bridge.acquisition_running = true;
        let now = Instant::now();
        bridge.next_frame = now
            .checked_sub(Duration::from_millis(35))
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
}
