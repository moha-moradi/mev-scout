import sqlite3
f="core/cache/explorer-avalanche.sqlite"
con=sqlite3.connect(f, timeout=5)
tables=con.execute("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name").fetchall()
print("tables in core/cache/explorer-avalanche.sqlite:", len(tables))
for r in tables:
    print("  ", r[0])
print("\n--- blocks table ---")
try:
    print("blocks count/min/max:", con.execute("SELECT COUNT(*), MIN(block), MAX(block) FROM blocks").fetchone())
except Exception as e:
    print("ERR:", e)
print("\n--- blocks_classified ---")
try:
    print("classified count/min/max:", con.execute("SELECT COUNT(*), MIN(block), MAX(block) FROM blocks_classified").fetchone())
except Exception as e:
    print("ERR:", e)
print("\n--- mev_ops ---")
try:
    print("ops count/min/max:", con.execute("SELECT COUNT(*), MIN(block_number), MAX(block_number) FROM mev_ops").fetchone())
    print("ops by kind:")
    for r in con.execute("SELECT kind, COUNT(*), MIN(block_number), MAX(block_number) FROM mev_ops GROUP BY kind ORDER BY 2 DESC"):
        print("  ", r)
except Exception as e:
    print("ERR:", e)
print("\n--- last blocks ---")
try:
    for r in con.execute("SELECT block, ts, hash FROM blocks ORDER BY block DESC LIMIT 8"):
        print("  ", r[0], r[1])
except Exception as e:
    print("ERR:", e)
con.close()
