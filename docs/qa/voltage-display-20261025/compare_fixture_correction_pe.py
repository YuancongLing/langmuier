"""Read-only PE section comparison; never assumes a binary difference is metadata."""
import hashlib
import json
from pathlib import Path
import struct

REPO = Path(__file__).resolve().parents[3]
QA = Path(__file__).resolve().parent
OLD = REPO / 'target/delivery/voltage-before-fixture-correction-20261025/LangmuierProbeGroundControl.exe'
NEW = REPO / 'target/delivery/windows-x64/LangmuierProbeGroundControl.exe'


def digest(data):
    return hashlib.sha256(data).hexdigest()


def parse(path):
    data = path.read_bytes()
    assert data[:2] == b'MZ'
    pe = struct.unpack_from('<I', data, 0x3c)[0]
    assert data[pe:pe + 4] == b'PE\0\0'
    machine, count, timestamp, _, _, optional_size, _ = struct.unpack_from('<HHIIIHH', data, pe + 4)
    optional = pe + 24
    assert struct.unpack_from('<H', data, optional)[0] == 0x20b
    sections = []
    for index in range(count):
        base = optional + optional_size + index * 40
        name = data[base:base + 8].rstrip(b'\0').decode('ascii')
        virtual_size, virtual_address, raw_size, raw_offset = struct.unpack_from('<IIII', data, base + 8)
        section = {'name': name, 'virtual_size': virtual_size, 'virtual_address': virtual_address,
            'raw_size': raw_size, 'raw_offset': raw_offset, 'characteristics': struct.unpack_from('<I', data, base + 36)[0]}
        section['sha256'] = digest(data[raw_offset:raw_offset + raw_size])
        sections.append(section)
    def rva_offset(rva):
        for section in sections:
            if section['virtual_address'] <= rva < section['virtual_address'] + max(section['virtual_size'], section['raw_size']):
                return section['raw_offset'] + rva - section['virtual_address']
        raise ValueError(f'RVA {rva:x} not contained in a section')
    debug_rva, debug_size = struct.unpack_from('<II', data, optional + 112 + 6 * 8)
    ranges = [{'name': 'COFF.TimeDateStamp', 'start': pe + 8, 'bytes': 4}]
    entries = []
    if debug_rva:
        debug_offset = rva_offset(debug_rva)
        assert debug_size % 28 == 0
        for offset in range(debug_offset, debug_offset + debug_size, 28):
            characteristics, debug_timestamp, major, minor, kind, size, address, pointer = struct.unpack_from('<IIHHIIII', data, offset)
            entry = {'offset': offset, 'type': kind, 'size': size, 'payload_offset': pointer,
                'timestamp': debug_timestamp, 'major': major, 'minor': minor}
            ranges.append({'name': f'DEBUG_DIRECTORY.type{kind}.TimeDateStamp', 'start': offset + 4, 'bytes': 4})
            if kind == 2 and data[pointer:pointer + 4] == b'RSDS':
                entry.update({'format': 'RSDS', 'guid_hex': data[pointer + 4:pointer + 20].hex(),
                    'age': struct.unpack_from('<I', data, pointer + 20)[0],
                    'pdb_path': data[pointer + 24:pointer + size].rstrip(b'\0').decode('utf-8', errors='replace')})
                ranges.append({'name': 'CodeView.RSDS.GUID', 'start': pointer + 4, 'bytes': 16})
            if kind == 16:
                entry['payload_hex'] = data[pointer:pointer + size].hex()
                ranges.append({'name': 'IMAGE_DEBUG_TYPE_REPRO.payload', 'start': pointer, 'bytes': size})
            entries.append(entry)
    return data, {'path': str(path), 'bytes': len(data), 'sha256': digest(data),
        'machine': hex(machine), 'coff_timestamp': timestamp, 'sections': sections,
        'debug_entries': entries, 'identified_metadata_ranges': ranges}


old_data, old = parse(OLD)
new_data, new = parse(NEW)
assert len(old_data) == len(new_data)
old_layout = [{key: value for key, value in section.items() if key != 'sha256'} for section in old['sections']]
new_layout = [{key: value for key, value in section.items() if key != 'sha256'} for section in new['sections']]
layout_equal = old_layout == new_layout
differences = [index for index, (a, b) in enumerate(zip(old_data, new_data)) if a != b]
groups = []
for index in differences:
    if groups and groups[-1]['end_exclusive'] == index:
        groups[-1]['end_exclusive'] += 1
    else:
        groups.append({'start': index, 'end_exclusive': index + 1})
unexplained = []
for group in groups:
    group.update({'bytes': group['end_exclusive'] - group['start'],
        'old_hex': old_data[group['start']:group['end_exclusive']].hex(),
        'new_hex': new_data[group['start']:group['end_exclusive']].hex()})
    names = sorted({item['name'] for item in old['identified_metadata_ranges']
        if item['start'] <= group['start'] and item['start'] + item['bytes'] >= group['end_exclusive']})
    group['metadata_ranges_covering_entire_difference'] = names
    if not names:
        unexplained.append(group)
sections = []
for a, b in zip(old['sections'], new['sections']):
    section_diff = [index for index in differences if a['raw_offset'] <= index < a['raw_offset'] + a['raw_size']]
    sections.append({'name': a['name'], 'raw_bytes': a['raw_size'], 'old_sha256': a['sha256'],
        'new_sha256': b['sha256'], 'identical': a['sha256'] == b['sha256'],
        'different_byte_count': len(section_diff), 'executable': bool(a['characteristics'] & 0x20000000)})
metadata_only = layout_equal and not unexplained
report = {'result': 'PASS', 'scope': 'independent full PE byte and section comparison only; not a native GUI test',
    'old': old, 'new': new, 'same_file_length': True, 'identical_section_layout': layout_equal,
    'different_byte_count': len(differences), 'difference_runs': groups, 'sections': sections,
    'all_executable_sections_identical': all(section['identical'] for section in sections if section['executable']),
    'all_differences_fall_within_identified_pe_build_metadata': metadata_only,
    'unexplained_difference_runs': unexplained,
    'gui_evidence_note': 'Different whole-file hashes identify different builds; report old and final GUI artifacts with their own build hash.'}
with (QA / 'pe-fixture-correction-comparison.json').open('x', encoding='utf-8', newline='\n') as stream:
    json.dump(report, stream, ensure_ascii=False, indent=2)
    stream.write('\n')
print(json.dumps({'result': 'PASS', 'old_sha256': old['sha256'], 'new_sha256': new['sha256'],
    'different_byte_count': len(differences), 'sections': sections,
    'all_executable_sections_identical': report['all_executable_sections_identical'],
    'metadata_only': metadata_only, 'difference_runs': groups}, ensure_ascii=False, indent=2))
