"""Stop the background indexer by walking the process tree from the shell PID."""
from __future__ import annotations

import sys

try:
    import psutil
except ImportError:
    psutil = None

shell_pid = int(sys.argv[1]) if len(sys.argv) > 1 else 18324

if psutil is None:
    # Fallback: taskkill tree
    import subprocess

    subprocess.run(
        ["taskkill", "/PID", str(shell_pid), "/T", "/F"],
        check=False,
    )
    print(f"taskkill tree {shell_pid}")
else:
    try:
        parent = psutil.Process(shell_pid)
    except psutil.NoSuchProcess:
        print("shell gone")
        raise SystemExit(0)
    kids = parent.children(recursive=True)
    for p in kids + [parent]:
        try:
            print(f"kill {p.pid} {p.name()}")
            p.kill()
        except Exception as e:
            print(f"  skip {e}")
