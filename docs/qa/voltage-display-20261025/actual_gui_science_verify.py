"""Verify actual full-file GUI exports against independent real-PCAP JSONL.

Reads existing files only. Never writes scientific values, captures, or LPD.
The expected codes and device times come from the independent PCAP audit's
selected prefixes, not from the production decoder or a generated waveform.
"""
import argparse
from collections import Counter
import csv
import datetime
import json
import ntpath
from pathlib import Path
import sys

REPO = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(REPO / 'tools/qa'))
from lvds_txt_reference import (  # independent standard-library readers only
    CSV_COLUMNS, VerificationError, csv_rows, file_info, require_equal,
    verify_csv, verify_lpd,
)

QUALITY_VERSION = '# LANGMUIR_QUALITY_CSV_VERSION=1'
QUALITY_COLUMNS = tuple(CSV_COLUMNS) + (
    'quality', 'raw_record_id', 'recovery_shift', 'recovery_validation', 'source',
)
EXPECTED_COUNTS = {'NATIVE': 237, 'RECOVERY_CANDIDATE': 229, 'REJECTED': 1}
VALIDATION_KEYS = (
    'structure', 'unique', 'full_bits', 'outer_sequence', 'inner_sequence',
    'device_time',
)


def load_reference(path):
    with Path(path).open(encoding='utf-8-sig') as stream:
        rows = [json.loads(line) for line in stream if line.strip()]
    require_equal(len(rows), 467, 'independent actual PCAP UDP count')
    require_equal(dict(Counter(row['quality'] for row in rows)), EXPECTED_COUNTS,
                  'independent actual PCAP quality counts')
    native, qualified = [], []
    anchor = None
    for record_index, row in enumerate(rows):
        context = f'independent PCAP record {record_index}'
        # This independently generated JSONL uses zero-based observation IDs.
        # Production RawArchive/HistoryReader IDs start at one.
        require_equal(row['record_id'], record_index, f'{context} record_id')
        require_equal(row.get('cross_record_bits_used', False), False,
                      f'{context} no borrowed bits')
        selected = row.get('selected')
        if row['quality'] == 'REJECTED':
            require_equal(selected, None, f'{context} rejected selected prefix')
            continue
        if not selected or not selected.get('structure_valid'):
            raise VerificationError(f'{context}: missing valid independent prefix')
        require_equal(selected['sample_counts'], [100, 100],
                      f'{context} sample counts')
        seconds, micros = selected['seconds'], selected['microseconds']
        if not 0 <= micros < 1_000_000:
            raise VerificationError(f'{context}: invalid reference microseconds')
        require_equal(selected['device_time_us'], seconds * 1_000_000 + micros,
                      f'{context} reference device time')
        for key in ('channel_1_code10', 'channel_2_code10'):
            codes = selected[key]
            require_equal(len(codes), 100, f'{context} {key} length')
            if any(type(code) is not int or not 0 <= code <= 1023 for code in codes):
                raise VerificationError(f'{context}: invalid reference {key}')
        validation = {
            'structure': True,
            'unique': len(row['candidates']) == 1 and row['omitted_candidates'] == 0,
            'full_bits': selected['complete_269_bytes'],
            'outer_sequence': False,
            'inner_sequence': False,
            'device_time': False,
        }
        if row['quality'] == 'NATIVE':
            require_equal(selected['complete_269_bytes'], True,
                          f'{context} native full frame')
            require_equal(selected['shift'], 0, f'{context} native shift')
        else:
            require_equal(row['quality'], 'RECOVERY_CANDIDATE', f'{context} quality')
            require_equal(selected['complete_269_bytes'], False,
                          f'{context} candidate lacks real tail bits')
            if anchor is not None:
                previous, previous_selected = anchor
                validation.update({
                    'outer_sequence': row['outer_sequence_u32'] ==
                        (previous['outer_sequence_u32'] + 1) % (1 << 32),
                    'inner_sequence': selected['seq8'] ==
                        (previous_selected['seq8'] + 1) % 256,
                    'device_time': selected['device_time_us'] ==
                        previous_selected['device_time_us'] + 50_000,
                })
        frame = {
            'frame_index': len(qualified),
            'frame_seconds': seconds,
            'frame_microseconds': micros,
            'frame_seq_u8': selected['seq8'],
            'first_sample_time_us': selected['device_time_us'],
            'channel_1_code10': selected['channel_1_code10'],
            'channel_2_code10': selected['channel_2_code10'],
            'quality': row['quality'],
            'raw_record_id': record_index + 1,
            'shift': selected['shift'],
            'validation': validation,
        }
        qualified.append(frame)
        if row['quality'] == 'NATIVE':
            native.append({**frame, 'frame_index': len(native)})
            # Diagnostic candidates never become continuity anchors. The
            # capture has one source/session and no confirmed recovered frames.
            anchor = row, selected
    require_equal(len(native), 237, 'selected native frames')
    require_equal(len(qualified), 466, 'selected native plus candidate frames')
    return rows, native, qualified


def normalized_source(value):
    # Compare Windows paths without rejecting equivalent slash/case spelling.
    return ntpath.normcase(ntpath.normpath(value))


def verify_quality_csv(path, frames, expected_source):
    row_count = 0
    quality_counts = Counter()
    sources = set()
    with Path(path).open(encoding='utf-8', newline='') as stream:
        reader = csv.reader(stream)
        require_equal(next(reader, None), [QUALITY_VERSION], 'quality CSV version line')
        require_equal(next(reader, None), list(QUALITY_COLUMNS), 'quality CSV 16-column header')
        for frame_index, original_frame in enumerate(frames):
            frame = {**original_frame, 'frame_index': frame_index}
            quality_counts[frame['quality']] += 1
            for sample_index, expected_base in enumerate(csv_rows([frame])):
                row_count += 1
                context = f'quality CSV data row {row_count} frame {frame_index} sample {sample_index}'
                actual = next(reader, None)
                if actual is None:
                    raise VerificationError(f'{context}: unexpected EOF')
                require_equal(len(actual), 16, f'{context} column count')
                for name, actual_value, expected in zip(CSV_COLUMNS, actual[:11], expected_base):
                    require_equal(actual_value, str(expected), f'{context} {name}')
                require_equal(actual[11], frame['quality'], f'{context} quality')
                require_equal(actual[12], str(frame['raw_record_id']), f'{context} raw_record_id')
                require_equal(actual[13], str(frame['shift']), f'{context} recovery_shift')
                if not actual[14].strip():
                    raise VerificationError(f'{context}: empty recovery_validation')
                validation = json.loads(actual[14])
                if not isinstance(validation, dict):
                    raise VerificationError(f'{context}: validation must be a JSON object')
                require_equal(set(validation), set(VALIDATION_KEYS), f'{context} validation fields')
                if any(type(validation[key]) is not bool for key in VALIDATION_KEYS):
                    raise VerificationError(f'{context}: validation fields must be booleans')
                require_equal(validation, frame['validation'], f'{context} recovery_validation')
                if not actual[15].strip():
                    raise VerificationError(f'{context}: empty source')
                require_equal(normalized_source(actual[15]), normalized_source(expected_source),
                              f'{context} source history-file association')
                sources.add(actual[15])
        if next(reader, None) is not None:
            raise VerificationError(f'quality CSV has extra rows after {row_count} samples')
    require_equal(row_count, len(frames) * 100, 'quality CSV full sample count')
    return {
        'status': 'PASS', 'version': 1, 'columns': 16,
        'frames': len(frames), 'data_rows': row_count,
        'columns_checked': row_count * 16,
        'codes_checked': row_count * 2,
        'frame_quality_counts': dict(quality_counts),
        'sources': sorted(sources),
        'source_association_checked_against': expected_source,
        'validation_fields_checked_per_row': list(VALIDATION_KEYS),
        'candidate_never_promoted_to_recovered': True,
    }


def default_source(reference_path):
    suffix = '-frames.jsonl'
    if not reference_path.name.endswith(suffix):
        return None, None
    summary = reference_path.with_name(reference_path.name[:-len(suffix)] + '-summary.json')
    if not summary.is_file():
        return None, None
    data = json.loads(summary.read_text(encoding='utf-8-sig'))
    return data.get('capture', {}).get('path'), summary


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--reference-jsonl', type=Path, required=True)
    parser.add_argument('--csv', type=Path, help='actual full ordinary 11-column Native CSV')
    parser.add_argument('--quality-csv', type=Path, help='actual versioned 16-column history CSV')
    parser.add_argument('--quality-selection', choices=('native-candidate', 'native'),
                        default='native-candidate', help='explicitly selected quality export contents')
    parser.add_argument('--lpd', type=Path, help='actual full Native LPD v3')
    parser.add_argument('--source-file', type=Path,
                        help='history input recorded in quality source; default independent summary capture path')
    parser.add_argument('--output-json', type=Path, help='exclusively create UTF-8 evidence JSON')
    args = parser.parse_args(argv)
    report = {
        'status': 'NOT_RUN',
        'generated_utc': datetime.datetime.now(datetime.UTC).isoformat(),
        'scope': 'actual existing exports checked against independent real-PCAP selected prefixes',
        'scientific_data_generated': False,
        'crc_integrity': 'CRC_NOT_CHECKED; independent reference CRC is not an admission gate',
        'checks': {},
    }
    exit_code = 0
    try:
        report['verifier'] = file_info(__file__)
        report['independent_readback_helper'] = file_info(REPO / 'tools/qa/lvds_txt_reference.py')
        report['reference_jsonl'] = file_info(args.reference_jsonl)
        report['requested_existing_exports'] = {
            name: file_info(path)
            for name, path in [('csv', args.csv), ('quality_csv', args.quality_csv), ('lpd_v3', args.lpd)]
            if path is not None
        }
        rows, native, qualified = load_reference(args.reference_jsonl)
        report['reference'] = {
            'udp_records': len(rows), 'quality_counts': EXPECTED_COUNTS,
            'native_frames': len(native), 'native_sample_rows': len(native) * 100,
            'native_plus_candidate_frames': len(qualified),
            'native_plus_candidate_sample_rows': len(qualified) * 100,
            'record_id_mapping': 'raw_record_id = zero-based independent JSONL record_id + 1',
            'expected_values': 'independent selected seconds/microseconds/seq8/code10; ADC=code10*64-32768; dt=500us',
        }
        if args.csv:
            report['checks']['csv'] = {**file_info(args.csv), **verify_csv(args.csv, native),
                                       'frames': len(native), 'codes_checked': len(native) * 200}
        if args.lpd:
            report['checks']['lpd_v3'] = {**file_info(args.lpd), **verify_lpd(args.lpd, native),
                'integrity_limit': 'LPD v3 contains codes/time/seq8, not a CRC or quality field'}
        if args.quality_csv:
            expected_source, summary_path = default_source(args.reference_jsonl)
            if args.source_file:
                expected_source = str(args.source_file)
            if not expected_source:
                raise VerificationError('quality CSV requires --source-file or sibling independent capture summary')
            if summary_path:
                report['reference_summary'] = file_info(summary_path)
            source_path = Path(expected_source)
            if not source_path.is_file():
                raise VerificationError(f'quality source history input does not exist: {expected_source}')
            report['quality_source_file'] = file_info(source_path)
            selected = qualified if args.quality_selection == 'native-candidate' else native
            report['checks']['quality_csv'] = {
                **file_info(args.quality_csv),
                **verify_quality_csv(args.quality_csv, selected, expected_source),
            }
        report['status'] = 'PASS' if report['checks'] else 'REFERENCE_VALIDATED_EXPORTS_NOT_RUN'
    except (OSError, ValueError, KeyError, TypeError, csv.Error) as error:
        report['status'] = 'FAIL'
        report['error'] = str(error)
        exit_code = 1
    serialized = json.dumps(report, ensure_ascii=False, indent=2) + '\n'
    if args.output_json:
        try:
            with args.output_json.open('x', encoding='utf-8', newline='\n') as stream:
                stream.write(serialized)
        except OSError as error:
            print(f'Could not exclusively create evidence JSON: {error}', file=sys.stderr)
            return 1
    print(serialized, end='')
    return exit_code


if __name__ == '__main__':
    raise SystemExit(main())
