import json, urllib.request
u = "https://gateway.biglabs.eu/api/avalanche/arbitrages?per_page=30&sortBy=-created_at"
req = urllib.request.Request(u, headers={
  "User-Agent":"Mozilla/5.0",
  "Origin":"https://explorer.mev.zone",
  "Referer":"https://explorer.mev.zone/mevlive",
  "Accept":"application/json",
})
body = urllib.request.urlopen(req, timeout=40).read()
data = json.loads(body)
print("type", type(data).__name__, "len", len(data) if hasattr(data,"__len__") else None)
if isinstance(data, dict):
    print(data.keys())
    data = data.get("data") or data.get("items") or data
rows = data if isinstance(data, list) else []
print("rows", len(rows))
if rows:
    print("keys", sorted(rows[0].keys()))
    print("newest", rows[0].get("blockNumber"), rows[0].get("time"), rows[0].get("hash"), rows[0].get("type"), rows[0].get("usdProfit"))
    print("oldest", rows[-1].get("blockNumber"), rows[-1].get("time"), rows[-1].get("type"))
    from collections import Counter
    print("types", Counter(r.get("type") for r in rows))
open("tmp-compare/mevlive-now.json","w",encoding="utf-8").write(json.dumps(rows, indent=2))
print("wrote", len(rows))

import os, sqlite3
db = os.path.join("cache", "explorer-avalanche.sqlite")
con = sqlite3.connect(db)
sync = con.execute("select head, indexed_to from sync_state where chain_id=43114").fetchone()
print("sync", sync)
if not rows:
    raise SystemExit(0)
lo = min(int(r["blockNumber"]) for r in rows)
hi = max(int(r["blockNumber"]) for r in rows)
print("window", lo, hi)
local = con.execute(
    "select lower(tx_hash), block_number, kind, profit_usd, net_profit_usd from mev_ops where block_number between ? and ?",
    (lo, hi),
).fetchall()
by = {}
for h, b, k, p, n in local:
    by.setdefault(h, []).append((b, k, p, n))
classified = con.execute(
    "select count(*) from blocks_classified where block between ? and ?",
    (lo, hi),
).fetchone()[0]
print("classified", classified, "of", hi - lo + 1, "local_ops", len(local), "local_hashes", len(by))
both = missed = 0
arb_both = other = 0
print("--- mevlive vs local ---")
deltas = []
for r in rows:
    h = r["hash"].lower()
    hit = by.get(h)
    if not hit:
        missed += 1
        path = ">".join((p.get("symbol") or "?") for p in (r.get("path") or []))
        print("MISS", r["blockNumber"], r["hash"], "usd", r.get("usdProfit"), path)
        continue
    both += 1
    kinds = [x[1] for x in hit]
    if "arb_atomic" in kinds:
        arb_both += 1
    else:
        other += 1
    arb = next((x for x in hit if x[1] == "arb_atomic"), hit[0])
    ml_p = float(r.get("usdProfit") or 0)
    loc_p = arb[2]
    deltas.append((abs((loc_p or 0) - ml_p), r["blockNumber"], r["hash"], arb[1], ml_p, loc_p, arb[3]))
print("overlap", both, "arb_atomic", arb_both, "other_kind", other, "missed", missed)
if rows:
    print("recall", round(both / len(rows), 3))
deltas.sort(reverse=True)
print("--- biggest profit deltas ---")
for d in deltas[:12]:
    print("delta", round(d[0], 4), "blk", d[1], d[2][:12], "kind", d[3], "mevlive", round(d[4], 4), "local_profit", d[5], "local_net", d[6])
ml_hashes = {r["hash"].lower() for r in rows}
extra_arb = []
for h, items in by.items():
    if h in ml_hashes:
        continue
    for b, k, p, n in items:
        if k == "arb_atomic":
            extra_arb.append((p or 0, b, h, n))
extra_arb.sort(reverse=True)
print("local arb_atomic not on this mevlive page", len(extra_arb))
for p, b, h, n in extra_arb[:8]:
    print("EXTRA", b, h, "profit", p, "net", n)
from collections import Counter
print("local kinds", Counter(k for items in by.values() for _, k, _, _ in items))
