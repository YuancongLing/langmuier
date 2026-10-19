//! Protocol and state primitives for the Langmuir probe control application.
//!
//! This crate deliberately contains no serial, UI, storage, or mock-data code.
//! Decoders are pure functions and stream framers accept arbitrary input chunks.

mod checksum;
mod command;
mod error;
mod framing;
mod lvds;
mod model;
mod rs422;
mod udp_lvds;
mod udp_lvds_dedup;
mod udp_lvds_sequence;

pub use checksum::{additive_checksum, crc16_ccitt_false};
pub use command::{
    COMMAND_FRAME_LEN, Command, CommandError, CommandFrame, CommandFrameError, MAX_SCAN_FREQUENCY,
    ScanFrequency, command_checksum, validate_command_frame,
};
pub use error::{FrameError, Protocol};
pub use framing::{DecodeBatch, LvdsStreamDecoder, Rs422StreamDecoder, StreamMetrics};
pub use lvds::{
    LVDS_CRC_CALC_END, LVDS_CRC_CALC_START, LVDS_CRC_FIELD_OFFSET, LVDS_CRC_INPUT_LEN,
    LVDS_DATA_TYPE, LVDS_DEVICE_ID, LVDS_FRAME_LEN, LVDS_PAYLOAD_LEN, LVDS_RECEIVER_ADDRESS,
    LVDS_SENDER_ADDRESS, SAMPLE_COUNT, SAMPLE_INTERVAL_MICROS, ScientificFrame,
    ScientificFrameDiagnosis, Timestamp, UnverifiedScientificFrame, decode_scientific_frame,
    decode_scientific_frame_with_sequence, diagnose_scientific_frame,
    validate_scientific_frame_crc16,
};
pub use model::{
    AcquisitionState, CommandCounters, CoreMetrics, DeviceState, LinkState, ScientificFrameSummary,
};
pub use rs422::{
    RS422_DEVICE_ID, RS422_FRAME_LEN, RS422_PAYLOAD_LEN, RS422_RECEIVER_ADDRESS,
    RS422_SENDER_ADDRESS, RS422_TELEMETRY_TYPE, Rs422Telemetry, decode_rs422_telemetry,
};
pub use udp_lvds::{
    DEFAULT_UDP_LVDS_REASSEMBLY_TTL, DEPLOYED_LVDS_RAW_FRAME_LEN, ReassembledLvdsFrame,
    UDP_LVDS_HEADER_LEN, UDP_LVDS_MAGIC, UDP_LVDS_TYPE, UDP_LVDS_VERSION, UdpLvdsDatagram,
    UdpLvdsError, UdpLvdsHeader, UdpLvdsReassembler, UdpLvdsReassemblyMetrics,
    parse_udp_lvds_datagram,
};
pub use udp_lvds_dedup::{
    DEFAULT_UDP_LVDS_DEDUP_CAPACITY, DEFAULT_UDP_LVDS_DEDUP_TTL, UdpLvdsDedupMetrics,
    UdpLvdsDisposition, UdpLvdsScienceGate, UdpLvdsScienceResult,
};
pub use udp_lvds_sequence::{
    DEFAULT_UDP_SEQUENCE_CAPACITY, DEFAULT_UDP_SEQUENCE_EXPIRED_HISTORY,
    DEFAULT_UDP_SEQUENCE_GAP_WAIT, DEFAULT_UDP_SEQUENCE_MAX_ADVANCE, UdpLvdsSequenceMetrics,
    UdpLvdsSequenceMonitor,
};

/// Two-byte marker shared by all supported wire protocols.
pub const FRAME_HEADER: [u8; 2] = [0xEB, 0x90];

/// Two-byte trailer shared by all supported wire protocols.
pub const FRAME_TAIL: [u8; 2] = [0x09, 0xD7];
