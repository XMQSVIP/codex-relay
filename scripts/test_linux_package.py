"""Cross-platform regressions for static ELF and Linux package payload boundaries."""
import json
from pathlib import Path
import stat
import struct
import tempfile
import unittest
from unittest.mock import patch
import zipfile

import package_linux
from portable_contents import write_notices
from test_linux_portable import extract_verified


def elf(machine=62, segment=1, needed=False):
    header = bytearray(64)
    header[:7] = b"\x7fELF\x02\x01\x01"
    struct.pack_into("<HH", header, 16, 2, machine)
    struct.pack_into("<Q", header, 32, 64)
    struct.pack_into("<HH", header, 54, 56, 1)
    program = bytearray(56)
    struct.pack_into("<I", program, 0, segment)
    struct.pack_into("<Q", program, 8, 120)
    struct.pack_into("<Q", program, 32, 16)
    return header + program + struct.pack("<QQ", 1 if needed else 0, 0)


class LinuxPackageTests(unittest.TestCase):
    def test_elf_rejects_dynamic_linking_and_wrong_architecture(self):
        with tempfile.TemporaryDirectory() as temporary:
            binary = Path(temporary) / "codex-relay"
            for data, architecture, accepted in [
                (elf(), "x64", True), (elf(183), "arm64", True),
                (elf(segment=2), "x64", True),
                (elf(183), "x64", False), (elf(segment=3), "x64", False),
                (elf(segment=2, needed=True), "x64", False),
                (b"not an ELF", "x64", False), (elf()[:70], "x64", False),
            ]:
                with self.subTest(architecture=architecture, accepted=accepted, data=data):
                    binary.write_bytes(data)
                    if accepted:
                        package_linux.check_binary(binary, architecture)
                    else:
                        with self.assertRaises(RuntimeError):
                            package_linux.check_binary(binary, architecture)

    def make_package(self, root):
        for relative, content in {
            "package.json": '{"version":"test"}',
            "relay.config.json": '{"port":9999,"private":"DO_NOT_SHIP"}',
            "scripts/start-linux.sh": "#!/bin/sh\nexec ./codex-relay serve\n",
            "examples/codex-relay.service": "[Service]\n",
            "docs/LINUX-SERVER.md": "Server instructions",
            "web/dist/index.html": '<div id="root"></div>',
            "web/dist/assets/index.js": "console.log('test')",
            "data/auth.json": "DO_NOT_SHIP", ".cache/launch-config.json": "DO_NOT_SHIP",
        }.items():
            path = root / relative
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(content)
        binary = root / "codex-relay"
        binary.write_bytes(elf())
        license_dir = root / "dependency"
        license_dir.mkdir()
        (license_dir / "LICENSE").write_bytes(b"Copyright test\r\nMIT\r\n")
        archive = root / "linux.zip"
        with patch.object(package_linux, "collect_notices", side_effect=lambda _, stage, target:
                          write_notices(stage, [("rust", {"name": "fixture", "version": "1",
                                                        "license": "MIT"}, license_dir)])):
            package_linux.write_archive(root, binary, archive, "CodexRelay-test", "x64", "test")
        return archive

    def test_package_preserves_modes_and_excludes_private_data(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            archive = self.make_package(root)
            package, manifest = extract_verified(archive, root / "extracted")
            self.assertEqual(manifest["target"], "x86_64-unknown-linux-musl")
            self.assertEqual(json.loads((package / "relay.config.json").read_text()), {"port": 4317})
            self.assertFalse((package / "data").exists())
            self.assertFalse((package / ".cache").exists())
            with zipfile.ZipFile(archive) as bundle:
                for name in ("codex-relay", "start-linux.sh"):
                    self.assertEqual(stat.S_IMODE(bundle.getinfo(f"CodexRelay-test/{name}").external_attr >> 16),
                                     0o755)

    def test_verifier_rejects_tampering_and_traversal_before_extracting(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            archive = self.make_package(root)
            with zipfile.ZipFile(archive) as bundle:
                contents = [(info, bundle.read(info)) for info in bundle.infolist()]
            for index, (replacement, data) in enumerate([
                ("CodexRelay-test/web/dist/index.html", b"tampered"),
                ("CodexRelay-test/../../escape", b"escape"),
                ("CodexRelay-test/data/auth.json", b"credentials"),
            ]):
                corrupt = root / f"corrupt-{index}.zip"
                with zipfile.ZipFile(corrupt, "w") as output:
                    for info, original in contents:
                        output.writestr(info, data if info.filename == replacement else original)
                    if replacement not in [info.filename for info, _ in contents]:
                        output.writestr(replacement, data)
                extracted = root / f"extract-{index}"
                with self.assertRaises(RuntimeError):
                    extract_verified(corrupt, extracted)
                self.assertFalse(extracted.exists())


if __name__ == "__main__":
    unittest.main()
