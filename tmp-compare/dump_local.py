import sqlite3
from pathlib import Path

start = 97780173
db = Path(__file__).resolve().parents[1] / "cache" / "explorer-avalanche.sqlite"
c = sqlite3.connect(str(db))
print("sync", list(c.execute("SELECT * FROM sync_state")))
print(
    "ops",
    list(
        c.execute(
            "SELECT COUNT(*), MIN(block_number), MAX(block_number) FROM mev_ops WHERE block_number>=?",
            (start,),
        )
    ),
)
print(
    "kinds",
    list(
        c.execute(
            "SELECT kind, COUNT(*) FROM mev_ops WHERE block_number>=? GROUP BY kind ORDER BY 2 DESC",
            (start,),
        )
    ),
)
print("=== arb_atomic ===")
for r in c.execute(
    """SELECT block_number, tx_hash, profit_usd, net_profit_usd, eoa
       FROM mev_ops WHERE block_number>=? AND kind='arb_atomic' ORDER BY block_number""",
    (start,),
):
    print(r)
c.close()
