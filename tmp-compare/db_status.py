import sqlite3
from pathlib import Path

db = Path(__file__).resolve().parents[1] / "cache" / "explorer-avalanche.sqlite"
print("db", db, "exists", db.exists(), "size", db.stat().st_size if db.exists() else 0)
conn = sqlite3.connect(str(db))
for row in conn.execute("SELECT name FROM sqlite_master WHERE type='table' ORDER BY 1"):
    print("table", row[0])
try:
    print("sync", list(conn.execute("SELECT * FROM sync_state")))
except Exception as e:
    print("sync_state err", e)
try:
    print(
        "mev_ops",
        list(
            conn.execute(
                "SELECT COUNT(*), MIN(block_number), MAX(block_number) FROM mev_ops"
            )
        ),
    )
    print(
        "blocks_classified",
        list(
            conn.execute(
                "SELECT COUNT(*), MIN(block), MAX(block) FROM blocks_classified"
            )
        ),
    )
except Exception as e:
    print("ops err", e)
conn.close()
