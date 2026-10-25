"""Read original supplied TXT for the final EXE positive cursor screenshot."""
import argparse
import bisect
import json
from pathlib import Path
import sys

REPO = Path(__file__).resolve().parents[3]
QA = Path(__file__).resolve().parent
sys.path.insert(0, str(REPO / 'tools' / 'qa'))
from lvds_txt_reference import decode_frames, file_info, assert_supplied_baseline
from lvds_txt_to_pcapng import read_hex_frames

parser = argparse.ArgumentParser()
parser.add_argument('--top-time-us', type=int, required=True)
parser.add_argument('--screenshot', type=Path, required=True)
args = parser.parse_args()
source = REPO / 'lvds.txt'
frames = decode_frames(read_hex_frames(source))
assert_supplied_baseline(frames)
points = []
for frame in frames:
    for sample in range(100):
        codes = [frame[f'channel_{channel}_code10'][sample] for channel in (1, 2)]
        points.append({'time_us': frame['first_sample_time_us'] + sample * 500,
            'frame_index': frame['frame_index'], 'seq8': frame['frame_seq_u8'],
            'sample_index': sample, 'code10': codes,
            'adc16': [code * 64 - 32768 for code in codes],
            'volts': [round((code * 64 - 32768) * 0.00015625, 12) for code in codes],
            'display': [f'{(code * 64 - 32768) * 0.00015625:.2f} V' for code in codes]})
times = [point['time_us'] for point in points]
index = bisect.bisect_left(times, args.top_time_us)
point = min(points[max(0, index - 1):index + 1], key=lambda value: abs(value['time_us'] - args.top_time_us))
assert point['display'] == ['3.74 V', '3.34 V']
process = json.loads((QA / 'final-native-gui-process.json').read_text(encoding='utf-8-sig'))
report = {'status': 'PASS', 'scope': 'original TXT independent values for final non-Mock EXE positive cursor screenshot',
    'exe_sha256': process['exe_sha256'], 'source': file_info(source), 'source_is_network_capture': False,
    'network_envelope': 'existing artificial UDP/PCAPNG wrapper; inner bytes identical to TXT',
    'pointer_time_us': args.top_time_us, 'pointer_to_nearest_sample_delta_us': args.top_time_us - point['time_us'],
    'nearest_sample': point, 'exact_zero_code512_samples': sum(code == 512 for frame in frames for channel in (1, 2) for code in frame[f'channel_{channel}_code10']),
    'zero_axis_note': 'actual waveforms cross zero; exact zero sample verified by production numerical and legal-frame automated tests',
    'screenshot': file_info(args.screenshot.resolve())}
with (QA / 'final-txt-positive-cursor-independent-readback.json').open('x', encoding='utf-8', newline='\n') as stream:
    json.dump(report, stream, ensure_ascii=False, indent=2)
    stream.write('\n')
print(json.dumps(report, ensure_ascii=False, indent=2))
