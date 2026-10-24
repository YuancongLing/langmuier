"""Converter contract tests; all frames here are temporary synthetic fixtures."""
import hashlib
import json
from pathlib import Path
import struct
import subprocess
import sys
import tempfile
import unittest

import lvds_txt_to_pcapng as converter


ROOT = Path(__file__).resolve().parents[2]


def synthetic_frame(seed):
    return bytes.fromhex('eb9055aa0107aa') + bytes((index + seed) % 256 for index in range(262))


def pcap_packets(path):
    data = path.read_bytes()
    offset = 0
    packets = []
    while offset < len(data):
        kind, size = struct.unpack_from('<II', data, offset)
        assert size >= 12 and size % 4 == 0
        assert struct.unpack_from('<I', data, offset + size - 4)[0] == size
        if kind == 6:
            interface, high, low, captured, original = struct.unpack_from('<IIIII', data, offset + 8)
            assert interface == 0 and captured == original
            packet = data[offset + 28:offset + 28 + captured]
            packets.append(((high << 32) + low, packet))
        offset += size
    assert offset == len(data)
    return data, packets


class ConverterTests(unittest.TestCase):
    def setUp(self):
        (ROOT / 'target/qa').mkdir(parents=True, exist_ok=True)
        self.temp = tempfile.TemporaryDirectory(prefix='txt-converter-tests-', dir=ROOT / 'target/qa')
        self.addCleanup(self.temp.cleanup)
        self.directory = Path(self.temp.name)
        self.source = self.directory / 'SYNTHETIC-input.txt'
        self.output = self.directory / 'derived.pcapng'
        self.frames = [synthetic_frame(7), synthetic_frame(23)]
        self.original = ('  ' + self.frames[0].hex().upper() + '\t\r\n' + self.frames[1].hex() + '\n').encode()
        self.source.write_bytes(self.original)

    def convert(self, **kwargs):
        options = {'source': ('198.51.100.17', 1534), 'destination': ('203.0.113.19', 4321), 'start_sequence': 0x1234ffff}
        options.update(kwargs)
        return converter.convert(self.source, self.output, **options)

    def test_trim_edges_and_preserve_frame_bytes_and_order(self):
        self.assertEqual(converter.read_hex_frames(self.source), self.frames)
        self.assertEqual(self.source.read_bytes(), self.original)

    def test_malformed_line_reports_line_and_reason(self):
        cases = [('', 'empty'), ('AA GG', 'hex'), ('A' * 537, 'odd'), ('AA' * 268, '269'), ('AA' * 270, '269'), ('AA' * 100 + '\t' + 'AA' * 169, 'hex')]
        for invalid, reason in cases:
            with self.subTest(reason=reason, length=len(invalid)):
                self.source.write_text(self.frames[0].hex() + '\n' + invalid + '\n', encoding='utf-8')
                with self.assertRaisesRegex(ValueError, rf'(?i)line 2.*{reason}'):
                    converter.read_hex_frames(self.source)

    def test_empty_file_and_invalid_utf8_report_line(self):
        for data, line in [(b'', 1), (self.frames[0].hex().encode() + b'\n\xff\n', 2)]:
            with self.subTest(data=data[-5:]):
                self.source.write_bytes(data)
                with self.assertRaisesRegex(ValueError, rf'(?i)line {line}'):
                    converter.read_hex_frames(self.source)

    def test_capture_has_explicit_endpoints_envelope_timing_and_exact_payload(self):
        provenance = self.convert()
        raw, packets = pcap_packets(self.output)
        self.assertIn('用户原始 LVDS HEX + 人工 UDP/PCAPNG 封装'.encode(), raw)
        self.assertEqual(len(packets), 2)
        for index, (time_us, packet) in enumerate(packets):
            self.assertEqual(time_us, index * 50_000)
            self.assertEqual(packet[12:14], b'\x08\x00')
            ip = packet[14:]
            self.assertEqual(ip[0], 0x45)
            self.assertEqual(ip[9], 17)
            self.assertEqual(ip[12:20], bytes([198, 51, 100, 17, 203, 0, 113, 19]))
            words = struct.unpack('>10H', ip[:20])
            checksum = sum(words)
            while checksum >> 16:
                checksum = (checksum & 0xffff) + (checksum >> 16)
            self.assertEqual(checksum, 0xffff)
            udp = ip[20:]
            self.assertEqual(struct.unpack_from('>HHHH', udp), (1534, 4321, 301, 0))
            envelope = udp[8:32]
            self.assertEqual(envelope[:4], bytes.fromhex('55aa01a5'))
            self.assertEqual(struct.unpack_from('>IIHHIHH', envelope, 4), (0x1234ffff + index, 269, 0, 1, 0, 269, 0))
            self.assertEqual(udp[32:], self.frames[index])
        self.assertEqual(provenance['input']['sha256'], hashlib.sha256(self.original).hexdigest())
        self.assertEqual(provenance['output']['sha256'], hashlib.sha256(raw).hexdigest())
        self.assertEqual(provenance['frame_count'], len(self.frames))
        self.assertEqual(provenance['packet_interval_us'], 50_000)
        self.assertEqual(provenance['source_endpoint'], {'address': '198.51.100.17', 'port': 1534})
        self.assertEqual(provenance['destination_endpoint'], {'address': '203.0.113.19', 'port': 4321})
        sidecar = self.output.with_name(self.output.name + '.provenance.json')
        self.assertEqual(json.loads(sidecar.read_text(encoding='utf-8')), provenance)
        self.assertEqual(self.source.read_bytes(), self.original)

    def test_converter_preserves_invalid_inner_protocol_for_negative_fixture(self):
        frame = bytearray(self.frames[0])
        frame[2:4] = bytes.fromhex('04ff')
        self.source.write_text(frame.hex() + '\n', encoding='utf-8')
        self.convert()
        self.assertEqual(pcap_packets(self.output)[1][0][1][66:], frame)

    def test_invalid_input_creates_no_capture_or_provenance(self):
        self.source.write_text('not hex\n', encoding='utf-8')
        with self.assertRaises(ValueError):
            self.convert()
        self.assertFalse(self.output.exists())
        self.assertFalse(self.output.with_name(self.output.name + '.provenance.json').exists())

    def test_existing_capture_and_sidecar_are_never_overwritten(self):
        for path in [self.output, self.output.with_name(self.output.name + '.provenance.json')]:
            with self.subTest(path=path.name):
                path.write_bytes(b'USER-EXISTING')
                with self.assertRaises(FileExistsError):
                    self.convert()
                self.assertEqual(path.read_bytes(), b'USER-EXISTING')
                path.unlink()
        self.assertFalse(self.output.exists())

    def test_input_output_alias_and_output_outside_test_directory_are_rejected(self):
        self.output = self.source
        with self.assertRaisesRegex(ValueError, '(?i)input|alias'):
            self.convert()
        self.output = ROOT / 'outside-test-output.pcapng'
        with self.assertRaisesRegex(ValueError, 'target[/\\\\]qa'):
            self.convert()
        self.assertEqual(self.source.read_bytes(), self.original)
        self.assertFalse(self.output.exists())

    def test_output_extension_and_input_metadata_alias_are_rejected(self):
        self.output = self.directory / 'wrong-extension.bin'
        with self.assertRaisesRegex(ValueError, r'\.pcapng'):
            self.convert()
        self.output = self.directory / 'derived.pcapng'
        self.source = self.output.with_name(self.output.name + '.provenance.json')
        self.source.write_bytes(self.original)
        with self.assertRaisesRegex(ValueError, '(?i)alias'):
            self.convert()
        self.assertEqual(self.source.read_bytes(), self.original)
        self.assertFalse(self.output.exists())

    def test_hard_link_to_input_is_rejected_without_altering_either_name(self):
        self.output.hardlink_to(self.source)
        with self.assertRaisesRegex(ValueError, '(?i)alias'):
            self.convert()
        self.assertEqual(self.output.read_bytes(), self.original)
        self.assertEqual(self.source.read_bytes(), self.original)

    def test_endpoint_and_outer_sequence_errors_leave_no_output(self):
        cases = [{'source': ('not-an-ip', 1534)}, {'destination': ('::1', 4321)}, {'source': ('198.51.100.17', 0)}, {'destination': ('203.0.113.19', 65536)}, {'start_sequence': -1}, {'start_sequence': 0xffffffff}]
        for kwargs in cases:
            with self.subTest(kwargs=kwargs):
                with self.assertRaises(ValueError):
                    self.convert(**kwargs)
                self.assertFalse(self.output.exists())

    def test_cli_requires_explicit_source_and_destination(self):
        result = subprocess.run([sys.executable, str(Path(converter.__file__)), str(self.source), str(self.output)], capture_output=True, text=True)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('--source', result.stderr)
        self.assertIn('--destination', result.stderr)
        self.assertFalse(self.output.exists())

    def test_cli_converts_with_explicit_endpoints_and_outer_sequence(self):
        result = subprocess.run([sys.executable, str(Path(converter.__file__)), str(self.source), str(self.output), '--source', '198.51.100.17:1534', '--destination', '203.0.113.19:4321', '--start-sequence', '305419896'], capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        report = json.loads(result.stdout)
        self.assertEqual(report['outer_sequence_start'], 305419896)
        self.assertEqual(report['frame_count'], 2)
        self.assertEqual(self.source.read_bytes(), self.original)


if __name__ == '__main__':
    unittest.main()
