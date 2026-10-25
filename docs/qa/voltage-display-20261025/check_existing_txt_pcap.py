"""Read existing TXT-derived PCAPNG only; never generate replacement packets."""
import json
from pathlib import Path
import sys

REPO = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(REPO / 'tools' / 'qa'))
from lvds_raw_recovery_reference import read_pcap_udp, _unwrap
from lvds_txt_reference import file_info
from lvds_txt_to_pcapng import read_hex_frames

txt = REPO / 'lvds.txt'
pcap = REPO / 'lvds.pcapng'
wires = read_hex_frames(txt)
records, summary = read_pcap_udp(pcap)
assert len(records) == len(wires) == 20
for index, (record, wire) in enumerate(zip(records, wires)):
    wrapper, original_inner = _unwrap(record['payload'])
    assert original_inner == wire, f'TXT-derived PCAP record {index} bytes/order differ'
report = {'status': 'PASS', 'scope': 'existing derived input, not a network capture',
          'scientific_inputs_generated': False, 'existing_txt': file_info(txt),
          'existing_derived_pcap': file_info(pcap), 'pcap_readback': summary,
          'inner_frames_checked': len(wires), 'inner_bytes_checked': len(wires) * 269,
          'bytes_and_order_equal': True,
          'existing_provenance': file_info(REPO / 'lvds.pcapng.provenance.json')}
with (Path(__file__).resolve().parent / 'existing-txt-pcap-provenance-check.json').open('x', encoding='utf-8', newline='\n') as stream:
    json.dump(report, stream, ensure_ascii=False, indent=2)
    stream.write('\n')
print(json.dumps(report, ensure_ascii=False, indent=2))
