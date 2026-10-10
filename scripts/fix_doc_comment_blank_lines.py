#!/usr/bin/env python3
"""Remove blank lines between rustdoc comments and documented items."""

from __future__ import annotations

import re
import sys
from pathlib import Path

DOC_PREFIXES = ("///", "//!")

ITEM_RE = re.compile(
    r"^(?:"
    r"(?:pub\s+(?:\(crate\)\s+)?)?"
    r"(?:async\s+|unsafe\s+|const\s+)?fn\s"
    r"|(?:pub\s+(?:\(crate\)\s+)?)?struct\s"
    r"|(?:pub\s+(?:\(crate\)\s+)?)?enum\s"
    r"|(?:pub\s+(?:\(crate\)\s+)?)?union\s"
    r"|(?:pub\s+(?:\(crate\)\s+)?)?type\s"
    r"|(?:pub\s+(?:\(crate\)\s+)?)?use\s"
    r"|(?:pub\s+(?:\(crate\)\s+)?)?const\s"
    r"|(?:pub\s+(?:\(crate\)\s+)?)?static\s"
    r"|(?:pub\s+(?:\(crate\)\s+)?)?mod\s"
    r"|(?:pub\s+(?:\(crate\)\s+)?)?trait\s"
    r"|(?:pub\s+(?:\(crate\)\s+)?)?impl\s"
    r"|macro_rules!\s"
    r"|extern\s+"
    r")"
)


def is_blank(line: str) -> bool:
    return line.strip() == ""


def is_doc_line(line: str) -> bool:
    stripped = line.lstrip()
    return stripped.startswith(DOC_PREFIXES)


def follows_doc_block(line: str) -> bool:
    """True if this line may immediately follow a doc comment (no blank line)."""
    if is_doc_line(line):
        return True
    stripped = line.lstrip()
    if stripped.startswith("#["):
        return True
    if stripped.startswith("#![") and not stripped.startswith("#![allow"):
        # Inner/crate attributes on the documented item.
        return True
    return ITEM_RE.match(stripped) is not None


def fix_content(text: str) -> str:
    lines = text.splitlines(keepends=True)
    if not lines and text:
        lines = [text]
    if text and not text.endswith(("\n", "\r")):
        # splitlines(keepends=True) drops trailing newline on last line; restore later.
        trailing_newline = False
    else:
        trailing_newline = text.endswith("\n")

    out: list[str] = []
    i = 0
    while i < len(lines):
        line = lines[i]
        if is_blank(line) and out:
            j = len(out) - 1
            while j >= 0 and is_blank(out[j]):
                j -= 1
            if j >= 0 and is_doc_line(out[j]):
                k = i + 1
                while k < len(lines) and is_blank(lines[k]):
                    k += 1
                if k < len(lines) and follows_doc_block(lines[k]):
                    i += 1
                    continue
        out.append(line)
        i += 1

    result = "".join(out)
    if trailing_newline and result and not result.endswith("\n"):
        result += "\n"
    return result


def process_file(path: Path) -> bool:
    original = path.read_text(encoding="utf-8")
    fixed = fix_content(original)
    if fixed != original:
        path.write_text(fixed, encoding="utf-8", newline="\n")
        return True
    return False


def main() -> int:
    roots = [Path(p) for p in sys.argv[1:]] if len(sys.argv) > 1 else []
    if not roots:
        repo = Path(__file__).resolve().parents[1]
        roots = [repo / "core" / "src", repo / "cli" / "src"]

    fixed_count = 0
    for root in roots:
        for path in sorted(root.rglob("*.rs")):
            if process_file(path):
                fixed_count += 1
                print(path)
    print(f"fixed_files={fixed_count}", file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
