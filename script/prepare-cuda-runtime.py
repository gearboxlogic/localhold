#!/usr/bin/env python3
"""Materialize LocalHold's checksum-pinned CUDA runtime without installing Python packages."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import shutil
import sys
import tarfile
import urllib.error
import urllib.request
import zipfile
from pathlib import Path, PurePosixPath
from typing import BinaryIO


REPO_ROOT = Path(__file__).resolve().parent.parent
DEFAULT_SPEC = REPO_ROOT / "release" / "cuda-linux-x86_64.json"
CHUNK_SIZE = 1024 * 1024


class CudaRuntimeError(RuntimeError):
    """Actionable CUDA runtime materialization error."""


def sha256_file(path: Path) -> str:
    """Return a file's SHA-256 without loading it into memory."""
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(CHUNK_SIZE), b""):
            digest.update(chunk)
    return digest.hexdigest()


def validate_relative_path(value: str, field: str) -> PurePosixPath:
    """Reject absolute and traversal paths from a release specification."""
    path = PurePosixPath(value)
    if not value or path.is_absolute() or ".." in path.parts or "." in path.parts:
        raise CudaRuntimeError(f"{field} must be a normalized relative path: {value!r}")
    return path


def load_spec(path: Path) -> dict[str, object]:
    """Load and structurally validate a CUDA release specification."""
    try:
        spec = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise CudaRuntimeError(f"could not read CUDA release specification {path}: {error}") from error
    if not isinstance(spec, dict) or spec.get("schema_version") != 1:
        raise CudaRuntimeError("CUDA release specification must use schema_version 1")
    artifact = spec.get("artifact")
    if not isinstance(artifact, dict) or artifact.get("target") != "x86_64-unknown-linux-gnu-cuda12":
        raise CudaRuntimeError("CUDA release specification must declare the CUDA 12 Linux x86_64 target")
    if not isinstance(spec.get("compatibility"), dict):
        raise CudaRuntimeError("CUDA release specification must declare compatibility metadata")
    for field in ("system_libraries", "required_loaded_libraries", "prohibited_library_patterns"):
        values = spec.get(field)
        if not isinstance(values, list) or not values or not all(isinstance(value, str) and value for value in values):
            raise CudaRuntimeError(f"CUDA release specification must declare non-empty string {field}")
    sources = spec.get("sources")
    if not isinstance(sources, list) or not sources:
        raise CudaRuntimeError("CUDA release specification must contain sources")

    destinations: set[str] = set()
    filenames: set[str] = set()
    for source in sources:
        if not isinstance(source, dict):
            raise CudaRuntimeError("each CUDA source must be an object")
        for field in ("id", "version", "filename", "url", "sha256", "archive", "files"):
            if not source.get(field):
                raise CudaRuntimeError(f"CUDA source is missing {field}")
        if not str(source["url"]).startswith("https://"):
            raise CudaRuntimeError(f"CUDA source URL must use HTTPS: {source['url']}")
        filename = str(validate_relative_path(str(source["filename"]), "filename"))
        if "/" in filename or filename in filenames:
            raise CudaRuntimeError(f"source filename must be unique and unqualified: {filename}")
        filenames.add(filename)
        if source["archive"] not in ("tar.gz", "zip"):
            raise CudaRuntimeError(f"unsupported archive type for {source['id']}: {source['archive']}")
        sha256 = str(source["sha256"])
        if len(sha256) != 64 or any(character not in "0123456789abcdef" for character in sha256):
            raise CudaRuntimeError(f"invalid SHA-256 for {source['id']}")
        if not isinstance(source["files"], list) or not source["files"]:
            raise CudaRuntimeError(f"CUDA source {source['id']} must declare extracted files")
        for mapping in source["files"]:
            if not isinstance(mapping, dict) or set(mapping) != {"source", "destination"}:
                raise CudaRuntimeError(f"invalid extracted-file mapping for {source['id']}")
            validate_relative_path(str(mapping["source"]), "source member")
            destination = str(validate_relative_path(str(mapping["destination"]), "destination"))
            if not destination.startswith(("lib/", "licenses/", "notices/")):
                raise CudaRuntimeError(f"destination is outside the runtime layout: {destination}")
            if destination in destinations:
                raise CudaRuntimeError(f"duplicate CUDA runtime destination: {destination}")
            destinations.add(destination)
    return spec


def download(source: dict[str, object], cache_dir: Path, offline: bool) -> Path:
    """Return a verified cached source, downloading atomically when allowed."""
    destination = cache_dir / str(source["filename"])
    expected = str(source["sha256"])
    if destination.is_file() and sha256_file(destination) == expected:
        return destination
    if destination.exists():
        destination.unlink()
    if offline:
        raise CudaRuntimeError(f"verified cached source is unavailable in offline mode: {destination.name}")

    cache_dir.mkdir(parents=True, exist_ok=True)
    temporary = destination.with_suffix(destination.suffix + ".partial")
    temporary.unlink(missing_ok=True)
    try:
        with urllib.request.urlopen(str(source["url"])) as response, temporary.open("wb") as output:
            shutil.copyfileobj(response, output, length=CHUNK_SIZE)
        actual = sha256_file(temporary)
        if actual != expected:
            raise CudaRuntimeError(
                f"SHA-256 mismatch for {destination.name}: expected {expected}, got {actual}"
            )
        os.replace(temporary, destination)
    except (OSError, urllib.error.URLError) as error:
        raise CudaRuntimeError(f"could not download {source['url']}: {error}") from error
    finally:
        temporary.unlink(missing_ok=True)
    return destination


def archive_member(archive: Path, archive_type: str, member: str) -> BinaryIO:
    """Open one declared regular file from a tarball or ZIP archive."""
    if archive_type == "tar.gz":
        container = tarfile.open(archive, "r:gz")
        try:
            info = container.getmember(member)
            if not info.isfile():
                raise CudaRuntimeError(f"declared source member is not a regular file: {member}")
            extracted = container.extractfile(info)
            if extracted is None:
                raise CudaRuntimeError(f"could not read declared source member: {member}")
            return _ClosingReader(extracted, container)
        except (KeyError, tarfile.TarError) as error:
            container.close()
            raise CudaRuntimeError(f"missing declared source member {member} in {archive.name}") from error

    container = zipfile.ZipFile(archive)
    try:
        info = container.getinfo(member)
        if info.is_dir():
            raise CudaRuntimeError(f"declared source member is not a regular file: {member}")
        return _ClosingReader(container.open(info), container)
    except (KeyError, zipfile.BadZipFile) as error:
        container.close()
        raise CudaRuntimeError(f"missing declared source member {member} in {archive.name}") from error


class _ClosingReader:
    """Close an archive after its member stream."""

    def __init__(self, reader: BinaryIO, container: object) -> None:
        self.reader = reader
        self.container = container

    def __enter__(self) -> BinaryIO:
        return self.reader

    def __exit__(self, *args: object) -> None:
        self.reader.close()
        close = getattr(self.container, "close")
        close()


def materialize(spec: dict[str, object], cache_dir: Path, output_dir: Path, offline: bool) -> Path:
    """Create a complete runtime tree and its resolved per-file manifest."""
    if output_dir.exists() and any(output_dir.iterdir()):
        raise CudaRuntimeError(f"output directory must be empty: {output_dir}")
    output_dir.mkdir(parents=True, exist_ok=True)
    resolved: list[dict[str, object]] = []

    sources = spec["sources"]
    assert isinstance(sources, list)
    for raw_source in sources:
        assert isinstance(raw_source, dict)
        source_archive = download(raw_source, cache_dir, offline)
        mappings = raw_source["files"]
        assert isinstance(mappings, list)
        for mapping in mappings:
            assert isinstance(mapping, dict)
            relative = Path(str(mapping["destination"]))
            destination = output_dir / relative
            destination.parent.mkdir(parents=True, exist_ok=True)
            with archive_member(source_archive, str(raw_source["archive"]), str(mapping["source"])) as reader:
                with destination.open("wb") as writer:
                    shutil.copyfileobj(reader, writer, length=CHUNK_SIZE)
            destination.chmod(0o644)
            resolved.append(
                {
                    "path": relative.as_posix(),
                    "sha256": sha256_file(destination),
                    "size": destination.stat().st_size,
                    "source": raw_source["id"],
                    "source_version": raw_source["version"],
                }
            )

    manifest_dir = output_dir / "manifest"
    manifest_dir.mkdir()
    manifest = {
        "schema_version": spec["schema_version"],
        "artifact": spec["artifact"],
        "compatibility": spec["compatibility"],
        "system_libraries": spec["system_libraries"],
        "required_loaded_libraries": spec["required_loaded_libraries"],
        "prohibited_library_patterns": spec["prohibited_library_patterns"],
        "inputs": [
            {
                "id": source["id"],
                "version": source["version"],
                "filename": source["filename"],
                "url": source["url"],
                "sha256": source["sha256"],
            }
            for source in sources
        ],
        "files": sorted(resolved, key=lambda item: str(item["path"])),
    }
    manifest_path = manifest_dir / "cuda-runtime.json"
    manifest_path.write_text(json.dumps(manifest, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    manifest_path.chmod(0o644)
    return manifest_path


def parser() -> argparse.ArgumentParser:
    """Construct the command-line parser."""
    result = argparse.ArgumentParser(description=__doc__)
    result.add_argument("--spec", type=Path, default=DEFAULT_SPEC)
    result.add_argument("--cache-dir", required=True, type=Path)
    result.add_argument("--output-dir", required=True, type=Path)
    result.add_argument("--offline", action="store_true")
    return result


def main() -> int:
    """Materialize the configured runtime with concise errors."""
    try:
        args = parser().parse_args()
        spec = load_spec(args.spec)
        manifest = materialize(spec, args.cache_dir, args.output_dir, args.offline)
        print(manifest)
    except (CudaRuntimeError, OSError, tarfile.TarError, zipfile.BadZipFile) as error:
        print(f"error: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
