use std::env;
use std::fs::{self, File, OpenOptions};
use std::io::{BufWriter, Write};
use std::net::{SocketAddr, UdpSocket};
use std::path::Path;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use langmuir_core::{
    Command, DEPLOYED_LVDS_RAW_FRAME_LEN, UdpLvdsReassembler, decode_rs422_telemetry,
    decode_scientific_frame, parse_udp_lvds_datagram,
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
            let frame = decode_scientific_frame(&bytes).map_err(|error| error.to_string())?;
            Ok(format!(
                "timestamp={:?}\nsequence={}\nch1_first={}\nch2_first={}\nsamples={}",
                frame.timestamp,
                frame.sequence,
                frame.channel_1[0],
                frame.channel_2[0],
                frame.channel_1.len()
            ))
        }
        "receive-udp-lvds" => receive_udp_lvds(&arguments[1..]),
        _ => Err(format!("unknown action: {action}")),
    }
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
    writeln!(index_writer, "frame_sequence,offset_bytes,frame_bytes")
        .map_err(|error| format!("cannot write {index_path}: {error}"))?;

    let deadline = Instant::now() + Duration::from_secs(timeout_seconds);
    let mut buffer = vec![0_u8; 65_535];
    let mut reassembler = UdpLvdsReassembler::default();
    let mut received_datagrams = 0_u64;
    let mut rejected_datagrams = 0_u64;
    let mut completed_frames = 0_usize;
    let mut output_offset = 0_u64;

    while completed_frames < frame_count && Instant::now() < deadline {
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
            continue;
        };
        if usize::try_from(datagram.header.frame_length).ok() != Some(DEPLOYED_LVDS_RAW_FRAME_LEN) {
            rejected_datagrams = rejected_datagrams.saturating_add(1);
            continue;
        }
        let Ok(frame) = reassembler.push(&buffer[..bytes]) else {
            rejected_datagrams = rejected_datagrams.saturating_add(1);
            continue;
        };
        let Some(frame) = frame else {
            continue;
        };

        debug_assert_eq!(frame.bytes.len(), DEPLOYED_LVDS_RAW_FRAME_LEN);
        output_writer
            .write_all(&frame.bytes)
            .map_err(|error| format!("cannot write {}: {error}", output.display()))?;
        writeln!(
            index_writer,
            "{},{},{}",
            frame.frame_sequence,
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

    if completed_frames != frame_count {
        return Err(format!(
            "timed out after {timeout_seconds}s: completed {completed_frames}/{frame_count} frames, received {received_datagrams} matching UDP datagrams, rejected {rejected_datagrams}"
        ));
    }
    Ok(format!(
        "reassembled {completed_frames} LVDS frames ({output_offset} bytes, {DEPLOYED_LVDS_RAW_FRAME_LEN} bytes/frame) from {received_datagrams} UDP datagrams; index={index_path}"
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
        .chunks_exact(2)
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
    use super::{parse_hex, run};

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
