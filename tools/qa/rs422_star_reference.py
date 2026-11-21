"""Independent standard-library reference for the 2026-07-28 RS422 tables.

No Rust encoder/decoder is imported. Fixed SYNTHETIC positioning scenarios,
not a continuous orbit or receiver measurements. Run from repository root.
"""

from datetime import datetime, timezone
from pathlib import Path
import argparse
import json
import math
import struct


POINTS = [(0.0, 0.0, 400000.0), (35.0, 116.0, 450000.0), (-35.0, -70.0, 500000.0)]
EPOCH = datetime(2018, 1, 1, tzinfo=timezone.utc)
FIXED = datetime(2026, 11, 16, 0, 0, 0, 999999, tzinfo=timezone.utc)


def rounded(value):
    return math.floor(value + 0.5) if value >= 0 else math.ceil(value - 0.5)


def wrap(payload):
    return bytes.fromhex("EB90F055") + struct.pack(">HB", len(payload) + 2, 255) + payload + bytes([(255 + sum(payload)) % 256])


def ecef(latitude, longitude, height):
    # Independently use the semi-minor axis formulation of the prime vertical.
    a, flattening = 6378137.0, 1 / 298.257223563
    b = a * (1 - flattening)
    lat, lon = math.radians(latitude), math.radians(longitude)
    n = a * a / math.sqrt(a * a * math.cos(lat) ** 2 + b * b * math.sin(lat) ** 2)
    return [(n + height) * math.cos(lat) * math.cos(lon),
            (n + height) * math.cos(lat) * math.sin(lon),
            (b * b / (a * a) * n + height) * math.sin(lat)]


def fixtures():
    seconds = int((FIXED - EPOCH).total_seconds())
    micros = FIXED.microsecond
    time = wrap(struct.pack(">IH", seconds, micros // 25))
    attitude = bytearray(struct.pack(">I", seconds) + micros.to_bytes(3, "big"))
    for angle in (1.0, -2.0, 3.0):
        attitude += (rounded(angle / .00005) & 0xffffff).to_bytes(3, "big")
    for rate in (.1, -.2, .3):
        attitude += struct.pack(">h", rounded(rate / .0001))
    attitude += b"\x40"
    attitude += struct.pack(">6f", 7000., .001, 51.6, 20., 30., 40.)
    attitude += struct.pack(">6h", 1, -2, 3, -4, 5, -6)
    assert len(attitude) == 59
    result = {"source": "SYNTHETIC independent Python reference; not hardware transmission",
              "protocol": "513-feedback 2026-07-28 tables 2-5 through 2-9",
              "time_policy": "UTC since 2018; zero inserted leap seconds since epoch as of 2026-11-16; floor 25us",
              "ancillary_test_defaults": "W20-23 uptime=0; W91-94 local time=star seconds (unit/epoch not specified in source); SNR=45, GDOP=2, versions=0, counters=0; static points and zero velocity",
              "utc": FIXED.isoformat(), "seconds": seconds, "microseconds": micros,
              "time": time.hex().upper(), "attitude": wrap(attitude).hex().upper(), "points": []}
    for latitude, longitude, height in POINTS:
        p = bytearray(114)
        def put(w, data):
            p[w - 1:w - 1 + len(data)] = data
        put(1, bytes.fromhex("09F5FFFF006B03FF"))
        # Position valid, GPS+BDS, GPS time, valid PPS, main receiver.
        put(9, bytes.fromhex("62C12D2D8888000000000000000000"))
        put(24, struct.pack(">I", seconds))
        p[27] = 0xFF  # 16 stars -> 4-bit saturated single-point/filter counts.
        xyz = ecef(latitude, longitude, height)
        quantized = [rounded(v * 100) for v in xyz]
        for start in (31, 55):
            put(start, struct.pack(">3i", *quantized))
            put(start + 12, bytes(12))  # Static point, zero ECEF velocity.
        put(79, struct.pack(">iii", rounded(math.radians(latitude) * 1e7),
                            rounded(math.radians(longitude) * 1e7), rounded(height * 100)))
        put(91, struct.pack(">I", seconds))
        put(95, struct.pack(">H", 0))
        put(97, bytes([18, 23, 0, 0]))  # GPS-UTC, protocol default temperature, synthetic versions.
        put(101, struct.pack(">I", (2 << 23) | (1 << 7)))  # 45dB band; orbit invalid (position only).
        put(105, bytes(8))  # Reset/upload/SRAM/RF status, convergence: test zero.
        put(113, bytes([2]))  # PPS enabled, default BDS params, CAN-A, synthetic GDOP 2.
        p[113] = sum(p[8:113]) % 256
        frame = wrap(p)
        assert len(frame) == 122
        assert frame[-2] == sum(frame[15:-2]) % 256
        result["points"].append({"latitude_deg": latitude, "longitude_deg": longitude,
                                 "height_m": height, "ecef_m": xyz,
                                 "ecef_centimeters": quantized,
                                 "latitude_rad_1e7": rounded(math.radians(latitude) * 1e7),
                                 "longitude_rad_1e7": rounded(math.radians(longitude) * 1e7),
                                 "height_centimeters": rounded(height * 100),
                                 "internal_sum": p[113], "outer_sum": frame[-1],
                                 "frame": frame.hex().upper()})
    return result


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--output", type=Path, default=Path("docs/qa/star-simulator/golden-vectors.json"))
    args = parser.parse_args()
    result = fixtures()
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(result, indent=2) + "\n", encoding="utf-8")
    for name in ("time", "attitude"):
        args.output.with_name(f"{name}.hex").write_text(result[name] + "\n", encoding="ascii")
    for index, point in enumerate(result["points"]):
        args.output.with_name(f"gnss-{index}.hex").write_text(point["frame"] + "\n", encoding="ascii")
    print(f"Independent reference: {args.output}; 14B, 67B, 3 x 122B; checksums verified")


if __name__ == "__main__":
    main()
