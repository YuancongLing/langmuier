"""Read-only final LPD normalized cursor lookup from original PCAP."""
import bisect
import hashlib
import json
from pathlib import Path

QA = Path(__file__).resolve().parent
REPO = QA.parents[2]
points = []
for row in map(json.loads, (QA / 'actual-pcap-reference-frames.jsonl').read_text(encoding='utf-8').splitlines()):
    if row['quality'] != 'NATIVE':
        continue
    selected = row['selected']
    for sample in range(100):
        codes = [selected[f'channel_{channel}_code10'][sample] for channel in (1, 2)]
        points.append({'time_us': selected['device_time_us'] + sample * 500,
            'seq8': selected['seq8'], 'sample_index': sample, 'code10': codes,
            'adc16': [code * 64 - 32768 for code in codes],
            'volts': [round(code * 0.01 - 5.12, 12) for code in codes],
            'display': [f'{code * 0.01 - 5.12:.2f} V' for code in codes]})
times = [point['time_us'] for point in points]


def nearest(time):
    index = bisect.bisect_left(times, time)
    point = min(points[max(0, index - 1):index + 1], key=lambda value: abs(value['time_us'] - time))
    return {'pointer_time_us': time, 'delta_us': time - point['time_us'], 'nearest_sample': point}


top = nearest(350_090_011)
assert top['nearest_sample']['display'] == ['-2.39 V', '-2.38 V']
hover = nearest(350_099_964)
assert hover['delta_us'] == 0
min_v, max_v = -3.45, -1.91
assert min(point['volts'][1] for point in points) == min_v
assert max(point['volts'][1] for point in points) == max_v
normalized = (hover['nearest_sample']['volts'][1] - min_v) / (max_v - min_v)
assert f'{normalized:.2f}' == '0.49'
screenshot = REPO / 'docs/qa/LVDS_VOLTAGE_FINAL_NORMALIZED_CURSOR.jpg'
process = json.loads((QA / 'final-native-gui-process.json').read_text(encoding='utf-8-sig'))
report = {'status': 'PASS', 'scope': 'final non-Mock EXE LPD normalized screenshot literal lookup from original actual PCAP',
    'exe_sha256': process['exe_sha256'], 'top': top,
    'hover': {**hover, 'channel': 2, 'min_voltage_v': min_v, 'max_voltage_v': max_v,
        'normalized_value': normalized, 'normalized_display': f'{normalized:.2f}', 'normalized_unit': 'dimensionless'},
    'screenshot': {'path': str(screenshot), 'bytes': screenshot.stat().st_size,
        'sha256': hashlib.sha256(screenshot.read_bytes()).hexdigest()}}
with (QA / 'final-normalized-cursor-independent-readback.json').open('x', encoding='utf-8', newline='\n') as stream:
    json.dump(report, stream, ensure_ascii=False, indent=2)
    stream.write('\n')
print(json.dumps(report, ensure_ascii=False, indent=2))
