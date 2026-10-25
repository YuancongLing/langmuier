"""Independent history cursor lookup from native samples of actual PCAP."""
import bisect
import json
from pathlib import Path

QA = Path(__file__).resolve().parent
rows = [json.loads(line) for line in (QA / 'actual-pcap-reference-frames.jsonl').read_text(encoding='utf-8').splitlines()]
points = []
native_index = 0
for row in rows:
    if row['quality'] != 'NATIVE':
        continue
    selected = row['selected']
    for sample in range(100):
        codes = [selected[f'channel_{channel}_code10'][sample] for channel in (1, 2)]
        points.append({'time_us': selected['device_time_us'] + sample * 500,
            'frame_index': native_index, 'seq8': selected['seq8'], 'sample_index': sample,
            'code10': codes, 'adc16': [code * 64 - 32768 for code in codes],
            'volts': [round(code * 0.01 - 5.12, 12) for code in codes],
            'display': [f'{code * 0.01 - 5.12:.2f} V' for code in codes]})
    native_index += 1
times = [point['time_us'] for point in points]


def nearest(time):
    index = bisect.bisect_left(times, time)
    point = min(points[max(0, index - 1):index + 1], key=lambda value: abs(value['time_us'] - time))
    return {'pointer_time_us': time, 'delta_us': time - point['time_us'], 'nearest_sample': point}


normalized_top = nearest(350_078_420)
assert normalized_top['nearest_sample']['display'] == ['-2.04 V', '-2.03 V']
normalized_hover = nearest(350_099_964)
assert normalized_hover['delta_us'] == 0
min_v, max_v = -3.45, -1.91
normalized = (normalized_hover['nearest_sample']['volts'][1] - min_v) / (max_v - min_v)
assert f'{normalized:.2f}' == '0.49'
pan_top = nearest(350_894_364)
assert pan_top['nearest_sample']['display'] == ['-2.44 V', '-2.43 V']
pan_hover = nearest(350_911_464)
assert pan_hover['delta_us'] == 0 and pan_hover['nearest_sample']['display'][1] == '-2.96 V'
report = {'status': 'PASS', 'scope': 'new non-Mock GUI history/normalized screenshot literal lookup from original actual PCAP',
    'normalized_top': normalized_top,
    'normalized_hover': {**normalized_hover, 'channel': 2, 'min_voltage_v': min_v,
        'max_voltage_v': max_v, 'normalized_value': normalized,
        'normalized_display': f'{normalized:.2f}', 'normalized_unit': 'dimensionless'},
    'pan_top': pan_top, 'pan_hover': pan_hover,
    'screenshot_inputs': ['docs/qa/LVDS_VOLTAGE_NORMALIZED_CURSOR.jpg',
        'docs/qa/LVDS_VOLTAGE_LPD_PAN_MANUAL.jpg', 'docs/qa/LVDS_VOLTAGE_LPD_MANUAL_RETAINED.jpg']}
with (QA / 'history-cursor-independent-readback.json').open('x', encoding='utf-8', newline='\n') as stream:
    json.dump(report, stream, ensure_ascii=False, indent=2)
    stream.write('\n')
print(json.dumps(report, ensure_ascii=False, indent=2))
