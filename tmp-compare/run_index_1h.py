"""Launch explorer index --loop --duration 1h and stream logs."""
from __future__ import annotations

import os
import subprocess
import sys
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
LOG = Path(__file__).resolve().parent / "index-1h.log"
ext = "." + "exe"

# Prefer release binary; fall back to cargo run.
release = ROOT / "target" / "release" / ("mev-scout" + ext)
debug = ROOT / "target" / "debug" / ("mev-scout" + ext)
cargo = Path.home() / ".cargo" / "bin" / ("cargo" + ext)

config = str(ROOT / "mev-scout.avalanche-run.toml")
args_tail = [
    "--config",
    config,
    "explorer",
    "index",
    "--loop",
    "--duration",
    "1h",
]

if release.exists():
    cmd = [str(release), *args_tail]
elif debug.exists():
    cmd = [str(debug), *args_tail]
elif cargo.exists():
    cmd = [
        str(cargo),
        "run",
        "-p",
        "mev-scout-cli",
        "--release",
        "--",
        *args_tail,
    ]
else:
    print("no mev-scout binary or cargo found", file=sys.stderr)
    sys.exit(1)

print(f"starting: {' '.join(cmd)}", flush=True)
print(f"logging to {LOG}", flush=True)
start = time.time()

with LOG.open("w", encoding="utf-8", errors="replace") as log:
    log.write(f"# started {time.strftime('%Y-%m-%dT%H:%M:%SZ', time.gmtime())}\n")
    log.write(f"# cmd: {' '.join(cmd)}\n")
    log.flush()
    proc = subprocess.Popen(
        cmd,
        cwd=str(ROOT),
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
        bufsize=1,
    )
    assert proc.stdout is not None
    for line in proc.stdout:
        sys.stdout.write(line)
        sys.stdout.flush()
        log.write(line)
        log.flush()
    rc = proc.wait()
    elapsed = time.time() - start
    footer = f"\n# exit={rc} elapsed_s={elapsed:.1f}\n"
    log.write(footer)
    print(footer, flush=True)
    sys.exit(rc)
