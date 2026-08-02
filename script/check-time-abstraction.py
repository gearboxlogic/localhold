#!/usr/bin/env python3
"""Reject production Rust code that bypasses LocalHold's clock abstraction."""

from __future__ import annotations

import os
import re
import sys
from dataclasses import dataclass
from pathlib import Path


TIME_ACCESS = re.compile(
    r"(?:chrono::)?Utc::now\("
    r"|SystemTime::now\("
    r"|(?:tokio::time::|std::time::)?Instant::now\("
    r"|tokio::time::(?:sleep|sleep_until|interval|timeout)\("
    r"|(?:std::)?thread::sleep\("
)
TEST_MODULE_PREFIX = "modtests{"
EXEMPT_SOURCES = {Path("src/clock.rs"), Path("src/config/tests.rs")}


@dataclass(frozen=True)
class LexedSource:
    lines: list[str]
    error: str | None


def _replace_with_spaces(output: list[str], source: str, start: int, end: int) -> None:
    for index in range(start, end):
        if source[index] != "\n":
            output[index] = " "


def _char_literal_end(source: str, start: int) -> int | None:
    quote = start
    if source[start : start + 2] in {"b'", "c'"}:
        quote += 1
    if quote >= len(source) or source[quote] != "'" or quote + 1 >= len(source):
        return None

    content = quote + 1
    if source[content] != "\\":
        closing = content + 1
    elif source.startswith("\\u{", content):
        closing = source.find("}", content + 3)
        if closing < 0:
            return None
        closing += 1
    elif source.startswith("\\x", content):
        closing = content + 4
    else:
        closing = content + 2

    if closing < len(source) and source[closing] == "'":
        return closing + 1
    return None


def lex_rust(source: str) -> LexedSource:
    """Mask comments and literals in one linear pass while retaining line layout."""
    output = list(source)
    index = 0
    block_comment_depth = 0
    normal_string = False
    raw_hashes: str | None = None

    while index < len(source):
        if block_comment_depth:
            if source.startswith("/*", index):
                _replace_with_spaces(output, source, index, index + 2)
                block_comment_depth += 1
                index += 2
            elif source.startswith("*/", index):
                _replace_with_spaces(output, source, index, index + 2)
                block_comment_depth -= 1
                index += 2
            else:
                _replace_with_spaces(output, source, index, index + 1)
                index += 1
            continue

        if raw_hashes is not None:
            closing = f'"{raw_hashes}'
            if source.startswith(closing, index):
                _replace_with_spaces(output, source, index, index + len(closing))
                raw_hashes = None
                index += len(closing)
            else:
                _replace_with_spaces(output, source, index, index + 1)
                index += 1
            continue

        if normal_string:
            if source[index] == "\\" and index + 1 < len(source):
                _replace_with_spaces(output, source, index, index + 2)
                index += 2
            else:
                if source[index] == '"':
                    normal_string = False
                _replace_with_spaces(output, source, index, index + 1)
                index += 1
            continue

        if source.startswith("//", index):
            line_end = source.find("\n", index)
            if line_end < 0:
                line_end = len(source)
            _replace_with_spaces(output, source, index, line_end)
            index = line_end
            continue
        if source.startswith("/*", index):
            _replace_with_spaces(output, source, index, index + 2)
            block_comment_depth = 1
            index += 2
            continue

        raw_start = index
        if source[index : index + 2] in {"br", "cr"}:
            raw_start += 1
        if source[raw_start : raw_start + 1] == "r":
            cursor = raw_start + 1
            while source[cursor : cursor + 1] == "#":
                cursor += 1
            if source[cursor : cursor + 1] == '"':
                raw_hashes = source[raw_start + 1 : cursor]
                _replace_with_spaces(output, source, index, cursor + 1)
                index = cursor + 1
                continue

        if source[index] == '"' or source[index : index + 2] in {'b"', 'c"'}:
            end = index + (2 if source[index] in {"b", "c"} else 1)
            _replace_with_spaces(output, source, index, end)
            normal_string = True
            index = end
            continue

        char_end = _char_literal_end(source, index)
        if char_end is not None:
            _replace_with_spaces(output, source, index, char_end)
            index = char_end
            continue
        index += 1

    error = None
    if block_comment_depth:
        error = "unterminated block comment"
    elif raw_hashes is not None:
        error = "unterminated raw string"
    elif normal_string:
        error = "unterminated string"
    return LexedSource("".join(output).splitlines(), error)


def _compact(text: str) -> str:
    return "".join(text.split())


def _test_only_cfg_attribute(attribute: str) -> bool:
    compact = _compact(attribute)
    if compact == "#[cfg(test)]":
        return True
    matched = re.fullmatch(r"#\[cfg\(all\((.*)\)\)\]", compact, re.DOTALL)
    if matched is None:
        return False
    predicates = matched.group(1)
    return "any(" not in predicates and "not(" not in predicates and re.search(r"(?:^|,)test(?:,|$)", predicates) is not None


def _consume_test_module(line: str, start: int, depth: int) -> tuple[int, str | None]:
    """Consume a test-module segment and return its depth and any suffix."""
    for index in range(start, len(line)):
        if line[index] == "{":
            depth += 1
        elif line[index] == "}":
            depth -= 1
            if depth == 0:
                return 0, line[index + 1 :]
    return depth, None


def _rust_sources(root: Path) -> list[Path]:
    sources: list[Path] = []
    for directory, names, filenames in os.walk(root, followlinks=False):
        base = Path(directory)
        for name in names:
            candidate = base / name
            if candidate.is_symlink() and candidate.suffix == ".rs":
                sources.append(candidate)
        for name in filenames:
            if name.endswith(".rs"):
                sources.append(base / name)
    return sorted(sources, key=lambda path: path.as_posix())


def _enter_test_module(line: str) -> tuple[int, str | None]:
    opening = line.find("{")
    if opening < 0:
        raise ValueError("test module prefix did not contain an opening brace")
    return _consume_test_module(line, opening, 0)


def scan_source(path: Path) -> tuple[list[tuple[int, str]], str | None]:
    try:
        source = path.read_text(encoding="utf-8")
    except (OSError, UnicodeError) as error:
        return [], f"could not read Rust source: {error}"

    lexed = lex_rust(source)
    if lexed.error is not None:
        return [], lexed.error
    original_lines = source.splitlines()

    matches: list[tuple[int, str]] = []
    in_tests = False
    test_depth = 0
    pending_test_cfg = False
    pending_cfg_attribute: str | None = None
    pending_test_module: str | None = None

    for line_number, (original, masked) in enumerate(zip(original_lines, lexed.lines, strict=True), start=1):
        if in_tests:
            test_depth, suffix = _consume_test_module(masked, 0, test_depth)
            if suffix is None:
                continue
            in_tests = False
            masked = suffix

        compact = _compact(masked)
        opens_test_module = False
        if pending_test_module is not None:
            if compact:
                pending_test_module += compact
            if pending_test_module.startswith(TEST_MODULE_PREFIX):
                opens_test_module = True
            elif TEST_MODULE_PREFIX.startswith(pending_test_module):
                continue
            else:
                pending_test_cfg = False
                pending_test_module = None
        elif pending_cfg_attribute is not None:
            pending_cfg_attribute = f"{pending_cfg_attribute} {masked}"
            if ")]" in compact:
                pending_test_cfg = _test_only_cfg_attribute(pending_cfg_attribute)
                pending_cfg_attribute = None
        elif compact.startswith("#[cfg(") and ")]" not in compact:
            pending_cfg_attribute = masked
            pending_test_cfg = False
        elif _test_only_cfg_attribute(masked):
            pending_test_cfg = True
        elif pending_test_cfg and compact and (TEST_MODULE_PREFIX.startswith(compact) or compact.startswith(TEST_MODULE_PREFIX)):
            pending_test_module = compact
            opens_test_module = compact.startswith(TEST_MODULE_PREFIX)
        elif compact and re.fullmatch(r"#\[.*\]", compact) is None:
            pending_test_cfg = False

        if opens_test_module:
            test_depth, suffix = _enter_test_module(masked)
            pending_test_cfg = False
            pending_test_module = None
            if suffix is None:
                in_tests = True
                continue
            masked = suffix

        if TIME_ACCESS.search(masked):
            matches.append((line_number, original))

    if pending_cfg_attribute is not None:
        return matches, "unterminated cfg attribute"
    if pending_test_module is not None:
        return matches, "unterminated inline test-module declaration"
    if in_tests:
        return matches, "could not identify the inline test-module boundary"
    return matches, None


def main() -> int:
    sources = _rust_sources(Path("src"))
    if not sources:
        print("time abstraction check found no Rust sources", file=sys.stderr)
        return 1

    failed = False
    for path in sources:
        if path in EXEMPT_SOURCES:
            continue
        if path.is_symlink() or not path.is_file():
            print(f"time abstraction check requires a regular non-symlink Rust source: {path}", file=sys.stderr)
            failed = True
            continue

        matches, error = scan_source(path)
        if error is not None:
            print(f"time abstraction check {error} in {path}", file=sys.stderr)
            failed = True
            continue
        if matches:
            print(f"direct time access bypasses Clock in {path}:", file=sys.stderr)
            for line_number, line in matches:
                print(f"{line_number}:{line}", file=sys.stderr)
            failed = True

    if failed:
        print("route runtime clocks, sleeps, and deadlines through src/clock.rs", file=sys.stderr)
        return 1
    print("time abstraction check passed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
