#!/usr/bin/env python3
"""Validate a materialized CUDA runtime and, optionally, a live LocalHold process."""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import subprocess
import sys
from pathlib import Path


CHUNK_SIZE = 1024 * 1024
NEEDED = re.compile(r"\(NEEDED\).*Shared library: \[([^]]+)]")


class ValidationError(RuntimeError):
    """Actionable CUDA runtime validation error."""


def sha256_file(path: Path) -> str:
    """Return a file's SHA-256 without loading it into memory."""
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(CHUNK_SIZE), b""):
            digest.update(chunk)
    return digest.hexdigest()


def load_manifest(root: Path) -> dict[str, object]:
    """Load the resolved runtime manifest."""
    path = root / "manifest" / "cuda-runtime.json"
    try:
        manifest = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise ValidationError(f"could not read CUDA runtime manifest {path}: {error}") from error
    if not isinstance(manifest, dict) or manifest.get("schema_version") != 1:
        raise ValidationError("CUDA runtime manifest must use schema_version 1")
    return manifest


def declared_files(manifest: dict[str, object]) -> list[dict[str, object]]:
    """Return validated file records."""
    files = manifest.get("files")
    if not isinstance(files, list) or not files:
        raise ValidationError("CUDA runtime manifest does not declare files")
    result: list[dict[str, object]] = []
    seen: set[str] = set()
    for record in files:
        if not isinstance(record, dict):
            raise ValidationError("CUDA runtime file record must be an object")
        path = record.get("path")
        if not isinstance(path, str) or not path.startswith(("lib/", "licenses/", "notices/")):
            raise ValidationError(f"invalid CUDA runtime file path: {path!r}")
        relative = Path(path)
        if relative.is_absolute() or ".." in relative.parts or path in seen:
            raise ValidationError(f"unsafe or duplicate CUDA runtime file path: {path}")
        seen.add(path)
        result.append(record)
    return result


def parse_needed(output: str) -> set[str]:
    """Parse DT_NEEDED entries from readelf output."""
    return set(NEEDED.findall(output))


def validate_files(root: Path, manifest: dict[str, object]) -> set[str]:
    """Verify exact inventory, hashes, sizes, and native dependency closure."""
    records = declared_files(manifest)
    expected = {str(record["path"]) for record in records}
    actual = {
        path.relative_to(root).as_posix()
        for directory in ("lib", "licenses", "notices")
        for path in (root / directory).rglob("*")
        if path.is_file()
    }
    if actual != expected:
        missing = sorted(expected - actual)
        unexpected = sorted(actual - expected)
        raise ValidationError(f"CUDA runtime inventory mismatch; missing={missing}, unexpected={unexpected}")

    for record in records:
        path = root / str(record["path"])
        if path.is_symlink() or not path.is_file():
            raise ValidationError(f"CUDA runtime entry must be a regular file: {record['path']}")
        actual_size = path.stat().st_size
        if actual_size != record.get("size"):
            raise ValidationError(
                f"size mismatch for {record['path']}: expected {record.get('size')}, got {actual_size}"
            )
        actual_hash = sha256_file(path)
        if actual_hash != record.get("sha256"):
            raise ValidationError(
                f"SHA-256 mismatch for {record['path']}: expected {record.get('sha256')}, got {actual_hash}"
            )

    libraries = {path.name for path in (root / "lib").iterdir() if path.is_file()}
    prohibited = manifest.get("prohibited_library_patterns")
    if not isinstance(prohibited, list):
        raise ValidationError("CUDA runtime manifest does not declare prohibited library patterns")
    for library in libraries:
        lowered = library.lower()
        if any(str(pattern).lower() in lowered for pattern in prohibited):
            raise ValidationError(f"prohibited CUDA runtime library is bundled: {library}")

    system = manifest.get("system_libraries")
    if not isinstance(system, list):
        raise ValidationError("CUDA runtime manifest does not declare system libraries")
    allowed = libraries | {str(library) for library in system}
    for library in sorted(libraries):
        path = root / "lib" / library
        result = subprocess.run(
            ["readelf", "-d", str(path)],
            check=False,
            capture_output=True,
            text=True,
        )
        if result.returncode != 0:
            raise ValidationError(f"readelf could not inspect {library}: {result.stderr.strip()}")
        unresolved = parse_needed(result.stdout) - allowed
        if unresolved:
            raise ValidationError(f"{library} has undeclared native dependencies: {sorted(unresolved)}")
    return libraries


def mapped_paths(pid: int) -> dict[str, set[Path]]:
    """Return file-backed mappings grouped by basename for a live process."""
    maps = Path(f"/proc/{pid}/maps")
    try:
        lines = maps.read_text(encoding="utf-8").splitlines()
    except OSError as error:
        raise ValidationError(f"could not read live process mappings {maps}: {error}") from error
    result: dict[str, set[Path]] = {}
    for line in lines:
        fields = line.split(maxsplit=5)
        if len(fields) != 6 or not fields[5].startswith("/"):
            continue
        path = Path(fields[5].removesuffix(" (deleted)"))
        result.setdefault(path.name, set()).add(path)
    return result


def is_accelerator_runtime_library(name: str) -> bool:
    """Identify user-space accelerator libraries that must remain artifact-owned."""
    if name.startswith("libcuda.so"):
        return False
    return name.startswith(
        (
            "libonnxruntime",
            "libcublas",
            "libcudart",
            "libcudnn",
            "libcufft",
            "libcurand",
            "libnvrtc",
            "libnvJitLink",
            "libnvinfer",
        )
    )


def validate_live_process(root: Path, manifest: dict[str, object], pid: int) -> None:
    """Prove every required runtime module was loaded from the artifact's private lib directory."""
    required = manifest.get("required_loaded_libraries")
    if not isinstance(required, list) or not required:
        raise ValidationError("CUDA runtime manifest does not declare required loaded libraries")
    private_lib = (root / "lib").resolve()
    mappings = mapped_paths(pid)
    for name in required:
        candidates = mappings.get(str(name), set())
        if not candidates:
            raise ValidationError(f"live process did not load required artifact library: {name}")
        outside = [path for path in candidates if path.resolve().parent != private_lib]
        if outside:
            raise ValidationError(f"live process loaded {name} outside the artifact: {outside}")

    driver_paths = {
        path
        for name, paths in mappings.items()
        if name.startswith("libcuda.so")
        for path in paths
    }
    if not driver_paths:
        raise ValidationError("live CUDA process did not load the host-owned NVIDIA driver library")
    for path in driver_paths:
        if path.resolve().parent == private_lib:
            raise ValidationError(f"system-owned NVIDIA driver library was bundled: {path}")

    for name, paths in mappings.items():
        if not is_accelerator_runtime_library(name):
            continue
        outside = [path for path in paths if path.resolve().parent != private_lib]
        if outside:
            raise ValidationError(f"live process loaded undeclared accelerator runtime {name} outside the artifact: {outside}")


def parser() -> argparse.ArgumentParser:
    """Construct the command-line parser."""
    result = argparse.ArgumentParser(description=__doc__)
    result.add_argument("root", type=Path, help="runtime root containing lib/ and manifest/")
    result.add_argument("--pid", type=int, help="live LocalHold PID to verify against the manifest")
    return result


def main() -> int:
    """Validate with concise errors."""
    try:
        args = parser().parse_args()
        root = args.root.resolve()
        manifest = load_manifest(root)
        libraries = validate_files(root, manifest)
        if args.pid is not None:
            validate_live_process(root, manifest, args.pid)
        print(f"validated {len(libraries)} CUDA runtime libraries from {root}")
    except (ValidationError, OSError) as error:
        print(f"error: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
