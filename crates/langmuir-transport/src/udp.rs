use std::{
    io::{self, Read, Write},
    net::{SocketAddr, UdpSocket},
    time::Duration,
};

use crate::{Connector, Transport, TransportError};

const MAX_UDP_DATAGRAM_BYTES: usize = 65_535;

/// Configuration for a local UDP endpoint supplied by an LVDS-to-UDP
/// converter. Datagram payloads are exposed unchanged to the protocol reader.
#[derive(Clone, Debug)]
pub struct UdpConfig {
    pub bind_address: SocketAddr,
    pub read_timeout: Duration,
}

impl UdpConfig {
    #[must_use]
    pub const fn new(bind_address: SocketAddr) -> Self {
        Self {
            bind_address,
            read_timeout: Duration::from_millis(25),
        }
    }

    fn validate(&self) -> Result<(), TransportError> {
        if self.read_timeout.is_zero() {
            return Err(TransportError::InvalidConfiguration(
                "UDP read timeout must be non-zero".to_owned(),
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug)]
pub struct UdpConnector {
    config: UdpConfig,
}

impl UdpConnector {
    #[must_use]
    pub const fn new(config: UdpConfig) -> Self {
        Self { config }
    }

    /// Binds a local UDP socket. The converter's source endpoint is not fixed,
    /// so this accepts packets from any peer and performs no outbound writes.
    ///
    /// # Errors
    ///
    /// Returns a bind or timeout-configuration error.
    pub fn bind_udp(&self) -> Result<UdpTransport, TransportError> {
        self.config.validate()?;
        let requested_endpoint = self.config.bind_address.to_string();
        let socket =
            UdpSocket::bind(self.config.bind_address).map_err(|error| TransportError::Open {
                endpoint: requested_endpoint.clone(),
                message: error.to_string(),
            })?;
        let endpoint = socket
            .local_addr()
            .map_err(|error| TransportError::Configure {
                endpoint: requested_endpoint,
                message: error.to_string(),
            })?
            .to_string();
        socket
            .set_read_timeout(Some(self.config.read_timeout))
            .map_err(|error| TransportError::Configure {
                endpoint: endpoint.clone(),
                message: error.to_string(),
            })?;
        Ok(UdpTransport {
            endpoint,
            socket,
            datagram: vec![0_u8; MAX_UDP_DATAGRAM_BYTES],
            pending_start: 0,
            pending_end: 0,
        })
    }
}

impl Connector for UdpConnector {
    fn connect(&self) -> Result<Box<dyn Transport>, TransportError> {
        self.bind_udp()
            .map(|transport| Box::new(transport) as Box<dyn Transport>)
    }
}

pub struct UdpTransport {
    endpoint: String,
    socket: UdpSocket,
    datagram: Vec<u8>,
    pending_start: usize,
    pending_end: usize,
}

impl std::fmt::Debug for UdpTransport {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("UdpTransport")
            .field("endpoint", &self.endpoint)
            .finish_non_exhaustive()
    }
}

impl Read for UdpTransport {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if buffer.is_empty() {
            return Ok(0);
        }
        while self.pending_start == self.pending_end {
            let (received, _) = self.socket.recv_from(&mut self.datagram)?;
            self.pending_start = 0;
            self.pending_end = received;
        }
        let available = self.pending_end - self.pending_start;
        let copied = available.min(buffer.len());
        buffer[..copied].copy_from_slice(
            &self.datagram[self.pending_start..self.pending_start.saturating_add(copied)],
        );
        self.pending_start += copied;
        Ok(copied)
    }
}

impl Write for UdpTransport {
    fn write(&mut self, _buffer: &[u8]) -> io::Result<usize> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            format!("UDP listener {} is receive-only", self.endpoint),
        ))
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Transport for UdpTransport {
    fn endpoint(&self) -> &str {
        &self.endpoint
    }

    fn set_read_timeout(&mut self, timeout: Duration) -> Result<(), TransportError> {
        self.socket
            .set_read_timeout(Some(timeout))
            .map_err(|error| TransportError::Configure {
                endpoint: self.endpoint.clone(),
                message: error.to_string(),
            })
    }
}

#[cfg(test)]
mod tests {
    use std::{io::Read, net::UdpSocket, time::Duration};

    use super::{UdpConfig, UdpConnector};
    use crate::{ReaderConfig, ReaderEvent, ReaderWorker, Transport, mock::lvds_packet};
    use langmuir_core::LvdsStreamDecoder;

    #[test]
    fn listener_delivers_large_datagram_over_multiple_reads() {
        let sender = UdpSocket::bind("127.0.0.1:0").unwrap();
        let listener = UdpConnector::new(UdpConfig::new("127.0.0.1:0".parse().unwrap()))
            .bind_udp()
            .unwrap();
        let endpoint = listener.endpoint.clone();
        sender.send_to(&[1, 2, 3, 4, 5], &endpoint).unwrap();

        let mut listener = listener;
        listener.set_read_timeout(Duration::from_secs(1)).unwrap();
        let mut first = [0_u8; 2];
        let mut second = [0_u8; 3];
        assert_eq!(listener.read(&mut first).unwrap(), 2);
        assert_eq!(listener.read(&mut second).unwrap(), 3);
        assert_eq!(first, [1, 2]);
        assert_eq!(second, [3, 4, 5]);
    }

    #[test]
    fn udp_payload_reaches_lvds_decoder_worker() {
        let sender = UdpSocket::bind("127.0.0.1:0").unwrap();
        let listener = UdpConnector::new(UdpConfig::new("127.0.0.1:0".parse().unwrap()))
            .bind_udp()
            .unwrap();
        let endpoint = listener.endpoint.clone();
        let worker = ReaderWorker::spawn(
            Box::new(listener),
            LvdsStreamDecoder::new(),
            ReaderConfig::default(),
        )
        .unwrap();

        sender
            .send_to(&lvds_packet(37, 1_700_000_000), endpoint)
            .unwrap();
        let frame = loop {
            match worker
                .events()
                .recv_timeout(Duration::from_secs(1))
                .unwrap()
            {
                ReaderEvent::Frame(frame) => break frame,
                ReaderEvent::Started { .. } => {}
                event => panic!("unexpected UDP reader event: {event:?}"),
            }
        };
        assert_eq!(frame.sequence, 0);
        assert_eq!(frame.timestamp.utc_seconds, 1_700_000_000);
        assert_eq!(frame.channel_1[0], 2_085);
        worker.shutdown().unwrap();
    }
}
