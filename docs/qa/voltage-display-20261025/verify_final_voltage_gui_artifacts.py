"""Read-only final EXE cursor and complete science byte identity checks."""
import bisect
import json
from pathlib import Path
import sys

REPO = Path(__file__).resolve().parents[3]
QA = Path(__file__).resolve().parent
sys.path.insert(0, str(QA))
sys.path.insert(0, str(REPO / 'tools' / 'qa'))
from actual_gui_science_verify import load_reference
from lvds_txt_reference import file_info

process = json.loads((QA / 'final-native-gui-process.json').read_text(encoding='utf-8-sig'))
assert process['exe_sha256'] == 'f2805c059a699b02164d00da77f9b335a371954474481180fe35e3426cc9269a'
assert file_info(Path(process['path']))['sha256'] == process['exe_sha256']
_, frames, _ = load_reference(QA / 'actual-pcap-reference-frames.jsonl')
points = []
for frame in frames:
    for sample in range(100):
        points.append({'time_us': frame['first_sample_time_us'] + sample * 500,
            'frame_index': frame['frame_index'], 'seq8': frame['frame_seq_u8'],
            'sample_index': sample, 'code10': [frame['channel_1_code10'][sample], frame['channel_2_code10'][sample]]})
times = [point['time_us'] for point in points]
assert times == sorted(times)


def with_values(point):
    return {**point, 'adc16': [code * 64 - 32768 for code in point['code10']],
        'volts': [round((code * 64 - 32768) * 0.00015625, 12) for code in point['code10']],
        'display': [f'{(code * 64 - 32768) * 0.00015625:.2f} V' for code in point['code10']]}


cursor = 366_892_665
index = bisect.bisect_left(times, cursor)
near = min(points[max(0, index - 1):index + 1], key=lambda point: abs(point['time_us'] - cursor))
assert with_values(near)['display'] == ['-3.18 V', '-3.19 V']
tooltip_time = 366_923_464
tooltip = points[bisect.bisect_left(times, tooltip_time)]
assert tooltip['time_us'] == tooltip_time
assert with_values(tooltip)['display'][1] == '-2.23 V'
cursor_report = {'status': 'PASS', 'scope': 'independent actual-PCAP sample lookup for final non-Mock EXE screenshot text',
    'screenshot': file_info(REPO / 'docs/qa/LVDS_VOLTAGE_FINAL_ACTUAL_PCAP_CURSOR.jpg'),
    'final_exe_process': process, 'top_cursor_time_us': cursor,
    'nearest_sample': with_values(near), 'top_cursor_to_nearest_sample_delta_us': cursor - near['time_us'],
    'tooltip_sample': with_values(tooltip), 'tooltip_channel': 2,
    'all_native_samples': len(points), 'reference': file_info(QA / 'actual-pcap-reference-frames.jsonl')}
with (QA / 'final-gui-cursor-independent-readback.json').open('x', encoding='utf-8', newline='\n') as stream:
    json.dump(cursor_report, stream, ensure_ascii=False, indent=2)
    stream.write('\n')

comparisons = []
for suffix in ('.csv', '.lpd'):
    old = REPO / 'data' / ('scientific-20261025-023840.901' + suffix)
    prior = REPO / 'data' / ('scientific-20261025-115147.359' + suffix)
    final = REPO / 'data' / ('scientific-20261025-121748.495' + suffix)
    same = old.read_bytes() == final.read_bytes()
    prior_same = prior.read_bytes() == final.read_bytes()
    assert same and prior_same, f'ADC/prior/final {suffix} differ'
    comparisons.append({'old_adc': file_info(old), 'prior_voltage': file_info(prior), 'final_voltage': file_info(final),
        'old_adc_vs_final_byte_for_byte_equal': same, 'prior_voltage_vs_final_byte_for_byte_equal': prior_same})
storage_report = {'status': 'PASS', 'scope': 'original ADC-display, prior voltage-display and final voltage-display actual-PCAP scientific outputs',
    'final_exe_sha256': process['exe_sha256'], 'display_unit_change_modifies_scientific_files': False, 'files': comparisons}
with (QA / 'final-vs-existing-storage-byte-identity.json').open('x', encoding='utf-8', newline='\n') as stream:
    json.dump(storage_report, stream, ensure_ascii=False, indent=2)
    stream.write('\n')
print(json.dumps({'cursor': cursor_report, 'storage': storage_report}, ensure_ascii=False, indent=2))
