use std::env;
use std::fs::{self, File, OpenOptions};
use std::io::{BufWriter, Write};
use std::net::{SocketAddr, UdpSocket};
use std::path::Path;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use langmuir_core::{
    Command, DEPLOYED_LVDS_RAW_FRAME_LEN, UdpLvdsReassembler, UdpLvdsScienceGate,
    UdpLvdsSequenceMonitor, decode_rs422_telemetry, decode_scientific_frame,
    parse_udp_lvds_datagram, reconstruct_adc16,
};

fn main() -> ExitCode {
    let arguments: Vec<_> = env::args().skip(1).collect();
    match run(&arguments) {
        Ok(output) => {
            println!("{output}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("error: {error}");
            eprintln!("{}", usage());
            ExitCode::FAILURE
        }
    }
}

fn run(arguments: &[String]) -> Result<String, String> {
    let Some(action) = arguments.first().map(String::as_str) else {
        return Err("missing action".to_owned());
    };
    match action {
        "command" => encode_command(&arguments[1..]),
        "decode-rs422" => {
            let hex = arguments.get(1).ok_or("missing hexadecimal frame")?;
            let bytes = parse_hex(hex)?;
            let telemetry = decode_rs422_telemetry(&bytes).map_err(|error| error.to_string())?;
            Ok(format!("{telemetry:#?}"))
        }
        "decode-lvds" => {
            let path = arguments.get(1).ok_or("missing frame file path")?;
            let bytes = fs::read(path).map_err(|error| format!("cannot read {path}: {error}"))?;
            decode_lvds_output(&bytes)
        }
        "receive-udp-lvds" => receive_udp_lvds(&arguments[1..]),
        _ => Err(format!("unknown action: {action}")),
    }
}

fn decode_lvds_output(bytes: &[u8]) -> Result<String, String> {
    let frame = decode_scientific_frame(bytes).map_err(|error| error.to_string())?;
    Ok(format!(
        "timestamp={:?}\nframe_seq_u8={}\nch1_first_code10={}\nch2_first_code10={}\nch1_first_adc16_reconstructed={}\nch2_first_adc16_reconstructed={}\nsamples={}\nintegrity=CRC_NOT_CHECKED",
        frame.timestamp,
        frame.frame_seq_u8,
        frame.channel_1[0],
        frame.channel_2[0],
        reconstruct_adc16(frame.channel_1[0]),
        reconstruct_adc16(frame.channel_2[0]),
        frame.channel_1.len()
    ))
}

#[allow(clippy::too_many_lines)]
fn receive_udp_lvds(arguments: &[String]) -> Result<String, String> {
    let bind_address = parse_socket_address(arguments.first(), "bind address")?;
    let source_address = parse_socket_address(arguments.get(1), "source address")?;
    let frame_count: usize = parse_number(arguments.get(2), "frame count")?;
    if frame_count == 0 {
        return Err("frame count must be non-zero".to_owned());
    }
    let output_path = arguments.get(3).ok_or("missing output file path")?;
    let timeout_seconds: u64 = match arguments.get(4) {
        Some(value) => parse_number(Some(value), "timeout seconds")?,
        None => 10,
    };
    if timeout_seconds == 0 {
        return Err("timeout seconds must be non-zero".to_owned());
    }

    let output = Path::new(output_path);
    if output.exists() {
        return Err(format!("output file already exists: {}", output.display()));
    }
    if let Some(parent) = output
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)
            .map_err(|error| format!("cannot create {}: {error}", parent.display()))?;
    }
    let index_path = format!("{output_path}.index.csv");
    if Path::new(&index_path).exists() {
        return Err(format!("index file already exists: {index_path}"));
    }

    let socket = UdpSocket::bind(bind_address)
        .map_err(|error| format!("cannot bind {bind_address}: {error}"))?;
    socket
        .set_read_timeout(Some(Duration::from_millis(100)))
        .map_err(|error| format!("cannot set UDP read timeout: {error}"))?;
    let output_file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(output)
        .map_err(|error| format!("cannot create {}: {error}", output.display()))?;
    let index_file = File::create_new(&index_path)
        .map_err(|error| format!("cannot create {index_path}: {error}"))?;
    let mut output_writer = BufWriter::new(output_file);
    let mut index_writer = BufWriter::new(index_file);
    writeln!(
        index_writer,
        "udp_frame_sequence_u32,frame_seq_u8,offset_bytes,frame_bytes"
    )
    .map_err(|error| format!("cannot write {index_path}: {error}"))?;

    let deadline = Instant::now() + Duration::from_secs(timeout_seconds);
    let mut buffer = vec![0_u8; 65_535];
    let mut reassembler = UdpLvdsReassembler::default();
    // One invocation receives one filtered source endpoint in one session.
    let mut science_gate = UdpLvdsScienceGate::default();
    let mut sequence_monitor = UdpLvdsSequenceMonitor::default();
    let mut received_datagrams = 0_u64;
    let mut rejected_datagrams = 0_u64;
    let mut invalid_udp_datagrams = 0_u64;
    let mut rejected_scientific_frames = 0_u64;
    let mut last_scientific_error = None;
    let mut completed_frames = 0_usize;
    let mut output_offset = 0_u64;

    while completed_frames < frame_count && Instant::now() < deadline {
        // Also reclaim partial frames while recv_from times out or the source
        // is idle; push() performs the same cleanup before checking capacity.
        reassembler.expire();
        let now = Instant::now();
        science_gate.expire_at(now);
        sequence_monitor.expire_at(now);
        let (bytes, source) = match socket.recv_from(&mut buffer) {
            Ok(result) => result,
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
                ) =>
            {
                continue;
            }
            Err(error) => return Err(format!("UDP receive failed: {error}")),
        };
        if source != source_address {
            continue;
        }
        received_datagrams = received_datagrams.saturating_add(1);
        let Ok(datagram) = parse_udp_lvds_datagram(&buffer[..bytes]) else {
            rejected_datagrams = rejected_datagrams.saturating_add(1);
            invalid_udp_datagrams = invalid_udp_datagrams.saturating_add(1);
            continue;
        };
        if usize::try_from(datagram.header.frame_length).ok() != Some(DEPLOYED_LVDS_RAW_FRAME_LEN) {
            rejected_datagrams = rejected_datagrams.saturating_add(1);
            invalid_udp_datagrams = invalid_udp_datagrams.saturating_add(1);
            continue;
        }
        let Ok(frame) = reassembler.push(&buffer[..bytes]) else {
            rejected_datagrams = rejected_datagrams.saturating_add(1);
            continue;
        };
        let Some(frame) = frame else {
            continue;
        };

        // The shared gate validates science before remembering a full u32
        // sequence and its bytes. Duplicate/conflicting frames never reach
        // either capture file or the requested unique-frame count.
        let now = Instant::now();
        let scientific = match science_gate.accept_at(&frame, now) {
            Ok(result) => match result.into_frame() {
                Some(scientific) => scientific,
                None => continue,
            },
            Err(error) => {
                rejected_scientific_frames = rejected_scientific_frames.saturating_add(1);
                last_scientific_error = Some(error.to_string());
                continue;
            }
        };
        // Observe only Accepted science using the untruncated wrapper tag.
        // This never reorders capture writes or inserts missing samples.
        sequence_monitor.observe_at(frame.frame_sequence, now);
        debug_assert_eq!(frame.bytes.len(), DEPLOYED_LVDS_RAW_FRAME_LEN);
        output_writer
            .write_all(&frame.bytes)
            .map_err(|error| format!("cannot write {}: {error}", output.display()))?;
        writeln!(
            index_writer,
            "{},{},{},{}",
            frame.frame_sequence,
            scientific.frame_seq_u8,
            output_offset,
            frame.bytes.len()
        )
        .map_err(|error| format!("cannot write {index_path}: {error}"))?;
        output_offset = output_offset
            .saturating_add(u64::try_from(frame.bytes.len()).expect("frame length fits in u64"));
        completed_frames += 1;
    }
    output_writer
        .flush()
        .map_err(|error| format!("cannot flush {}: {error}", output.display()))?;
    index_writer
        .flush()
        .map_err(|error| format!("cannot flush {index_path}: {error}"))?;

    reassembler.expire();
    let now = Instant::now();
    science_gate.expire_at(now);
    sequence_monitor.expire_at(now);
    let reassembly = reassembler.metrics();
    let fragment_errors = invalid_udp_datagrams.saturating_add(reassembly.fragment_errors);
    let reassembly_diagnostics = format!(
        "UDP fragment errors={fragment_errors}; reassembled={}; expired incomplete frames={}; capacity rejections={}; pending={}; reserved payload bytes={}",
        reassembly.completed_frames,
        reassembly.expired_frames,
        reassembly.capacity_rejections,
        reassembly.in_flight_frames,
        reassembly.buffered_payload_bytes,
    );
    let dedup = science_gate.metrics();
    let scientific_diagnostics = format!(
        "science parsed={}; integrity=CRC_NOT_CHECKED; duplicates suppressed={}; sequence conflicts={}; unique frames written={completed_frames}; dedup cached={}; dedup payload bytes={}; rejected scientific frames={rejected_scientific_frames}; last scientific error={}",
        dedup.validated_frames,
        dedup.duplicate_frames,
        dedup.sequence_conflicts,
        dedup.cached_frames,
        dedup.cached_payload_bytes,
        last_scientific_error.as_deref().unwrap_or("none")
    );
    let sequence_diagnostics = format!(
        "Accepted-science sequence quality={:?} (unobserved valid science is not proof of network loss)",
        sequence_monitor.metrics()
    );
    if completed_frames != frame_count {
        return Err(format!(
            "timed out after {timeout_seconds}s: completed {completed_frames}/{frame_count} frames, received {received_datagrams} matching UDP datagrams, rejected datagrams={rejected_datagrams}; {reassembly_diagnostics}; {scientific_diagnostics}; {sequence_diagnostics}"
        ));
    }
    Ok(format!(
        "parsed {completed_frames} LVDS frames ({output_offset} bytes, {DEPLOYED_LVDS_RAW_FRAME_LEN} bytes/frame) from {received_datagrams} UDP datagrams; rejected datagrams={rejected_datagrams}; {reassembly_diagnostics}; {scientific_diagnostics}; {sequence_diagnostics}; index={index_path}"
    ))
}

fn encode_command(arguments: &[String]) -> Result<String, String> {
    let kind = arguments
        .first()
        .map(String::as_str)
        .ok_or("missing command kind")?;
    let command = match kind {
        "telemetry" => Command::RequestTelemetry,
        "triangle" => {
            Command::configure_triangle(parse_number(arguments.get(1), "triangle frequency")?)
                .map_err(|error| error.to_string())?
        }
        "sine" => Command::configure_sine(parse_number(arguments.get(1), "sine frequency")?)
            .map_err(|error| error.to_string())?,
        "dc" => Command::ConfigureDc(parse_number(arguments.get(1), "DC voltage")?),
        "frequency" => {
            return Err(
                "'frequency' is ambiguous; use 'triangle <0..=100>' or 'sine <0..=100>'".to_owned(),
            );
        }
        "mode" => {
            return Err(
                "'mode' is ambiguous; use 'triangle <0..=100>', 'sine <0..=100>', or 'dc <u16>'"
                    .to_owned(),
            );
        }
        "voltage" => {
            return Err("'voltage' is deprecated; use 'dc <raw-u16>'".to_owned());
        }
        "lvds" => match arguments.get(1).map(String::as_str) {
            Some("on") => Command::SetLvdsEnabled(true),
            Some("off") => Command::SetLvdsEnabled(false),
            _ => return Err("LVDS state must be 'on' or 'off'".to_owned()),
        },
        _ => return Err(format!("unknown command kind: {kind}")),
    };
    Ok(format_hex(command.encode().as_bytes()))
}

fn parse_number<T>(value: Option<&String>, name: &str) -> Result<T, String>
where
    T: TryFrom<u64>,
{
    let value = value.ok_or_else(|| format!("missing {name}"))?;
    let parsed = if let Some(hex) = value
        .strip_prefix("0x")
        .or_else(|| value.strip_prefix("0X"))
    {
        u64::from_str_radix(hex, 16)
    } else {
        value.parse()
    }
    .map_err(|_| format!("invalid {name}: {value}"))?;
    T::try_from(parsed).map_err(|_| format!("{name} is out of range: {value}"))
}

fn parse_socket_address(value: Option<&String>, name: &str) -> Result<SocketAddr, String> {
    let value = value.ok_or_else(|| format!("missing {name}"))?;
    value
        .parse()
        .map_err(|_| format!("invalid {name}: {value}"))
}

fn parse_hex(input: &str) -> Result<Vec<u8>, String> {
    let compact: String = input
        .chars()
        .filter(|character| !character.is_ascii_whitespace() && *character != '_')
        .collect();
    let compact = compact
        .strip_prefix("0x")
        .or_else(|| compact.strip_prefix("0X"))
        .unwrap_or(&compact);
    if !compact.len().is_multiple_of(2) {
        return Err("hex input must contain complete bytes".to_owned());
    }
    compact
        .as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| {
            let text = std::str::from_utf8(pair)
                .map_err(|_| "hex input must contain only ASCII digits".to_owned())?;
            u8::from_str_radix(text, 16).map_err(|_| format!("invalid hex byte: {text}"))
        })
        .collect()
}

fn format_hex(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|byte| format!("{byte:02X}"))
        .collect::<Vec<_>>()
        .join(" ")
}

fn usage() -> &'static str {
    "usage:\n  langmuir-protocol-cli command telemetry\n  langmuir-protocol-cli command triangle <0..=100>\n  langmuir-protocol-cli command sine <0..=100>\n  langmuir-protocol-cli command dc <raw-u16>\n  langmuir-protocol-cli command lvds <on|off>\n  langmuir-protocol-cli decode-rs422 <hex>\n  langmuir-protocol-cli decode-lvds <binary-file>\n  langmuir-protocol-cli receive-udp-lvds <bind-ip:port> <source-ip:port> <frame-count> <output-file> [timeout-seconds]\n\nLegacy 'frequency', 'mode', and 'voltage' command names are rejected as ambiguous; use the explicit names above."
}

#[cfg(test)]
mod tests {
    use std::net::UdpSocket;
    use std::time::{Duration, SystemTime};

    use super::{decode_lvds_output, parse_hex, receive_udp_lvds, run};

    fn golden_frame(frame_seq_u8: u8) -> [u8; 269] {
        // Literal packed vectors supplied with the protocol specification.
        // The reserved CRC bytes intentionally carry no acceptance claim.
        let mut frame = [0_u8; 269];
        frame[..15].copy_from_slice(&[
            0xEB, 0x90, 0x55, 0xAA, 0x01, 0x07, 0xAA, 0x65, 0x00, 0x00, 0x00, 0x00, 0x0F, 0x41,
            0x78,
        ]);
        for group in frame[15..140].as_chunks_mut::<5>().0 {
            group.copy_from_slice(&[0xFF, 0xC0, 0x08, 0x00, 0x01]);
        }
        for group in frame[140..265].as_chunks_mut::<5>().0 {
            group.copy_from_slice(&[0x00, 0x3F, 0xF0, 0x03, 0xFF]);
        }
        frame[265] = frame_seq_u8;
        frame[267..].copy_from_slice(&[0x12, 0x34]);
        frame
    }

    #[test]
    fn lvds_cli_decodes_golden_frame_and_ignores_crc_trailer_changes() {
        let mut frame = golden_frame(255);
        let output = decode_lvds_output(&frame).unwrap();
        assert!(output.contains("utc_seconds: 1694498816"));
        assert!(output.contains("microseconds: 999800"));
        assert!(output.contains("frame_seq_u8=255"));
        assert!(output.contains("ch1_first_code10=1023"));
        assert!(output.contains("ch2_first_code10=0"));
        assert!(output.contains("ch1_first_adc16_reconstructed=32704"));
        assert!(output.contains("ch2_first_adc16_reconstructed=-32768"));
        assert!(output.contains("samples=100"));

        frame[268] ^= 1;
        assert_eq!(decode_lvds_output(&frame).unwrap(), output);
        assert!(output.contains("integrity=CRC_NOT_CHECKED"));
        frame[11..15].copy_from_slice(&1_000_000_u32.to_be_bytes());
        assert!(
            decode_lvds_output(&frame)
                .unwrap_err()
                .contains("microseconds")
        );
    }

    #[test]
    fn udp_cli_capture_preserves_outer_u32_and_inner_seq8() {
        let reservation = UdpSocket::bind("127.0.0.1:0").unwrap();
        let bind_address = reservation.local_addr().unwrap();
        let source = UdpSocket::bind("127.0.0.1:0").unwrap();
        let source_address = source.local_addr().unwrap();
        let unique = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory =
            std::env::temp_dir().join(format!("langmuir-cli-udp-{}-{unique}", std::process::id()));
        std::fs::create_dir(&directory).unwrap();
        let output_path = directory.join("capture.bin");
        let arguments = vec![
            bind_address.to_string(),
            source_address.to_string(),
            "2".to_owned(),
            output_path.to_str().unwrap().to_owned(),
            "2".to_owned(),
        ];
        drop(reservation);
        let receiver = std::thread::spawn(move || receive_udp_lvds(&arguments));
        let first = golden_frame(255);
        let second = golden_frame(0);
        for _ in 0..200 {
            for (sequence, frame) in [(65_535_u32, &first), (65_536, &second)] {
                let mut datagram = vec![0_u8; 24];
                datagram[..4].copy_from_slice(&[0x55, 0xAA, 0x01, 0xA5]);
                datagram[4..8].copy_from_slice(&sequence.to_be_bytes());
                datagram[8..12].copy_from_slice(&269_u32.to_be_bytes());
                datagram[14..16].copy_from_slice(&1_u16.to_be_bytes());
                datagram[20..22].copy_from_slice(&269_u16.to_be_bytes());
                datagram.extend_from_slice(frame);
                source.send_to(&datagram, bind_address).unwrap();
            }
            if receiver.is_finished() {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        let result = receiver.join().unwrap().unwrap();
        assert!(result.contains("unique frames written=2"));
        assert_eq!(
            std::fs::read(&output_path).unwrap(),
            [first, second].concat()
        );
        let index_path = format!("{}.index.csv", output_path.display());
        assert_eq!(
            std::fs::read_to_string(index_path).unwrap(),
            "udp_frame_sequence_u32,frame_seq_u8,offset_bytes,frame_bytes\n65535,255,0,269\n65536,0,269,269\n"
        );
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn command_cli_matches_python_default() {
        let output = run(&["command".to_owned(), "telemetry".to_owned()]).unwrap();
        assert_eq!(output, "EB 90 F0 55 00 06 11 9B 9B 9B 9B 7D");
    }

    #[test]
    fn parses_readable_hex_and_rejects_partial_byte() {
        assert_eq!(parse_hex("0xEB 90_0A").unwrap(), [0xEB, 0x90, 0x0A]);
        assert!(parse_hex("ABC").is_err());
        assert!(parse_hex("€€").is_err());
    }

    #[test]
    fn validates_command_ranges() {
        assert!(
            run(&[
                "command".to_owned(),
                "triangle".to_owned(),
                "101".to_owned()
            ])
            .is_err()
        );
        assert!(run(&["command".to_owned(), "sine".to_owned(), "101".to_owned()]).is_err());
        assert!(run(&["command".to_owned(), "sine".to_owned(), "0".to_owned()]).is_ok());
        assert!(run(&["command".to_owned(), "dc".to_owned(), "0x1388".to_owned(),]).is_ok());
        assert!(
            run(&["command".to_owned(), "mode".to_owned(), "0xAA".to_owned(),])
                .unwrap_err()
                .contains("ambiguous")
        );
    }

    #[test]
    fn command_cli_matches_all_golden_vectors() {
        let cases = [
            (vec!["telemetry"], "EB 90 F0 55 00 06 11 9B 9B 9B 9B 7D"),
            (
                vec!["triangle", "50"],
                "EB 90 F0 55 00 06 21 55 32 9B 9B DE",
            ),
            (vec!["sine", "50"], "EB 90 F0 55 00 06 21 AA 32 9B 9B 33"),
            (vec!["dc", "1000"], "EB 90 F0 55 00 06 21 BB 03 E8 9B 62"),
            (vec!["lvds", "on"], "EB 90 F0 55 00 06 31 55 9B 9B 9B 57"),
            (vec!["lvds", "off"], "EB 90 F0 55 00 06 31 AA 9B 9B 9B AC"),
        ];
        for (arguments, expected) in cases {
            let arguments = arguments.into_iter().map(str::to_owned).collect::<Vec<_>>();
            assert_eq!(
                run(&[vec!["command".to_owned()], arguments].concat()).unwrap(),
                expected
            );
        }
    }
}
