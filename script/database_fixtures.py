#!/usr/bin/env python3
"""Validate provenance and checksums for public database upgrade fixtures."""

from __future__ import annotations

import hashlib
import json
import re
import subprocess
from pathlib import Path
from typing import Any


REPO_ROOT = Path(__file__).resolve().parent.parent
FIXTURE_ROOT = REPO_ROOT / "tests" / "fixtures" / "database-upgrades"
MANIFEST_PATH = FIXTURE_ROOT / "manifest.json"
SHA256 = re.compile(r"^[0-9a-f]{64}$")
COMMIT = re.compile(r"^[0-9a-f]{40}$")
INCLUDE = re.compile(r"^\s*-- fixture-include: (\S+)\s*$")

# Trusted offline inventory of public GitHub Releases. Keep this independent of
# the mutable fixture manifest so deleting an old fixture cannot validate.
PUBLISHED_DATABASE_RELEASES = frozenset({"v0.1.0-beta.2", "v0.1.0-beta.3", "v0.2.0"})


class FixtureError(RuntimeError):
    """An upgrade fixture is absent, unverified, or has invalid provenance."""


def sha256(data: bytes) -> str:
    """Return a lowercase SHA-256 digest."""
    return hashlib.sha256(data).hexdigest()


def expand_fixture(path: Path, fixture_root: Path, stack: tuple[str, ...] = ()) -> bytes:
    """Expand safe, local SQL includes into the effective fixture bytes."""
    try:
        relative = path.relative_to(fixture_root)
    except ValueError as error:
        raise FixtureError(f"fixture include escapes the fixture directory: {path}") from error
    name = relative.as_posix()
    if relative.name != name:
        raise FixtureError(f"fixture include must be a basename: {name}")
    if name in stack:
        raise FixtureError(f"fixture include cycle: {' -> '.join((*stack, name))}")
    try:
        source = path.read_text(encoding="utf-8")
    except (OSError, UnicodeDecodeError) as error:
        raise FixtureError(f"cannot read fixture {path}: {error}") from error
    expanded = bytearray()
    for line in source.splitlines(keepends=True):
        match = INCLUDE.fullmatch(line.rstrip("\r\n"))
        if match is None:
            expanded.extend(line.encode("utf-8"))
            continue
        include_name = match.group(1)
        if Path(include_name).name != include_name:
            raise FixtureError(f"fixture include must be a basename: {include_name}")
        expanded.extend(expand_fixture(fixture_root / include_name, fixture_root, (*stack, name)))
    return bytes(expanded)


def _read_json(path: Path) -> dict[str, Any]:
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise FixtureError(f"cannot read fixture manifest {path}: {error}") from error
    if not isinstance(value, dict):
        raise FixtureError("fixture manifest root must be an object")
    return value


def _git_bytes(ref: str, source: str, repo_root: Path) -> bytes | None:
    result = subprocess.run(
        ["git", "show", f"{ref}:{source}"],
        cwd=repo_root,
        capture_output=True,
        check=False,
    )
    if result.returncode == 0:
        return result.stdout
    return None


def _git_commit(ref: str, repo_root: Path) -> str | None:
    result = subprocess.run(
        ["git", "rev-parse", "--verify", f"{ref}^{{commit}}"],
        cwd=repo_root,
        capture_output=True,
        check=False,
        text=True,
    )
    return result.stdout.strip() if result.returncode == 0 else None


def _git_is_ancestor(ancestor: str, descendant: str, repo_root: Path) -> bool:
    result = subprocess.run(
        ["git", "merge-base", "--is-ancestor", ancestor, descendant],
        cwd=repo_root,
        capture_output=True,
        check=False,
    )
    return result.returncode == 0


def _validate_backend(
    tag: str,
    backend: str,
    value: Any,
    repo_root: Path,
    fixture_root: Path,
    declared_commit: str,
    source_ref: str,
) -> None:
    if not isinstance(value, dict):
        raise FixtureError(f"{tag} {backend} fixture entry must be an object")
    required = {"fixture", "fixture_sha256", "source", "source_sha256"}
    missing = sorted(required.difference(value))
    if missing:
        raise FixtureError(f"{tag} {backend} fixture entry is missing: {', '.join(missing)}")
    fixture_name = value["fixture"]
    if not isinstance(fixture_name, str) or Path(fixture_name).name != fixture_name:
        raise FixtureError(f"{tag} {backend} fixture must be a file name within the fixture directory")
    fixture_path = fixture_root / fixture_name
    try:
        fixture_bytes = expand_fixture(fixture_path, fixture_root)
    except OSError as error:
        raise FixtureError(f"cannot read {tag} {backend} fixture {fixture_path}: {error}") from error
    expected_fixture = value["fixture_sha256"]
    if not isinstance(expected_fixture, str) or not SHA256.fullmatch(expected_fixture):
        raise FixtureError(f"{tag} {backend} fixture_sha256 must be a lowercase SHA-256 digest")
    actual_fixture = sha256(fixture_bytes)
    if actual_fixture != expected_fixture:
        raise FixtureError(f"{tag} {backend} fixture checksum mismatch: expected {expected_fixture}, got {actual_fixture}")
    sql = fixture_bytes.decode("utf-8")
    if "CREATE TABLE" not in sql and "fixture-include:" not in sql:
        raise FixtureError(f"{tag} {backend} fixture does not build a schema")
    if "INSERT INTO" not in sql and "fixture-include:" not in sql:
        raise FixtureError(f"{tag} {backend} fixture does not contain survival data")

    source = value["source"]
    expected_source = value["source_sha256"]
    if not isinstance(source, str) or Path(source).is_absolute() or ".." in Path(source).parts:
        raise FixtureError(f"{tag} {backend} source path is invalid")
    if not isinstance(expected_source, str) or not SHA256.fullmatch(expected_source):
        raise FixtureError(f"{tag} {backend} source_sha256 must be a lowercase SHA-256 digest")
    declared_source_bytes = _git_bytes(declared_commit, source, repo_root)
    if declared_source_bytes is None:
        raise FixtureError(
            f"cannot verify {tag} {backend} source {source}: declared provenance commit source is unavailable"
        )
    actual_declared_source = sha256(declared_source_bytes)
    if actual_declared_source != expected_source:
        raise FixtureError(
            f"{tag} {backend} declared provenance commit source checksum mismatch: "
            f"expected {expected_source}, got {actual_declared_source}"
        )

    release_source_bytes = _git_bytes(source_ref, source, repo_root)
    if release_source_bytes is None:
        raise FixtureError(f"cannot verify {tag} {backend} source {source}: release source object is unavailable")
    actual_release_source = sha256(release_source_bytes)
    if actual_release_source != expected_source:
        raise FixtureError(
            f"{tag} {backend} release source checksum mismatch: "
            f"expected {expected_source}, got {actual_release_source}"
        )


def validate_manifest(release_tag: str | None = None, *, repo_root: Path = REPO_ROOT, manifest_path: Path = MANIFEST_PATH) -> None:
    """Validate every fixture and require coverage for ``release_tag`` when set."""
    manifest = _read_json(manifest_path)
    if manifest.get("format_version") != 1:
        raise FixtureError("fixture manifest format_version must be 1")
    if not isinstance(manifest.get("retention"), str) or not manifest["retention"].strip():
        raise FixtureError("fixture manifest must define a non-empty retention policy")
    releases = manifest.get("releases")
    if not isinstance(releases, list) or not releases:
        raise FixtureError("fixture manifest releases must be a non-empty array")
    fixture_root = manifest_path.parent
    tags: set[str] = set()
    for release in releases:
        if not isinstance(release, dict) or not isinstance(release.get("tag"), str):
            raise FixtureError("every fixture release must have a string tag")
        tag = release["tag"]
        if tag in tags:
            raise FixtureError(f"duplicate fixture release: {tag}")
        tags.add(tag)

    if release_tag is not None and release_tag not in tags:
        raise FixtureError(f"database upgrade fixtures are missing for release {release_tag}")

    missing_published = sorted(PUBLISHED_DATABASE_RELEASES.difference(tags))
    unexpected = sorted(tags.difference(PUBLISHED_DATABASE_RELEASES))
    if missing_published or unexpected:
        details: list[str] = []
        if missing_published:
            details.append(f"missing published releases: {', '.join(missing_published)}")
        if unexpected:
            details.append(f"outside trusted published inventory: {', '.join(unexpected)}")
        raise FixtureError(f"database upgrade fixture inventory mismatch: {'; '.join(details)}")

    for release in releases:
        tag = release["tag"]
        commit = release.get("commit")
        if not isinstance(commit, str) or not COMMIT.fullmatch(commit):
            raise FixtureError(f"{tag} commit must be a lowercase 40-character Git object ID")
        resolved_commit = _git_commit(tag, repo_root)
        if resolved_commit is None:
            if tag != release_tag:
                raise FixtureError(f"cannot verify {tag} fixture provenance: historical tag is unavailable")
            head_commit = _git_commit("HEAD", repo_root)
            if head_commit is None:
                raise FixtureError(f"cannot verify {tag} fixture provenance: HEAD is unavailable")
            if not _git_is_ancestor(commit, head_commit, repo_root):
                raise FixtureError(f"{tag} fixture provenance commit {commit} is not an ancestor of HEAD")
            source_ref = head_commit
        else:
            if not _git_is_ancestor(commit, resolved_commit, repo_root):
                raise FixtureError(
                    f"{tag} fixture provenance commit {commit} is not an ancestor of tag commit {resolved_commit}"
                )
            source_ref = tag
        _validate_backend(tag, "sqlite", release.get("sqlite"), repo_root, fixture_root, commit, source_ref)
        _validate_backend(tag, "postgres", release.get("postgres"), repo_root, fixture_root, commit, source_ref)


if __name__ == "__main__":
    validate_manifest()
    print("database upgrade fixtures valid")
