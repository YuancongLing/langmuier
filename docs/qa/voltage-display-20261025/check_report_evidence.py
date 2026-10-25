"""Read-only final report links, screenshots, source and package audit."""
import hashlib
import json
from pathlib import Path
import re

REPO = Path(__file__).resolve().parents[3]
QA = Path(__file__).resolve().parent
REPORT = REPO / 'docs' / 'LVDS_VOLTAGE_DISPLAY_QA.md'
MANIFEST = QA / 'final-evidence-manifest.json'


def info(path):
    content = path.read_bytes()
    return {'path': str(path), 'bytes': len(content), 'sha256': hashlib.sha256(content).hexdigest()}


text = REPORT.read_text(encoding='utf-8')
links = []
for target in re.findall(r'!?\[[^\]]*\]\(([^)]+)\)', text):
    if target.startswith(('https://', 'http://', '#')):
        continue
    destination = (REPORT.parent / target).resolve()
    # This manifest is written only after every other link is verified.
    is_self = destination == MANIFEST.resolve()
    assert is_self or destination.is_file(), target
    links.append({'target': target, 'resolved': str(destination),
                  'status': 'SELF_REFERENCE' if is_self else 'PASS'})
assert 'IN_PROGRESS' not in text and '待补' not in text and '待验' not in text
required = json.loads((QA / 'required-checks.json').read_text(encoding='utf-8-sig'))
assert len(required) == 4 and all(row['exit_code'] == 0 for row in required)
raw = (QA / 'test.log').read_bytes()
test_text = raw.decode('utf-16') if raw.startswith((b'\xff\xfe', b'\xfe\xff')) else raw.decode('utf-8-sig')
suites = [{'passed': int(a), 'failed': int(b), 'ignored': int(c)}
          for a, b, c in re.findall(r'test result: ok\. (\d+) passed; (\d+) failed; (\d+) ignored;', test_text)]
total = {key: sum(suite[key] for suite in suites) for key in ('passed', 'failed', 'ignored')}
assert total == {'passed': 357, 'failed': 0, 'ignored': 7}
frozen = json.loads((QA / 'frozen-source-manifest.json').read_text(encoding='utf-8'))
for original in frozen['source_files']:
    assert info(Path(original['path'])) == original, original['path']
protected = json.loads((QA / 'unchanged-baseline.json').read_text(encoding='utf-8-sig'))
backups = json.loads((QA / 'previous-package.json').read_text(encoding='utf-8-sig'))
for original in protected['files'] + backups['files']:
    assert info(Path(original['path'])) == original, original['path']
package = json.loads((QA / 'windows-package-acceptance-verification.json').read_text(encoding='utf-8'))
assert package['result'] == 'PASS' and package['mock_feature'] is False
for key in ('source', 'delivered_exe', 'archive', 'readme', 'delivered_readme'):
    assert info(Path(package[key]['path'])) == package[key], key
evidence_files = []
for name in ('actual-voltage-data-audit.json', 'existing-storage-independent-readback.json',
             'existing-txt-pcap-provenance-check.json', 'new-gui-storage-independent-readback.json',
             'new-vs-existing-storage-byte-identity.json', 'gui-cursor-independent-readback.json',
             'history-cursor-independent-readback.json', 'txt-positive-cursor-independent-readback.json',
             'final-gui-storage-independent-readback.json', 'final-gui-cursor-independent-readback.json',
             'final-vs-existing-storage-byte-identity.json', 'final-normalized-cursor-independent-readback.json',
             'final-txt-positive-cursor-independent-readback.json'):
    evidence = json.loads((QA / name).read_text(encoding='utf-8'))
    assert evidence['status'] == 'PASS', name
    evidence_files.append(info(QA / name))
    if isinstance(evidence.get('screenshot'), dict):
        assert info(Path(evidence['screenshot']['path'])) == evidence['screenshot'], name
process = json.loads((QA / 'final-native-gui-process.json').read_text(encoding='utf-8-sig'))
assert process['exe_sha256'] == package['delivered_exe']['sha256']
assert Path(process['path']).resolve() == Path(package['delivered_exe']['path']).resolve()
pe = json.loads((QA / 'pe-fixture-correction-comparison.json').read_text(encoding='utf-8'))
assert pe['result'] == 'PASS'
screenshots = [info(path) for path in sorted((REPO / 'docs' / 'qa').glob('LVDS_VOLTAGE_*.jpg'))]
assert len(screenshots) >= 19
manifest = {'result': 'PASS', 'scope': 'current voltage software acceptance local artifact consistency; no hardware/calibration claim',
    'report': info(REPORT), 'plan': info(REPO / 'docs/superpowers/plans/2026-10-25-lvds-voltage-display.md'),
    'report_links_count': len(links), 'report_links': links, 'screenshot_count': len(screenshots),
    'screenshots': screenshots, 'required_commands': required, 'test_suites': suites, 'test_total': total,
    'frozen_source_files': len(frozen['source_files']), 'protected_files_unchanged': len(protected['files']),
    'old_package_backup_files_unchanged': len(backups['files']),
    'current_package': package, 'final_native_gui_process': process,
    'passing_evidence_files': evidence_files, 'pe_comparison': info(QA / 'pe-fixture-correction-comparison.json'),
    'source_or_scientific_data_modified_by_this_script': False}
with MANIFEST.open('x', encoding='utf-8', newline='\n') as stream:
    json.dump(manifest, stream, ensure_ascii=False, indent=2)
    stream.write('\n')
print(json.dumps({'result': 'PASS', 'report_links': len(links), 'screenshots': len(screenshots),
    'source_files': len(frozen['source_files']), 'protected': len(protected['files']),
    'old_package_backup_files': len(backups['files']), 'test_total': total}, ensure_ascii=False))
