"""SYNTHETIC malformed-package checks for the S01 evidence tool."""
import importlib
import struct
import tempfile
import unittest
import zipfile
from pathlib import Path


def synthetic_pe(import_name=b"KERNEL32.dll"):
    """Minimal PE header fixture, not a runnable or deliverable executable."""
    data = bytearray(1024)
    data[:2] = b"MZ"
    struct.pack_into("<I", data, 0x3C, 0x80)
    data[0x80:0x84] = b"PE\0\0"
    struct.pack_into("<HH", data, 0x84, 0x8664, 1)
    struct.pack_into("<H", data, 0x94, 240)
    struct.pack_into("<H", data, 0x98, 0x20B)
    struct.pack_into("<I", data, 0x98 + 108, 16)
    struct.pack_into("<II", data, 0x98 + 120, 0x1000, 40)
    struct.pack_into("<IIII", data, 0x188 + 8, 512, 0x1000, 512, 512)
    struct.pack_into("<I", data, 512 + 12, 0x1080)
    data[640:640 + len(import_name) + 1] = import_name + b"\0"
    return bytes(data)


class PackageChecks(unittest.TestCase):
    def setUp(self):
        try:
            self.verifier = importlib.import_module("lvds_multitype_s01_verify")
        except ModuleNotFoundError:
            self.fail("S01 verifier has not been implemented")

    def test_static_package_content_and_hashes(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            exe, readme, package = root / "app.exe", root / "README.md", root / "app.zip"
            exe.write_bytes(synthetic_pe())
            readme.write_text("SYNTHETIC self-test", encoding="utf-8")
            with zipfile.ZipFile(package, "w") as archive:
                archive.write(exe, "LangmuierProbeGroundControl.exe")
                archive.write(readme, "README.md")
            result = self.verifier.verify_package(exe, package, readme)
            self.assertEqual(result["result"], "PASS")
            self.assertEqual(result["imports"], ["KERNEL32.dll"])
            self.assertEqual(result["architecture"], "PE32+ Windows x64")
            self.assertTrue(result["zip_crc_and_content_match"])
            exe.write_bytes(exe.read_bytes() + b"mismatch")
            with self.assertRaisesRegex(ValueError, "EXE.*match"):
                self.verifier.verify_package(exe, package, readme)

    def test_dynamic_crt_and_wrong_architecture_are_refused(self):
        with self.assertRaisesRegex(ValueError, "dynamic CRT"):
            self.verifier.inspect_pe(synthetic_pe(b"VCRUNTIME140.dll"))
        data = bytearray(synthetic_pe())
        struct.pack_into("<H", data, 0x84, 0x14C)
        with self.assertRaisesRegex(ValueError, "AMD64"):
            self.verifier.inspect_pe(bytes(data))

    def test_common_type_classification_excludes_wrong_addresses_and_short_frames(self):
        header = bytes.fromhex("eb9055aa0107")
        for type_byte, expected in [(0xAA, "AA"), (0xAB, "AB"), (0xAC, "AC"), (0xAD, "UNKNOWN_AD")]:
            frame = header + bytes([type_byte]) + bytes(262)
            self.assertEqual(self.verifier.common_type(frame), expected)
            self.assertIsNone(self.verifier.common_type(frame[:-1]))
            self.assertIsNone(self.verifier.common_type(b"\x00" + frame[1:]))


if __name__ == "__main__":
    unittest.main()
