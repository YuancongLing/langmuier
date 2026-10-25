"""Read-only actual-data voltage audit. Does not create scientific inputs."""
import hashlib
import json
from pathlib import Path
import sys

REPO = Path(__file__).resolve().parents[3]
QA = Path(__file__).resolve().parent
sys.path.insert(0, str(REPO / 'tools' / 'qa'))
from lvds_raw_recovery_reference import audit_records, read_pcap_udp
from lvds_txt_reference import decode_frames, file_info, assert_supplied_baseline
from lvds_txt_to_pcapng import read_hex_frames


def literal(code):
    adc = code * 64 - 32768
    volts = adc * 0.00015625
    assert abs(volts - (code * 0.01 - 5.12)) < 1e-12
    return {'code10': code, 'adc16': adc, 'voltage_v': round(volts, 12),
            'display': f'{volts:.2f} V'}


def summarize_frames(frames):
    result = {'frames': len(frames), 'samples_per_channel': len(frames) * 100,
              'first_frame': {}, 'channels': {}}
    for channel in (1, 2):
        key = f'channel_{channel}_code10'
        values = [code for frame in frames for code in frame[key]]
        result['first_frame'][key] = [literal(code) for code in frames[0][key][:12]]
        counts = {'negative': sum(code < 512 for code in values),
                  'zero': values.count(512), 'positive': sum(code > 512 for code in values)}
        examples = {}
        for frame_index, frame in enumerate(frames):
            for sample_index, code in enumerate(frame[key]):
                category = 'negative' if code < 512 else 'zero' if code == 512 else 'positive'
                examples.setdefault(category, {'frame_index': frame_index,
                    'frame_seconds': frame['frame_seconds'],
                    'frame_microseconds': frame['frame_microseconds'],
                    'frame_seq_u8': frame['frame_seq_u8'], 'sample_index': sample_index,
                    'sample_time_us': frame['first_sample_time_us'] + sample_index * 500,
                    **literal(code)})
        result['channels'][str(channel)] = {
            'minimum': literal(min(values)), 'maximum': literal(max(values)),
            'sign_counts': counts, 'first_sign_examples': examples,
            'unique_code_count': len(set(values)),
            'all_samples_on_0_01v_grid': all(abs(literal(code)['voltage_v'] * 100 - round(literal(code)['voltage_v'] * 100)) < 1e-9 for code in values)}
    return result


def baseline_check():
    baseline = json.loads((QA / 'unchanged-baseline.json').read_text(encoding='utf-8-sig'))
    checks = []
    for item in baseline['files']:
        path = Path(item['path'])
        actual = file_info(path)
        ok = (actual['bytes'], actual['sha256']) == (item['bytes'], item['sha256'])
        checks.append({'path': str(path), 'status': 'PASS' if ok else 'FAIL',
                       'expected_bytes': item['bytes'], 'expected_sha256': item['sha256'],
                       'actual_bytes': actual['bytes'], 'actual_sha256': actual['sha256']})
    return {'status': 'PASS' if all(row['status'] == 'PASS' for row in checks) else 'FAIL',
            'file_count': len(checks), 'files': checks}


def main():
    capture = Path('D:/RUST/2026102401.pcapng')
    records, packet_summary = read_pcap_udp(capture)
    rows, audit_summary = audit_records(records)
    native = []
    for row in rows:
        if row['quality'] != 'NATIVE':
            continue
        selected = row['selected']
        native.append({'frame_index': len(native), 'frame_seconds': selected['seconds'],
            'frame_microseconds': selected['microseconds'], 'frame_seq_u8': selected['seq8'],
            'first_sample_time_us': selected['device_time_us'],
            'channel_1_code10': selected['channel_1_code10'],
            'channel_2_code10': selected['channel_2_code10']})
    txt = REPO / 'lvds.txt'
    txt_frames = decode_frames(read_hex_frames(txt))
    assert_supplied_baseline(txt_frames)
    expected = [(0, -32768, -5.12), (256, -16384, -2.56), (512, 0, 0.00),
                (701, 12096, 1.89), (702, 12160, 1.90), (825, 20032, 3.13),
                (1023, 32704, 5.11)]
    numerical = []
    for code, adc, volts in expected:
        actual = literal(code)
        assert actual['adc16'] == adc and abs(actual['voltage_v'] - volts) < 1e-12
        assert actual['display'] == f'{volts:.2f} V'
        numerical.append(actual)
    for code in range(1024):
        literal(code)
    report = {'status': 'PASS', 'scope': 'independent actual input readback and numeric formula; not production Rust or GUI acceptance',
        'scientific_inputs_generated': False, 'source_mutated': False,
        'voltage_formula': 'adc16 * 0.00015625; equivalent to code10 * 0.01 - 5.12',
        'numeric_literal_cases': numerical, 'all_1024_formula_equivalence': 'PASS',
        'actual_pcap': {'source': file_info(capture), 'packets': packet_summary,
                        'audit': audit_summary, 'native_voltage': summarize_frames(native)},
        'supplied_txt': {'source': file_info(txt), 'baseline_assertion': 'PASS',
                         'voltage': summarize_frames(txt_frames)},
        'protected_baseline': baseline_check()}
    if report['protected_baseline']['status'] != 'PASS':
        raise ValueError('a protected source or existing data file changed')
    with (QA / 'actual-voltage-data-audit.json').open('x', encoding='utf-8', newline='\n') as stream:
        json.dump(report, stream, ensure_ascii=False, indent=2)
        stream.write('\n')
    with (QA / 'actual-pcap-reference-frames.jsonl').open('x', encoding='utf-8', newline='\n') as stream:
        for row in rows:
            stream.write(json.dumps(row, ensure_ascii=False) + '\n')
    print(json.dumps(report, ensure_ascii=False, indent=2))


if __name__ == '__main__':
    main()
