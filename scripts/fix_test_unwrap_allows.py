from pathlib import Path
import re

ALLOW = "#![allow(clippy::unwrap_used, clippy::expect_used)]\n"
ALLOW_RE = re.compile(
    r"(?m)^\s*#!\[allow\(clippy::unwrap_used, clippy::expect_used\)\]\s*\n?"
)
TRAIL_RE = re.compile(
    r"\n?!\[allow\(clippy::unwrap_used, clippy::expect_used\)\]\s*$"
)

files = list(Path("core/tests").glob("*.rs")) + list(Path("cli/tests").glob("*.rs"))
for path in files:
    text = path.read_text(encoding="utf-8")
    text2 = ALLOW_RE.sub("", text)
    text2 = TRAIL_RE.sub("\n", text2)
    m = re.match(r"(?s)^((?:(?://!.*\n)|(?:#!\[.*?\]\n)|(?:\n))*)", text2)
    prefix = m.group(1) if m else ""
    rest = text2[len(prefix) :]
    new_text = prefix + ALLOW + rest
    if not new_text.endswith("\n"):
        new_text += "\n"
    path.write_text(new_text, encoding="utf-8", newline="\n")
    print(f"fixed {path}")

p = Path("core/tests/replay.rs")
t = p.read_text(encoding="utf-8")
t = t.replace(
    "/// ── Test 1b: Deterministic golden `run_block` output on the fixed synthetic\n"
    "/// cache (C.2 offline fixture) ─────────────────────────────────────────────\n"
    "///\n"
    "\n"
    "/// `GasModel::HistoricalExact` pins gas so a single `run_block` is",
    "/// ── Test 1b: Deterministic golden `run_block` output on the fixed synthetic\n"
    "/// cache (C.2 offline fixture) ─────────────────────────────────────────────\n"
    "///\n"
    "/// `GasModel::HistoricalExact` pins gas so a single `run_block` is",
)
p.write_text(t, encoding="utf-8", newline="\n")
print("fixed replay doc blank")
