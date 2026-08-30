use std::fmt;

#[cfg(test)]
use crossbeam_channel::{Receiver, Sender, TrySendError, bounded};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkKind {
    Rs422,
    Lvds,
}

impl fmt::Display for LinkKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Rs422 => "RS422",
            Self::Lvds => "LVDS",
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogLevel {
    Info,
    Warning,
    Error,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TelemetrySnapshot {
    pub absolute_time_seconds: u32,
    pub local_time_seconds: u32,
    pub cpu_temperature_celsius: u8,
    pub telemetry_request_count: u8,
    pub accepted_commands: u8,
    pub rejected_commands: u8,
    pub work_mode: u8,
    pub scan_frequency: u8,
    pub scan_voltage: u16,
    pub ad9833_status: u8,
    pub dac8563_status: u8,
    pub adc_1_status: u8,
    pub adc_2_status: u8,
    pub lvds_status: u8,
    pub last_injected_command: [u8; 5],
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LinkMetricsSnapshot {
    pub bytes_read: u64,
    pub frames_decoded: u64,
    pub rejected_frames: u64,
    pub checksum_failures: u64,
    pub length_failures: u64,
    pub events_dropped: u64,
    pub queue_overflows: u64,
    pub io_errors: u64,
    pub max_event_queue_depth: u64,
    /// Frames discarded only between the controller and UI. This is separate
    /// from transport reader backpressure counters above.
    pub ui_display_events_dropped: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BackendStateSnapshot {
    pub revision: u64,
    pub rs422_connected: bool,
    pub rs422_detail: String,
    pub lvds_connected: bool,
    pub lvds_detail: String,
    pub acquisition_running: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum UiRequest {
    RefreshPorts,
    Connect {
        link: LinkKind,
        port: String,
        baud: u32,
    },
    Disconnect {
        link: LinkKind,
    },
    StartAcquisition,
    StopAcquisition,
    SendCommand([u8; 12]),
    ExportCsv {
        suggested_name: String,
    },
    #[cfg(feature = "mock")]
    InjectMockFault,
}

#[derive(Debug, Clone, PartialEq)]
pub enum BackendEvent {
    PortsUpdated {
        rs422: Vec<String>,
        lvds: Vec<String>,
    },
    LinkChanged {
        link: LinkKind,
        connected: bool,
        detail: String,
    },
    AcquisitionChanged {
        running: bool,
    },
    /// Reliable, coalesced state published outside the bounded critical-event
    /// history. It repairs UI state after old critical events are evicted.
    StateSnapshot(BackendStateSnapshot),
    Telemetry(TelemetrySnapshot),
    LinkMetrics {
        link: LinkKind,
        connected: bool,
        metrics: LinkMetricsSnapshot,
    },
    ScientificSamples {
        channel_1_start_seconds: f64,
        channel_2_start_seconds: f64,
        sample_interval_seconds: f64,
        channel_1: Vec<f64>,
        channel_2: Vec<f64>,
    },
    CommandSent([u8; 12]),
    CsvExported {
        path: String,
    },
    Fault {
        source: String,
        message: String,
    },
    Log {
        level: LogLevel,
        message: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BridgeError(pub String);

impl fmt::Display for BridgeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for BridgeError {}

/// UI/backend boundary. Implementations must queue expensive transport,
/// parsing, and storage work instead of performing it synchronously here.
pub trait BackendBridge: Send {
    fn request(&mut self, request: UiRequest) -> Result<(), BridgeError>;
    fn try_next_event(&mut self) -> Option<BackendEvent>;
}

/// Channel facade for a real backend. The UI only performs non-blocking sends
/// and receives; acquisition workers own protocol parsing and persistence.
#[cfg(test)]
pub struct ChannelBridge {
    request_tx: Sender<UiRequest>,
    event_rx: Receiver<BackendEvent>,
}

#[cfg(test)]
pub struct BackendEndpoint {
    pub request_rx: Receiver<UiRequest>,
    pub event_tx: Sender<BackendEvent>,
}

#[cfg(test)]
#[must_use]
pub fn channel_bridge(capacity: usize) -> (ChannelBridge, BackendEndpoint) {
    let capacity = capacity.max(1);
    let (request_tx, request_rx) = bounded(capacity);
    let (event_tx, event_rx) = bounded(capacity);
    (
        ChannelBridge {
            request_tx,
            event_rx,
        },
        BackendEndpoint {
            request_rx,
            event_tx,
        },
    )
}

#[cfg(test)]
impl BackendBridge for ChannelBridge {
    fn request(&mut self, request: UiRequest) -> Result<(), BridgeError> {
        self.request_tx
            .try_send(request)
            .map_err(|error| match error {
                TrySendError::Full(_) => BridgeError("后端请求队列已满".to_owned()),
                TrySendError::Disconnected(_) => BridgeError("后端已断开".to_owned()),
            })
    }

    fn try_next_event(&mut self) -> Option<BackendEvent> {
        self.event_rx.try_recv().ok()
    }
}

#[cfg(test)]
mod tests {
    use super::{BackendBridge, BackendEvent, LinkKind, UiRequest, channel_bridge};

    #[test]
    fn channel_facade_moves_requests_and_events_without_blocking() {
        let (mut frontend, backend) = channel_bridge(2);
        frontend.request(UiRequest::RefreshPorts).unwrap();
        assert_eq!(backend.request_rx.try_recv(), Ok(UiRequest::RefreshPorts));

        backend
            .event_tx
            .try_send(BackendEvent::LinkChanged {
                link: LinkKind::Rs422,
                connected: true,
                detail: "loopback".to_owned(),
            })
            .unwrap();
        assert!(matches!(
            frontend.try_next_event(),
            Some(BackendEvent::LinkChanged {
                link: LinkKind::Rs422,
                connected: true,
                ..
            })
        ));
    }
}
