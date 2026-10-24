"""Independent synthetic fixture and CSV/LPD-v3 readback; never hardware evidence.

Python standard library only. The reference packs/decodes a 40-bit integer,
independently of the Rust decoder's byte-field implementation.
"""
import argparse
import csv
import hashlib
import json
import math
import struct
from pathlib import Path


def block(kind, body):
    body += b'\0' * (-len(body) % 4)
    length = len(body) + 12
    return struct.pack('<II', kind, length) + body + struct.pack('<I', length)


def pack_channel(values):
    result = bytearray()
    for offset in range(0, 100, 4):
        word = 0
        for value in values[offset:offset + 4]:
            assert 0 <= value <= 1023
            word = word * 1024 + value
        result += word.to_bytes(5, 'big')
    return result


def generate(path, count):
    """Create clearly named synthetic PCAPNG with a device-clock rollback."""
    data = bytearray(block(0x0A0D0D0A, struct.pack('<IHHq', 0x1A2B3C4D, 1, 0, -1)))
    data += block(1, struct.pack('<HHI', 1, 0, 65535))
    for index in range(count):
        if index == 0:
            ch1 = [1023, 0, 512, 1] * 25
            ch2 = [0, 1023, 0, 1023] * 25
        else:
            ch1 = [round(512 + 460 * math.sin((index * 100 + point) * 2 * math.pi / 151)) for point in range(100)]
            ch2 = [round(512 + 320 * math.cos((index * 100 + point) * 2 * math.pi / 239)) for point in range(100)]
        time_us = 148_999_800 + index * 50_000 - (2_000_000 if index >= 100 else 0)
        seconds, micros = divmod(time_us, 1_000_000)
        wire = bytes.fromhex('eb9055aa0107aa') + struct.pack('>II', seconds, micros)
        wire += pack_channel(ch1) + pack_channel(ch2) + bytes([(255 + index) % 256, 0, index % 256, (index * 13) % 256])
        assert len(wire) == 269
        envelope = bytes.fromhex('55aa01a5') + struct.pack('>IIHHIHH', 0x1234FFFF + index, 269, 0, 1, 0, 269, 0)
        udp = struct.pack('>HHHH', 1534, 1534, len(envelope) + len(wire) + 8, 0) + envelope + wire
        ip = bytes([0x45, 0]) + struct.pack('>HHHBBH', 20 + len(udp), index, 0, 64, 17, 0)
        ip += bytes([192, 168, 1, 128, 192, 168, 1, 100])
        ethernet = bytes.fromhex('00112233445566778899aabb0800') + ip + udp
        packet_us = index * 50_000
        data += block(6, struct.pack('<IIIII', 0, packet_us >> 32, packet_us & 0xFFFFFFFF, len(ethernet), len(ethernet)) + ethernet)
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(data)


def reference_frames(path):
    data = path.read_bytes()
    frames = []
    offset = 0
    total = 0
    while offset < len(data):
        kind, length = struct.unpack_from('<II', data, offset)
        assert length >= 12 and struct.unpack_from('<I', data, offset + length - 4)[0] == length
        if kind == 6:
            total += 1
            captured_len = struct.unpack_from('<I', data, offset + 20)[0]
            ethernet = data[offset + 28:offset + 28 + captured_len]
            assert ethernet[12:14] == bytes.fromhex('0800')
            ip = ethernet[14:]
            assert ip[9] == 17
            udp = ip[(ip[0] & 15) * 4:]
            envelope = udp[8:]
            assert envelope[:4] == bytes.fromhex('55aa01a5')
            wire = envelope[24:]
            if wire[:7] != bytes.fromhex('eb9055aa0107aa') or len(wire) != 269 or wire[266] != 0:
                offset += length
                continue
            seconds, micros = struct.unpack_from('>II', wire, 7)
            if micros >= 1_000_000:
                offset += length
                continue
            channels = []
            for start in (15, 140):
                codes = []
                for group in range(25):
                    word = int.from_bytes(wire[start + group * 5:start + group * 5 + 5], 'big')
                    codes.extend((word >> shift) & 1023 for shift in (30, 20, 10, 0))
                channels.append(codes)
            frames.append((seconds, micros, wire[265], *channels))
        offset += length
    return total, frames


def verify_csv(path, frames):
    with path.open(encoding='utf-8', newline='') as stream:
        reader = csv.reader(stream)
        assert next(reader) == 'frame_index,frame_seconds,frame_microseconds,frame_seq_u8,sample_index,sample_time_us,ch1_code10,ch2_code10,ch1_adc16_reconstructed,ch2_adc16_reconstructed,integrity'.split(',')
        for index, (seconds, micros, sequence, ch1, ch2) in enumerate(frames):
            for point in range(100):
                values = next(reader)
                assert len(values) == 11 and values[-1] == 'CRC_NOT_CHECKED'
                expected = [index, seconds, micros, sequence, point, seconds * 1_000_000 + micros + point * 500, ch1[point], ch2[point], ch1[point] * 64 - 32768, ch2[point] * 64 - 32768]
                assert list(map(int, values[:-1])) == expected, (index, point, values, expected)
        assert next(reader, None) is None


def verify_lpd(path, frames):
    data = path.read_bytes()
    assert struct.unpack_from('<8sHHIIIIIIHH', data) == (b'LMPROBE\0', 3, 40, 0x01020304, 100, 500, 409, 1, 3, 2, 8)
    assert len(data) == 40 + len(frames) * 409
    for index, (seconds, micros, sequence, ch1, ch2) in enumerate(frames):
        offset = 40 + index * 409
        assert struct.unpack_from('<IIB', data, offset) == (seconds, micros, sequence)
        assert list(struct.unpack_from('<100H', data, offset + 9)) == ch1
        assert list(struct.unpack_from('<100H', data, offset + 209)) == ch2


def info(path):
    return {'path': str(path.resolve()), 'bytes': path.stat().st_size, 'sha256': hashlib.sha256(path.read_bytes()).hexdigest()}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('capture', type=Path)
    parser.add_argument('--generate', type=int, metavar='SYNTHETIC_FRAME_COUNT')
    parser.add_argument('--csv', type=Path)
    parser.add_argument('--lpd', type=Path)
    parser.add_argument('--json', type=Path)
    args = parser.parse_args()
    if args.generate is not None:
        assert args.generate > 0
        assert 'SYNTHETIC' in args.capture.name, 'Synthetic fixtures must be visibly labeled'
        assert not args.capture.exists(), 'Never overwrite any existing capture'
        generate(args.capture, args.generate)
    total, frames = reference_frames(args.capture)
    if args.csv:
        verify_csv(args.csv, frames)
    if args.lpd:
        verify_lpd(args.lpd, frames)
    report = {'capture': info(args.capture), 'source': 'SYNTHETIC independent fixture; NOT real device acceptance', 'total_packets': total, 'accepted_frames': len(frames), 'csv_data_rows': len(frames) * 100, 'signed_adc_values_checked': len(frames) * 200 if args.csv else 0, 'rollback_indexes': [i for i in range(1, len(frames)) if frames[i][0] * 1_000_000 + frames[i][1] < frames[i - 1][0] * 1_000_000 + frames[i - 1][1]], 'csv': info(args.csv) if args.csv else None, 'lpd_v3': info(args.lpd) if args.lpd else None, 'readback': 'PASS' if args.csv or args.lpd else 'fixture only'}
    if args.json:
        args.json.parent.mkdir(parents=True, exist_ok=True)
        args.json.write_text(json.dumps(report, ensure_ascii=False, indent=2) + '\n', encoding='utf-8')
    print(json.dumps(report, ensure_ascii=False, indent=2))


if __name__ == '__main__':
    main()
