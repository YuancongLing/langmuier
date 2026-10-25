"""Read-only package checks for the current Langmuier Probe Ground Control delivery.

Run explicitly after packaging; this script never builds, launches, or repackages.
"""
import hashlib
import json
import struct
import zipfile
from pathlib import Path

repo = Path(__file__).resolve().parents[3]
source = repo / 'target/x86_64-pc-windows-msvc/release/langmuier.exe'
delivered = repo / 'target/delivery/windows-x64/LangmuierProbeGroundControl.exe'
readme = repo / 'docs/WINDOWS_README.md'
archive = repo / 'target/delivery/LangmuierProbeGroundControl-windows-x64.zip'
blob = source.read_bytes()
assert blob[:2] == b'MZ'
pe = struct.unpack_from('<I', blob, 0x3c)[0]
assert blob[pe:pe + 4] == b'PE\x00\x00'
machine, section_count = struct.unpack_from('<HH', blob, pe + 4)
optional_size = struct.unpack_from('<H', blob, pe + 20)[0]
optional = pe + 24
assert machine == 0x8664, hex(machine)
assert struct.unpack_from('<H', blob, optional)[0] == 0x20b
sections = []
for index in range(section_count):
    off = optional + optional_size + index * 40
    virtual_size, rva, raw_size, raw_offset = struct.unpack_from('<IIII', blob, off + 8)
    sections.append((rva, max(virtual_size, raw_size), raw_offset))


def offset(address):
    for start, size, raw in sections:
        if start <= address < start + size:
            return raw + address - start
    raise AssertionError(f'RVA not in PE sections: {address:#x}')


imports_rva, imports_size = struct.unpack_from('<II', blob, optional + 112 + 8)
cursor = offset(imports_rva)
imports = []
while any(blob[cursor:cursor + 20]):
    name_rva = struct.unpack_from('<I', blob, cursor + 12)[0]
    name_offset = offset(name_rva)
    end = blob.index(0, name_offset)
    imports.append(blob[name_offset:end].decode('ascii'))
    cursor += 20
assert not any(name.lower().startswith(('vcruntime', 'msvcp', 'ucrtbase', 'api-ms-win-crt'))
               for name in imports), imports
assert delivered.read_bytes() == blob
delivered_readme = delivered.with_name('README.md')
assert delivered_readme.read_bytes() == readme.read_bytes()
with zipfile.ZipFile(archive) as package:
    entries = sorted(package.namelist())
    assert entries == ['LangmuierProbeGroundControl.exe', 'README.md'], entries
    assert package.read('LangmuierProbeGroundControl.exe') == blob
    assert package.read('README.md') == readme.read_bytes()
    assert package.testzip() is None


def info(path):
    data = path.read_bytes()
    return {'path': str(path), 'bytes': len(data), 'sha256': hashlib.sha256(data).hexdigest()}


report = {
    'result': 'PASS',
    'source': info(source),
    'delivered_exe': info(delivered),
    'archive': info(archive),
    'readme': info(readme),
    'delivered_readme': info(delivered_readme),
    'zip_entries': entries,
    'zip_crc_and_content_match': True,
    'pe_machine': hex(machine),
    'pe_format': 'PE32+ Windows x64',
    'imports': sorted(imports),
    'no_dynamic_msvc_crt_imports': True,
    'build_command': 'cargo +stable-x86_64-pc-windows-msvc build --release -p langmuir-desktop --target x86_64-pc-windows-msvc --target-dir target --no-default-features --locked',
    'rustflags': '-C target-feature=+crt-static',
    'mock_feature': False,
    'hdf5_feature': False,
    'native_ui_evidence': 'separate current Windows GUI acceptance; this script verifies package bytes only',
}
destination = Path(__file__).with_name('windows-package-acceptance-verification.json')
destination.write_text(json.dumps(report, ensure_ascii=False, indent=2) + '\n', encoding='utf-8')
print(json.dumps(report, ensure_ascii=False, indent=2))
