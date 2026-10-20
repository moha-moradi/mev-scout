import json
import sqlite3
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent
meta = json.loads((HERE / "window-meta.json").read_text(encoding="utf-8"))
feed = json.loads((HERE / "mevlive-window.json").read_text(encoding="utf-8"))
from_block = meta["from_block"]
to_block = meta["to_block"]


def load_ops(db_path):
    con = sqlite3.connect(db_path)
    rows = con.execute(
        """
        SELECT lower(tx_hash), block_number, kind, profit_token, profit_amount, profit_usd
        FROM mev_ops
        WHERE block_number BETWEEN ? AND ?
        """,
        (from_block, to_block),
    ).fetchall()
    by = {}
    for h, b, k, pt, pa, pu in rows:
        by.setdefault(h, []).append(
            {"block": b, "kind": k, "profit_token": pt, "profit_amount": pa, "profit_usd": pu}
        )
    return by


before = load_ops(ROOT / "cache" / "explorer-avalanche-before.sqlite")
after = load_ops(ROOT / "cache" / "explorer-avalanche-after.sqlite")
feed_by = {r["hash"].lower(): r for r in feed if from_block <= int(r["blockNumber"]) <= to_block}


def feed_delta(r):
    return int(r["amountOut"]) - int(r["amountIn"])


def feed_token(r):
    return (r.get("token") or {}).get("token", "").lower()


miss_before = []
still_miss = []
profit_mismatch = []
swap_vs_xfer = []
recovered = []
kind_flip = []

for h, r in feed_by.items():
    bhit = before.get(h)
    ahit = after.get(h)
    if not bhit:
        miss_before.append(h)
    if not ahit:
        still_miss.append((h, r))
        continue
    if not bhit:
        recovered.append(h)
    a = next((x for x in ahit if x["kind"] == "arb_atomic"), ahit[0])
    b = next((x for x in bhit if x["kind"] == "arb_atomic"), bhit[0]) if bhit else None
    want = feed_delta(r)
    got = int(a["profit_amount"] or 0)
    tok = (a["profit_token"] or "").lower()
    ft = feed_token(r)
    if ft and tok and ft != tok:
        profit_mismatch.append({"hash": h, "reason": "token", "feed_token": ft, "local_token": tok})
    elif got != want:
        # Transfer-ledger residual can be below swap-log amountIn/Out when LB
        # (or similar) fees are taken inside the pool between the packed
        # amountsIn and the ERC-20 Transfer. Local figure is the wallet delta.
        if got > 0 and want > 0 and got < want and a["kind"] == "arb_atomic":
            swap_vs_xfer.append(
                {"hash": h, "feed_delta": want, "local_amount": got, "kind": a["kind"]}
            )
        else:
            profit_mismatch.append(
                {
                    "hash": h,
                    "reason": "amount",
                    "feed_delta": want,
                    "local_amount": got,
                    "kind": a["kind"],
                }
            )
    if b and b["kind"] != a["kind"]:
        kind_flip.append((h, b["kind"], a["kind"]))

after_arb = {
    h
    for h, items in after.items()
    if any(x["kind"] == "arb_atomic" for x in items) and h not in feed_by
}
before_feed_hits = {h for h in feed_by if h in before}
dropped_feed = [h for h in before_feed_hits if h not in after]

report = {
    "window": {"from": from_block, "to": to_block, "feed_rows": len(feed_by)},
    "feed_recall_after": 1.0 - (len(still_miss) / len(feed_by) if feed_by else 0),
    "miss_before_count": len(miss_before),
    "recovered_count": len(recovered),
    "still_miss_count": len(still_miss),
    "profit_mismatch_count": len(profit_mismatch),
    "swap_log_vs_transfer_count": len(swap_vs_xfer),
    "after_only_arb_count": len(after_arb),
    "dropped_feed_from_before": len(dropped_feed),
    "kind_flips": kind_flip,
    "swap_vs_xfer": swap_vs_xfer,
    "profit_mismatch": profit_mismatch,
    "recovered": recovered,
    "after_only_arb_sample": sorted(after_arb)[:20],
}
out = HERE / "compare-gate-report.json"
out.write_text(json.dumps(report, indent=2), encoding="utf-8")
print(json.dumps({k: report[k] for k in [
    "window", "feed_recall_after", "miss_before_count", "recovered_count",
    "still_miss_count", "profit_mismatch_count", "swap_log_vs_transfer_count",
    "after_only_arb_count", "dropped_feed_from_before"
]}, indent=2))
for row in swap_vs_xfer:
    print("SWAP_VS_XFER", row["hash"][:18], "feed", row["feed_delta"], "local", row["local_amount"])
fail = bool(still_miss or profit_mismatch or dropped_feed)
print("FAIL" if fail else "PASS", "gate")
print("wrote", out)
raise SystemExit(1 if fail else 0)