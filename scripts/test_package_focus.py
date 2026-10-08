from __future__ import annotations

import hashlib
import json
import os
import platform
import stat
import shutil
import subprocess
import tarfile
import tempfile
import unittest
from pathlib import Path

from scripts import package_focus


REPO_ROOT = Path(__file__).resolve().parents[1]
INSTALLER = REPO_ROOT / "packaging" / "linux" / "install.sh"


def write_executable(path: Path, content: str) -> None:
    path.write_text(content, encoding="utf-8")
    path.chmod(0o755)


class FocusPackageBuilderTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory(prefix="herdr-focus-builder-")
        self.root = Path(self.temporary.name)
        self.binary = self.root / "herdr"
        elf = bytearray(64)
        elf[:4] = b"\x7fELF"
        elf[4:8] = bytes((2, 1, 1, 0))
        elf[16:18] = (2).to_bytes(2, "little")
        elf[18:20] = (62).to_bytes(2, "little")
        self.binary.write_bytes(elf + b"GLIBC_2.39\0")
        self.output_dir = self.root / "dist"

    def tearDown(self) -> None:
        self.temporary.cleanup()

    def test_archive_has_stable_contents_and_generated_metadata(self) -> None:
        archive_path = package_focus.build_package(self.binary, self.output_dir)

        self.assertEqual(archive_path.name, "herdr-focus-linux-x86_64.tar.gz")
        checksum_path = self.output_dir / "herdr-focus-linux-x86_64.tar.gz.sha256"
        self.assertEqual(
            checksum_path.read_text(encoding="ascii"),
            f"{hashlib.sha256(archive_path.read_bytes()).hexdigest()}  {archive_path.name}\n",
        )
        with tarfile.open(archive_path, "r:gz") as archive:
            members = {member.name: member for member in archive.getmembers()}
            self.assertEqual(
                set(members),
                {
                    "herdr-focus",
                    "herdr-focus/BUILD_INFO.json",
                    "herdr-focus/README.md",
                    "herdr-focus/SHA256SUMS",
                    "herdr-focus/herdr",
                    "herdr-focus/install.sh",
                },
            )
            sums = archive.extractfile("herdr-focus/SHA256SUMS")
            assert sums is not None
            self.assertEqual(
                sums.read().decode(),
                f"{hashlib.sha256(self.binary.read_bytes()).hexdigest()}  herdr\n",
            )
            info_file = archive.extractfile("herdr-focus/BUILD_INFO.json")
            assert info_file is not None
            info = json.load(info_file)
            self.assertEqual(info["architecture"], "linux-x86_64")
            self.assertEqual(info["required_glibc"], "2.39")
            self.assertEqual(
                info["binary_sha256"], hashlib.sha256(self.binary.read_bytes()).hexdigest()
            )
            self.assertRegex(info["repository_revision"], r"^[0-9a-f]{40}$")
            self.assertTrue(members["herdr-focus/herdr"].mode & stat.S_IXUSR)
            self.assertTrue(members["herdr-focus/install.sh"].mode & stat.S_IXUSR)

    def test_non_linux_elf_is_rejected(self) -> None:
        invalid = self.root / "invalid"
        invalid.write_bytes(b"not an executable")
        with self.assertRaisesRegex(ValueError, "not an ELF"):
            package_focus.build_package(invalid, self.output_dir)


@unittest.skipUnless(
    platform.system() == "Linux"
    and platform.machine() == "x86_64"
    and shutil.which("bash"),
    "Linux x86-64 installer tests require bash",
)
class FocusInstallerTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory(prefix="herdr-focus-installer-")
        self.root = Path(self.temporary.name)
        self.package = self.root / "herdr-focus"
        self.package.mkdir()
        shutil.copy2(INSTALLER, self.package / "install.sh")
        (self.package / "install.sh").chmod(0o755)
        self.bin_dir = self.root / "installed"
        self.log = self.root / "handoff.log"
        self.home = self.root / "home"
        self.home.mkdir()
        self._write_package_binary(version_ok=True, handoff_ok=True)

    def tearDown(self) -> None:
        self.temporary.cleanup()

    def _write_package_binary(self, *, version_ok: bool, handoff_ok: bool) -> None:
        version_exit = "0" if version_ok else "23"
        handoff_exit = "0" if handoff_ok else "37"
        write_executable(
            self.package / "herdr",
            f"""#!/bin/sh
if [ "$1" = "--version" ]; then
    exit {version_exit}
fi
if [ "$4" = "live-handoff" ]; then
    printf '%s\\n' "$*" >> "$HANDOFF_LOG"
    exit {handoff_exit}
fi
printf 'unexpected invocation: %s\\n' "$*" >&2
exit 41
""",
        )
        digest = hashlib.sha256((self.package / "herdr").read_bytes()).hexdigest()
        (self.package / "SHA256SUMS").write_text(f"{digest}  herdr\n", encoding="ascii")

    def _run(self, *arguments: str) -> subprocess.CompletedProcess[str]:
        environment = {
            **os.environ,
            "HOME": str(self.home),
            "HANDOFF_LOG": str(self.log),
        }
        return subprocess.run(
            ["bash", str(self.package / "install.sh"), *arguments],
            cwd=self.package,
            env=environment,
            capture_output=True,
            text=True,
            check=False,
        )

    def test_install_replaces_atomically_without_handoff(self) -> None:
        self.bin_dir.mkdir()
        installed = self.bin_dir / "herdr"
        installed.write_text("old process image\n", encoding="utf-8")
        result = self._run("--bin-dir", str(self.bin_dir))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue(installed.is_file())
        self.assertFalse(self.log.exists())

    def test_checksum_failure_preserves_existing_binary(self) -> None:
        self.bin_dir.mkdir()
        installed = self.bin_dir / "herdr"
        installed.write_text("old\n", encoding="utf-8")
        (self.package / "SHA256SUMS").write_text(f"{'0' * 64}  herdr\n", encoding="ascii")
        result = self._run("--bin-dir", str(self.bin_dir))
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("checksum", result.stderr)
        self.assertEqual(installed.read_text(encoding="utf-8"), "old\n")

    def test_version_failure_preserves_existing_binary(self) -> None:
        self.bin_dir.mkdir()
        installed = self.bin_dir / "herdr"
        installed.write_text("old\n", encoding="utf-8")
        self._write_package_binary(version_ok=False, handoff_ok=True)
        result = self._run("--bin-dir", str(self.bin_dir))
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("--version", result.stderr)
        self.assertEqual(installed.read_text(encoding="utf-8"), "old\n")

    def test_session_requires_explicit_handoff(self) -> None:
        result = self._run("--bin-dir", str(self.bin_dir), "--session", "work")
        self.assertEqual(result.returncode, 2)
        self.assertIn("requires --handoff", result.stderr)
        self.assertFalse((self.bin_dir / "herdr").exists())

    def test_handoff_uses_requested_session_and_never_stop(self) -> None:
        result = self._run(
            "--bin-dir",
            str(self.bin_dir),
            "--handoff",
            "--session",
            "work",
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(
            self.log.read_text(encoding="utf-8"),
            f"--session work server live-handoff --import-exe {self.bin_dir / 'herdr'}\n",
        )
        self.assertNotIn("stop", self.log.read_text(encoding="utf-8"))

    def test_failed_handoff_is_reported_without_fallback(self) -> None:
        self._write_package_binary(version_ok=True, handoff_ok=False)
        result = self._run("--bin-dir", str(self.bin_dir), "--handoff")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("Live handoff failed", result.stderr)
        self.assertEqual(self.log.read_text(encoding="utf-8").count("live-handoff"), 1)
        self.assertNotIn("stop", self.log.read_text(encoding="utf-8"))

    def test_existing_destination_directory_is_not_replaced(self) -> None:
        self.bin_dir.mkdir()
        destination = self.bin_dir / "herdr"
        destination.mkdir()
        result = self._run("--bin-dir", str(self.bin_dir))
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("directory", result.stderr)


if __name__ == "__main__":
    unittest.main()
