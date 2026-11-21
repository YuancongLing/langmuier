"""Read the original PH20261115 workbook without Excel or third-party libraries."""
import hashlib
import json
from pathlib import Path
import xml.etree.ElementTree as ET
from zipfile import ZipFile


def audit(workbook):
    ns = {"m": "http://schemas.openxmlformats.org/spreadsheetml/2006/main"}
    rel_ns = "http://schemas.openxmlformats.org/officeDocument/2006/relationships"
    with ZipFile(workbook) as archive:
        shared = []
        if "xl/sharedStrings.xml" in archive.namelist():
            shared = ["".join(t.text or "" for t in item.findall(".//m:t", ns))
                      for item in ET.fromstring(archive.read("xl/sharedStrings.xml"))]
        relationships = {item.attrib["Id"]: item.attrib["Target"]
                         for item in ET.fromstring(archive.read("xl/_rels/workbook.xml.rels"))}
        sheets = ET.fromstring(archive.read("xl/workbook.xml")).findall("m:sheets/m:sheet", ns)
        selected = {}
        for sheet in sheets:
            name = sheet.attrib["name"]
            if name not in ("LVDS_\u6fc0\u52b1\u4fe1\u53f7\u6570\u636e\u5305", "LVDS_\u516c\u5171\u683c\u5f0f"):
                continue
            target = relationships[sheet.attrib[f"{{{rel_ns}}}id"]]
            target = target.lstrip("/") if target.startswith("/") else "xl/" + target
            cells = {}
            for cell in ET.fromstring(archive.read(target)).findall(".//m:sheetData/m:row/m:c", ns):
                address = cell.attrib["r"]
                value = cell.find("m:v", ns)
                text = value.text if value is not None else None
                if cell.attrib.get("t") == "s" and text is not None:
                    text = shared[int(text)]
                elif cell.attrib.get("t") == "inlineStr":
                    text = "".join(t.text or "" for t in cell.findall(".//m:t", ns))
                if text is not None:
                    cells[address] = text
            if name.endswith("\u6570\u636e\u5305"):
                addresses = [f"{column}{row}" for row in range(17, 32) for column in ("I", "J")]
                addresses += [f"F{row}" for row in range(34, 39)]
                cells = {address: cells.get(address) for address in addresses}
            selected[name] = cells
    assert len(selected) == 2
    return {"workbook": str(workbook.resolve()), "sha256": hashlib.sha256(workbook.read_bytes()).hexdigest(),
            "sheets": selected, "interpretation": "Raw workbook cells; no vendor status semantics invented"}


if __name__ == "__main__":
    original = next(Path.cwd().glob("*PH20261115*.xlsx"))
    report = audit(original)
    output = Path("docs/qa/lvds-multitype-s02/protocol-cells.json")
    with output.open("x", encoding="utf-8") as stream:
        json.dump(report, stream, ensure_ascii=False, indent=2)
        stream.write("\n")
    print(json.dumps(report, ensure_ascii=True, indent=2))
