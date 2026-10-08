#!/usr/bin/env python3
"""Build the Linux x86-64 package used by the focus workflow."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import shutil
import struct
import subprocess
import sys
import tarfile
import tempfile
from pathlib import Path


PROJECT_ROOT = Path(__file__).resolve().parents[1]
PACKAGE_SOURCE = PROJECT_ROOT / "packaging" / "linux"
ARCHIVE_NAME = "herdr-focus-linux-x86_64.tar.gz"
PACKAGE_ROOT = "herdr-focus"
GLIBC_SYMBOL_RE = re.compile(rb"GLIBC_(\d+)\.(\d+)")


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def validate_linux_x86_64(binary: Path) -> str | None:
    """Validate the ELF header and return the highest referenced glibc version."""
    data = binary.read_bytes()
    if len(data) < 64 or data[:4] != b"\x7fELF":
        raise ValueError(f"{binary} is not an ELF executable")
    if data[4] != 2:
        raise ValueError(f"{binary} is not an ELF64 executable")
    if data[5] != 1:
        raise ValueError(f"{binary} is not a little-endian ELF executable")
    if data[6] != 1:
        raise ValueError(f"{binary} has an unsupported ELF version")
    if data[7] not in (0, 3):
        raise ValueError(f"{binary} has an unsupported Linux ELF OS ABI")
    elf_type = struct.unpack_from("<H", data, 16)[0]
    if elf_type not in (2, 3):
        raise ValueError(f"{binary} is not an executable ELF image")
    machine = struct.unpack_from("<H", data, 18)[0]
    if machine != 62:
        raise ValueError(f"{binary} is not an x86-64 ELF executable")

    versions = {
        (int(match.group(1)), int(match.group(2)))
        for match in GLIBC_SYMBOL_RE.finditer(data)
    }
    if not versions:
        return None
    major, minor = max(versions)
    return f"{major}.{minor}"


def repository_revision() -> str:
    result = subprocess.run(
        ["git", "rev-parse", "HEAD"],
        cwd=PROJECT_ROOT,
        check=False,
        capture_output=True,
        text=True,
    )
    if result.returncode != 0 or not result.stdout.strip():
        raise RuntimeError("cannot determine the package builder repository revision")
    return result.stdout.strip()


def default_binary() -> Path:
    target_dir = Path(os.environ.get("CARGO_TARGET_DIR", "target"))
    if not target_dir.is_absolute():
        target_dir = PROJECT_ROOT / target_dir
    return target_dir / "release" / "herdr"


def add_file(archive: tarfile.TarFile, path: Path, archive_name: str) -> None:
    info = archive.gettarinfo(str(path), arcname=archive_name)
    info.mode = 0o755 if path.is_dir() or path.name in {"herdr", "install.sh"} else 0o644
    info.mtime = 0
    info.uid = 0
    info.gid = 0
    info.uname = ""
    info.gname = ""
    if path.is_file():
        with path.open("rb") as source:
            archive.addfile(info, source)
    else:
        archive.addfile(info)


def build_package(binary: Path, output_dir: Path) -> Path:
    binary = binary.resolve()
    if not binary.is_file():
        raise ValueError(f"Herdr executable does not exist: {binary}")
    required_glibc = validate_linux_x86_64(binary)
    binary_sha256 = sha256_file(binary)
    revision = repository_revision()

    for name in ("install.sh", "README.md"):
        if not (PACKAGE_SOURCE / name).is_file():
            raise RuntimeError(f"package source is missing {name}")

    output_dir.mkdir(parents=True, exist_ok=True)
    archive_path = output_dir / ARCHIVE_NAME
    with tempfile.TemporaryDirectory(prefix="herdr-focus-package-") as temporary:
        staging = Path(temporary) / PACKAGE_ROOT
        staging.mkdir()
        shutil.copy2(PACKAGE_SOURCE / "install.sh", staging / "install.sh")
        shutil.copy2(PACKAGE_SOURCE / "README.md", staging / "README.md")
        shutil.copy2(binary, staging / "herdr")
        (staging / "install.sh").chmod(0o755)
        (staging / "herdr").chmod(0o755)
        (staging / "SHA256SUMS").write_text(
            f"{binary_sha256}  herdr\n", encoding="ascii"
        )
        build_info = {
            "architecture": "linux-x86_64",
            "binary_sha256": binary_sha256,
            "package_root": PACKAGE_ROOT,
            "repository_revision": revision,
            "required_glibc": required_glibc,
        }
        (staging / "BUILD_INFO.json").write_text(
            json.dumps(build_info, indent=2, sort_keys=True) + "\n",
            encoding="utf-8",
        )

        temporary_archive_handle = tempfile.NamedTemporaryFile(
            mode="wb",
            prefix=f".{ARCHIVE_NAME}.",
            suffix=".tmp",
            dir=output_dir,
            delete=False,
        )
        temporary_archive = Path(temporary_archive_handle.name)
        temporary_archive_handle.close()
        try:
            with temporary_archive.open("wb") as output:
                with tarfile.open(fileobj=output, mode="w:gz") as archive:
                    add_file(archive, staging, PACKAGE_ROOT)
                    for name in (
                        "BUILD_INFO.json",
                        "README.md",
                        "SHA256SUMS",
                        "herdr",
                        "install.sh",
                    ):
                        add_file(archive, staging / name, f"{PACKAGE_ROOT}/{name}")
            temporary_archive.replace(archive_path)
            archive_sha256 = sha256_file(archive_path)
            checksum_path = output_dir / f"{ARCHIVE_NAME}.sha256"
            checksum_temp_handle = tempfile.NamedTemporaryFile(
                mode="w",
                encoding="ascii",
                prefix=f".{checksum_path.name}.",
                suffix=".tmp",
                dir=output_dir,
                delete=False,
            )
            checksum_temp = Path(checksum_temp_handle.name)
            try:
                checksum_temp_handle.write(f"{archive_sha256}  {ARCHIVE_NAME}\n")
                checksum_temp_handle.close()
                checksum_temp.replace(checksum_path)
            finally:
                checksum_temp.unlink(missing_ok=True)
        finally:
            temporary_archive.unlink(missing_ok=True)
    return archive_path


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--binary",
        type=Path,
        default=None,
        help="existing Herdr binary (default: CARGO_TARGET_DIR/release/herdr)",
    )
    parser.add_argument(
        "--output-dir",
        type=Path,
        default=PROJECT_ROOT / "dist",
        help="directory for the stable archive name (default: repo/dist)",
    )
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    binary = args.binary or default_binary()
    try:
        archive = build_package(binary, args.output_dir.resolve())
    except (OSError, RuntimeError, ValueError) as error:
        print(f"package_focus: {error}", file=sys.stderr)
        return 1
    print(archive)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
