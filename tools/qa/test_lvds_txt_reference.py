"""Unit fixtures below are constructed test bytes, never supplied device data."""
import csv
from contextlib import redirect_stderr, redirect_stdout
import importlib.util
import io
import json
from pathlib import Path
import struct
import tempfile
import unittest


MODULE_PATH = Path(__file__).with_name('lvds_txt_reference.py')
SPEC = importlib.util.spec_from_file_location('lvds_txt_reference', MODULE_PATH)


def wire(sequence=0, seconds=1700000000, micros=0):
    data = bytearray(bytes.fromhex('eb9055aa0107aa'))
    data += struct.pack('>II', seconds, micros)
    data += bytes.fromhex('ffc0080001') * 25
    data += bytes.fromhex('003ff003ff') * 25
    data += bytes([sequence, 0, 0x12, 0x34])
    return bytes(data)


class ReferenceTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        # Missing implementation is reported as an assertion, not import noise.
        if not MODULE_PATH.exists():
            return
        cls.reference = importlib.util.module_from_spec(SPEC)
        SPEC.loader.exec_module(cls.reference)

    def setUp(self):
        self.assertTrue(MODULE_PATH.exists(), 'independent TXT reference decoder is not implemented')
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.path = Path(self.directory.name)
        self.frames = self.reference.decode_frames([wire(), wire(1, micros=50000)])

    def csv_file(self):
        path = self.path / 'UNIT-output.csv'
        self.reference.write_reference_csv(path, self.frames)
        return path

    def source_file(self):
        path = self.path / 'UNIT-source.txt'
        path.write_bytes((wire().hex() + '\n' + wire(1, micros=50000).hex() + '\n').encode('ascii'))
        return path

    def run_cli(self, *arguments):
        output, errors = io.StringIO(), io.StringIO()
        with redirect_stdout(output), redirect_stderr(errors):
            result = self.reference.main([str(argument) for argument in arguments])
        return result, output.getvalue(), errors.getvalue()

    def lpd_file(self):
        # Literal independent file layout, rather than the reference encoder.
        data = bytearray(struct.pack('<8sHHIIIIIIHH', b'LMPROBE\0', 3, 40,
                                    0x01020304, 100, 500, 409, 1, 3, 2, 8))
        for index in range(2):
            data += struct.pack('<IIB', 1700000000, index * 50000, index)
            data += struct.pack('<100H', *([1023, 0, 512, 1] * 25))
            data += struct.pack('<100H', *([0, 1023, 0, 1023] * 25))
        path = self.path / 'UNIT-output.lpd'
        path.write_bytes(data)
        return path

    def test_literal_40bit_groups_restore_every_channel_point(self):
        frame = self.frames[0]
        self.assertEqual(frame['channel_1_code10'], [1023, 0, 512, 1] * 25)
        self.assertEqual(frame['channel_2_code10'], [0, 1023, 0, 1023] * 25)
        self.assertEqual(frame['first_sample_time_us'], 1700000000000000)
        self.assertEqual(frame['last_sample_time_us'], 1700000000049500)
        self.assertEqual(frame['frame_seq_u8'], 0)
        self.assertEqual(frame['wire_hex'], wire().hex())

    def test_decode_rejects_wrong_addresses_reserved_time_and_truncation(self):
        for offset, value, context in [(2, 4, 'SRC'), (3, 255, 'DST'),
                                       (4, 0, 'LEN'), (6, 0, 'TYPE'),
                                       (266, 1, 'DATA251')]:
            with self.subTest(offset=offset):
                bad = bytearray(wire())
                bad[offset] = value
                with self.assertRaisesRegex(ValueError, context):
                    self.reference.decode_frames([bytes(bad)])
        with self.assertRaisesRegex(ValueError, 'microseconds'):
            self.reference.decode_frames([wire(micros=1000000)])
        with self.assertRaisesRegex(ValueError, '269'):
            self.reference.decode_frames([wire()[:-1]])
        with self.assertRaisesRegex(ValueError, 'empty'):
            self.reference.decode_frames([])

    def test_csv_reference_has_all_eleven_columns_and_signed_values(self):
        path = self.csv_file()
        with path.open(newline='', encoding='utf-8') as stream:
            rows = list(csv.reader(stream))
        self.assertEqual(len(rows), 201)
        self.assertEqual(rows[1], ['0', '1700000000', '0', '0', '0',
                                  '1700000000000000', '1023', '0',
                                  '32704', '-32768', 'CRC_NOT_CHECKED'])
        self.assertEqual(rows[101][:6], ['1', '1700000000', '50000', '1',
                                       '0', '1700000000050000'])
        self.reference.verify_csv(path, self.frames)

    def test_csv_catches_each_changed_column_with_row_sample_actual_expected(self):
        for column in range(11):
            with self.subTest(column=column):
                path = self.path / f'UNIT-corrupt-{column}.csv'
                self.reference.write_reference_csv(path, self.frames)
                with path.open(newline='', encoding='utf-8') as stream:
                    rows = list(csv.reader(stream))
                rows[6][column] = 'WRONG' if column == 10 else '9999999'
                with path.open('w', newline='', encoding='utf-8') as stream:
                    csv.writer(stream).writerows(rows)
                with self.assertRaisesRegex(ValueError, r'CSV row 7.*frame 0.*sample 5.*actual.*expected'):
                    self.reference.verify_csv(path, self.frames)

    def test_csv_catches_header_truncation_and_extra_rows(self):
        path = self.csv_file()
        contents = path.read_text(encoding='utf-8')
        for name, value, pattern in [
            ('header', contents.replace('frame_seconds', 'wrong_seconds', 1), 'header'),
            ('truncated', '\n'.join(contents.splitlines()[:-1]) + '\n', r'row 201.*sample 99.*EOF'),
            ('extra', contents + contents.splitlines()[-1] + '\n', r'row 202.*EOF'),
        ]:
            with self.subTest(case=name):
                bad = self.path / f'UNIT-{name}.csv'
                bad.write_text(value, encoding='utf-8')
                with self.assertRaisesRegex(ValueError, pattern):
                    self.reference.verify_csv(bad, self.frames)

    def test_lpd_reads_all_records_and_codes(self):
        path = self.lpd_file()
        self.assertEqual(path.stat().st_size, 858)
        self.reference.verify_lpd(path, self.frames)

    def test_lpd_catches_header_metadata_every_field(self):
        path = self.lpd_file()
        original = path.read_bytes()
        for offset in [0, 8, 10, 12, 16, 20, 24, 28, 32, 36, 38]:
            with self.subTest(offset=offset):
                bad = bytearray(original)
                bad[offset] ^= 1
                path.write_bytes(bad)
                with self.assertRaisesRegex(ValueError, r'LPD header.*actual.*expected'):
                    self.reference.verify_lpd(path, self.frames)

    def test_lpd_catches_both_channels_endpoints_time_and_sequence(self):
        path = self.lpd_file()
        original = path.read_bytes()
        for within_record, context in [(0, 'frame_seconds'), (4, 'frame_microseconds'),
                                       (8, 'frame_seq_u8'), (9, 'channel 1 sample 0'),
                                       (207, 'channel 1 sample 99'),
                                       (209, 'channel 2 sample 0'),
                                       (407, 'channel 2 sample 99')]:
            with self.subTest(field=context):
                bad = bytearray(original)
                bad[40 + 409 + within_record] ^= 1
                path.write_bytes(bad)
                with self.assertRaisesRegex(ValueError, f'LPD record 1.*{context}.*actual.*expected'):
                    self.reference.verify_lpd(path, self.frames)

    def test_lpd_catches_truncation_and_trailing_bytes(self):
        path = self.lpd_file()
        original = path.read_bytes()
        for data, pattern in [(original[:39], 'LPD header.*truncated'),
                              (original[:-1], 'LPD record 1.*truncated'),
                              (original + b'\0', 'LPD EOF.*actual.*expected')]:
            with self.subTest(pattern=pattern):
                path.write_bytes(data)
                with self.assertRaisesRegex(ValueError, pattern):
                    self.reference.verify_lpd(path, self.frames)

    def test_baseline_assertion_does_not_treat_generic_fixture_as_supplied_data(self):
        with self.assertRaisesRegex(ValueError, 'baseline frame_count'):
            self.reference.assert_supplied_baseline(self.frames)
        # Constructed in memory only: exercises the assertions, not data acceptance.
        wires = [bytearray(wire(index, micros=index * 50000)) for index in range(20)]
        wires[0][15:20] = bytes.fromhex('ddb25b4e82')
        wires[0][140:145] = bytes.fromhex('d3b10b4693')
        baseline = self.reference.decode_frames([bytes(value) for value in wires])
        self.reference.assert_supplied_baseline(baseline)
        baseline[12]['frame_seq_u8'] = 99
        with self.assertRaisesRegex(ValueError, r'baseline frame 12.*frame_seq_u8.*99.*12'):
            self.reference.assert_supplied_baseline(baseline)

    def test_manifest_contains_all_input_bytes_and_decoded_values(self):
        source = self.path / 'UNIT-source.txt'
        source.write_bytes((wire().hex() + '\n').encode('ascii'))
        manifest = self.reference.build_manifest(source, self.frames)
        self.assertEqual(manifest['schema_version'], 1)
        self.assertEqual(manifest['frame_count'], 2)
        self.assertEqual(manifest['sample_count_per_channel'], 200)
        self.assertEqual(manifest['source']['bytes'], 539)
        self.assertEqual(len(manifest['source']['sha256']), 64)
        self.assertEqual(manifest['frames'], self.frames)
        json.dumps(manifest)

    def test_reference_writer_never_overwrites_existing_file(self):
        path = self.csv_file()
        original = path.read_bytes()
        with self.assertRaises(FileExistsError):
            self.reference.write_reference_csv(path, self.frames)
        self.assertEqual(path.read_bytes(), original)

    def test_cli_reads_source_writes_manifest_and_verifies_existing_gui_outputs(self):
        source = self.source_file()
        original = source.read_bytes()
        reference_csv = self.path / 'UNIT-reference.csv'
        manifest = self.path / 'UNIT-manifest.json'
        result, output, errors = self.run_cli(
            '--txt', source, '--reference-csv', reference_csv, '--manifest', manifest,
            '--csv', self.csv_file(), '--lpd', self.lpd_file())
        self.assertEqual(result, 0, errors)
        self.assertEqual(errors, '')
        report = json.loads(output)
        self.assertEqual(report['frame_count'], 2)
        self.assertEqual(report['sample_count_per_channel'], 200)
        self.assertEqual(report['baseline_assertion'], 'NOT_RUN')
        self.assertEqual(report['csv']['columns_checked'], 2200)
        self.assertEqual(report['lpd_v3']['records'], 2)
        self.assertEqual(report['lpd_v3']['codes_checked'], 400)
        decoded = json.loads(manifest.read_text(encoding='utf-8'))
        self.assertEqual(decoded['frames'][1]['frame_seq_u8'], 1)
        self.assertEqual(decoded['frames'][1]['first_sample_time_us'], 1700000000050000)
        with reference_csv.open(newline='', encoding='utf-8') as stream:
            rows = list(csv.reader(stream))
        self.assertEqual(rows[1][6:], ['1023', '0', '32704', '-32768', 'CRC_NOT_CHECKED'])
        self.assertEqual(len(rows), 201)
        self.assertEqual(source.read_bytes(), original)

    def test_cli_refuses_to_overwrite_source_or_existing_reference_and_manifest(self):
        source = self.source_file()
        original = source.read_bytes()
        existing = self.path / 'UNIT-existing-output'
        existing.write_bytes(b'USER-EXISTING')
        for option in ['--reference-csv', '--manifest']:
            for destination in [source, existing]:
                with self.subTest(option=option, destination=destination.name):
                    result, output, errors = self.run_cli('--txt', source, option, destination)
                    self.assertNotEqual(result, 0)
                    self.assertEqual(output, '')
                    self.assertIn('FAIL:', errors)
                    self.assertEqual(source.read_bytes(), original)
                    self.assertEqual(existing.read_bytes(), b'USER-EXISTING')

    def test_cli_rejects_generic_baseline_and_corrupt_csv_with_nonzero_exit(self):
        source = self.source_file()
        reference_csv = self.path / 'UNIT-not-created.csv'
        result, output, errors = self.run_cli(
            '--txt', source, '--assert-supplied-baseline', '--reference-csv', reference_csv)
        self.assertNotEqual(result, 0)
        self.assertEqual(output, '')
        self.assertRegex(errors, r'baseline frame_count.*actual 2.*expected 20')
        self.assertFalse(reference_csv.exists())
        bad_csv = self.csv_file()
        contents = bad_csv.read_text(encoding='utf-8')
        bad_csv.write_text(contents.replace('32704,-32768', '7,-32768', 1), encoding='utf-8')
        before = bad_csv.read_bytes()
        result, output, errors = self.run_cli('--txt', source, '--csv', bad_csv)
        self.assertNotEqual(result, 0)
        self.assertEqual(output, '')
        self.assertRegex(errors, r'CSV row 2 frame 0 sample 0.*ch1_adc16_reconstructed.*actual.*7.*expected.*32704')
        self.assertEqual(bad_csv.read_bytes(), before)


if __name__ == '__main__':
    unittest.main()
