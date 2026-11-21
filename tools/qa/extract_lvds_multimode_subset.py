"""Extract a small real LVDS mode/state fixture without rewriting packet bytes.

Only the Python standard library is used. A source packet's Ethernet/IP/UDP,
timestamps and complete PCAPNG enhanced packet block remain byte-for-byte
unchanged. The subset intentionally has sequence gaps and is not a loss test.
"""
import argparse
from collections import Counter
import hashlib
import ipaddress
import json
from pathlib import Path
import struct

SHB = 0x0A0D0D0A
FPGA_SOURCE = "192.168.1.128:1534"


def require(condition, description):
    if not condition:
        raise ValueError(description)


def digest(data):
    return hashlib.sha256(data).hexdigest()


def udp_packet(frame):
    require(len(frame) >= 14, "truncated Ethernet frame")
    kind = int.from_bytes(frame[12:14], "big")
    start = 14
    while kind in (0x8100, 0x88A8):
        require(len(frame) >= start + 4, "truncated VLAN header")
        kind = int.from_bytes(frame[start + 2:start + 4], "big")
        start += 4
    if kind != 0x0800:
        return None
    ip = frame[start:]
    require(len(ip) >= 20 and ip[0] >> 4 == 4, "invalid IPv4 header")
    header_size = (ip[0] & 15) * 4
    total = int.from_bytes(ip[2:4], "big")
    require(20 <= header_size <= total <= len(ip), "truncated IPv4 packet")
    if ip[9] != 17:
        return None
    require(int.from_bytes(ip[6:8], "big") & 0x3FFF == 0, "fragmented UDP is unsupported")
    udp = ip[header_size:total]
    require(len(udp) >= 8, "truncated UDP header")
    source_port, destination_port, length = struct.unpack_from(">HHH", udp)
    require(8 <= length <= len(udp), "invalid UDP length")
    return {
        "source": f"{ipaddress.ip_address(ip[12:16])}:{source_port}",
        "destination": f"{ipaddress.ip_address(ip[16:20])}:{destination_port}",
        "payload": udp[8:length],
    }


def read_source(data):
    """Read one Ethernet PCAPNG section, retaining exact source block bytes."""
    offset = 0
    endian = None
    headers, interfaces, packets = [], [], []
    skipped_blocks = Counter()
    while offset < len(data):
        require(offset + 12 <= len(data), "truncated PCAPNG block")
        if data[offset:offset + 4] == bytes.fromhex("0a0d0d0a"):
            require(endian is None, "only a single-section capture is supported")
            bom = data[offset + 8:offset + 12]
            require(bom in (bytes.fromhex("4d3c2b1a"), bytes.fromhex("1a2b3c4d")), "invalid byte-order magic")
            endian = "<" if bom == bytes.fromhex("4d3c2b1a") else ">"
        require(endian is not None, "PCAPNG must begin with a section header")
        kind, size = struct.unpack_from(endian + "II", data, offset)
        require(size >= 12 and size % 4 == 0 and offset + size <= len(data), "invalid block length")
        block = data[offset:offset + size]
        require(struct.unpack_from(endian + "I", block, size - 4)[0] == size, "block length trailer mismatch")
        body = block[8:-4]
        if kind == SHB:
            headers.append(block)
        elif kind == 1:
            require(not packets, "interfaces after packets are unsupported")
            require(len(body) >= 8, "truncated interface description")
            linktype, _, snaplen = struct.unpack_from(endian + "HHI", body)
            require(linktype == 1, "only Ethernet interfaces are supported")
            cursor, resolution, seconds_offset = 8, 6, 0
            while cursor + 4 <= len(body):
                code, length = struct.unpack_from(endian + "HH", body, cursor)
                cursor += 4
                if code == 0:
                    break
                require(cursor + length <= len(body), "truncated interface option")
                value = body[cursor:cursor + length]
                if code == 9:
                    require(length == 1, "invalid if_tsresol")
                    resolution = value[0]
                elif code == 14:
                    require(length == 8, "invalid if_tsoffset")
                    seconds_offset = struct.unpack(endian + "q", value)[0]
                cursor += (length + 3) // 4 * 4
            units = 2 ** (resolution & 127) if resolution & 128 else 10 ** resolution
            interfaces.append({"linktype": linktype, "snaplen": snaplen, "if_tsresol": resolution,
                               "ticks_per_second": units, "offset_seconds": seconds_offset})
            headers.append(block)
        elif kind == 6:
            require(len(body) >= 20, "truncated enhanced packet block")
            interface, high, low, captured, original = struct.unpack_from(endian + "IIIII", body)
            require(interface < len(interfaces) and captured == original and 20 + captured <= len(body),
                    "truncated capture or invalid interface")
            network = udp_packet(body[20:20 + captured])
            ticks = (high << 32) | low
            meta = interfaces[interface]
            packet = {"original_packet_index": len(packets), "block_offset": offset,
                      "block": block, "timestamp_ns": ticks * 1_000_000_000 // meta["ticks_per_second"]
                      + meta["offset_seconds"] * 1_000_000_000, "kind": "NON_UDP"}
            if network is not None:
                packet.update(network)
                payload = packet["payload"]
                packet["kind"] = "OTHER_UDP"
                if network["source"] == FPGA_SOURCE:
                    require(len(payload) == 293 and payload[24:30] == bytes.fromhex("eb9055aa0107"),
                            "invalid FPGA payload/public frame")
                    packet["kind"] = f"{payload[30]:02X}"
                    packet["outer_sequence"] = int.from_bytes(payload[4:8], "big")
                    if packet["kind"] == "AB":
                        values = struct.unpack_from(">IIIHHBBHBBBB", payload, 31)
                        fields = ("seconds", "microseconds", "f_act_mhz", "interval_us", "points",
                                  "work_mode", "scan_freq", "scan_voltage_mv", "ad9833_status",
                                  "dac8563_status", "sci_frame_seq", "pa0_seq")
                        packet["ab"] = dict(zip(fields, values))
            packets.append(packet)
        elif kind in (2, 3):
            raise ValueError("legacy/simple packet blocks are unsupported")
        else:
            # In particular, source ISB totals describe the whole capture, not a subset.
            skipped_blocks[str(kind)] += 1
        offset += size
    require(headers and packets, "capture has no section/packet data")
    return headers, interfaces, packets, dict(skipped_blocks)


def stage_key(packet):
    frame = packet["ab"]
    return frame["work_mode"], frame["scan_freq"], frame["scan_voltage_mv"]


def select_packets(packets):
    ab = [packet for packet in packets if packet["kind"] == "AB"]
    require(ab, "capture contains no AB frames")
    selected_ab = set()
    for index, packet in enumerate(ab):
        if index == 0 or stage_key(packet) != stage_key(ab[index - 1]):
            selected_ab.add(index)
        if index and ab[index - 1]["ab"]["pa0_seq"] == 255 and packet["ab"]["pa0_seq"] == 0:
            selected_ab.update((index - 1, index))
    selected = set()
    aa = [packet["original_packet_index"] for packet in packets if packet["kind"] == "AA"]
    ac = [packet["original_packet_index"] for packet in packets if packet["kind"] == "AC"]
    for ordinal in sorted(selected_ab):
        packet = ab[ordinal]
        index = packet["original_packet_index"]
        selected.add(index)
        before = [value for value in aa if value < index]
        after = [value for value in aa if value > index]
        auxiliary = [value for value in ac if value > index]
        require(before and after and auxiliary, "selected AB needs preceding/following AA and following AC")
        selected.update((before[-1], after[0], auxiliary[0]))
    others = [packet["original_packet_index"] for packet in packets if packet["kind"] == "OTHER_UDP"]
    if others:
        selected.add(others[0])
    return [packet for packet in packets if packet["original_packet_index"] in selected], selected_ab


def extract(input_path, output_path, provenance_path):
    require(input_path.is_file(), "input PCAPNG does not exist")
    require(not output_path.exists() and not provenance_path.exists(), "outputs must not already exist")
    require(len({input_path.resolve(), output_path.resolve(), provenance_path.resolve()}) == 3,
            "input and output paths must be distinct")
    original = input_path.read_bytes()
    headers, interfaces, packets, skipped = read_source(original)
    selected, ab_indexes = select_packets(packets)
    fixture = b"".join(headers + [packet["block"] for packet in selected])
    _, fixture_interfaces, fixture_packets, _ = read_source(fixture)
    require(fixture_interfaces == interfaces and len(fixture_packets) == len(selected), "subset readback mismatch")
    mapping = []
    for index, (before, after) in enumerate(zip(selected, fixture_packets)):
        require(before["block"] == after["block"], "enhanced packet bytes changed")
        require(before["timestamp_ns"] == after["timestamp_ns"], "capture timestamp changed")
        row = {"subset_packet_index": index, "original_packet_index": before["original_packet_index"],
               "original_block_offset": before["block_offset"], "block_bytes": len(before["block"]),
               "block_sha256": digest(before["block"]), "type": before["kind"],
               "timestamp_ns": before["timestamp_ns"], "payload_sha256": digest(before["payload"])}
        if "outer_sequence" in before:
            row["outer_sequence"] = before["outer_sequence"]
        if "ab" in before:
            row["ab"] = before["ab"]
        mapping.append(row)
    receipt = {
        "schema_version": 1, "label": "REAL_CAPTURE_SUBSET",
        "source": {"file": input_path.name, "bytes": len(original), "sha256": digest(original)},
        "fixture": {"file": output_path.name, "bytes": len(fixture), "sha256": digest(fixture)},
        "selection": "First AB of each contiguous configuration stage, both sides of PA0 255-to-0; preceding/following AA, following AC, first other UDP.",
        "source_ab_indexes": sorted(ab_indexes), "counts": dict(Counter(packet["kind"] for packet in selected)),
        "interfaces": interfaces, "unchanged_header_block_sha256": [digest(block) for block in headers],
        "discarded_nonpacket_block_types": skipped, "all_selected_packet_blocks_byte_identical": True,
        "limits": ["Subset has intentional u32/AA/AB sequence and capture-time gaps; it is not a loss/continuity acceptance capture.",
                   "Original time gaps remain: use historical frame selection for fast mode inspection.",
                   "Original capture ISB totals are omitted; SHB/IDB and selected EPB blocks are unchanged.",
                   "Offline sample does not certify live hardware or native Windows GUI operation."],
        "packets": mapping,
    }
    output_path.parent.mkdir(parents=True, exist_ok=True)
    provenance_path.parent.mkdir(parents=True, exist_ok=True)
    with output_path.open("xb") as stream:
        stream.write(fixture)
    with provenance_path.open("x", encoding="utf-8", newline="\n") as stream:
        json.dump(receipt, stream, indent=2, ensure_ascii=False)
        stream.write("\n")
    require(input_path.read_bytes() == original, "source changed during extraction")
    return receipt


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--input", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--provenance", type=Path, required=True)
    args = parser.parse_args()
    result = extract(args.input, args.output, args.provenance)
    print(json.dumps({"fixture": result["fixture"], "counts": result["counts"],
                      "source_ab_indexes": result["source_ab_indexes"]}, indent=2))


if __name__ == "__main__":
    main()
