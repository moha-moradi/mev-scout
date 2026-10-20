import sqlite3
c = sqlite3.connect(r"d:\gitlab.dte.repo\mev-scout\cache\avalanche-mev-scout.sqlite")
tabs = [r[0] for r in c.execute("SELECT name FROM sqlite_master WHERE type='table'").fetchall()]
print(tabs[:40])
for t in tabs:
    if "pool" in t.lower() or "token" in t.lower():
        print("table", t)
pools = ["0x4224f6f4c9280509724db2dbac314621e4465c29", "0xebdff15cdddb59a8d32e5ec1e3800708b306f4ee"]
for t in tabs:
    try:
        cols = [r[1] for r in c.execute("PRAGMA table_info(%s)" % t).fetchall()]
    except Exception:
        continue
    addr_cols = [x for x in cols if "addr" in x.lower() or x.lower() in ("pool", "address", "id")]
    if not addr_cols and "pool" not in t.lower():
        continue
    for p in pools:
        for col in cols:
            try:
                n = c.execute("select count(*) from %s where lower(%s)=?" % (t, col), (p,)).fetchone()[0]
            except Exception:
                continue
            if n:
                print("hit", t, col, p, n)