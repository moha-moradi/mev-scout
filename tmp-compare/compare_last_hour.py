"""Compare gateway.biglabs.eu Avalanche arbitrages vs local explorer for a fixed block window.

Window is block-based so both sides see exactly the same chain segment.
Vendor timestamp field is `time` (the API does not return `created_at`); it equals
the block timestamp.  Run after `explorer index --from-block LO --to-block HI`.
"""
import gzip
import json
import sqlite3
import sys
import urllib.request
from collections import Counter

LO = 98496629
HI = 98500407
DB = "cache/explorer-avalanche.sqlite"
API = "https://gateway.biglabs.eu/api/avalanche/arbitrages?per_page=50&sortBy=-created_at&page={}"
OUT = "tmp-compare/compare-last-hour.json"


def fetch_vendor():
    rows = {}
    page = 1
    while page <= 10:
        req = urllib.request.Request(
            API.format(page),
            headers={
                "User-Agent": "Mozilla/5.0",
                "Accept": "application/json",
                "Accept-Encoding": "gzip",
            },
        )
        with urllib.request.urlopen(req, timeout=90) as r:
            raw = r.read()
            if r.headers.get("Content-Encoding") == "gzip":
                raw = gzip.decompress(raw)
        batch = json.loads(raw)
        if not isinstance(batch, list) or not batch:
            break
        for row in batch:
            rows[row["hash"].lower()] = row
        oldest = min(int(row["blockNumber"]) for row in batch)
        if oldest < LO:
            break
        page += 1
    return [r for r in rows.values() if LO <= int(r["blockNumber"]) <= HI]


def main():
    vendor = fetch_vendor()
    vh = {r["hash"].lower(): r for r in vendor}

    con = sqlite3.connect(f"file:{DB}?mode=ro", uri=True, timeout=10)
    ours_rows = con.execute(
        """select block_number, tx_hash, ts, kind, eoa, contract, confidence,
                  profit_usd, net_profit_usd, gas_cost_usd, details_json
             from mev_ops
            where kind='arb_atomic' and block_number between ? and ?""",
        (LO, HI),
    ).fetchall()
    ours = {}
    for (blk, h, ts, kind, eoa, contract, conf, gross, net, gas, details) in ours_rows:
        d = json.loads(details or "{}")
        rec = {
            "block": blk,
            "hash": h.lower(),
            "ts": ts,
            "searcher": eoa,
            "contract": contract,
            "confidence": conf,
            "gross_usd": gross,
            "net_usd": net,
            "gas_usd": gas,
            "tags": d.get("tags", []),
            "shape": (d.get("arb_meta") or {}).get("arb_shape"),
            "cross_dex": (d.get("arb_meta") or {}).get("is_cross_dex"),
            "hop_count": (d.get("arb_meta") or {}).get("hop_count"),
            "route": d.get("route", []),
        }
        # keep the highest-gross row if a tx somehow produced duplicates
        if rec["hash"] not in ours or (gross or 0) > (ours[rec["hash"]]["gross_usd"] or 0):
            ours[rec["hash"]] = rec

    oh = set(ours)
    vset = set(vh)
    matched = sorted(vset & oh)
    vendor_only = sorted(vset - oh)
    ours_only = sorted(oh - vset)

    def tx_info(h):
        tx = con.execute(
            'select block_number, tx_index, "from", "to", success from txs where hash=?',
            (h,),
        ).fetchone()
        ops = con.execute(
            "select kind, eoa, profit_usd, net_profit_usd from mev_ops where tx_hash=?",
            (h,),
        ).fetchall()
        n_swaps = con.execute(
            "select count(*) from swaps where block_number=? and tx_index=?",
            (tx[0], tx[1]),
        ).fetchone()[0] if tx else None
        n_transfers = con.execute(
            "select count(*) from transfers where block_number=? and tx_index=?",
            (tx[0], tx[1]),
        ).fetchone()[0] if tx else None
        return {
            "indexed_tx": bool(tx),
            "tx_from": tx[2] if tx else None,
            "tx_to": tx[3] if tx else None,
            "success": bool(tx[4]) if tx else None,
            "ops": [{"kind": o[0], "searcher": o[1], "gross_usd": o[2], "net_usd": o[3]} for o in ops],
            "swaps": n_swaps,
            "transfers": n_transfers,
        }

    vendor_only_detail = []
    for h in vendor_only:
        v = vh[h]
        info = tx_info(h)
        vendor_only_detail.append(
            {
                "hash": h,
                "block": int(v["blockNumber"]),
                "time": v["time"],
                "usd_profit": float(v["usdProfit"]),
                "path": "->".join((t.get("symbol") or "?") for t in v.get("path") or []),
                "sender": v["sender"],
                **info,
            }
        )

    ours_only_detail = []
    for h in ours_only:
        o = ours[h]
        ours_only_detail.append(
            {
                "hash": h,
                "block": o["block"],
                "searcher": o["searcher"],
                "contract": o["contract"],
                "gross_usd": o["gross_usd"],
                "net_usd": o["net_usd"],
                "gas_usd": o["gas_usd"],
                "shape": o["shape"],
                "cross_dex": o["cross_dex"],
                "hop_count": o["hop_count"],
                "tags": o["tags"],
                "route_pools": [x.get("pool") for x in o["route"]],
                "tx": tx_info(h),
            }
        )

    matched_detail = []
    for h in matched:
        v = vh[h]
        o = ours[h]
        vp = float(v["usdProfit"])
        op = o["gross_usd"] or 0.0
        matched_detail.append(
            {
                "hash": h,
                "block": o["block"],
                "time": v["time"],
                "vendor_usd_profit": vp,
                "explorer_gross_usd": op,
                "explorer_net_usd": o["net_usd"],
                "abs_delta": abs(op - vp),
                "pct_delta": (abs(op - vp) / vp * 100.0) if vp else None,
                "vendor_sender": v["sender"],
                "explorer_searcher": o["searcher"],
                "path": "->".join((t.get("symbol") or "?") for t in v.get("path") or []),
                "shape": o["shape"],
                "cross_dex": o["cross_dex"],
                "tags": o["tags"],
            }
        )

    all_kinds = con.execute(
        "select kind, count(*) from mev_ops where block_number between ? and ? group by kind order by 2 desc",
        (LO, HI),
    ).fetchall()
    classified = con.execute(
        "select count(*) from blocks_classified where block between ? and ?", (LO, HI)
    ).fetchone()[0]

    nv, no, nm = len(vset), len(oh), len(matched)
    result = {
        "window": {"from_block": LO, "to_block": HI, "blocks_classified": classified},
        "vendor": {
            "n": nv,
            "total_usd_profit": sum(float(r["usdProfit"]) for r in vendor),
            "rows": sorted(
                [
                    {
                        "hash": r["hash"].lower(),
                        "block": int(r["blockNumber"]),
                        "time": r["time"],
                        "usd_profit": float(r["usdProfit"]),
                        "sender": r["sender"],
                        "path": "->".join((t.get("symbol") or "?") for t in r.get("path") or []),
                    }
                    for r in vendor
                ],
                key=lambda x: x["block"],
            ),
        },
        "explorer": {
            "n_arb_atomic": no,
            "ops_by_kind": dict(all_kinds),
            "rows": sorted(
                [
                    {
                        "hash": o["hash"],
                        "block": o["block"],
                        "searcher": o["searcher"],
                        "gross_usd": o["gross_usd"],
                        "net_usd": o["net_usd"],
                        "shape": o["shape"],
                        "cross_dex": o["cross_dex"],
                    }
                    for o in ours.values()
                ],
                key=lambda x: x["block"],
            ),
        },
        "agreement": {
            "matched": nm,
            "vendor_only": len(vendor_only),
            "explorer_only": len(ours_only),
            "coverage_recall": (nm / nv) if nv else None,
            "precision_vs_vendor": (nm / no) if no else None,
            "f1": (2 * nm / (nv + no)) if (nv + no) else None,
        },
        "matched": sorted(matched_detail, key=lambda x: x["block"]),
        "vendor_only": sorted(vendor_only_detail, key=lambda x: x["block"]),
        "explorer_only": sorted(ours_only_detail, key=lambda x: x["block"]),
    }
    with open(OUT, "w", encoding="utf-8") as f:
        json.dump(result, f, indent=1, ensure_ascii=False)

    print(f"window {LO}..{HI} classified={classified}")
    print(f"vendor={nv} explorer_arb_atomic={no} matched={nm} "
          f"vendor_only={len(vendor_only)} explorer_only={len(ours_only)}")
    print(f"coverage={result['agreement']['coverage_recall']:.1%} "
          f"precision_vs_vendor={result['agreement']['precision_vs_vendor']:.1%} "
          f"f1={result['agreement']['f1']:.1%}")
    print("ops_by_kind", dict(all_kinds))
    print("\nmatched:")
    for m in result["matched"]:
        print(f"  blk={m['block']} {m['hash'][:12]} vendor=${m['vendor_usd_profit']:.5f} "
              f"ours_gross=${m['explorer_gross_usd']:.5f} delta={m['pct_delta']:.1f}% "
              f"{m['path']} {m['shape']}")
    print("\nvendor_only:")
    for m in result["vendor_only"]:
        print(f"  blk={m['block']} {m['hash'][:12]} ${m['usd_profit']:.5f} {m['path']} "
              f"indexed={m['indexed_tx']} ops={[o['kind'] for o in m['ops']]} swaps={m['swaps']}")
    print("\nexplorer_only:")
    for m in result["explorer_only"]:
        print(f"  blk={m['block']} {m['hash'][:12]} gross=${m['gross_usd'] or 0:.5f} "
              f"net=${m['net_usd'] or 0:.5f} {m['shape']} cross_dex={m['cross_dex']} "
              f"hops={m['hop_count']} searcher={m['searcher']} tx_from={m['tx']['tx_from']}")
    print(f"\nwrote {OUT}")


if __name__ == "__main__":
    sys.exit(main())
