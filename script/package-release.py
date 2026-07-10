#!/usr/bin/env python3
"""Build a deterministic LocalHold archive from a compiled binary."""

from __future__ import annotations

import argparse
import datetime as dt
import os
import re
import shutil
import stat
import subprocess
import sys
import tarfile
import tempfile
import zipfile
from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parent.parent
TARGET_NAME = re.compile(r"^[A-Za-z0-9_.-]+$")
PACKAGE_FILES = (
    "README.md",
    "CHANGELOG.md",
    "LICENSE",
    "NOTICE",
    "THIRD_PARTY_NOTICES.md",
    "localhold.example.toml",
)


class PackageError(RuntimeError):
    """Actionable archive construction error."""


def validate_inputs(tag: str, target: str, binary: Path) -> None:
    """Validate metadata and package-specific arguments before staging."""
    subprocess.run(
        [sys.executable, str(REPO_ROOT / "script" / "release.py"), "validate", tag],
        cwd=REPO_ROOT,
        check=True,
    )
    if not TARGET_NAME.fullmatch(target):
        raise PackageError(f"invalid release target name: {target}")
    if not binary.is_file():
        raise PackageError(f"release binary does not exist: {binary}")


def source_date_epoch() -> int:
    """Use an explicit reproducible timestamp or the current commit timestamp."""
    configured = os.environ.get("SOURCE_DATE_EPOCH")
    if configured is not None:
        try:
            return int(configured)
        except ValueError as error:
            raise PackageError("SOURCE_DATE_EPOCH must be an integer") from error

    result = subprocess.run(
        ["git", "show", "-s", "--format=%ct", "HEAD"],
        cwd=REPO_ROOT,
        check=True,
        capture_output=True,
        text=True,
    )
    return int(result.stdout.strip())


def stage_package(stage_root: Path, binary: Path) -> None:
    """Create the platform-neutral release archive layout."""
    bin_dir = stage_root / "bin"
    bin_dir.mkdir(parents=True)
    binary_name = "hold.exe" if binary.suffix.lower() == ".exe" else "hold"
    destination = bin_dir / binary_name
    shutil.copy2(binary, destination)
    destination.chmod(0o755)

    for relative in PACKAGE_FILES:
        shutil.copy2(REPO_ROOT / relative, stage_root / relative)
    shutil.copytree(REPO_ROOT / "docs", stage_root / "docs")


def archive_paths(stage_root: Path) -> list[Path]:
    """Return a stable parent-before-child package traversal."""
    return [stage_root, *sorted(stage_root.rglob("*"), key=lambda path: path.as_posix())]


def write_tar(stage_root: Path, destination: Path, epoch: int) -> None:
    """Write a deterministic uncompressed POSIX tar archive."""
    with tarfile.open(destination, mode="w", format=tarfile.PAX_FORMAT) as archive:
        for path in archive_paths(stage_root):
            arcname = path.relative_to(stage_root.parent).as_posix()
            info = archive.gettarinfo(str(path), arcname)
            info.uid = 0
            info.gid = 0
            info.uname = "root"
            info.gname = "root"
            info.mtime = epoch
            if path.is_file():
                with path.open("rb") as source:
                    archive.addfile(info, source)
            else:
                archive.addfile(info)


def write_tar_zst(stage_root: Path, destination: Path, epoch: int) -> None:
    """Compress a deterministic tar archive with reproducible high-ratio zstd."""
    uncompressed = stage_root.parent / f"{stage_root.name}.tar"
    write_tar(stage_root, uncompressed, epoch)
    subprocess.run(
        [
            "zstd",
            "-19",
            "--threads=1",
            "--no-progress",
            "--quiet",
            "--force",
            str(uncompressed),
            "-o",
            str(destination),
        ],
        check=True,
    )


def write_zip(stage_root: Path, destination: Path, epoch: int) -> None:
    """Write a deterministic ZIP archive with executable mode metadata."""
    earliest = int(dt.datetime(1980, 1, 1, tzinfo=dt.timezone.utc).timestamp())
    timestamp = dt.datetime.fromtimestamp(max(epoch, earliest), tz=dt.timezone.utc)
    date_time = (
        timestamp.year,
        timestamp.month,
        timestamp.day,
        timestamp.hour,
        timestamp.minute,
        timestamp.second,
    )

    with zipfile.ZipFile(destination, "w", compression=zipfile.ZIP_DEFLATED) as archive:
        for path in archive_paths(stage_root):
            relative = path.relative_to(stage_root.parent).as_posix()
            is_directory = path.is_dir()
            name = f"{relative}/" if is_directory else relative
            info = zipfile.ZipInfo(name, date_time)
            info.create_system = 3
            mode = stat.S_IMODE(path.stat().st_mode)
            file_type = stat.S_IFDIR if is_directory else stat.S_IFREG
            info.external_attr = (file_type | mode) << 16
            info.compress_type = zipfile.ZIP_DEFLATED
            archive.writestr(info, b"" if is_directory else path.read_bytes())


def package(args: argparse.Namespace) -> Path:
    """Build the selected release archive."""
    validate_inputs(args.tag, args.target, args.binary)
    extension = "tar.zst" if args.archive_format == "tar.zst" else "zip"
    stem = f"localhold-{args.tag}-{args.target}"
    args.output_dir.mkdir(parents=True, exist_ok=True)
    destination = args.output_dir / f"{stem}.{extension}"
    epoch = source_date_epoch()

    with tempfile.TemporaryDirectory(prefix="localhold-release-") as temporary:
        stage_root = Path(temporary) / stem
        stage_root.mkdir()
        stage_package(stage_root, args.binary.resolve())
        if args.archive_format == "tar.zst":
            write_tar_zst(stage_root, destination, epoch)
        else:
            write_zip(stage_root, destination, epoch)

    print(destination)
    return destination


def parser() -> argparse.ArgumentParser:
    """Construct the archive-tool command line parser."""
    result = argparse.ArgumentParser(description=__doc__)
    result.add_argument("--tag", required=True, help="validated release tag")
    result.add_argument("--target", required=True, help="Rust target triple")
    result.add_argument("--binary", required=True, type=Path, help="compiled hold binary")
    result.add_argument(
        "--format", required=True, choices=("tar.zst", "zip"), dest="archive_format"
    )
    result.add_argument("--output-dir", required=True, type=Path)
    return result


def main() -> int:
    """Build the archive with concise errors."""
    try:
        package(parser().parse_args())
    except (PackageError, subprocess.CalledProcessError, OSError, ValueError) as error:
        print(f"error: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
