"""Create a clearly labeled SYNTHETIC 20AA+AB+AC PCAPNG for S01 CLI QA.

This creates new test artifacts only. It never edits real captures or app input
configuration. Auxiliary DATA uses opaque markers, not decoded business values.
"""
import argparse
import hashlib
import json
from pathlib import Path
import socket
import struct


def pcap_block(kind, body):
    body += bytes(-len(body) % 4)
    length = len(body) + 12
    return struct.pack("<II", kind, length) + body + struct.pack("<I", length)


def checksum(data):
    if len(data) % 2:
        data += b"\0"
    total = sum(struct.unpack(">" + "H" * (len(data) // 2), data))
    while total >> 16:
        total = (total & 0xFFFF) + (total >> 16)
    return (~total) & 0xFFFF


def create_fixture(output, manifest):
    output, manifest = Path(output), Path(manifest)
    if output.resolve() == manifest.resolve() or output.exists() or manifest.exists():
        raise ValueError("fixture and manifest must be distinct new paths; refusing overwrite")
    if "SYNTHETIC" not in output.name:
        raise ValueError("fixture filename must contain SYNTHETIC")
    source, destination = socket.inet_aton("192.168.1.128"), socket.inet_aton("192.168.1.100")
    blob = bytearray(pcap_block(0x0A0D0D0A, struct.pack("<IHHq", 0x1A2B3C4D, 1, 0, -1)))
    blob.extend(pcap_block(1, struct.pack("<HHI", 1, 0, 65535)))
    rows = []
    # 40-bit integer packing is independent of the Rust byte-field decoder.
    ch1 = int("0000000000" + "0000000001" + "1000000000" + "1111111111", 2).to_bytes(5, "big") * 25
    ch2 = int("1111111111" + "1000000000" + "0000000001" + "0000000000", 2).to_bytes(5, "big") * 25
    for index, kind in enumerate([0xAA] * 20 + [0xAB, 0xAC]):
        if kind == 0xAA:
            data = struct.pack(">II", 1_700_000_000, index * 50_000) + ch1 + ch2 + bytes([index, 0])
            received_us = index * 50_000
        else:
            data = bytes([0xB3 if kind == 0xAB else 0xC4]) * 260
            received_us = 960_000 if kind == 0xAB else 970_000
        frame = bytes.fromhex("eb9055aa0107") + bytes([kind]) + data + bytes.fromhex("1234")
        if len(frame) != 269:
            raise ValueError("fixture construction produced the wrong frame length")
        sequence = 0x1234FFF8 + index
        wrapper = bytes.fromhex("55aa01a5") + struct.pack(">IIHHIHH", sequence, 269, 0, 1, 0, 269, 0)
        payload = wrapper + frame
        udp = struct.pack(">HHHH", 1534, 1534, len(payload) + 8, 0) + payload
        udp_check = checksum(source + destination + struct.pack(">BBH", 0, 17, len(udp)) + udp) or 0xFFFF
        udp = udp[:6] + struct.pack(">H", udp_check) + udp[8:]
        ip = struct.pack(">BBHHHBBH", 0x45, 0, len(udp) + 20, index, 0, 64, 17, 0) + source + destination
        ip = ip[:10] + struct.pack(">H", checksum(ip)) + ip[12:]
        ethernet = bytes.fromhex("00112233445566778899aabb0800") + ip + udp
        ticks = 1_700_000_000_000_000 + received_us
        body = struct.pack("<IIIII", 0, ticks >> 32, ticks & 0xFFFFFFFF, len(ethernet), len(ethernet)) + ethernet
        blob.extend(pcap_block(6, body))
        rows.append({"record_id": index + 1, "type": f"{kind:02X}",
                     "outer_sequence_u32": sequence, "science_seq8": index if kind == 0xAA else None,
                     "received_timestamp_us": ticks, "udp_payload_bytes": len(payload),
                     "udp_payload_sha256": hashlib.sha256(payload).hexdigest(),
                     "raw_crc_hex": "1234"})
    report = {"scope": "SYNTHETIC software test input; not FPGA capture or device acceptance",
              "output": str(output.resolve()), "bytes": len(blob), "sha256": hashlib.sha256(blob).hexdigest(),
              "source": "192.168.1.128:1534", "destination": "192.168.1.100:1534",
              "expected": {"udp_records": 22, "udp_payload_bytes": 6446, "AA": 20, "AB": 1,
                           "AC": 1, "unknown": 0, "structure_errors": 0,
                           "science_samples_per_channel": 2000, "science_csv_data_rows": 2000,
                           "science_lpd_v3_bytes": 8220, "outer_u32_gaps": 0},
              "science_ch1_repeating_code10": [0, 1, 512, 1023],
              "science_ch2_repeating_code10": [1023, 512, 1, 0],
              "auxiliary_DATA": "AB=260 bytes B3, AC=260 bytes C4; opaque, no business decoder claim",
              "crc_policy": "original arbitrary 1234 retained; CRC_NOT_CHECKED", "records": rows}
    output.parent.mkdir(parents=True, exist_ok=True)
    manifest.parent.mkdir(parents=True, exist_ok=True)
    with output.open("xb") as stream:
        stream.write(blob)
    with manifest.open("x", encoding="utf-8") as stream:
        stream.write(json.dumps(report, ensure_ascii=False, indent=2) + "\n")
    return report


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--manifest", type=Path, required=True)
    args = parser.parse_args()
    report = create_fixture(args.output, args.manifest)
    print(json.dumps({key: report[key] for key in ("scope", "output", "bytes", "sha256", "expected")}, indent=2))


if __name__ == "__main__":
    main()
