"""Read-only inputs and exclusive evidence outputs for LVDS-MULTITYPE-S01.

The package command checks bytes, PE architecture/imports and ZIP contents.
It does not infer Cargo features or run the executable. The capture command
counts only unchanged, complete public frames; bit candidates are diagnostic.
"""
import argparse
from collections import Counter
import hashlib
import json
from pathlib import Path
import struct
import zipfile


def file_info(path):
    path = Path(path)
    data = path.read_bytes()
    return {"path": str(path.resolve()), "bytes": len(data),
            "sha256": hashlib.sha256(data).hexdigest()}


def inspect_pe(blob):
    if len(blob) < 64 or blob[:2] != b"MZ":
        raise ValueError("missing DOS MZ header")
    pe = struct.unpack_from("<I", blob, 0x3C)[0]
    if pe + 24 > len(blob) or blob[pe:pe + 4] != b"PE\0\0":
        raise ValueError("missing/truncated PE header")
    machine, section_count = struct.unpack_from("<HH", blob, pe + 4)
    if machine != 0x8664:
        raise ValueError(f"expected AMD64 machine 8664, got {machine:04x}")
    optional_size = struct.unpack_from("<H", blob, pe + 20)[0]
    optional = pe + 24
    if optional_size < 128 or optional + optional_size > len(blob):
        raise ValueError("truncated PE optional header")
    if struct.unpack_from("<H", blob, optional)[0] != 0x20B:
        raise ValueError("expected PE32+ optional header")
    sections = []
    for index in range(section_count):
        off = optional + optional_size + index * 40
        if off + 40 > len(blob):
            raise ValueError("truncated PE section table")
        virtual_size, rva, raw_size, raw = struct.unpack_from("<IIII", blob, off + 8)
        sections.append((rva, max(virtual_size, raw_size), raw_size, raw))

    def offset(address):
        for start, size, raw_size, raw in sections:
            if start <= address < start + size:
                relative = address - start
                if relative >= raw_size or raw + relative >= len(blob):
                    break
                return raw + relative
        raise ValueError(f"PE RVA has no file bytes: {address:#x}")

    imports_rva, imports_size = struct.unpack_from("<II", blob, optional + 120)
    imports = []
    if imports_rva:
        if imports_size < 20:
            raise ValueError("invalid PE import directory size")
        for index in range(imports_size // 20):
            cursor = offset(imports_rva + index * 20)
            if cursor + 20 > len(blob):
                raise ValueError("truncated PE import descriptor")
            if not any(blob[cursor:cursor + 20]):
                break
            name = offset(struct.unpack_from("<I", blob, cursor + 12)[0])
            end = blob.find(b"\0", name, min(len(blob), name + 4096))
            if end < 0:
                raise ValueError("unterminated PE import name")
            imports.append(blob[name:end].decode("ascii"))
        else:
            raise ValueError("PE import directory lacks terminating descriptor")
    forbidden = ("vcruntime", "msvcp", "ucrtbase", "api-ms-win-crt", "msvcr")
    if any(name.lower().startswith(forbidden) for name in imports):
        raise ValueError(f"dynamic CRT import found: {imports}")
    return sorted(imports)


def verify_package(exe, package, readme):
    exe, package, readme = Path(exe), Path(package), Path(readme)
    blob = exe.read_bytes()
    imports = inspect_pe(blob)
    with zipfile.ZipFile(package) as archive:
        names = archive.namelist()
        expected = ["LangmuierProbeGroundControl.exe", "README.md"]
        if sorted(names) != expected:
            raise ValueError(f"unexpected ZIP entries: {names}")
        if archive.testzip() is not None:
            raise ValueError("ZIP CRC failure")
        if archive.read(expected[0]) != blob:
            raise ValueError("ZIP EXE bytes do not match the provided executable")
        if archive.read(expected[1]) != readme.read_bytes():
            raise ValueError("ZIP README bytes do not match the provided README")
    return {"result": "PASS", "exe": file_info(exe), "package": file_info(package),
            "readme": file_info(readme), "architecture": "PE32+ Windows x64",
            "imports": imports, "no_dynamic_crt_imports": True,
            "zip_crc_and_content_match": True,
            "cargo_feature_verification": "NOT_RUN: requires separate build evidence",
            "native_gui_verification": "NOT_RUN: package byte inspection does not launch the app"}


def common_type(frame):
    if len(frame) != 269 or frame[:6] != bytes.fromhex("eb9055aa0107"):
        return None
    return {0xAA: "AA", 0xAB: "AB", 0xAC: "AC"}.get(frame[6], f"UNKNOWN_{frame[6]:02X}")


def audit_capture(path):
    from lvds_raw_recovery_reference import _unwrap, audit_records, read_pcap_udp
    records, packet_summary = read_pcap_udp(path)
    outer_errors, native, byte_sync = Counter(), Counter(), Counter()
    inner_lengths, unvalidated_byte6 = Counter(), Counter()
    for record in records:
        try:
            _, inner = _unwrap(record["payload"])
        except ValueError as error:
            outer_errors[str(error)] += 1
            continue
        inner_lengths[str(len(inner))] += 1
        if len(inner) > 6:
            unvalidated_byte6[f"{inner[6]:02X}"] += 1
        kind = common_type(inner)
        if kind:
            native[kind] += 1
        else:
            native["STRUCTURE_INVALID"] += 1
        for offset in range(1, max(1, len(inner) - 269 + 1)):
            found = common_type(inner[offset:offset + 269])
            if found:
                byte_sync[found] += 1
    _, diagnostic = audit_records(records)
    return {"result": "PASS", "capture": file_info(path), **packet_summary,
            "udp_payload_bytes": sum(len(row["payload"]) for row in records),
            "outer_errors": dict(outer_errors), "inner_length_counts": dict(inner_lengths),
            "native_exact_public_frames": dict(native),
            "complete_byte_sync_frames_at_positive_offsets": dict(byte_sync),
            "unvalidated_offset6_byte_counts_not_packet_types": dict(unvalidated_byte6),
            "existing_AA_diagnostic_quality_counts": diagnostic["quality_counts"],
            "existing_AA_candidate_shift_counts": diagnostic["candidate_shift_counts"],
            "existing_AA_candidate_missing_tail_bits": diagnostic["candidate_missing_tail_bit_counts"],
            "production_crc_policy": "CRC_NOT_CHECKED",
            "cross_record_bits_used": False,
            "scope": "Independent original-PCAP software audit. Bit-shift candidates are not native auxiliary frames.",
            "real_mixed_FPGA_acceptance": "BLOCKED: requires real AB/AC mixed capture or device input"}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    package = commands.add_parser("package")
    for name in ("exe", "package", "readme"):
        package.add_argument("--" + name, type=Path, required=True)
    capture = commands.add_parser("capture")
    capture.add_argument("--pcap", type=Path, required=True)
    for command in (package, capture):
        command.add_argument("--output", type=Path, required=True,
                             help="new JSON path; existing files are never overwritten")
    args = parser.parse_args()
    if args.output.exists():
        parser.error("output already exists; choose a new evidence path")
    report = (verify_package(args.exe, args.package, args.readme) if args.command == "package"
              else audit_capture(args.pcap))
    args.output.parent.mkdir(parents=True, exist_ok=True)
    with args.output.open("x", encoding="utf-8") as stream:
        stream.write(json.dumps(report, ensure_ascii=False, indent=2) + "\n")
    print(json.dumps(report, ensure_ascii=False, indent=2))


if __name__ == "__main__":
    main()
