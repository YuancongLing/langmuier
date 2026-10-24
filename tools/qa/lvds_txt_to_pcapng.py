"""Wrap original LVDS HEX bytes for offline GUI testing, without decoding them.

The output is derived test data, never an original network capture. Both IPv4
endpoints must be supplied explicitly. No device, socket, or production setting
is touched. Existing captures, metadata, and the input are never overwritten.
"""
import argparse
import hashlib
import ipaddress
import json
from pathlib import Path
import re
import struct

from lvds_10bit_reference import block


PROVENANCE = '用户原始 LVDS HEX + 人工 UDP/PCAPNG 封装'
QA_ROOT = Path(__file__).resolve().parents[2] / 'target/qa'


def _parse_hex_bytes(data):
    try:
        text = data.decode('utf-8')
    except UnicodeDecodeError as error:
        line = data[:error.start].count(b'\n') + 1
        raise ValueError(f'line {line}: invalid UTF-8 text') from error
    lines = text.split('\n')
    if len(lines) > 1 and lines[-1] == '':
        lines.pop()  # A final newline terminates a line; it is not an empty line.
    frames = []
    for number, raw in enumerate(lines, 1):
        line = raw.strip()
        if not line:
            raise ValueError(f'line {number}: empty HEX frame')
        if re.fullmatch('[0-9a-fA-F]+', line) is None:
            raise ValueError(f'line {number}: non-HEX character or internal whitespace')
        if len(line) % 2:
            raise ValueError(f'line {number}: odd number of HEX characters ({len(line)})')
        frame = bytes.fromhex(line)
        if len(frame) != 269:
            raise ValueError(f'line {number}: expected 269 bytes, received {len(frame)}')
        frames.append(frame)
    return frames


def read_hex_frames(path):
    """Read strictly one 269-byte continuous HEX frame per line, in input order.

    Format errors raise ValueError with a one-based line number. I/O errors are
    left as OSError. Inner science fields and CRC bytes are never altered.
    """
    return _parse_hex_bytes(Path(path).read_bytes())


def _endpoint(value, label):
    try:
        address, port = value
        if not isinstance(address, str):
            raise ValueError('IPv4 address must be text')
        parsed = ipaddress.IPv4Address(address)
        if type(port) is not int or not 1 <= port <= 65535:
            raise ValueError('UDP port must be in 1..65535')
    except (TypeError, ValueError) as error:
        raise ValueError(f'invalid {label} endpoint: {error}') from error
    return parsed, port


def _aliases(first, second):
    if first.resolve() == second.resolve():
        return True
    return first.exists() and second.exists() and first.samefile(second)


def _ipv4_checksum(header):
    value = sum(struct.unpack('>10H', header))
    while value >> 16:
        value = (value & 0xffff) + (value >> 16)
    return ~value & 0xffff


def _capture(frames, source, destination, start_sequence):
    comment = (PROVENANCE + '; derived test fixture, NOT an original NIC capture').encode('utf-8')
    options = struct.pack('<HH', 1, len(comment)) + comment + b'\0' * (-len(comment) % 4)
    options += struct.pack('<HH', 0, 0)
    data = bytearray(block(0x0a0d0d0a, struct.pack('<IHHq', 0x1a2b3c4d, 1, 0, -1) + options))
    data += block(1, struct.pack('<HHI', 1, 0, 65535))  # Ethernet; default microsecond timestamps.
    for index, frame in enumerate(frames):
        envelope = bytes.fromhex('55aa01a5') + struct.pack('>IIHHIHH', start_sequence + index, 269, 0, 1, 0, 269, 0)
        udp = struct.pack('>HHHH', source[1], destination[1], 8 + 24 + len(frame), 0) + envelope + frame
        ip = bytes([0x45, 0]) + struct.pack('>HHHBBH', 20 + len(udp), index % 65536, 0, 64, 17, 0)
        ip += source[0].packed + destination[0].packed
        ip = ip[:10] + struct.pack('>H', _ipv4_checksum(ip)) + ip[12:]
        # Locally administered test MAC addresses; these identify no hardware.
        ethernet = bytes.fromhex('0200000000010200000000020800') + ip + udp
        timestamp = index * 50_000
        data += block(6, struct.pack('<IIIII', 0, timestamp >> 32, timestamp & 0xffffffff, len(ethernet), len(ethernet)) + ethernet)
    return bytes(data)


def convert(input_path, output_path, *, source, destination, start_sequence):
    """Exclusively create a derived PCAPNG plus ``.provenance.json`` sidecar.

    Output is restricted to this project's target/qa directory. The returned
    provenance includes hashes of the exact input bytes and generated capture.
    Science addresses are deliberately not validated here: the production
    decoder remains responsible for accepting or rejecting unchanged payloads.
    """
    input_path, output_path = Path(input_path), Path(output_path)
    metadata_path = output_path.with_name(output_path.name + '.provenance.json')
    for path in (output_path, metadata_path):
        if _aliases(input_path, path):
            raise ValueError('input and output/provenance must not alias')
    if output_path.suffix.lower() != '.pcapng':
        raise ValueError('output must have the .pcapng extension')
    try:
        output_path.resolve().relative_to(QA_ROOT.resolve())
    except ValueError as error:
        raise ValueError('output must be inside the project target/qa directory') from error
    for path in (output_path, metadata_path):
        if path.exists():
            raise FileExistsError(f'refusing to overwrite existing file: {path}')
    source = _endpoint(source, 'source')
    destination = _endpoint(destination, 'destination')
    original = input_path.read_bytes()
    frames = _parse_hex_bytes(original)
    if type(start_sequence) is not int or not 0 <= start_sequence <= 0xffffffff:
        raise ValueError('start_sequence must be a u32 integer')
    if start_sequence + len(frames) - 1 > 0xffffffff:
        raise ValueError('incrementing outer sequence would exceed u32')
    capture = _capture(frames, source, destination, start_sequence)
    provenance = {
        'source': PROVENANCE,
        'not_original_network_capture': True,
        'input': {'path': str(input_path.resolve()), 'bytes': len(original), 'sha256': hashlib.sha256(original).hexdigest()},
        'output': {'path': str(output_path.resolve()), 'bytes': len(capture), 'sha256': hashlib.sha256(capture).hexdigest()},
        'frame_count': len(frames),
        'inner_frame_bytes': 269,
        'inner_bytes_and_order_preserved': True,
        'udp_wrapper_bytes': 24,
        'source_endpoint': {'address': str(source[0]), 'port': source[1]},
        'destination_endpoint': {'address': str(destination[0]), 'port': destination[1]},
        'outer_sequence_start': start_sequence,
        'outer_sequence_end': start_sequence + len(frames) - 1,
        'packet_interval_us': 50_000,
        'ethernet_addresses': 'Locally administered artificial test MACs; no real hardware identity',
    }
    metadata = (json.dumps(provenance, ensure_ascii=False, indent=2) + '\n').encode('utf-8')
    output_path.parent.mkdir(parents=True, exist_ok=True)
    created = []
    try:
        for path, content in ((output_path, capture), (metadata_path, metadata)):
            with path.open('xb') as stream:
                created.append(path)
                stream.write(content)
    except OSError:
        for path in created:
            path.unlink()
        raise
    return provenance


def _cli_endpoint(text):
    try:
        address, port = text.rsplit(':', 1)
        endpoint = _endpoint((address, int(port)), 'test')
    except ValueError as error:
        raise argparse.ArgumentTypeError(str(error)) from error
    return str(endpoint[0]), endpoint[1]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('input', type=Path, help='original LVDS HEX text; never modified')
    parser.add_argument('output', type=Path, help='new .pcapng path under target/qa')
    parser.add_argument('--source', type=_cli_endpoint, required=True, metavar='IPv4:PORT')
    parser.add_argument('--destination', type=_cli_endpoint, required=True, metavar='IPv4:PORT')
    parser.add_argument('--start-sequence', type=int, default=0, help='first artificial outer u32 sequence (default: 0)')
    args = parser.parse_args()
    try:
        provenance = convert(args.input, args.output, source=args.source, destination=args.destination, start_sequence=args.start_sequence)
    except (OSError, ValueError) as error:
        parser.error(str(error))
    print(json.dumps(provenance, ensure_ascii=True, indent=2))


if __name__ == '__main__':
    main()
