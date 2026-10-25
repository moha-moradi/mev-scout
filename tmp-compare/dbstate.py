import sqlite3

con = sqlite3.connect("cache/explorer-avalanche.sqlite", timeout=10)
print("=== tables ===")
for r in con.execute("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name"):
    print("  ", r[0])

print("\n=== block range (blocks table) ===")
for q in [
    "SELECT COUNT(*), MIN(block), MAX(block) FROM blocks",
]:
    try:
        print("  ", q, "=>", con.execute(q).fetchone())
    except Exception as e:
        print("  ", q, "=>", "ERR:", e)

print("\n=== blocks_classified range ===")
try:
    for r in con.execute("SELECT COUNT(*), MIN(block), MAX(block) FROM blocks_classified"):
        print("  ", r)
except Exception as e:
    print("  ", "ERR:", e)

print("\n=== mev_ops range + kinds ===")
try:
    for r in con.execute("SELECT COUNT(*), MIN(block_number), MAX(block_number) FROM mev_ops"):
        print("  all ops:", r)
    for r in con.execute("SELECT kind, COUNT(*), MIN(block_number), MAX(block_number) FROM mev_ops GROUP BY kind"):
        print("  ", r)
except Exception as e:
    print("  ", "ERR:", e)

print("\n=== last 10 blocks (max first) ===")
try:
    for r in con.execute("SELECT block FROM blocks ORDER BY block DESC LIMIT 10"):
        print("  ", r[0])
except Exception as e:
    print("  ", "ERR:", e)

con.close()
