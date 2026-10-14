#!/usr/bin/env python3
import sqlite3
import sys

path = sys.argv[1] if len(sys.argv) > 1 else "cache/avalanche-mev-scout.sqlite"
c = sqlite3.connect(path)
tables = [r[0] for r in c.execute("SELECT name FROM sqlite_master WHERE type='table'")]
print("db", path)
print("tables", tables)
for t in tables:
    try:
        n = c.execute(f"SELECT COUNT(*) FROM [{t}]").fetchone()[0]
        print(f"  {t}: {n}")
    except Exception as e:
        print(f"  {t}: err {e}")
