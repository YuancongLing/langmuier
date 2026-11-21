"""Independent S02-REAL-02 original-byte audit and complete storage read-back.

No production decoder is called. Established stdlib PCAPNG, RAW, AA and LPD
reference readers are reused without editing their prior-round artifacts.
Zero interval preserves configuration and slots but supplies no sample time.
"""
import argparse
from bisect import bisect_left
from collections import Counter
import csv
from decimal import Decimal
import json
from pathlib import Path
import struct

import lvds_s02_real_reference as base
from lvds_raw_recovery_reference import reference_crc, read_lvdsraw, compare_raw_records

AB_COLUMNS = base.AB_COLUMNS + ["interval_us", "points", "waveform_status"]
require = base.require
identity = base.identity
write_json = base.write_json


def pcap_interfaces(path):
    """Record actual IDB if_tsresol bytes rather than assuming a time unit."""
    interfaces = []
    endian = None
    section = -1
    with Path(path).open("rb") as stream:
        while header := stream.read(8):
            require(len(header) == 8, "short PCAPNG block header")
            if header[:4] == bytes.fromhex("0a0d0d0a"):
                bom = stream.read(4)
                endian = "<" if bom == bytes.fromhex("4d3c2b1a") else ">"
                size = struct.unpack(endian + "I", header[4:])[0]
                body = bom + stream.read(size - 12)
                section += 1
            else:
                require(endian is not None, "missing PCAPNG section")
                size = struct.unpack(endian + "I", header[4:])[0]
                body = stream.read(size - 8)
            require(len(body) == size - 8, "short PCAPNG interface audit block")
            if struct.unpack(endian + "I", header[:4])[0] != 1:
                continue
            linktype, reserved, snaplen = struct.unpack_from(endian + "HHI", body)
            cursor, tsresol = 8, 6
            while cursor + 4 <= len(body) - 4:
                kind, length = struct.unpack_from(endian + "HH", body, cursor)
                cursor += 4
                if kind == 0:
                    break
                option = body[cursor:cursor + length]
                if kind == 9:
                    require(length == 1, "invalid if_tsresol option")
                    tsresol = option[0]
                cursor += (length + 3) // 4 * 4
            units = 2 ** (tsresol & 127) if tsresol & 128 else 10 ** tsresol
            interfaces.append({"section": section, "linktype": linktype, "reserved": reserved,
                               "snaplen": snaplen, "if_tsresol": tsresol,
                               "ticks_per_second": units})
    return interfaces


def stage_key(frame):
    return (frame["work_mode"], frame["scan_voltage_mv"] if frame["work_mode"] == 0xBB else frame["scan_freq"])


def stages(frames):
    result = []
    groups = []
    for frame in frames:
        if not groups or stage_key(frame) != stage_key(groups[-1][-1]):
            groups.append([])
        frame["stage_index"] = len(groups) - 1
        groups[-1].append(frame)
    for index, group in enumerate(groups):
        first, last = group[0], group[-1]
        result.append({"stage_index": index, "start_ab_index": first["frame_index"],
                       "end_ab_index": last["frame_index"], "frames": len(group),
                       "work_mode": first["work_mode"], "mode": {0x55: "TRIANGLE", 0xAA: "SINE", 0xBB: "DC"}[first["work_mode"]],
                       "scan_freq": first["scan_freq"], "scan_voltage_mv": first["scan_voltage_mv"],
                       "f_act_mhz_values": sorted({row["f_act_mhz"] for row in group}),
                       "interval_us_values": sorted({row["interval_us"] for row in group}),
                       "state_counts": dict(Counter(row["state"] for row in group)),
                       "first_pa0_seq": first["pa0_seq"], "last_pa0_seq": last["pa0_seq"],
                       "first_raw_record_id": first["raw_record_id"], "last_raw_record_id": last["raw_record_id"],
                       "wave_range_mv": [min(min(row["wave_mv"]) for row in group), max(max(row["wave_mv"]) for row in group)]})
    return result


def associations(manifest):
    science = manifest["aa"]
    order = [frame["raw_record_id"] for frame in science]
    result = []
    for frame in manifest["ab"]:
        index = bisect_left(order, frame["raw_record_id"])
        previous = science[index - 1] if index else None
        following = science[index] if index < len(science) else None
        result.append({"ab_frame_index": frame["frame_index"], "stage_index": frame["stage_index"],
                       "raw_record_id": frame["raw_record_id"], "ab_outer_sequence": frame["outer_sequence"],
                       "pa0_seq": frame["pa0_seq"], "previous_aa_seq8": previous["seq8"] if previous else None,
                       "sci_frame_seq": frame["sci_frame_seq"], "next_aa_seq8": following["seq8"] if following else None,
                       "previous_aa_raw_record_id": previous["raw_record_id"] if previous else None,
                       "next_aa_raw_record_id": following["raw_record_id"] if following else None,
                       "matches_previous_aa": frame["sci_frame_seq"] == previous["seq8"] if previous else None,
                       "matches_next_aa": frame["sci_frame_seq"] == following["seq8"] if following else None,
                       "state": frame["state"]})
    return result


def baseline(path):
    manifest, pcap_records = base.baseline(path)
    crc_counts, crc_types, crc_errors = Counter(), Counter(), []
    crc_records = []
    for record in manifest["datagrams"]:
        if not record["source_matches"]:
            continue
        wire = bytes.fromhex(record["payload_hex"])[24:]
        actual = int.from_bytes(wire[-2:], "big")
        calculated = reference_crc(wire[7:267])
        matches = actual == calculated
        kind = f"{wire[6]:02X}"
        row = {"raw_record_id": record["raw_record_id"], "packet_type": kind,
               "raw_crc_u16": actual, "calculated_crc_u16": calculated, "matches": matches}
        crc_records.append(row)
        crc_counts["matching" if matches else "mismatch"] += 1
        crc_types[kind] += 1
        if not matches:
            crc_errors.append(row)
    for frame in manifest["ab"]:
        frame["state"] = "TIMED_SAMPLES" if frame["interval_us"] > 0 else "NO_SAMPLE_TIME"
        frame["waveform_status"] = frame["state"]
        if frame["interval_us"] == 0:
            frame["sample_time_us"] = [None] * len(frame["wave_mv"])
    manifest["stages"] = stages(manifest["ab"])
    manifest["schema_version"] = 2
    manifest["csv_version"] = 2
    manifest["crc_reference_records"] = crc_records
    manifest["interfaces"] = pcap_interfaces(path)
    ab = manifest["ab"]
    timed = [frame for frame in ab if frame["state"] == "TIMED_SAMPLES"]
    no_time = [frame for frame in ab if frame["state"] == "NO_SAMPLE_TIME"]
    manifest["summary"]["ab"].update({
        "state_counts": dict(Counter(frame["state"] for frame in ab)),
        "timed_sample_slots": sum(len(frame["wave_mv"]) for frame in timed),
        "no_sample_time_slots": sum(len(frame["wave_mv"]) for frame in no_time),
        "all_zero_frames": sum(not any(frame["wave_mv"]) for frame in ab),
        "no_sample_time_all_zero": all(not any(frame["wave_mv"]) for frame in no_time),
        "no_sample_time_all_zero_frequency": all(frame["f_act_mhz"] == 0 for frame in no_time),
        "window_spans_us": sorted({frame["sample_time_us"][-1] - frame["sample_time_us"][0] for frame in timed}),
        "overlapping_adjacent_windows": sum(a["state"] == b["state"] == "TIMED_SAMPLES"
                                            and b["sample_time_us"][0] < a["sample_time_us"][-1]
                                            for a, b in zip(ab, ab[1:])),
        "pa0_wraps": [{"previous_ab_index": a["frame_index"], "next_ab_index": b["frame_index"],
                       "previous_pa0": a["pa0_seq"], "next_pa0": b["pa0_seq"]}
                      for a, b in zip(ab, ab[1:]) if b["pa0_seq"] < a["pa0_seq"]]})
    manifest["summary"]["crc_offline_reference"] = {"algorithm": "CRC16-CCITT-FALSE DATA260",
                                                     "counts": dict(crc_counts), "per_type": dict(crc_types),
                                                     "errors": crc_errors,
                                                     "production_gate": "CRC_NOT_CHECKED (unchanged)"}
    manifest["summary"]["interfaces"] = manifest["interfaces"]
    manifest["summary"]["source_counts"] = dict(Counter(row["source"] for row in manifest["datagrams"]))
    manifest["summary"]["destination_counts"] = dict(Counter(row["destination"] for row in manifest["datagrams"]))
    manifest["summary"]["udp_payload_length_counts"] = dict(Counter(len(bytes.fromhex(row["payload_hex"])) for row in manifest["datagrams"]))
    manifest["summary"]["stage_count"] = len(manifest["stages"])
    manifest["summary"]["AA_device_start_interval_us_counts"] = dict(Counter(b["sample_time_us"][0] - a["sample_time_us"][0]
                                                                                   for a, b in zip(manifest["aa"], manifest["aa"][1:])))
    return manifest, pcap_records


def csv_table(path, rows):
    with Path(path).open("x", encoding="utf-8", newline="") as stream:
        writer = csv.DictWriter(stream, fieldnames=list(rows[0]) if rows else [])
        writer.writeheader()
        for row in rows:
            writer.writerow({key: json.dumps(value, sort_keys=True) if isinstance(value, (dict, list)) else value
                             for key, value in row.items()})


def audit(pcap, workbook, output):
    output = Path(output)
    manifest, _ = baseline(pcap)
    association = associations(manifest)
    report = {"result": "PASS", "input": manifest["input"], **manifest["summary"],
              "stages": manifest["stages"],
              "ab_aa_association": {"rows": len(association),
                                    "matches_previous_aa": sum(row["matches_previous_aa"] is True for row in association),
                                    "matches_next_aa": sum(row["matches_next_aa"] is True for row in association)},
              "scope": "Original multi-mode PCAP independent offline software audit; no native GUI/live hardware claim"}
    output.mkdir(parents=True, exist_ok=True)
    write_json(output / "independent-workbook-evidence.json", base.workbook_evidence(workbook))
    write_json(output / "independent-manifest.json", manifest)
    write_json(output / "independent-pcap-audit.json", report)
    csv_table(output / "independent-AB-stages.csv", manifest["stages"])
    csv_table(output / "independent-AB-AA-association.csv", association)
    with (output / "independent-pcap-audit.txt").open("x", encoding="utf-8", newline="\n") as stream:
        stream.write(json.dumps(report, ensure_ascii=False, indent=2) + "\n")
    return report


def verify_ab_csv(path, frames, session):
    require(isinstance(session, str) and session and session != "unavailable", "explicit production session metadata required")
    stream, reader = base.csv_rows(path, AB_COLUMNS)
    count, timed_slots, empty_times = 0, 0, 0
    with stream:
        for frame in frames:
            for index, millivolts in enumerate(frame["wave_mv"]):
                row = next(reader, None)
                require(row is not None, f"AB CSV ends before row {count}")
                sample_time = frame["sample_time_us"][index]
                values = [frame["frame_index"], frame["raw_record_id"], frame["outer_sequence"],
                          frame["pa0_seq"], frame["sci_frame_seq"], frame["seconds"], frame["microseconds"],
                          index, "" if sample_time is None else sample_time, millivolts, Decimal(millivolts) / 1000,
                          frame["f_act_mhz"], Decimal(frame["f_act_mhz"]) / 1000, f"0x{frame['work_mode']:02X}",
                          frame["scan_freq"], frame["scan_voltage_mv"], frame["ad9833_status"], frame["dac8563_status"],
                          "CRC_NOT_CHECKED", frame["source"], session, frame["received_unix_nanos"],
                          frame["interval_us"], frame["points"], frame["waveform_status"]]
                base.match_fields(row, dict(zip(AB_COLUMNS, values)), f"AB CSV row {count}")
                timed_slots += sample_time is not None
                empty_times += sample_time is None
                count += 1
        require(next(reader, None) is None, "AB CSV contains extra rows")
    return {"result": "PASS", "identity": identity(path), "csv_version": 2, "frames": len(frames),
            "data_rows": count, "total_csv_rows": count + 1, "columns_checked": AB_COLUMNS,
            "timed_sample_slots": timed_slots, "no_sample_time_slots": empty_times,
            "session_expectation_source": "Explicit production session metadata, never CSV-derived",
            "expected_protocol_session": session, "all_fields_and_exact_csv_eof": True}


def verify_source_flags(records, datagrams):
    require(len(records) == len(datagrams), "RAW source flag record count differs")
    for index, (actual, expected) in enumerate(zip(records, datagrams)):
        require(actual["source_matches"] == expected["source_matches"],
                f"RAW record {index} source_matches differs from independent source boundary")


def verified_manifest(frozen):
    schema = frozen.get("schema_version")
    require(schema in (1, 2), "unsupported independent manifest schema")
    fresh, pcap_records = baseline(frozen["input"]["path"])
    require(fresh["input"] == frozen["input"], "original PCAP identity changed")
    for key in ("datagrams", "aa", "ac"):
        require(fresh[key] == frozen[key], f"frozen independent {key} differs from original bytes")
    if schema == 1:
        require(len(fresh["ab"]) == len(frozen["ab"]), "frozen independent AB frame count differs")
        for expected, actual in zip(frozen["ab"], fresh["ab"]):
            require({key: actual[key] for key in expected} == expected,
                    "frozen independent schema1 AB fields differ from original bytes")
    else:
        for key in ("ab", "stages", "crc_reference_records", "interfaces"):
            require(fresh[key] == frozen[key], f"frozen independent {key} differs from original bytes")
    return fresh, pcap_records


def verify(manifest_path, summary_path, output, legacy_storage_only=False):
    frozen = json.loads(Path(manifest_path).read_text(encoding="utf-8-sig"))
    summary = json.loads(Path(summary_path).read_text(encoding="utf-8-sig"))
    require(not legacy_storage_only or frozen["schema_version"] == 1,
            "legacy storage-only mode is restricted to a frozen schema1 manifest")
    manifest, pcap_records = verified_manifest(frozen)
    metadata, files = summary["metadata"], summary["files"]
    require(Path(metadata["pcap_path"]).samefile(manifest["input"]["path"]), "production input identity differs")
    raw, raw_summary = read_lvdsraw(files["raw"])
    verify_source_flags(raw, manifest["datagrams"])
    require({"origin", "created_at_ns", "session_id_hex", "expected_source", "binding"}
            <= metadata["raw_header"].keys(), "explicit production RAW constructor metadata required")
    for key, expected in metadata["raw_header"].items():
        actual = raw_summary["raw_header"][key]
        if key == "session_id_hex":
            actual, expected = bytes.fromhex(actual), bytes.fromhex(expected)
        require(actual == expected, f"RAW explicit header metadata differs: {key}")
    report = {"result": "PASS", "input": manifest["input"], "manifest": identity(manifest_path),
              "production_summary": identity(summary_path),
              "frozen_manifest_schema_version": frozen["schema_version"],
              "csv_version": 2,
              "expected_values_source": "Original PCAP bytes independently recomputed and compared with frozen manifest fields",
              "raw": {**compare_raw_records(pcap_records, raw, raw_summary), **raw_summary, "identity": identity(files["raw"])},
              "science_csv": base.verify_aa_csv(files["science_csv"], manifest["aa"]),
              "science_lpd": base.verify_lpd(files["science_lpd"], manifest["aa"]),
              "excitation_csv": verify_ab_csv(files["excitation_csv"], manifest["ab"], metadata["excitation_protocol_session"]),
              "history_exports": [], "original_input_unchanged": True,
              "native_gui": "WINDOWS_GUI_NOT_RUN", "live_hardware": "REAL_HARDWARE_LIVE_NOT_RUN"}
    if "ui_entry_raw" in files:
        ui_raw, ui_summary = read_lvdsraw(files["ui_entry_raw"])
        verify_source_flags(ui_raw, manifest["datagrams"])
        report["ui_entry_raw"] = {**compare_raw_records(pcap_records, ui_raw, ui_summary), **ui_summary,
                                  "identity": identity(files["ui_entry_raw"])}
    history_exports = summary.get("history_exports", [])
    complete_history_exports = (any(task["input_kind"].upper() in ("PCAP", "PCAPNG") for task in history_exports)
                                and any(task["input_kind"].upper() in ("RAW", "LVDSRAW") for task in history_exports))
    require((legacy_storage_only and not history_exports) or complete_history_exports,
            "complete PCAP and RAW history CSV exports required")
    report["history_csv_scope"] = ("FULL_PCAP_AND_RAW_EXPORTS" if complete_history_exports
                                   else "NOT_PRODUCED_BY_LEGACY_SUMMARY; direct storage outputs only")
    for task in history_exports:
        session = task["protocol_session"]
        input_kind = task["input_kind"].upper()
        if input_kind in ("PCAP", "PCAPNG"):
            require(Path(task["input_path"]).samefile(manifest["input"]["path"]), "history export PCAP differs")
            require(session == "history:" + task["input_path"], "history PCAP session metadata mismatch")
        elif input_kind in ("RAW", "LVDSRAW"):
            require(Path(task["input_path"]).samefile(files["raw"]), "history export RAW differs")
            require(bytes.fromhex(session) == bytes.fromhex(raw_summary["raw_header"]["session_id_hex"]),
                    "history RAW session metadata/header mismatch")
        else:
            raise ValueError("unknown history export source kind")
        report["history_exports"].append({"input_kind": task["input_kind"], "input_path": task["input_path"],
                                           **verify_ab_csv(task["path"], manifest["ab"], session)})
    write_json(output, report)
    return report


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
    verify_parser.add_argument("--legacy-storage-only", action="store_true",
                               help="Schema1 legacy production summary has no history CSV exports; verify direct storage outputs only")
    args = parser.parse_args()
    report = (audit(args.pcap, args.workbook, args.output_dir) if args.command == "audit" else
              verify(args.manifest, args.production_summary, args.output, args.legacy_storage_only))
    print(json.dumps(report, ensure_ascii=True, indent=2))


if __name__ == "__main__":
    main()
