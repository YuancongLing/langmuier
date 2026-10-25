"""Read-only byte identity and screenshot cursor checks against actual PCAP."""
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


cursor = 366_837_146
index = bisect.bisect_left(times, cursor)
near = min(points[max(0, index - 1):index + 1], key=lambda point: abs(point['time_us'] - cursor))
assert with_values(near)['display'] == ['-2.03 V', '-2.02 V']
tooltip_time = 366_863_465
tooltip = points[bisect.bisect_left(times, tooltip_time)]
assert tooltip['time_us'] == tooltip_time
assert with_values(tooltip)['display'][1] == '-2.83 V'
cursor_report = {'status': 'PASS', 'scope': 'independent actual-PCAP sample lookup for native GUI screenshot text',
    'screenshot': 'docs/qa/LVDS_VOLTAGE_ACTUAL_PCAP_CURSOR.jpg',
    'top_cursor_time_us': cursor, 'nearest_sample': with_values(near),
    'top_cursor_to_nearest_sample_delta_us': cursor - near['time_us'],
    'tooltip_sample': with_values(tooltip), 'tooltip_channel': 2,
    'all_native_samples': len(points), 'reference': file_info(QA / 'actual-pcap-reference-frames.jsonl')}
with (QA / 'gui-cursor-independent-readback.json').open('x', encoding='utf-8', newline='\n') as stream:
    json.dump(cursor_report, stream, ensure_ascii=False, indent=2)
    stream.write('\n')

comparisons = []
for suffix in ('.csv', '.lpd'):
    old = REPO / 'data' / ('scientific-20261025-023840.901' + suffix)
    new = REPO / 'data' / ('scientific-20261025-115147.359' + suffix)
    same = old.read_bytes() == new.read_bytes()
    assert same, f'old/new {suffix} differ'
    comparisons.append({'old': file_info(old), 'new': file_info(new), 'byte_for_byte_equal': same})
storage_report = {'status': 'PASS', 'scope': 'previous ADC-display versus new voltage-display actual-PCAP scientific outputs',
    'display_unit_change_modifies_scientific_files': False, 'files': comparisons}
with (QA / 'new-vs-existing-storage-byte-identity.json').open('x', encoding='utf-8', newline='\n') as stream:
    json.dump(storage_report, stream, ensure_ascii=False, indent=2)
    stream.write('\n')
print(json.dumps({'cursor': cursor_report, 'storage': storage_report}, ensure_ascii=False, indent=2))
