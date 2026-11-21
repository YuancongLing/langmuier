"""Independent stdlib SYNTHETIC AB encoder and scientific CSV read-back.

The frozen samples and field layout come from protocol workbook PH20261115,
not from Rust code. These artifacts demonstrate software behavior only.
"""
import argparse
import csv
from decimal import Decimal
import hashlib
import io
import json
from pathlib import Path
import socket
import struct


TRIANGLE_MV = [
    0, 66, 132, 198, 264, 330, 396, 462, 528, 594,
    660, 726, 792, 858, 924, 990, 1056, 1122, 1188, 1254,
    1320, 1386, 1452, 1518, 1584, 1650, 1716, 1782, 1848, 1914,
    1980, 2046, 2112, 2178, 2244, 2310, 2376, 2442, 2508, 2574,
    2640, 2706, 2772, 2838, 2904, 2970, 3036, 3102, 3168, 3234,
    3300, 3234, 3168, 3102, 3036, 2970, 2904, 2838, 2772, 2706,
    2640, 2574, 2508, 2442, 2376, 2310, 2244, 2178, 2112, 2046,
    1980, 1914, 1848, 1782, 1716, 1650, 1584, 1518, 1452, 1386,
    1320, 1254, 1188, 1122, 1056, 990, 924, 858, 792, 726,
    660, 594, 528, 462, 396, 330, 264, 198, 132, 66,
]
SINE_MV = [
    1650, 1754, 1857, 1959, 2060, 2150, 2257, 2353, 2445, 2534,
    2620, 2702, 2780, 2853, 2921, 2985, 3043, 3096, 3143, 3184,
    3219, 3248, 3271, 3287, 3297, 3300, 3297, 3287, 3271, 3248,
    3219, 3184, 3143, 3096, 3043, 2985, 2921, 2853, 2780, 2702,
    2620, 2534, 2445, 2353, 2257, 2150, 2060, 1959, 1857, 1754,
    1650, 1546, 1443, 1341, 1240, 1150, 1043, 947, 855, 766,
    680, 598, 520, 447, 379, 315, 257, 204, 157, 116,
    81, 52, 29, 13, 3, 0, 3, 13, 29, 52,
    81, 116, 157, 204, 257, 315, 379, 447, 520, 598,
    680, 766, 855, 947, 1043, 1150, 1240, 1341, 1443, 1546,
]
REQUIRED_COLUMNS = (
    "ab_frame_index,raw_record_id,udp_sequence_u32,pa0_seq,sci_frame_seq,"
    "frame_seconds,frame_microseconds,sample_index,sample_time_us,wave_mV,wave_V,"
    "f_act_mHz,f_act_Hz,work_mode,scan_freq,scan_voltage_mV,ad9833_status,"
    "dac8563_status,integrity"
).split(",")
PROVENANCE_COLUMNS = ["source", "session", "received_unix_nanos"]
CSV_V2_COLUMNS = ["interval_us", "points", "waveform_status"]


def vectors():
    return [
        dict(name="triangle", seconds=9, microseconds=250000, f_act_mHz=931,
             interval_us=10800, points=100, work_mode=0x55, scan_freq=1,
             scan_voltage_mV=0, ad9833_status=0, dac8563_status=1,
             sci_frame_seq=17, pa0_seq=255, wave_mV=TRIANGLE_MV, crc="1234"),
        dict(name="sine", seconds=10, microseconds=50000, f_act_mHz=931,
             interval_us=10800, points=100, work_mode=0xAA, scan_freq=100,
             scan_voltage_mV=0, ad9833_status=1, dac8563_status=0,
             sci_frame_seq=200, pa0_seq=0, wave_mV=SINE_MV, crc="a55a"),
        dict(name="dc", seconds=11, microseconds=999999, f_act_mHz=0,
             interval_us=10000, points=100, work_mode=0xBB, scan_freq=255,
             scan_voltage_mV=5000, ad9833_status=1, dac8563_status=1,
             sci_frame_seq=3, pa0_seq=1, wave_mV=[2500] * 100, crc="0000"),
        dict(name="all_zero", seconds=12, microseconds=0, f_act_mHz=0,
             interval_us=10800, points=100, work_mode=0x55, scan_freq=0,
             scan_voltage_mV=0, ad9833_status=0, dac8563_status=0,
             sci_frame_seq=3, pa0_seq=2, wave_mV=[0] * 100, crc="ffff"),
    ]


def encode_ab(vector):
    samples = vector["wave_mV"]
    assert len(samples) == 100 and all(0 <= value <= 3300 for value in samples)
    fields = struct.pack(">IIIHHBBHBBBB", vector["seconds"], vector["microseconds"],
                         vector["f_act_mHz"], vector["interval_us"], vector["points"],
                         vector["work_mode"], vector["scan_freq"],
                         vector["scan_voltage_mV"], vector["ad9833_status"],
                         vector["dac8563_status"], vector["sci_frame_seq"], vector["pa0_seq"])
    wire = (bytes.fromhex("eb9055aa0107ab") + fields + bytes(36)
            + struct.pack(">100H", *samples) + bytes.fromhex(vector["crc"]))
    assert len(wire) == 269
    return wire


def fixture_manifest():
    result = []
    for vector in vectors():
        latest = vector["seconds"] * 1000000 + vector["microseconds"]
        row = dict(vector)
        row["sample_time_us"] = [latest - (99 - index) * vector["interval_us"] for index in range(100)]
        row["sha256"] = hashlib.sha256(encode_ab(vector)).hexdigest()
        result.append(row)
    assert result[0]["sample_time_us"][0] == 8180800
    assert result[0]["sample_time_us"][-1] == 9250000
    assert result[1]["sample_time_us"][0] == 8980800
    return {"scope": "SYNTHETIC software golden vectors; not real device data",
            "workbook": "PH20261115: LVDS excitation I18:J31 and F34:F38",
            "crc_policy": "CRC_NOT_CHECKED", "vectors": result}


def write_vectors(directory, check=False):
    directory = Path(directory)
    blobs = {f"SYNTHETIC-ab-{v['name']}.bin": encode_ab(v) for v in vectors()}
    blobs["SYNTHETIC-ab-manifest.json"] = (json.dumps(fixture_manifest(), indent=2) + "\n").encode()
    expected_csv = io.StringIO(newline="")
    writer = csv.writer(expected_csv, lineterminator="\n")
    writer.writerow(REQUIRED_COLUMNS + PROVENANCE_COLUMNS)
    expected_csv_v2 = io.StringIO(newline="")
    writer_v2 = csv.writer(expected_csv_v2, lineterminator="\n")
    writer_v2.writerow(REQUIRED_COLUMNS + PROVENANCE_COLUMNS + CSV_V2_COLUMNS)
    for frame_index, vector in enumerate(vectors()):
        latest = vector["seconds"] * 1000000 + vector["microseconds"]
        for index, millivolts in enumerate(vector["wave_mV"]):
            row = [
                frame_index, 21 + frame_index, 0x1235000C + frame_index,
                vector["pa0_seq"], vector["sci_frame_seq"], vector["seconds"], vector["microseconds"],
                index, latest - (99 - index) * vector["interval_us"], millivolts,
                f"{Decimal(millivolts) / 1000:.3f}", vector["f_act_mHz"],
                f"{Decimal(vector['f_act_mHz']) / 1000:.3f}", f"0x{vector['work_mode']:02X}",
                vector["scan_freq"], vector["scan_voltage_mV"], vector["ad9833_status"],
                vector["dac8563_status"], "CRC_NOT_CHECKED", "SYNTHETIC", "session-AB", 1234567890]
            writer.writerow(row)
            writer_v2.writerow(row + [vector["interval_us"], vector["points"], "TIMED_SAMPLES"])
    blobs["SYNTHETIC-ab-expected.csv"] = expected_csv.getvalue().encode()
    blobs["SYNTHETIC-ab-expected-v2.csv"] = expected_csv_v2.getvalue().encode()
    if not check:
        directory.mkdir(parents=True, exist_ok=True)
    for name, data in blobs.items():
        path = directory / name
        if check:
            assert path.read_bytes() == data, f"golden artifact differs: {path}"
        else:
            with path.open("xb") as stream:
                stream.write(data)
    return {"vectors": len(vectors()), "checked" if check else "written": str(directory)}


def block(kind, body):
    body += bytes(-len(body) % 4)
    size = len(body) + 12
    return struct.pack("<II", kind, size) + body + struct.pack("<I", size)


def checksum(data):
    data += bytes(len(data) % 2)
    total = sum(struct.unpack(">" + "H" * (len(data) // 2), data))
    while total >> 16:
        total = (total & 65535) + (total >> 16)
    return (~total) & 65535


def write_pcap(path, showcase=False):
    path = Path(path)
    if "SYNTHETIC" not in path.name:
        raise ValueError("fixture path must contain SYNTHETIC")
    source = socket.inet_aton("192.168.1.128")
    destination = socket.inet_aton("192.168.1.100")
    blob = bytearray(block(0x0A0D0D0A, struct.pack("<IHHq", 0x1A2B3C4D, 1, 0, -1)))
    blob.extend(block(1, struct.pack("<HHI", 1, 0, 65535)))
    channel1 = int("0000000000" "0000000001" "1000000000" "1111111111", 2).to_bytes(5, "big") * 25
    channel2 = int("1111111111" "1000000000" "0000000001" "0000000000", 2).to_bytes(5, "big") * 25
    frames = []
    for second, excitation in enumerate(vectors() if showcase else vectors()[:1]):
        for science in range(20):
            data = (struct.pack(">II", 9 + second, science * 50000) + channel1 + channel2
                    + bytes([(second * 20 + science) & 255, 0]))
            frames.append((bytes.fromhex("eb9055aa0107aa") + data + bytes.fromhex("1234"),
                           second * 1000000 + science * 50000))
        frames.append((encode_ab(excitation), second * 1000000 + 960000))
        frames.append((bytes.fromhex("eb9055aa0107ac") + bytes([0xC4]) * 260 + bytes.fromhex("1234"),
                       second * 1000000 + 970000))
    for index, (frame, received_us) in enumerate(frames):
        sequence = 0x1234FFF8 + index
        wrapper = bytes.fromhex("55aa01a5") + struct.pack(">IIHHIHH", sequence, 269, 0, 1, 0, 269, 0)
        payload = wrapper + frame
        udp = struct.pack(">HHHH", 1534, 1534, len(payload) + 8, 0) + payload
        udp_crc = checksum(source + destination + struct.pack(">BBH", 0, 17, len(udp)) + udp) or 65535
        udp = udp[:6] + struct.pack(">H", udp_crc) + udp[8:]
        ip = struct.pack(">BBHHHBBH", 0x45, 0, len(udp) + 20, index, 0, 64, 17, 0) + source + destination
        ip = ip[:10] + struct.pack(">H", checksum(ip)) + ip[12:]
        packet = bytes.fromhex("00112233445566778899aabb0800") + ip + udp
        ticks = 1700000000000000 + received_us
        blob.extend(block(6, struct.pack("<IIIII", 0, ticks >> 32, ticks & 0xFFFFFFFF, len(packet), len(packet)) + packet))
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("xb") as stream:
        stream.write(blob)
    return {"scope": "SYNTHETIC", "path": str(path), "udp_records": len(frames),
            "AA": len(frames) // 22 * 20, "AB": len(frames) // 22, "AC": len(frames) // 22,
            "sha256": hashlib.sha256(blob).hexdigest(), "bytes": len(blob)}


def verify_csv(path):
    known = {v["pa0_seq"]: v for v in vectors()}
    frames = 0
    rows = 0
    with Path(path).open(newline="", encoding="utf-8-sig") as stream:
        reader = csv.DictReader(stream)
        legacy_columns = REQUIRED_COLUMNS + PROVENANCE_COLUMNS
        assert reader.fieldnames in [legacy_columns, legacy_columns + CSV_V2_COLUMNS], reader.fieldnames
        csv_version = 2 if reader.fieldnames == legacy_columns + CSV_V2_COLUMNS else 1
        for row in reader:
            frame_index, sample_index = divmod(rows, 100)
            assert int(row["ab_frame_index"]) == frame_index
            assert int(row["sample_index"]) == sample_index
            vector = known[int(row["pa0_seq"])]
            latest = vector["seconds"] * 1000000 + vector["microseconds"]
            expected_time = latest - (99 - sample_index) * vector["interval_us"]
            expected_mv = vector["wave_mV"][sample_index]
            assert int(row["sample_time_us"]) == expected_time, row
            assert int(row["wave_mV"]) == expected_mv, row
            assert Decimal(row["wave_V"]) == Decimal(expected_mv) / 1000
            assert Decimal(row["f_act_Hz"]) == Decimal(vector["f_act_mHz"]) / 1000
            assert row["integrity"] == "CRC_NOT_CHECKED"
            for column, key in [("frame_seconds", "seconds"), ("frame_microseconds", "microseconds"),
                                ("sci_frame_seq", "sci_frame_seq"), ("f_act_mHz", "f_act_mHz"),
                                ("scan_freq", "scan_freq"), ("scan_voltage_mV", "scan_voltage_mV"),
                                ("ad9833_status", "ad9833_status"), ("dac8563_status", "dac8563_status")]:
                assert int(row[column]) == vector[key], (column, row)
            assert int(row["work_mode"], 16) == vector["work_mode"], row
            assert int(row["raw_record_id"]) > 0
            assert row["source"] and row["session"]
            if csv_version == 2:
                assert int(row["interval_us"]) == vector["interval_us"]
                assert int(row["points"]) == vector["points"]
                assert row["waveform_status"] == "TIMED_SAMPLES"
            if sample_index == 99:
                frames += 1
            rows += 1
    assert rows > 0 and rows % 100 == 0
    return {"scope": "Independent SYNTHETIC AB CSV read-back", "path": str(path),
            "csv_version": csv_version, "frames": frames, "data_rows": rows, "all_sample_times_and_mV": "PASS",
            "volts_frequency_fields_integrity_provenance": "PASS"}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--fixtures", type=Path)
    parser.add_argument("--check", action="store_true")
    parser.add_argument("--mixed", type=Path)
    parser.add_argument("--showcase", type=Path)
    parser.add_argument("--verify-csv", type=Path)
    args = parser.parse_args()
    reports = []
    if args.fixtures:
        reports.append(write_vectors(args.fixtures, args.check))
    if args.mixed:
        reports.append(write_pcap(args.mixed))
    if args.showcase:
        reports.append(write_pcap(args.showcase, showcase=True))
    if args.verify_csv:
        reports.append(verify_csv(args.verify_csv))
    print(json.dumps(reports or fixture_manifest(), indent=2))


if __name__ == "__main__":
    main()
