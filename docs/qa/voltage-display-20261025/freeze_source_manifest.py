import hashlib
import json
from pathlib import Path

repo = Path(__file__).resolve().parents[3]
qa = Path(__file__).parent


def info(path):
    content = path.read_bytes()
    return {"path": str(path), "bytes": len(content),
            "sha256": hashlib.sha256(content).hexdigest()}


protected = json.loads((qa / "unchanged-baseline.json").read_text(encoding="utf-8-sig"))
for original in protected["files"]:
    assert info(Path(original["path"])) == original, original["path"]
backups = json.loads((qa / "previous-package.json").read_text(encoding="utf-8-sig"))
for original in backups["files"]:
    assert info(Path(original["path"])) == original, original["path"]
paths = [repo / "Cargo.toml", repo / "Cargo.lock"]
for directory in ("apps", "crates", "tools/protocol-cli"):
    for path in (repo / directory).rglob("*"):
        if path.suffix == ".rs" or path.name == "Cargo.toml":
            paths.append(path)
paths = sorted(set(paths))
manifest = {"result": "PASS", "protected_files_unchanged": len(protected["files"]),
            "old_delivery_backup_unchanged": len(backups["files"]),
            "source_files": [info(path) for path in paths]}
destination = qa / "frozen-source-manifest.json"
if destination.exists():
    assert json.loads(destination.read_text(encoding="utf-8")) == manifest
else:
    destination.write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8")
print(json.dumps({"result": "PASS", "source_files": len(paths),
                  "protected": len(protected["files"]), "backup_files": len(backups["files"])}))
