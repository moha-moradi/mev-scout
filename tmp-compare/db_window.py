import sqlite3
import sys
from pathlib import Path

start = int(sys.argv[1]) if len(sys.argv) > 1 else 97780173
db = Path(__file__).resolve().parents[1] / "cache" / "explorer-avalanche.sqlite"
conn = sqlite3.connect(str(db))
print("sync", list(conn.execute("SELECT * FROM sync_state")))
print(
    "classified_in_window",
    list(
        conn.execute(
            "SELECT COUNT(*), MIN(block), MAX(block) FROM blocks_classified WHERE block >= ?",
            (start,),
        )
    ),
)
print(
    "ops_in_window",
    list(
        conn.execute(
            "SELECT COUNT(*), MIN(block_number), MAX(block_number) FROM mev_ops WHERE block_number >= ?",
            (start,),
        )
    ),
)
print("kinds", list(conn.execute(
    "SELECT kind, COUNT(*) FROM mev_ops WHERE block_number >= ? GROUP BY kind",
    (start,),
)))
rows = conn.execute(
    """SELECT block_number, tx_hash, kind, confidence, profit_usd, net_profit_usd, eoa
       FROM mev_ops WHERE block_number >= ? ORDER BY block_number""",
    (start,),
).fetchall()
for r in rows:
    print(r)
conn.close()
