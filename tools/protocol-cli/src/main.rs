use std::env;
use std::fs;
use std::process::ExitCode;

use langmuir_core::{Command, decode_rs422_telemetry, decode_scientific_frame};

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
                "ch1_time={:?}\nch2_time={:?}\nch1_first={}\nch2_first={}\nsamples={}",
                frame.channel_1_time,
                frame.channel_2_time,
                frame.channel_1[0],
                frame.channel_2[0],
                frame.channel_1.len()
            ))
        }
        _ => Err(format!("unknown action: {action}")),
    }
}

fn encode_command(arguments: &[String]) -> Result<String, String> {
    let kind = arguments
        .first()
        .map(String::as_str)
        .ok_or("missing command kind")?;
    let command = match kind {
        "telemetry" => Command::RequestTelemetry,
        "frequency" => Command::ConfigureFrequency(parse_number(arguments.get(1), "frequency")?),
        "mode" => Command::ConfigureMode(parse_number(arguments.get(1), "mode")?),
        "voltage" => Command::ConfigureVoltage(parse_number(arguments.get(1), "voltage")?),
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
            let text = std::str::from_utf8(pair).expect("ASCII hex pair");
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
    "usage:\n  langmuir-protocol-cli command telemetry\n  langmuir-protocol-cli command frequency <u8>\n  langmuir-protocol-cli command mode <u8>\n  langmuir-protocol-cli command voltage <u16>\n  langmuir-protocol-cli command lvds <on|off>\n  langmuir-protocol-cli decode-rs422 <hex>\n  langmuir-protocol-cli decode-lvds <binary-file>"
}

#[cfg(test)]
mod tests {
    use super::{parse_hex, run};

    #[test]
    fn command_cli_matches_python_default() {
        let output = run(&["command".to_owned(), "telemetry".to_owned()]).unwrap();
        assert_eq!(output, "EB 90 0A 04 11 9B 9B 9B 9B 9B 9B 9B 9B ED 09 D7");
    }

    #[test]
    fn parses_readable_hex_and_rejects_partial_byte() {
        assert_eq!(parse_hex("0xEB 90_0A").unwrap(), [0xEB, 0x90, 0x0A]);
        assert!(parse_hex("ABC").is_err());
    }

    #[test]
    fn validates_command_ranges() {
        assert!(
            run(&[
                "command".to_owned(),
                "frequency".to_owned(),
                "256".to_owned()
            ])
            .is_err()
        );
        assert!(
            run(&[
                "command".to_owned(),
                "voltage".to_owned(),
                "0x1388".to_owned()
            ])
            .is_ok()
        );
    }
}
