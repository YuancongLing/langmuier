"""Read-only S02-REAL PCAP baseline and independent complete storage read-back.

Only Python stdlib is used. The established PCAPNG/LVDSRAW container readers
are reused; AA/AB expectations are decoded directly from original wire bytes.
No Rust decoder, generated plot, or production counter supplies expectations.
"""
import argparse
from bisect import bisect_left
from collections import Counter
import csv
from decimal import Decimal
import hashlib
import json
from pathlib import Path
import statistics
import struct
import xml.etree.ElementTree as ET
import zipfile

from lvds_raw_recovery_reference import read_lvdsraw, read_pcap_udp, compare_raw_records

FPGA_SOURCE = "192.168.1.128:1534"
COMMON = bytes.fromhex("eb9055aa0107")
AA_COLUMNS = ("frame_index,frame_seconds,frame_microseconds,frame_seq_u8,sample_index,"
              "sample_time_us,ch1_code10,ch2_code10,ch1_adc16_reconstructed,"
              "ch2_adc16_reconstructed,integrity").split(",")
AB_COLUMNS = ("ab_frame_index,raw_record_id,udp_sequence_u32,pa0_seq,sci_frame_seq,"
              "frame_seconds,frame_microseconds,sample_index,sample_time_us,wave_mV,wave_V,"
              "f_act_mHz,f_act_Hz,work_mode,scan_freq,scan_voltage_mV,ad9833_status,"
              "dac8563_status,integrity,source,session,received_unix_nanos").split(",")


def require(condition, description):
    if not condition:
        raise ValueError(description)


def identity(path):
    path = Path(path)
    data = path.read_bytes()
    return {"path": str(path.resolve()), "bytes": len(data),
            "sha256": hashlib.sha256(data).hexdigest()}


def write_json(path, value):
    path = Path(path)
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("x", encoding="utf-8", newline="\n") as stream:
        json.dump(value, stream, ensure_ascii=False, indent=2)
        stream.write("\n")


def workbook_evidence(path):
    ns = {"s": "http://schemas.openxmlformats.org/spreadsheetml/2006/main"}
    sheets = {}
    with zipfile.ZipFile(path) as archive:
        shared = ["".join(item.itertext()) for item in ET.fromstring(
            archive.read("xl/sharedStrings.xml")).findall("s:si", ns)]
        links = {item.attrib["Id"]: item.attrib["Target"] for item in ET.fromstring(
            archive.read("xl/_rels/workbook.xml.rels"))}
        for item in ET.fromstring(archive.read("xl/workbook.xml")).find("s:sheets", ns):
            target = links[item.attrib["{http://schemas.openxmlformats.org/officeDocument/2006/relationships}id"]]
            location = target.lstrip("/") if target.startswith("/") else "xl/" + target
            cells = {}
            for cell in ET.fromstring(archive.read(location)).findall("s:sheetData/s:row/s:c", ns):
                value = cell.find("s:v", ns)
                value = "" if value is None else value.text
                if cell.attrib.get("t") == "s":
                    value = shared[int(value)]
                elif cell.attrib.get("t") == "inlineStr":
                    value = "".join(cell.find("s:is", ns).itertext())
                if value:
                    cells[cell.attrib["r"]] = value
            sheets[item.attrib["name"]] = cells
    ab = "LVDS_\u6fc0\u52b1\u4fe1\u53f7\u6570\u636e\u5305"
    ac = "LVDS_GNSS\u6570\u636e\u5305"
    public = "LVDS_\u516c\u5171\u683c\u5f0f"
    references = {}
    for name, refs in [
        (ab, [f"{column}{row}" for row in range(18, 32) for column in "HIJ"] + [f"F{row}" for row in range(34, 39)]),
        (ac, [f"{column}{row}" for row in range(18, 29) for column in "HIJ"] + ["F34", "F35"]),
        (public, ["F22", "F23", "F24", "F25", "I17", "I18"]),
    ]:
        references.update({name + "!" + ref: sheets[name][ref] for ref in refs if ref in sheets[name]})
    for reference, expected in [("I18", "0"), ("I19", "1"), ("I23", "127"), ("I24", "128")]:
        value = sheets[ac][reference]
        require(value.startswith("\u504f\u79fb" + expected + "\uff0c"), f"AC offset workbook mismatch {reference}: {value}")
    return {"workbook": identity(path), "direct_cell_references": references,
            "ac_data_offsets": {"gnss_valid": 0, "gnss_update_cnt": [1, 5],
                                "gnss_raw": [13, 127], "att_valid": 127,
                                "att_update_cnt": [128, 132], "att_raw": [140, 199],
                                "reserved": [199, 260]},
            "scope": "AC independent invalid-status evidence only; no S03 production decoder added"}


def unpack_science(data):
    channels = []
    for start in (8, 133):
        values = []
        for offset in range(start, start + 125, 5):
            packed = int.from_bytes(data[offset:offset + 5], "big")
            values.extend((packed >> shift) & 1023 for shift in (30, 20, 10, 0))
        channels.append(values)
    return channels


def cadence(values):
    deltas = [second - first for first, second in zip(values, values[1:])]
    return {"events": len(values), "duration_ns": values[-1] - values[0] if values else 0,
            "interval_min_ns": min(deltas) if deltas else None,
            "interval_max_ns": max(deltas) if deltas else None,
            "interval_median_ns": statistics.median(deltas) if deltas else None,
            "interval_mean_ns": sum(deltas) / len(deltas) if deltas else None,
            "backward_intervals": sum(delta < 0 for delta in deltas)}


def baseline(pcap):
    raw_records, packet_summary = read_pcap_udp(pcap)
    require(not packet_summary["packet_diagnostics"], "PCAP packet parse diagnostics must be reviewed")
    datagrams, aa, ab, ac, issues = [], [], [], [], []
    all_fpga, sequences, type_counts, fields = [], [], Counter(), Counter()
    for index, record in enumerate(raw_records):
        datum = {"udp_index": index, "raw_record_id": index + 1,
                 "packet_index": record["packet_index"], "source": record["source"],
                 "destination": record["destination"], "timestamp_ns": record["timestamp_ns"],
                 "payload_hex": record["payload"].hex(),
                 "payload_sha256": hashlib.sha256(record["payload"]).hexdigest(),
                 "source_matches": record["source"] == FPGA_SOURCE,
                 "capture_truncated": record["capture_truncated"]}
        datagrams.append(datum)
        require(not record["capture_truncated"], f"truncated PCAP packet {record['packet_index']}")
        if not datum["source_matches"]:
            continue
        all_fpga.append(datum)
        payload = record["payload"]
        try:
            require(len(payload) >= 24 and payload[:4] == bytes.fromhex("55aa01a5"), "outer magic/length")
            sequence, size, segment, segments, offset, length, flags = struct.unpack_from(">IIHHIHH", payload, 4)
            require((size, segment, segments, offset, length) == (269, 0, 1, 0, 269), "outer length/fragment fields")
            require(length == len(payload) - 24, "outer payload length")
            wire = payload[24:]
            require(len(wire) == 269 and wire[:6] == COMMON, "public LVDS framing")
            sequences.append(sequence)
            fields[(len(payload), flags)] += 1
            kind = {0xAA: "AA", 0xAB: "AB", 0xAC: "AC"}.get(wire[6], f"UNKNOWN_{wire[6]:02X}")
            type_counts[kind] += 1
            common = {"raw_record_id": index + 1, "packet_index": record["packet_index"],
                      "outer_sequence": sequence, "source": record["source"],
                      "received_unix_nanos": record["timestamp_ns"], "raw_crc_hex": wire[-2:].hex()}
            data = wire[7:267]
            if kind == "AA":
                seconds, micros = struct.unpack_from(">II", data)
                require(micros < 1000000 and data[259] == 0, "AA timestamp/reserved")
                ch1, ch2 = unpack_science(data)
                aa.append({**common, "frame_index": len(aa), "seconds": seconds, "microseconds": micros,
                           "seq8": data[258], "ch1_code10": ch1, "ch2_code10": ch2,
                           "sample_time_us": [seconds * 1000000 + micros + i * 500 for i in range(100)]})
            elif kind == "AB":
                values = struct.unpack_from(">IIIHHBBHBBBB", data)
                names = ["seconds", "microseconds", "f_act_mhz", "interval_us", "points", "work_mode",
                         "scan_freq", "scan_voltage_mv", "ad9833_status", "dac8563_status", "sci_frame_seq", "pa0_seq"]
                frame = dict(zip(names, values))
                wave = list(struct.unpack_from(">100H", data, 60))
                require(frame["microseconds"] < 1000000 and frame["points"] == 100, "AB time/points")
                require(frame["work_mode"] in (0x55, 0xAA, 0xBB), "AB mode")
                require(frame["ad9833_status"] in (0, 1) and frame["dac8563_status"] in (0, 1), "AB status")
                require(data[24:60] == bytes(36) and max(wave) <= 3300, "AB reserved/wave range")
                if frame["work_mode"] == 0xBB:
                    require(frame["scan_voltage_mv"] <= 5000, "AB DC setpoint")
                else:
                    require(frame["scan_freq"] <= 100 and frame["scan_voltage_mv"] == 0, "AB scan settings")
                latest = frame["seconds"] * 1000000 + frame["microseconds"]
                times = [latest - (99 - i) * frame["interval_us"] for i in range(100)]
                require(times[0] >= 0, "AB sample-time underflow")
                ab.append({**common, **frame, "frame_index": len(ab), "wave_mv": wave,
                           "sample_time_us": times, "reserved_hex": data[24:60].hex()})
            elif kind == "AC":
                ac.append({**common, "data_hex": data.hex(), "gnss_valid": data[0],
                           "gnss_update_cnt": int.from_bytes(data[1:5], "big"), "att_valid": data[127],
                           "att_update_cnt": int.from_bytes(data[128:132], "big"),
                           "gnss_raw_all_zero": not any(data[13:127]),
                           "att_raw_all_zero": not any(data[140:199]), "all_data_zero": not any(data)})
        except ValueError as error:
            issues.append({"raw_record_id": index + 1, "packet_index": record["packet_index"], "reason": str(error)})
    seq_deltas = [(second - first) & 0xFFFFFFFF for first, second in zip(sequences, sequences[1:])]
    ch1_counts = Counter(value for frame in aa for value in frame["ch1_code10"])
    ch2_counts = Counter(value for frame in aa for value in frame["ch2_code10"])
    summary = {**packet_summary, "fpga_udp_records": len(all_fpga),
               "other_udp_records": len(datagrams) - len(all_fpga), "type_counts": dict(type_counts),
               "other_udp_sources": dict(Counter(row["source"] for row in datagrams if not row["source_matches"])),
               "other_udp_destinations": dict(Counter(row["destination"] for row in datagrams if not row["source_matches"])),
               "other_udp_payload_lengths": dict(Counter(len(bytes.fromhex(row["payload_hex"])) for row in datagrams if not row["source_matches"])),
               "public_or_business_issues": issues, "all_udp_payload_bytes": sum(len(r["payload"]) for r in raw_records),
               "fpga_sources": dict(Counter(row["source"] for row in all_fpga)),
               "fpga_destinations": dict(Counter(row["destination"] for row in all_fpga)),
               "fpga_payload_length_flags": [{"payload_bytes": length, "flags": flags, "count": count} for (length, flags), count in fields.items()],
               "outer_sequence": {"first": sequences[0] if sequences else None, "last": sequences[-1] if sequences else None,
                                  "step_counts": dict(Counter(seq_deltas)), "non_unit_steps": sum(delta != 1 for delta in seq_deltas),
                                  "unique": len(set(sequences))},
               "all_udp_cadence": cadence([row["timestamp_ns"] for row in datagrams]),
               "fpga_cadence": cadence([row["timestamp_ns"] for row in all_fpga]),
               "aa": {"frames": len(aa), "samples_per_channel": sum(len(row["ch1_code10"]) for row in aa),
                      "ch1_code10_counts": dict(ch1_counts), "ch2_code10_counts": dict(ch2_counts),
                      "ch1_range": [min(ch1_counts), max(ch1_counts)] if ch1_counts else [],
                      "ch2_range": [min(ch2_counts), max(ch2_counts)] if ch2_counts else [],
                      "seq8_non_unit_steps": sum((b["seq8"] - a["seq8"]) % 256 != 1 for a, b in zip(aa, aa[1:])),
                      "cadence": cadence([row["received_unix_nanos"] for row in aa])},
               "ab": {"frames": len(ab), "samples": sum(len(row["wave_mv"]) for row in ab),
                      "wave_range_mv": [min(min(row["wave_mv"]) for row in ab), max(max(row["wave_mv"]) for row in ab)] if ab else [],
                      "pa0_seq": [row["pa0_seq"] for row in ab],
                      "pa0_non_unit_steps": sum((b["pa0_seq"] - a["pa0_seq"]) % 256 != 1 for a, b in zip(ab, ab[1:])),
                      "field_values": {key: sorted({row[key] for row in ab}) for key in ["f_act_mhz", "interval_us", "points", "work_mode", "scan_freq", "scan_voltage_mv", "ad9833_status", "dac8563_status"]},
                      "window_spans_us": sorted({row["sample_time_us"][-1] - row["sample_time_us"][0] for row in ab}),
                      "overlapping_adjacent_windows": sum(b["sample_time_us"][0] < a["sample_time_us"][-1] for a, b in zip(ab, ab[1:])),
                      "cadence": cadence([row["received_unix_nanos"] for row in ab])},
               "ac": {"frames": len(ac), "all_data_zero": all(row["all_data_zero"] for row in ac),
                      "field_values": {key: sorted({row[key] for row in ac}) for key in ["gnss_valid", "gnss_update_cnt", "att_valid", "att_update_cnt"]}}}
    require(not issues, f"independent original-PCAP audit found {len(issues)} structural/business issues")
    manifest = {"schema_version": 1, "input": identity(pcap), "summary": summary,
                "datagrams": datagrams, "aa": aa, "ab": ab, "ac": ac,
                "crc_policy": "Original CRC retained, production integrity CRC_NOT_CHECKED; CRC not an admission gate",
                "scope": "Actual original-PCAP offline software evidence; not live hardware or native GUI acceptance"}
    return manifest, raw_records


def associations(manifest):
    aa = manifest["aa"]
    order = [row["raw_record_id"] for row in aa]
    rows = []
    for frame in manifest["ab"]:
        position = bisect_left(order, frame["raw_record_id"])
        previous = aa[position - 1] if position else None
        following = aa[position] if position < len(aa) else None
        row = {"ab_frame_index": frame["frame_index"], "ab_raw_record_id": frame["raw_record_id"],
               "ab_packet_index": frame["packet_index"], "ab_outer_sequence_u32": frame["outer_sequence"],
               "pa0_seq": frame["pa0_seq"], "sci_frame_seq": frame["sci_frame_seq"],
               "ab_device_latest_us": frame["sample_time_us"][-1], "ab_received_unix_nanos": frame["received_unix_nanos"]}
        for label, science in [("previous_aa", previous), ("next_aa", following)]:
            row.update({label + "_raw_record_id": science["raw_record_id"] if science else None,
                        label + "_outer_sequence_u32": science["outer_sequence"] if science else None,
                        label + "_seq8": science["seq8"] if science else None,
                        label + "_device_start_us": science["sample_time_us"][0] if science else None,
                        "matches_" + label: frame["sci_frame_seq"] == science["seq8"] if science else None})
        rows.append(row)
    return rows


def audit(pcap, workbook, output_dir):
    output_dir = Path(output_dir)
    manifest, _ = baseline(pcap)
    association = associations(manifest)
    report = {"result": "PASS", "input": manifest["input"], **manifest["summary"],
              "ab_aa_association": {"rows": len(association),
                                    "matches_previous_aa": sum(row["matches_previous_aa"] is True for row in association),
                                    "matches_next_aa": sum(row["matches_next_aa"] is True for row in association),
                                    "meaning": "Original sci_frame_seq preserved; manufacturer must clarify latest-science semantics when next-AA matches"},
              "first_ab": manifest["ab"][0] if manifest["ab"] else None,
              "last_ab": manifest["ab"][-1] if manifest["ab"] else None,
              "scope": manifest["scope"]}
    evidence = workbook_evidence(workbook)
    write_json(output_dir / "independent-workbook-evidence.json", evidence)
    write_json(output_dir / "independent-manifest.json", manifest)
    write_json(output_dir / "independent-pcap-audit.json", report)
    with (output_dir / "independent-AB-AA-association.csv").open("x", encoding="utf-8", newline="") as stream:
        writer = csv.DictWriter(stream, fieldnames=list(association[0]) if association else [])
        writer.writeheader()
        writer.writerows(association)
    return report


def csv_rows(path, columns):
    stream = Path(path).open(newline="", encoding="utf-8-sig")
    reader = csv.DictReader(stream)
    require(reader.fieldnames == columns, f"CSV header mismatch: {path}: {reader.fieldnames}")
    return stream, reader


def match_fields(row, expected, context):
    require(None not in row, f"extra CSV fields {context}")
    for key, value in expected.items():
        actual = row[key]
        parsed = Decimal(actual) if isinstance(value, Decimal) else (int(actual) if isinstance(value, int) else actual)
        require(parsed == value, f"{context}: {key}: actual={actual!r}, expected={value!r}")


def verify_aa_csv(path, frames):
    stream, reader = csv_rows(path, AA_COLUMNS)
    count = 0
    with stream:
        for frame in frames:
            for index, (ch1, ch2) in enumerate(zip(frame["ch1_code10"], frame["ch2_code10"])):
                row = next(reader, None)
                require(row is not None, f"AA CSV ends before row {count}")
                expected = dict(zip(AA_COLUMNS, [frame["frame_index"], frame["seconds"], frame["microseconds"],
                                                frame["seq8"], index, frame["sample_time_us"][index], ch1, ch2,
                                                ch1 * 64 - 32768, ch2 * 64 - 32768, "CRC_NOT_CHECKED"]))
                match_fields(row, expected, f"AA CSV row {count}")
                count += 1
        require(next(reader, None) is None, "AA CSV contains extra rows")
    return {"result": "PASS", "identity": identity(path), "frames": len(frames), "data_rows": count,
            "columns_checked": AA_COLUMNS, "all_values_order_and_exact_csv_eof": True}


def verify_lpd(path, frames):
    data = Path(path).read_bytes()
    wanted_header = struct.pack("<8sHH6I2H", b"LMPROBE\0", 3, 40, 0x01020304, 100, 500, 409, 1, 3, 2, 8)
    require(data[:40] == wanted_header, "LPD v3 40-byte metadata/header mismatch")
    require(len(data) == 40 + len(frames) * 409, "LPD v3 missing/extra record bytes")
    for index, frame in enumerate(frames):
        values = struct.unpack_from("<IIB200H", data, 40 + index * 409)
        wanted = [frame["seconds"], frame["microseconds"], frame["seq8"], *frame["ch1_code10"], *frame["ch2_code10"]]
        require(list(values) == wanted, f"LPD v3 frame {index} raw time/seq8/code10 mismatch")
    return {"result": "PASS", "identity": identity(path), "frames": len(frames),
            "all_sample_values_checked": len(frames) * 200,
            "header_bytes": 40, "record_bytes": 409, "exact_eof": True}


def verify_ab_csv(path, frames, protocol_session):
    require(isinstance(protocol_session, str) and protocol_session and protocol_session != "unavailable",
            "production metadata must supply actual nonempty excitation_protocol_session")
    stream, reader = csv_rows(path, AB_COLUMNS)
    count = 0
    with stream:
        for frame in frames:
            for index, millivolts in enumerate(frame["wave_mv"]):
                row = next(reader, None)
                require(row is not None, f"AB CSV ends before row {count}")
                values = [frame["frame_index"], frame["raw_record_id"], frame["outer_sequence"],
                          frame["pa0_seq"], frame["sci_frame_seq"], frame["seconds"], frame["microseconds"],
                          index, frame["sample_time_us"][index], millivolts, Decimal(millivolts) / 1000,
                          frame["f_act_mhz"], Decimal(frame["f_act_mhz"]) / 1000,
                          f"0x{frame['work_mode']:02X}", frame["scan_freq"], frame["scan_voltage_mv"],
                          frame["ad9833_status"], frame["dac8563_status"], "CRC_NOT_CHECKED",
                          frame["source"], protocol_session, frame["received_unix_nanos"]]
                match_fields(row, dict(zip(AB_COLUMNS, values)), f"AB CSV row {count}")
                count += 1
        require(next(reader, None) is None, "AB CSV contains extra rows")
    return {"result": "PASS", "identity": identity(path), "frames": len(frames), "data_rows": count,
            "columns_checked": AB_COLUMNS, "all_fields_and_exact_csv_eof": True,
            "session_expectation_source": "production-summary.metadata.excitation_protocol_session (not CSV-derived)",
            "expected_protocol_session": protocol_session}


def verify(manifest_path, production_path, output):
    manifest = json.loads(Path(manifest_path).read_text(encoding="utf-8-sig"))
    production = json.loads(Path(production_path).read_text(encoding="utf-8-sig"))
    require(identity(manifest["input"]["path"]) == manifest["input"], "original PCAP identity changed since baseline")
    fresh_manifest, pcap_records = baseline(manifest["input"]["path"])
    for key in ("datagrams", "aa", "ab", "ac"):
        require(manifest[key] == fresh_manifest[key], f"cached independent {key} differs from original PCAP")
    metadata, files = production["metadata"], production["files"]
    require(Path(metadata["pcap_path"]).samefile(manifest["input"]["path"]),
            "production metadata points to a different PCAP input")
    actual_raw, raw_summary = read_lvdsraw(files["raw"])
    raw_comparison = compare_raw_records(pcap_records, actual_raw, raw_summary)
    for key, expected in metadata["raw_header"].items():
        actual = raw_summary["raw_header"][key]
        if key == "session_id_hex":
            actual, expected = bytes.fromhex(actual), bytes.fromhex(expected)
        require(actual == expected, f"RAW dynamic header mismatch: {key}")
    require(raw_summary["raw_header"]["expected_source"] == FPGA_SOURCE, "RAW expected-source header mismatch")
    require(raw_summary["footer"]["observed"] == len(manifest["datagrams"]), "RAW observed/complete input mismatch")
    result = {"result": "PASS", "input": manifest["input"], "manifest": identity(manifest_path),
              "production_summary": identity(production_path),
              "raw": {**raw_comparison, **raw_summary, "identity": identity(files["raw"]),
                      "all_record_metadata_and_bytes_checked": True},
              "science_csv": verify_aa_csv(files["science_csv"], manifest["aa"]),
              "science_lpd": verify_lpd(files["science_lpd"], manifest["aa"]),
              "excitation_csv": verify_ab_csv(files["excitation_csv"], manifest["ab"], metadata["excitation_protocol_session"]),
              "original_input_unchanged": True, "native_gui": "WINDOWS_GUI_NOT_RUN",
              "live_hardware": "REAL_HARDWARE_LIVE_NOT_RUN"}
    if "ui_entry_raw" in files:
        ui_raw, ui_summary = read_lvdsraw(files["ui_entry_raw"])
        result["ui_entry_raw"] = {**compare_raw_records(pcap_records, ui_raw, ui_summary),
                                  **ui_summary, "identity": identity(files["ui_entry_raw"]),
                                  "dynamic_header_semantics": "Container validated; separate UI export has its own opaque session ID"}
    write_json(output, result)
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    audit_parser = commands.add_parser("audit")
    audit_parser.add_argument("--pcap", type=Path, required=True)
    audit_parser.add_argument("--workbook", type=Path, required=True)
    audit_parser.add_argument("--output-dir", type=Path, required=True)
    verify_parser = commands.add_parser("verify")
    verify_parser.add_argument("--manifest", type=Path, required=True)
    verify_parser.add_argument("--production-summary", type=Path, required=True)
    verify_parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    result = (audit(args.pcap, args.workbook, args.output_dir) if args.command == "audit" else
              verify(args.manifest, args.production_summary, args.output))
    compact = {key: value for key, value in result.items() if key not in ("first_ab", "last_ab")}
    print(json.dumps(compact, ensure_ascii=True, indent=2))


if __name__ == "__main__":
    main()
