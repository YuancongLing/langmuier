"""Independently decode actual writes captured by the controlled Transport.

Only Python's standard library and the independent Python reference are used.
This verifies software I/O, not a real COM port or UART electrical timing.
"""

import argparse
from collections import Counter, defaultdict
import json
import math
from pathlib import Path
import statistics
import struct

from rs422_star_reference import POINTS, ecef, fixtures, rounded


SOURCE = "CONTROLLED_TRANSPORT_SYNTHETIC"
EPOCH_US = 1514764800 * 1000000


def require(condition, message):
    if not condition:
        raise ValueError(message)


def split_stream(wire):
    frames = []
    offset = 0
    while offset < len(wire):
        require(wire[offset:offset + 2] == b"\xEB\x90", f"missing EB90 at byte {offset}")
        require(offset + 6 <= len(wire), f"incomplete header at byte {offset}")
        end = offset + 6 + int.from_bytes(wire[offset + 4:offset + 6], "big")
        require(end <= len(wire), f"incomplete frame at byte {offset}")
        require(end > offset + 7, f"empty frame at byte {offset}")
        frames.append(wire[offset:end])
        offset = end
    return frames


def decode_frame(frame):
    require(frame[:4] == bytes.fromhex("EB90F055"), "sync/address mismatch")
    require(int.from_bytes(frame[4:6], "big") + 6 == len(frame), "outer length mismatch")
    require(sum(frame[6:-1]) % 256 == frame[-1], "outer SUM mismatch")
    kind, data = frame[6], frame[7:-1]
    if kind == 0x11:
        require(frame == bytes.fromhex("EB90F0550006119B9B9B9B7D"), "telemetry command mismatch")
        return {"kind": "telemetry"}
    if kind in (0x21, 0x31):
        require(len(frame) == 12, "control length mismatch")
        if kind == 0x21 and data[0] in (0x55, 0xAA):
            require(data[1] <= 100 and data[2:] == b"\x9B\x9B", "waveform control mismatch")
        elif kind == 0x21 and data[0] == 0xBB:
            require(data[3] == 0x9B, "DC control filler mismatch")
        elif kind == 0x31:
            require(data[0] in (0x55, 0xAA) and data[1:] == b"\x9B" * 3, "LVDS control mismatch")
        else:
            raise ValueError("unsupported control subtype")
        return {"kind": "control", "dc_1000": frame == bytes.fromhex("EB90F055000621BB03E89B62")}
    require(kind == 0xFF, "unknown data type")
    if len(frame) == 14:
        seconds, ticks = struct.unpack(">IH", data)
        require(ticks < 40000, "25us counter outside one second")
        return {"kind": "time", "star_microseconds": seconds * 1000000 + ticks * 25}
    if len(frame) == 67:
        require(int.from_bytes(data[4:7], "big") < 1000000, "24bit microseconds outside one second")
        expected = bytes.fromhex(fixtures()["attitude"])[7:-1]
        require(data[7:] == expected[7:], "synthetic attitude field bytes differ from independent reference")
        return {"kind": "attitude", "star_microseconds": int.from_bytes(data[:4], "big") * 1000000 + int.from_bytes(data[4:7], "big")}
    require(len(frame) == 122, "unknown broadcast size")
    require(data[:2] == b"\x09\xF5", "GNSS version/type/secondary/APID mismatch")
    sequence = int.from_bytes(data[2:4], "big")
    require(sequence >> 14 == 3, "GNSS sequence flags mismatch")
    require(data[4:8] == bytes.fromhex("006B03FF"), "GNSS inner length/service mismatch")
    require(sum(data[8:113]) % 256 == data[113], "GNSS inner SUM mismatch")
    require(data[8] == 0x62, "controlled positioning state/mode/source/PPS mismatch")
    require(data[9:14] == bytes.fromhex("C12D2D8888"), "controlled baseband/SNR/star count mismatch")
    require(data[14:23] == bytes(9), "controlled counters/drift/uptime must be synthetic zero")
    require(data[27:30] == bytes.fromhex("FF0000"), "star/reset state mismatch")
    xyz = struct.unpack(">3i", data[30:42])
    require(data[42:54] == bytes(12), "static test velocity must be zero")
    require(data[30:54] == data[54:78], "secondary positioning solution differs")
    latq, lonq, hq = struct.unpack(">3i", data[78:90])
    latitude, longitude, height = math.degrees(latq / 1e7), math.degrees(lonq / 1e7), hq / 100
    require(-90 <= latitude <= 90 and -180 <= longitude <= 180, "decoded geodetic range")
    # 1e-7 rad latitude/longitude quantization has about a 0.7m combined bound
    # at these heights. XYZ itself must match the unquantized preset to 0.01m.
    reconstructed = ecef(latitude, longitude, height)
    consistency_error = max(abs(actual / 100 - wanted) for actual, wanted in zip(xyz, reconstructed))
    require(consistency_error < 1.0, "ECEF and geodetic coordinates inconsistent")
    matches = []
    for index, (lat, lon, h) in enumerate(POINTS):
        if (latq, lonq, hq) == (rounded(math.radians(lat) * 1e7), rounded(math.radians(lon) * 1e7), rounded(h * 100)):
            expected_xyz = tuple(rounded(value * 100) for value in ecef(lat, lon, h))
            require(xyz == expected_xyz, "ECEF differs from independent exact preset quantization")
            matches.append(index)
    require(len(matches) == 1, "GNSS is not exactly one configured synthetic preset")
    require(data[90:94] == data[23:27], "synthetic local/star clocks disagree")
    require(data[94:100] == bytes.fromhex("000012170000"), "extrapolation/leap/temperature/version mismatch")
    require(data[100:113] == bytes.fromhex("01000080000000000000000002"), "health/upload/CAN/SRAM/convergence/GDOP mismatch")
    return {"kind": "gnss", "sequence": sequence & 0x3FFF, "point": matches[0],
            "star_microseconds": int.from_bytes(data[23:27], "big") * 1000000,
            "ecef_geodetic_error_m": consistency_error}


def validate_capture(path):
    capture = json.loads(path.read_text(encoding="utf-8"))
    require(capture["source"] == SOURCE and capture["real_com_port"] is False, "capture provenance mismatch")
    records = capture["frames"]
    require(records, "empty captured stream")
    wire = b"".join(bytes.fromhex(record["hex"]) for record in records)
    if "wire_hex" in capture:
        require(wire == bytes.fromhex(capture["wire_hex"]), "frame capture and actual raw wire differ")
    decoded = [decode_frame(frame) for frame in split_stream(wire)]
    require(len(decoded) == len(records), "flush boundary is not one complete frame")
    starts = defaultdict(list)
    time_delta = defaultdict(list)
    points, sequence, gaps = [], [], []
    previous_complete = None
    for index, (record, result) in enumerate(zip(records, decoded)):
        require(record["source"] == SOURCE, "frame provenance mismatch")
        started, complete = record["started_unix_micros"], record["completed_unix_micros"]
        require(started <= complete, "write completed before start")
        if previous_complete is not None:
            gap = started - previous_complete
            require(gap >= 1, f"frame {index} begins before prior completion + 1us")
            gaps.append(gap)
        previous_complete = complete
        starts[result["kind"]].append(started)
        if "star_microseconds" in result:
            delta = started - EPOCH_US - result["star_microseconds"]
            # GNSS only transmits whole seconds; time/attitude are timestamped
            # just before encoding, followed by the first Transport.write call.
            tolerance = 1100000 if result["kind"] == "gnss" else 100000
            require(0 <= delta <= tolerance, f"{result['kind']} timestamp is not near actual write start: {delta}us")
            time_delta[result["kind"]].append(delta)
        if result["kind"] == "gnss":
            points.append(result["point"])
            sequence.append(result["sequence"])
    require(all(b == (a + 1) % 16384 for a, b in zip(sequence, sequence[1:])), "GNSS packet counts are not consecutive modulo 16384")
    evidence = capture.get("evidence", {})
    if "broadcast_intervals_ms" in evidence:
        for kind in ("time", "attitude", "gnss"):
            require(len(starts[kind]) >= 7, f"missing {kind} periodic frames")
        require(len(starts["telemetry"]) >= 2, "keepalive telemetry polling absent")
        require(any(item.get("dc_1000") for item in decoded), "DC1000 command absent")
    if "initial_point" in evidence:
        require(len(points) >= 2 and points[0] == 0 and all(point == 2 for point in points[1:]), "queued A snapshot or subsequent C switch mismatch")
    statistics_by_kind = {}
    for kind, values in starts.items():
        intervals = [(b - a) / 1000 for a, b in zip(values, values[1:])]
        statistics_by_kind[kind] = {"frames": len(values), "intervals_ms": intervals}
        if intervals:
            statistics_by_kind[kind].update(min_ms=min(intervals), max_ms=max(intervals), mean_ms=statistics.mean(intervals))
            expected_period = None
            if "broadcast_intervals_ms" in evidence and kind in ("time", "attitude", "gnss"):
                expected_period = evidence["broadcast_intervals_ms"][("time", "attitude", "gnss").index(kind)]
            elif kind == "telemetry" and "telemetry_interval_ms" in evidence:
                expected_period = evidence["telemetry_interval_ms"]
            if expected_period is not None:
                errors = [interval - expected_period for interval in intervals]
                statistics_by_kind[kind].update(expected_period_ms=expected_period,
                    mean_error_ms=statistics.mean(errors), max_abs_error_ms=max(abs(error) for error in errors),
                    rms_error_ms=math.sqrt(statistics.mean(error * error for error in errors)),
                    stddev_ms=statistics.pstdev(intervals))
        if kind in time_delta:
            statistics_by_kind[kind]["timestamp_age_us"] = {"min": min(time_delta[kind]), "max": max(time_delta[kind])}
    markers = evidence.get("markers", {})
    stop = markers.get("stop_unix_micros")
    if stop is not None:
        require(all(record["completed_unix_micros"] <= stop for record, result in zip(records, decoded) if result["kind"] in ("time", "attitude", "gnss")), "periodic frame completed after stop completed")
        require(markers["observed_until_unix_micros"] - stop >= 100000, "stop observation interval too short")
    reconnect = markers.get("reconnect_unix_micros")
    if reconnect is not None:
        require(all(record["started_unix_micros"] < reconnect for record in records), "new transmission after reconnect without user start")
        require(markers["observed_until_unix_micros"] - reconnect >= 100000, "reconnect observation interval too short")
    observed_counts = Counter(item["kind"] for item in decoded)
    require(capture["status"]["sent"] == [observed_counts[kind] for kind in ("time", "attitude", "gnss")], "reported sent counts differ from actual captured frames")
    require(capture["status"]["active"] is False, "scheduler still active at capture end")
    return {"capture": str(path), "source": SOURCE, "real_com_port": False, "verified": True,
            "bytes": len(wire), "frames": len(records), "raw_wire_matched": "wire_hex" in capture,
            "counts": dict(Counter(item["kind"] for item in decoded)), "gnss_points": points,
            "gnss_sequences": sequence, "min_frame_gap_us": min(gaps) if gaps else None,
            "timing": statistics_by_kind,
            "lifecycle_test_assertions": evidence,
            "limits": "Lifecycle booleans and observation markers originate in Rust tests; byte stream, timestamp ordering, counts, sequences and coordinates are independently checked. This is not real COM/hardware QA."}


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("captures", type=Path, nargs="+")
    parser.add_argument("--report", type=Path, required=True)
    args = parser.parse_args()
    results = [validate_capture(path) for path in args.captures]
    args.report.parent.mkdir(parents=True, exist_ok=True)
    args.report.write_text(json.dumps({"source": SOURCE, "results": results}, indent=2) + "\n", encoding="utf-8")
    for result in results:
        print(f"PASS {result['capture']}: {result['frames']} complete frames, {result['bytes']}B, independent checksums/fields/timestamps/coordinates")


if __name__ == "__main__":
    main()
