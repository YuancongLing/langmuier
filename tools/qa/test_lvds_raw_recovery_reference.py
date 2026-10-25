"""Independent auditor tests; every capture below is explicitly synthetic."""
import importlib.util
from pathlib import Path
import struct
import tempfile
import unittest


MODULE = Path(__file__).with_name('lvds_raw_recovery_reference.py')


def crc(data):
    value = 0xffff
    for byte in data:
        value ^= byte << 8
        for _ in range(8):
            value = ((value << 1) ^ (0x1021 if value & 0x8000 else 0)) & 0xffff
    return value


def synthetic_frame(sequence=0, micros=0):
    frame = bytearray(bytes.fromhex('eb9055aa0107aa'))
    frame += struct.pack('>II', 1700000000, micros)
    frame += bytes.fromhex('ffc0080001') * 25
    frame += bytes.fromhex('003ff003ff') * 25
    frame += bytes([sequence, 0])
    frame += crc(frame[7:267]).to_bytes(2, 'big')
    return bytes(frame)


def shifted(frame, prefix_bits, complete):
    bits = '1' * prefix_bits + ''.join(f'{byte:08b}' for byte in frame)
    if complete:
        bits += '0' * (-len(bits) % 8)
    else:
        bits = bits[:len(frame) * 8]
    return int(bits, 2).to_bytes(len(bits) // 8, 'big')


def pcap_block(kind, body, endian='<'):
    body += b'\0' * (-len(body) % 4)
    size = len(body) + 12
    return struct.pack(endian + 'II', kind, size) + body + struct.pack(endian + 'I', size)


def packet(payload):
    udp = struct.pack('>HHHH', 1534, 1534, len(payload) + 8, 0) + payload
    ip = bytes.fromhex('4500') + struct.pack('>HHHBBH', len(udp) + 20, 0, 0, 64, 17, 0)
    ip += bytes([192, 168, 1, 128, 192, 168, 1, 100])
    return bytes.fromhex('0200000000010200000000020800') + ip + udp


def wrapper(frame, sequence):
    return bytes.fromhex('55aa01a5') + struct.pack('>IIHHIHH', sequence, len(frame), 0, 1, 0, len(frame), 0) + frame


def capture(payloads, endian='<'):
    data = pcap_block(0x0a0d0d0a, struct.pack(endian + 'IHHq', 0x1a2b3c4d, 1, 0, -1), endian)
    data += pcap_block(1, struct.pack(endian + 'HHI', 1, 0, 65535), endian)
    for index, payload in enumerate(payloads):
        ethernet = packet(payload)
        data += pcap_block(6, struct.pack(endian + 'IIIII', 0, 0, index * 50000, len(ethernet), len(ethernet)) + ethernet, endian)
    return data


def raw_endpoint(ip, port):
    return struct.pack('<BBH', 4, 0, port) + bytes(ip) + bytes(12) + bytes(4)


def synthetic_raw_archive(payloads):
    source = raw_endpoint([192, 168, 1, 128], 1534)
    destination = raw_endpoint([192, 168, 1, 100], 1534)
    header = bytearray(128)
    struct.pack_into('<8sHHIIIQII', header, 0, b'LVDSRAW\0', 1, 128, 0x01020304, 2, 65535, 0, 0, 0)
    header[40:56] = bytes(range(16))
    header[56:80], header[80:104] = source, destination
    data = bytearray(header)
    for index, payload in enumerate(payloads):
        record = bytearray(96)
        size = 100 + len(payload)
        struct.pack_into('<HHIQQII', record, 0, 1, 96, size, index + 1, 0, index * 50000000, 7)
        record[32:56], record[56:80] = source, destination
        struct.pack_into('<IIQ', record, 80, len(payload), 0, index)
        data += record + payload + struct.pack('<I', size)
    footer = bytearray(92)
    count = len(payloads)
    struct.pack_into('<HHIQQQQQQIIQ', footer, 0, 2, 88, 92, count, count, count, 0, sum(map(len, payloads)), count, 1, 1, count)
    struct.pack_into('<I', footer, 88, 92)
    return bytes(data + footer)


class RecoveryReferenceTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        if MODULE.exists():
            spec = importlib.util.spec_from_file_location('lvds_raw_recovery_reference', MODULE)
            cls.reference = importlib.util.module_from_spec(spec)
            spec.loader.exec_module(cls.reference)

    def setUp(self):
        self.assertTrue(MODULE.exists(), 'independent raw/bit-boundary auditor is not implemented')
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.directory = Path(self.temporary.name)

    def test_crc_reference_known_check_vector(self):
        self.assertEqual(self.reference.reference_crc(b'123456789'), 0x29b1)

    def test_native_frame_is_accepted_even_when_reference_crc_mismatches(self):
        frame = bytearray(synthetic_frame())
        frame[-1] ^= 1
        result = self.reference.audit_inner(bytes(frame))
        self.assertEqual(result['quality'], 'NATIVE')
        self.assertTrue(result['native_valid'])
        self.assertEqual(result['selected']['reference_crc']['status'], 'MISMATCH')
        self.assertEqual(result['selected']['sample_counts'], [100, 100])

    def test_full_one_bit_alignment_uses_only_real_bits(self):
        original = synthetic_frame()
        result = self.reference.audit_inner(shifted(original, 1, True))
        candidate = result['candidates'][0]
        self.assertEqual(candidate['bit_start'], 1)
        self.assertEqual(candidate['shift'], 1)
        self.assertTrue(candidate['complete_269_bytes'])
        self.assertEqual(candidate['missing_tail_bits'], 0)
        self.assertFalse(candidate['requires_cross_record_bits'])
        self.assertEqual(candidate['normalized_frame_hex'], original.hex())
        self.assertEqual(candidate['reference_crc']['status'], 'MATCH')

    def test_one_missing_bit_never_becomes_a_complete_frame(self):
        result = self.reference.audit_inner(shifted(synthetic_frame(), 1, False))
        self.assertEqual(result['quality'], 'RECOVERY_CANDIDATE')
        candidate = result['candidates'][0]
        self.assertFalse(candidate['complete_269_bytes'])
        self.assertEqual(candidate['missing_tail_bits'], 1)
        self.assertTrue(candidate['requires_cross_record_bits'])
        self.assertNotIn('normalized_frame_hex', candidate)
        self.assertEqual(candidate['reference_crc']['status'], 'NOT_EVALUATED')
        self.assertEqual(candidate['reference_crc']['received_crc_available_bits'], 15)
        self.assertTrue(candidate['reference_crc']['received_prefix_matches'])

    def test_four_bit_alignment_and_short_frame_boundaries(self):
        for length, missing in [(270, 0), (269, 4), (268, 12)]:
            with self.subTest(length=length):
                raw = shifted(synthetic_frame(), 4, True)[:length]
                candidate = self.reference.audit_inner(raw)['candidates'][0]
                self.assertEqual(candidate['shift'], 4)
                self.assertEqual(candidate['missing_tail_bits'], missing)
                self.assertEqual(candidate['complete_269_bytes'], missing == 0)

    def test_eb90_without_full_fixed_header_is_rejected(self):
        self.assertEqual(self.reference.audit_inner(b'noise\xeb\x90' + bytes(300))['quality'], 'REJECTED')

    def test_fixed_header_with_invalid_reserved_or_time_is_rejected(self):
        for offset, value in [(266, 2), (11, 255)]:
            with self.subTest(offset=offset):
                frame = bytearray(synthetic_frame())
                frame[offset] = value
                self.assertEqual(self.reference.audit_inner(bytes(frame))['quality'], 'REJECTED')

    def test_byte_sync_and_ambiguous_candidates_are_distinct(self):
        result = self.reference.audit_inner(b'noise' + synthetic_frame())
        self.assertEqual(result['selected']['bit_start'], 40)
        self.assertEqual(result['selected']['alignment_mode'], 'BYTE_SYNC')
        ambiguous = self.reference.audit_inner(synthetic_frame() + synthetic_frame(1, 50000))
        self.assertEqual(len(ambiguous['candidates']), 2)
        self.assertEqual(ambiguous['quality'], 'RECOVERY_CANDIDATE')
        self.assertIsNone(ambiguous['selected'])

    def test_pcap_reads_all_udp_endpoints_payloads_and_times_in_either_endian(self):
        payloads = [wrapper(synthetic_frame(), 101), wrapper(synthetic_frame(1, 50000), 102)]
        for endian in ['<', '>']:
            with self.subTest(endian=endian):
                path = self.directory / 'SYNTHETIC.pcapng'
                path.write_bytes(capture(payloads, endian))
                records, summary = self.reference.read_pcap_udp(path)
                self.assertEqual(summary['packets'], 2)
                self.assertEqual(len(records), 2)
                self.assertEqual([item['payload'] for item in records], payloads)
                self.assertEqual([item['timestamp_ns'] for item in records], [0, 50000000])
                self.assertEqual(records[0]['source'], '192.168.1.128:1534')
                self.assertEqual(records[0]['destination'], '192.168.1.100:1534')

    def test_pcap_truncation_is_reported_instead_of_silent_eof(self):
        path = self.directory / 'SYNTHETIC-truncated.pcapng'
        path.write_bytes(capture([wrapper(synthetic_frame(), 1)])[:-1])
        with self.assertRaisesRegex(ValueError, '(?i)truncat|boundary|length'):
            self.reference.read_pcap_udp(path)

    def test_unsupported_packet_blocks_cannot_silently_omit_capture_data(self):
        path = self.directory / 'SYNTHETIC-simple-packet.pcapng'
        path.write_bytes(capture([]) + pcap_block(3, struct.pack('<I', 0)))
        with self.assertRaisesRegex(ValueError, '(?i)unsupported.*packet'):
            self.reference.read_pcap_udp(path)

    def test_adjacent_datagrams_are_never_used_to_fill_missing_bits(self):
        path = self.directory / 'SYNTHETIC-no-cross-record.pcapng'
        payloads = [wrapper(shifted(synthetic_frame(), 1, False), 10), wrapper(synthetic_frame(1, 50000), 11)]
        path.write_bytes(capture(payloads))
        records, _ = self.reference.read_pcap_udp(path)
        rows, summary = self.reference.audit_records(records)
        self.assertEqual(rows[0]['quality'], 'RECOVERY_CANDIDATE')
        self.assertEqual(rows[0]['candidates'][0]['missing_tail_bits'], 1)
        self.assertEqual(rows[1]['quality'], 'NATIVE')
        self.assertEqual(summary['quality_counts']['NATIVE'], 1)
        self.assertEqual(summary['confirmed_recovered'], 0)

    def test_recovery_confirmation_needs_complete_unique_crc_and_continuity(self):
        for outer, source, microseconds, expected in [(11, '192.168.1.128:1534', 50000, 'RECOVERED_4BIT'), (12, '192.168.1.128:1534', 50000, 'RECOVERY_CANDIDATE'), (11, '192.0.2.1:1534', 50000, 'RECOVERY_CANDIDATE'), (11, '192.168.1.128:1534', 51000, 'RECOVERY_CANDIDATE')]:
            with self.subTest(outer=outer, source=source, micros=microseconds):
                records = [{'record_id': 0, 'packet_index': 0, 'timestamp_ns': 0, 'source': '192.168.1.128:1534', 'destination': '192.168.1.100:1534', 'payload': wrapper(synthetic_frame(), 10)}, {'record_id': 1, 'packet_index': 1, 'timestamp_ns': 50000000, 'source': source, 'destination': '192.168.1.100:1534', 'payload': wrapper(shifted(synthetic_frame(1, microseconds), 4, True), outer)}]
                rows, _ = self.reference.audit_records(records)
                self.assertEqual(rows[1]['quality'], expected)

    def test_reverse_one_bit_phase_and_recovery_resource_boundary(self):
        records = [{'record_id': 0, 'packet_index': 0, 'timestamp_ns': 0, 'source': '192.168.1.128:1534', 'destination': '192.168.1.100:1534', 'payload': wrapper(synthetic_frame(), 10)}, {'record_id': 1, 'packet_index': 1, 'timestamp_ns': 50000000, 'source': '192.168.1.128:1534', 'destination': '192.168.1.100:1534', 'payload': wrapper(shifted(synthetic_frame(1, 50000), 7, True), 11)}]
        rows, _ = self.reference.audit_records(records)
        self.assertEqual(rows[1]['quality'], 'RECOVERED_1BIT')
        self.assertEqual(rows[1]['selected']['shift'], 7)
        self.assertEqual(self.reference.audit_inner(bytes(4097) + synthetic_frame())['quality'], 'REJECTED')

    def test_raw_reader_and_pcap_compare_preserve_every_payload_and_metadata(self):
        payloads = [wrapper(synthetic_frame(), 101), b'BAD-WRAPPER-SHORT-AND-REJECTED']
        raw = self.directory / 'SYNTHETIC.lvdsraw'
        pcap = self.directory / 'SYNTHETIC.pcapng'
        raw.write_bytes(synthetic_raw_archive(payloads))
        pcap.write_bytes(capture(payloads))
        actual, summary = self.reference.read_lvdsraw(raw)
        expected, _ = self.reference.read_pcap_udp(pcap)
        self.assertEqual([record['payload'] for record in actual], payloads)
        self.assertTrue(summary['clean_raw_eof'])
        self.assertEqual(summary['footer']['written'], 2)
        self.assertEqual(self.reference.compare_raw_records(expected, actual, summary)['status'], 'PASS')

    def test_raw_reader_rejects_missing_footer_truncation_invalid_header_and_boundary(self):
        original = synthetic_raw_archive([wrapper(synthetic_frame(), 1)])
        bad_header = bytearray(original)
        bad_header[8] = 2
        bad_record = bytearray(original)
        bad_record[128 + 4] ^= 1
        cases = [(original[:-92], 'footer|incomplete'), (original[:-1], 'truncat'), (bytes(bad_header), 'version'), (bytes(bad_record), 'length|boundary|truncat')]
        path = self.directory / 'SYNTHETIC-invalid.lvdsraw'
        for data, pattern in cases:
            with self.subTest(pattern=pattern):
                path.write_bytes(data)
                with self.assertRaisesRegex(ValueError, '(?i)' + pattern):
                    self.reference.read_lvdsraw(path)

    def test_raw_comparison_detects_changed_payload_source_time_and_origin_index(self):
        payloads = [wrapper(synthetic_frame(), 1)]
        raw = self.directory / 'SYNTHETIC.lvdsraw'
        pcap = self.directory / 'SYNTHETIC.pcapng'
        raw.write_bytes(synthetic_raw_archive(payloads))
        pcap.write_bytes(capture(payloads))
        actual, summary = self.reference.read_lvdsraw(raw)
        expected, _ = self.reference.read_pcap_udp(pcap)
        for field, value in [('payload', b'changed'), ('source', '192.0.2.1:1534'), ('timestamp_ns', 1), ('origin_packet_index', 10)]:
            with self.subTest(field=field):
                changed = [dict(actual[0], **{field: value})]
                with self.assertRaisesRegex(ValueError, field):
                    self.reference.compare_raw_records(expected, changed, summary)

    def test_complete_raw_footer_cannot_hide_record_id_gaps_or_dropped_data(self):
        original = synthetic_raw_archive([wrapper(synthetic_frame(), 1)])
        path = self.directory / 'SYNTHETIC-id-gap.lvdsraw'
        gap = bytearray(original)
        struct.pack_into('<Q', gap, 128 + 8, 2)
        struct.pack_into('<Q', gap, len(gap) - 92 + 64, 2)
        path.write_bytes(gap)
        with self.assertRaisesRegex(ValueError, '(?i)id|counter'):
            self.reference.read_lvdsraw(path)
        dropped = bytearray(original)
        footer = len(dropped) - 92
        struct.pack_into('<Q', dropped, footer + 8, 2)
        struct.pack_into('<Q', dropped, footer + 32, 1)
        struct.pack_into('<Q', dropped, footer + 48, 2)
        struct.pack_into('<I', dropped, footer + 56, 2)
        path.write_bytes(dropped)
        with self.assertRaisesRegex(ValueError, '(?i)incomplete|loss'):
            self.reference.read_lvdsraw(path)


if __name__ == '__main__':
    unittest.main()
