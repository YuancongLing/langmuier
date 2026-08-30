use std::io::{self, Read, Write};
use std::time::Duration;

use serialport::{DataBits, FlowControl, Parity, SerialPort, StopBits};

use crate::{Connector, Transport, TransportError};

#[derive(Clone, Debug)]
pub struct SerialConfig {
    pub port: String,
    pub baud_rate: u32,
    pub data_bits: DataBits,
    pub flow_control: FlowControl,
    pub parity: Parity,
    pub stop_bits: StopBits,
    pub read_timeout: Duration,
}

impl SerialConfig {
    #[must_use]
    pub fn new(port: impl Into<String>, baud_rate: u32) -> Self {
        Self {
            port: port.into(),
            baud_rate,
            data_bits: DataBits::Eight,
            flow_control: FlowControl::None,
            parity: Parity::None,
            stop_bits: StopBits::One,
            read_timeout: Duration::from_millis(25),
        }
    }

    fn validate(&self) -> Result<(), TransportError> {
        if self.port.trim().is_empty() {
            return Err(TransportError::InvalidConfiguration(
                "serial port must not be empty".to_owned(),
            ));
        }
        if self.baud_rate == 0 {
            return Err(TransportError::InvalidConfiguration(
                "serial baud rate must be non-zero".to_owned(),
            ));
        }
        if self.read_timeout.is_zero() {
            return Err(TransportError::InvalidConfiguration(
                "serial read timeout must be non-zero".to_owned(),
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug)]
pub struct SerialConnector {
    config: SerialConfig,
}

impl SerialConnector {
    #[must_use]
    pub fn new(config: SerialConfig) -> Self {
        Self { config }
    }

    /// Opens and configures the requested serial port.
    ///
    /// # Errors
    ///
    /// Returns invalid configuration, device-open, or driver configuration
    /// errors. No mock fallback is attempted.
    pub fn connect_serial(&self) -> Result<SerialTransport, TransportError> {
        self.config.validate()?;
        let port_name = self.config.port.clone();
        let port = serialport::new(&port_name, self.config.baud_rate)
            .data_bits(self.config.data_bits)
            .flow_control(self.config.flow_control)
            .parity(self.config.parity)
            .stop_bits(self.config.stop_bits)
            .timeout(self.config.read_timeout)
            .open()
            .map_err(|error| TransportError::Open {
                endpoint: port_name.clone(),
                message: error.to_string(),
            })?;
        Ok(SerialTransport {
            endpoint: port_name,
            port,
        })
    }
}

impl Connector for SerialConnector {
    fn connect(&self) -> Result<Box<dyn Transport>, TransportError> {
        self.connect_serial()
            .map(|transport| Box::new(transport) as Box<dyn Transport>)
    }
}

pub struct SerialTransport {
    endpoint: String,
    port: Box<dyn SerialPort>,
}

impl std::fmt::Debug for SerialTransport {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SerialTransport")
            .field("endpoint", &self.endpoint)
            .finish_non_exhaustive()
    }
}

impl Read for SerialTransport {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        self.port.read(buffer)
    }
}

impl Write for SerialTransport {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.port.write(buffer)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.port.flush()
    }
}

impl Transport for SerialTransport {
    fn endpoint(&self) -> &str {
        &self.endpoint
    }

    fn set_read_timeout(&mut self, timeout: Duration) -> Result<(), TransportError> {
        self.port
            .set_timeout(timeout)
            .map_err(|error| TransportError::Configure {
                endpoint: self.endpoint.clone(),
                message: error.to_string(),
            })
    }
}

/// Lists serial ports reported by the host driver.
///
/// # Errors
///
/// Returns an error when the platform cannot enumerate its serial devices.
pub fn available_serial_ports() -> Result<Vec<String>, TransportError> {
    serialport::available_ports()
        .map(|ports| ports.into_iter().map(|port| port.port_name).collect())
        .map_err(|error| TransportError::Open {
            endpoint: "serial-port enumeration".to_owned(),
            message: error.to_string(),
        })
}
