#!/usr/bin/env python3
"""Validate and index flpdf module-to-qpdf correspondence annotations."""

from __future__ import annotations

import argparse
from dataclasses import dataclass
from pathlib import Path
import re
import sys


MIRROR_START = "//! Mirrors qpdf "
MIRROR_VERSION = "11.9.0"
CORRESPONDENCE_PREFIX = "//! qpdf correspondence: "
QPDF_PATH_RE = re.compile(r"libqpdf/[A-Za-z0-9_+-]+\.cc")
RUST_WHITESPACE_CHARS = (
    "\u0009\u000a\u000b\u000c\u000d\u0020\u0085\u200e\u200f\u2028\u2029"
)
RUST_WHITESPACE = frozenset(RUST_WHITESPACE_CHARS)


@dataclass(frozen=True)
class Classification:
    kind: str
    text: str


@dataclass
class _InnerAttributeState:
    depth: int = 0
    quote: str | None = None
    escaped: bool = False
    raw_hashes: int | None = None
    block_comment_depth: int = 0


@dataclass
class _InnerAttributePrefixState:
    next_token: int = 0
    block_comment_depth: int = 0


def _character_literal_end(line: str, quote_offset: int) -> int | None:
    payload_offset = quote_offset + 1
    if payload_offset >= len(line):
        return None

    char = line[payload_offset]
    if char in {"'", "\n", "\r"}:
        return None
    if char != "\\":
        closing_offset = payload_offset + 1
    elif payload_offset + 1 >= len(line):
        return None
    elif line[payload_offset + 1] in {"n", "r", "t", "\\", "0", "'", '"'}:
        closing_offset = payload_offset + 2
    elif line[payload_offset + 1] == "x":
        digits = line[payload_offset + 2 : payload_offset + 4]
        if len(digits) != 2 or any(
            digit not in "0123456789abcdefABCDEF" for digit in digits
        ):
            return None
        closing_offset = payload_offset + 4
    elif line.startswith("\\u{", payload_offset):
        brace_offset = line.find("}", payload_offset + 3)
        if brace_offset < 0:
            return None
        digits = line[payload_offset + 3 : brace_offset].replace("_", "")
        if not 1 <= len(digits) <= 6 or any(
            digit not in "0123456789abcdefABCDEF" for digit in digits
        ):
            return None
        closing_offset = brace_offset + 1
    else:
        return None

    if closing_offset >= len(line) or line[closing_offset] != "'":
        return None
    return closing_offset + 1


def _advance_inner_attribute(state: _InnerAttributeState, line: str) -> str:
    offset = 0
    while offset < len(line):
        if state.block_comment_depth:
            if line.startswith("/*", offset):
                state.block_comment_depth += 1
                offset += 2
            elif line.startswith("*/", offset):
                state.block_comment_depth -= 1
                offset += 2
            else:
                offset += 1
            continue

        if state.raw_hashes is not None:
            delimiter = '"' + "#" * state.raw_hashes
            if line.startswith(delimiter, offset):
                state.raw_hashes = None
                offset += len(delimiter)
            else:
                offset += 1
            continue

        if state.quote is not None:
            char = line[offset]
            if state.escaped:
                state.escaped = False
            elif char == "\\":
                state.escaped = True
            elif char == state.quote:
                state.quote = None
            offset += 1
            continue

        if line.startswith("//", offset):
            break
        if line.startswith("/*", offset):
            state.block_comment_depth = 1
            offset += 2
            continue

        raw_prefix_length = 0
        if line.startswith("br", offset):
            raw_prefix_length = 2
        elif line.startswith("r", offset):
            raw_prefix_length = 1
        if raw_prefix_length:
            delimiter_offset = offset + raw_prefix_length
            while delimiter_offset < len(line) and line[delimiter_offset] == "#":
                delimiter_offset += 1
            if delimiter_offset < len(line) and line[delimiter_offset] == '"':
                state.raw_hashes = delimiter_offset - offset - raw_prefix_length
                offset = delimiter_offset + 1
                continue

        if line.startswith('b"', offset):
            state.quote = '"'
            offset += 2
            continue
        if line[offset] == '"':
            state.quote = '"'
            offset += 1
            continue

        quote_offset = offset + 1 if line.startswith("b'", offset) else offset
        if quote_offset < len(line) and line[quote_offset] == "'":
            literal_end = _character_literal_end(line, quote_offset)
            if literal_end is not None:
                offset = literal_end
                continue

        if line[offset] == "[":
            state.depth += 1
        elif line[offset] == "]":
            state.depth -= 1
            if state.depth == 0:
                return line[offset + 1 :]
        offset += 1

    if state.quote is not None:
        state.escaped = False
    return ""


def _consume_leading_block_comments(line: str, depth: int) -> tuple[str, int]:
    offset = 0
    while offset < len(line):
        if depth == 0:
            while offset < len(line) and line[offset] in RUST_WHITESPACE:
                offset += 1
            if not line.startswith("/*", offset):
                return line[offset:], depth
            depth = 1
            offset += 2
            continue

        if line.startswith("/*", offset):
            depth += 1
            offset += 2
        elif line.startswith("*/", offset):
            depth -= 1
            offset += 2
        else:
            offset += 1

    return "", depth


def _advance_inner_attribute_prefix(
    state: _InnerAttributePrefixState, line: str
) -> tuple[str | None, bool]:
    offset = 0
    tokens = "#!["
    while offset < len(line):
        if state.block_comment_depth:
            if line.startswith("/*", offset):
                state.block_comment_depth += 1
                offset += 2
            elif line.startswith("*/", offset):
                state.block_comment_depth -= 1
                offset += 2
            else:
                offset += 1
            continue

        if line[offset] in RUST_WHITESPACE:
            offset += 1
            continue
        if line.startswith("//", offset):
            return None, True
        if line.startswith("/*", offset):
            state.block_comment_depth = 1
            offset += 2
            continue

        token = tokens[state.next_token]
        if line[offset] != token:
            return None, False
        if token == "[":
            return line[offset:], True
        state.next_token += 1
        offset += 1

    return None, True


def _leading_comment_lines(source: str) -> list[str]:
    """Return line comments in the Rust trivia before the first item."""
    leading: list[str] = []
    block_comment_depth = 0
    inner_attribute_prefix: _InnerAttributePrefixState | None = None
    inner_attribute: _InnerAttributeState | None = None
    lines = source.removeprefix("\ufeff").split("\n")

    for line_number, line in enumerate(lines):
        pending = line
        while True:
            if inner_attribute is not None:
                pending = _advance_inner_attribute(inner_attribute, pending)
                if inner_attribute.depth != 0:
                    break
                inner_attribute = None

            if inner_attribute_prefix is not None:
                attribute_body, valid_prefix = _advance_inner_attribute_prefix(
                    inner_attribute_prefix, pending
                )
                if not valid_prefix:
                    if line_number == 0 and line.startswith("#!"):
                        inner_attribute_prefix = None
                        break
                    return leading
                if attribute_body is None:
                    break
                inner_attribute_prefix = None
                inner_attribute = _InnerAttributeState()
                pending = _advance_inner_attribute(inner_attribute, attribute_body)
                if inner_attribute.depth != 0:
                    break
                inner_attribute = None
                continue

            stripped, block_comment_depth = _consume_leading_block_comments(
                pending, block_comment_depth
            )
            if not stripped:
                break
            if stripped.startswith("//"):
                leading.append(stripped)
                break
            if stripped.startswith("#"):
                inner_attribute_prefix = _InnerAttributePrefixState()
                pending = stripped
                continue
            return leading

    return leading


def classify_source(path: Path, source: str) -> Classification:
    candidates = [
        line
        for line in _leading_comment_lines(source)
        if line.startswith(MIRROR_START) or line.startswith(CORRESPONDENCE_PREFIX)
    ]
    if not candidates:
        raise ValueError(f"{path}: missing qpdf correspondence classification")
    if len(candidates) > 1:
        raise ValueError(f"{path}: multiple qpdf correspondence classifications")

    line = candidates[0]
    if line.startswith(CORRESPONDENCE_PREFIX):
        reason = line[len(CORRESPONDENCE_PREFIX) :].strip(RUST_WHITESPACE_CHARS)
        if not reason.endswith("."):
            raise ValueError(f"{path}: classification must end with a terminal period")
        reason = reason[:-1].rstrip(RUST_WHITESPACE_CHARS)
        if not reason:
            raise ValueError(f"{path}: qpdf correspondence reason must be non-empty")
        return Classification("correspondence", reason)

    match = re.fullmatch(r"//! Mirrors qpdf (\S+) (.+)", line)
    if match is None:
        raise ValueError(f"{path}: invalid Mirrors qpdf classification")
    version, path_list = match.groups()
    if version != MIRROR_VERSION:
        raise ValueError(
            f"{path}: Mirrors qpdf version must be {MIRROR_VERSION}, got {version}"
        )

    path_list = path_list.strip(RUST_WHITESPACE_CHARS)
    if not path_list.endswith("."):
        raise ValueError(f"{path}: classification must end with a terminal period")
    path_list = path_list[:-1].rstrip(RUST_WHITESPACE_CHARS)
    qpdf_paths = [
        item.strip(RUST_WHITESPACE_CHARS) for item in path_list.split(",")
    ]
    if not qpdf_paths or any(not QPDF_PATH_RE.fullmatch(item) for item in qpdf_paths):
        raise ValueError(f"{path}: invalid qpdf path list: {path_list}")
    return Classification("mirror", ", ".join(qpdf_paths))


def scan_modules(
    source_root: Path, repo_root: Path
) -> list[tuple[Path, Classification]]:
    if not source_root.is_dir():
        raise ValueError(f"{source_root}: source root is not a directory")

    entries: list[tuple[Path, Classification]] = []
    errors: list[str] = []
    symlinked_directories = sorted(
        (
            path
            for path in source_root.rglob("*")
            if path.is_symlink() and path.is_dir()
        ),
        key=lambda item: item.relative_to(repo_root).as_posix(),
    )
    if symlinked_directories:
        raise ValueError(
            "\n".join(
                f"{path.relative_to(repo_root)}: symlinked directory is not allowed"
                for path in symlinked_directories
            )
        )

    source_paths = sorted(
        source_root.rglob("*.rs"),
        key=lambda item: item.relative_to(repo_root).as_posix(),
    )
    if not source_paths:
        raise ValueError(f"{source_root}: no Rust modules found")
    for source_path in source_paths:
        relative_path = source_path.relative_to(repo_root)
        relative_path_posix = relative_path.as_posix()
        if "\n" in relative_path_posix or "\r" in relative_path_posix:
            raise ValueError(
                f"{relative_path_posix!r}: line breaks are not allowed in module paths"
            )
        try:
            resolved_source_path = source_path.resolve(strict=True)
            _require_under_root(resolved_source_path, source_root, "source file")
            _require_under_root(resolved_source_path, repo_root, "source file")
            classification = classify_source(
                relative_path, resolved_source_path.read_bytes().decode("utf-8")
            )
        except (OSError, ValueError) as error:
            errors.append(str(error))
            continue
        entries.append((relative_path, classification))

    if errors:
        raise ValueError("\n".join(errors))
    return entries


def _escape_markdown_cell(value: str) -> str:
    return value.replace("\\", "\\\\").replace("|", "\\|").replace("`", "\\`")


def _render_markdown_code_span(value: str) -> str:
    table_safe_value = re.sub(
        r"(\\*)\|",
        lambda match: match.group(1) * 2 + r"\|",
        value,
    )
    longest_backtick_run = max(
        (len(match.group()) for match in re.finditer(r"`+", table_safe_value)),
        default=0,
    )
    delimiter = "`" * (longest_backtick_run + 1)
    padding = (
        " "
        if table_safe_value.startswith("`") or table_safe_value.endswith("`")
        else ""
    )
    return f"{delimiter}{padding}{table_safe_value}{padding}{delimiter}"


def render_index(entries: list[tuple[Path, Classification]]) -> str:
    lines = [
        "# qpdf Module Doc Index",
        "",
        "<!-- Generated by `python3 scripts/qpdf-module-docs.py --write`; do not edit. -->",
        "",
        "| flpdf module | classification | qpdf correspondence |",
        "|---|---|---|",
    ]
    for source_path, classification in entries:
        rendered_path = _render_markdown_code_span(source_path.as_posix())
        rendered_text = _escape_markdown_cell(classification.text)
        lines.append(
            f"| {rendered_path} | {classification.kind} | {rendered_text} |"
        )
    return "\n".join(lines) + "\n"


def _resolve_under_root(root: Path, value: Path) -> Path:
    return value if value.is_absolute() else root / value


def _require_under_root(path: Path, root: Path, label: str) -> None:
    try:
        path.relative_to(root)
    except ValueError as error:
        raise ValueError(f"{label} {path} is outside --root {root}") from error


def _reject_symlinked_path_components(path: Path, root: Path, label: str) -> None:
    try:
        relative_path = path.relative_to(root)
    except ValueError:
        return

    current_path = root
    for component in relative_path.parts:
        current_path /= component
        if current_path.is_symlink():
            raise ValueError(f"{label} {current_path} is a symlink")


def _parse_args(argv: list[str] | None) -> argparse.Namespace:
    default_root = Path(__file__).resolve().parent.parent
    parser = argparse.ArgumentParser(
        description="Validate and index flpdf qpdf module doc annotations"
    )
    parser.add_argument("--root", type=Path, default=default_root)
    parser.add_argument(
        "--source-root", type=Path, default=Path("crates/flpdf/src")
    )
    parser.add_argument(
        "--index", type=Path, default=Path("docs/qpdf-module-doc-index.md")
    )
    mode = parser.add_mutually_exclusive_group(required=True)
    mode.add_argument("--write", action="store_true")
    mode.add_argument("--check", action="store_true")
    return parser.parse_args(argv)


def main(argv: list[str] | None = None) -> int:
    args = _parse_args(argv)
    root = args.root.resolve()
    source_root = _resolve_under_root(root, args.source_root).resolve()
    unresolved_index_path = _resolve_under_root(root, args.index)
    index_path = unresolved_index_path.resolve()

    try:
        _require_under_root(source_root, root, "source root")
        _require_under_root(index_path, root, "index")
        _reject_symlinked_path_components(unresolved_index_path, root, "index")
        entries = scan_modules(source_root, root)
    except (OSError, ValueError) as error:
        print(error, file=sys.stderr)
        return 1
    expected = render_index(entries).encode("utf-8")

    if args.write:
        try:
            index_path.parent.mkdir(parents=True, exist_ok=True)
            index_path.write_bytes(expected)
        except OSError as error:
            print(error, file=sys.stderr)
            return 1
        return 0

    try:
        actual = index_path.read_bytes()
    except FileNotFoundError:
        print(
            f"{index_path}: generated index is missing; "
            "run `python3 scripts/qpdf-module-docs.py --write`",
            file=sys.stderr,
        )
        return 1
    except OSError as error:
        print(error, file=sys.stderr)
        return 1
    if actual != expected:
        print(
            f"{index_path}: generated index is stale; "
            "run `python3 scripts/qpdf-module-docs.py --write`",
            file=sys.stderr,
        )
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
