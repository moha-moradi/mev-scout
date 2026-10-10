"""Insert crate-level unwrap/expect allows at the top of integration tests."""
from __future__ import annotations

from pathlib import Path

ALLOW = "#![allow(clippy::unwrap_used, clippy::expect_used)]\n"


def insert_allow(path: Path) -> None:
    raw = path.read_bytes()
    try:
        text = raw.decode("utf-8")
    except UnicodeDecodeError:
        text = raw.decode("utf-8", errors="surrogateescape")

    if "clippy::unwrap_used" in text:
        return

    lines = text.splitlines(keepends=True)
    i = 0
    while i < len(lines):
        s = lines[i].lstrip("\ufeff")
        if s.startswith("//!") or s.startswith("#![") or s.strip() == "":
            i += 1
            continue
        break

    lines.insert(i, ALLOW)
    out = "".join(lines)
    if not out.endswith("\n"):
        out += "\n"
    path.write_bytes(out.encode("utf-8", errors="surrogateescape"))
    print(f"added {path}")


def main() -> None:
    for root in (Path("core/tests"), Path("cli/tests")):
        for path in sorted(root.glob("*.rs")):
            insert_allow(path)


if __name__ == "__main__":
    main()
