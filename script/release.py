#!/usr/bin/env python3
"""Validate LocalHold release metadata and extract release notes."""

from __future__ import annotations

import argparse
import datetime as dt
import json
import re
import subprocess
import sys
from pathlib import Path

try:
    from script.database_fixtures import SEMVER_TAG, FixtureError, validate_manifest
except ModuleNotFoundError:
    from database_fixtures import SEMVER_TAG, FixtureError, validate_manifest


REPO_ROOT = Path(__file__).resolve().parent.parent
PACKAGE_FILES = (
    "README.md",
    "CHANGELOG.md",
    "LICENSE",
    "NOTICE",
    "THIRD_PARTY_NOTICES.md",
    "localhold.example.toml",
)
TAG_TOKEN_CHARACTERS = "0-9A-Za-z.+-"


class ReleaseError(RuntimeError):
    """Actionable release validation error."""


def cargo_version() -> str:
    """Read the LocalHold package version through Cargo's metadata API."""
    result = subprocess.run(
        ["cargo", "metadata", "--locked", "--no-deps", "--format-version", "1"],
        cwd=REPO_ROOT,
        check=True,
        capture_output=True,
        text=True,
    )
    packages = [
        package
        for package in json.loads(result.stdout)["packages"]
        if package["name"] == "localhold"
    ]
    if len(packages) != 1:
        raise ReleaseError("Cargo metadata must contain exactly one localhold package")
    return str(packages[0]["version"])


def resolve_tag(tag: str | None) -> tuple[str, str]:
    """Return a validated tag and version, inferring the tag when omitted."""
    version = cargo_version()
    resolved = tag or f"v{version}"
    if not SEMVER_TAG.fullmatch(resolved):
        raise ReleaseError(f"release tag is not valid SemVer with a v prefix: {resolved}")
    if resolved != f"v{version}":
        raise ReleaseError(
            f"release tag {resolved} does not match Cargo package version {version}"
        )
    return resolved, version


def changelog_notes(version: str) -> str:
    """Extract a version's body from CHANGELOG.md."""
    lines = (REPO_ROOT / "CHANGELOG.md").read_text(encoding="utf-8").splitlines()
    heading = re.compile(
        rf"^## \[{re.escape(version)}\] - ([0-9]{{4}}-[0-9]{{2}}-[0-9]{{2}})$"
    )
    start: int | None = None
    for index, line in enumerate(lines):
        match = heading.fullmatch(line)
        if match is None:
            continue
        try:
            dt.date.fromisoformat(match.group(1))
        except ValueError as error:
            raise ReleaseError(f"invalid changelog release date: {match.group(1)}") from error
        start = index + 1
        break
    if start is None:
        raise ReleaseError(
            f"CHANGELOG.md is missing '## [{version}] - YYYY-MM-DD'"
        )

    end = next(
        (
            index
            for index in range(start, len(lines))
            if lines[index].startswith("## ")
            or re.match(r"^\[[^]]+\]:\s", lines[index]) is not None
        ),
        len(lines),
    )
    notes = "\n".join(lines[start:end]).strip()
    if not notes:
        raise ReleaseError(f"CHANGELOG.md has no release notes for {version}")
    return f"{notes}\n"


def references_tag(text: str, tag: str) -> bool:
    """Return whether text contains tag as a complete SemVer-like token."""
    pattern = rf"(?<![{TAG_TOKEN_CHARACTERS}]){re.escape(tag)}(?![{TAG_TOKEN_CHARACTERS}])"
    return re.search(pattern, text) is not None


def validate(tag: str | None) -> None:
    """Validate metadata shared by local and GitHub release workflows."""
    resolved, version = resolve_tag(tag)
    changelog_notes(version)
    validate_manifest(resolved)

    installation = (REPO_ROOT / "docs" / "installation.md").read_text(
        encoding="utf-8"
    )
    if not references_tag(installation, resolved):
        raise ReleaseError(
            f"docs/installation.md must reference the release tag {resolved}"
        )

    missing = [path for path in PACKAGE_FILES if not (REPO_ROOT / path).is_file()]
    if not (REPO_ROOT / "docs").is_dir():
        missing.append("docs/")
    if missing:
        raise ReleaseError(f"release package inputs are missing: {', '.join(missing)}")

    print(f"release metadata valid for {resolved}")


def parser() -> argparse.ArgumentParser:
    """Construct the release-tool command line parser."""
    result = argparse.ArgumentParser(description=__doc__)
    commands = result.add_subparsers(dest="command", required=True)

    commands.add_parser("tag", help="print the tag for the current Cargo version")
    validate_command = commands.add_parser("validate", help="validate release metadata")
    validate_command.add_argument("tag", nargs="?", help="release tag; defaults to Cargo version")
    notes_command = commands.add_parser("notes", help="extract release notes")
    notes_command.add_argument("tag", nargs="?", help="release tag; defaults to Cargo version")
    notes_command.add_argument("--output", type=Path, help="write notes to this path")
    return result


def main() -> int:
    """Run the selected release operation with concise errors."""
    args = parser().parse_args()
    try:
        if args.command == "tag":
            resolved, _version = resolve_tag(None)
            print(resolved)
        elif args.command == "validate":
            validate(args.tag)
        else:
            _tag, version = resolve_tag(args.tag)
            notes = changelog_notes(version)
            if args.output is None:
                sys.stdout.write(notes)
            else:
                args.output.parent.mkdir(parents=True, exist_ok=True)
                args.output.write_text(notes, encoding="utf-8")
    except (FixtureError, ReleaseError, subprocess.CalledProcessError, OSError, KeyError, json.JSONDecodeError) as error:
        print(f"error: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
