import sqlite3, json
con = sqlite3.connect(r"d:\gitlab.dte.repo\mev-scout\cache\explorer-avalanche-after.sqlite")
feed = set(r["hash"].lower() for r in json.load(open("mevlive-window.json", encoding="utf-8")))
rows = con.execute(
    "select lower(tx_hash), profit_amount, profit_usd from mev_ops where kind='arb_atomic' and block_number between 98039265 and 98041064"
).fetchall()
extra = [r for r in rows if r[0] not in feed]
print("after_only", len(extra), "total_arb", len(rows), "overlap", sum(1 for r in rows if r[0] in feed))
for r in extra[:10]:
    print(r[0][:20], "amt", r[1], "usd", r[2])