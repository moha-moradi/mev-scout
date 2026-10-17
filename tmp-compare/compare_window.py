"""Fair window compare: mevlive vs local for [start, end] inclusive."""
from __future__ import annotations

import json
import sqlite3
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[0]
DB = ROOT / "cache" / "explorer-avalanche.sqlite"
START = 97780173
END = 97783612  # last sync_state.indexed_to when indexer stopped


def load_items(path: Path) -> list[dict]:
    raw = json.loads(path.read_text(encoding="utf-8"))
    if isinstance(raw, list):
        return raw
    return raw.get("items") or []


def nh(h: str | None) -> str:
    h = (h or "").lower()
    return h if h.startswith("0x") else "0x" + h


def main() -> None:
    ml_all = load_items(HERE / "mevlive-end.json")
    # Also merge start raw if present for more coverage
    for extra in ("mevlive-end-raw.json", "mevlive-start-raw.json", "mevlive-t0-raw.json"):
        p = HERE / extra
        if p.exists():
            try:
                ml_all.extend(load_items(p) if False else (
                    json.loads(p.read_text()) if isinstance(json.loads(p.read_text()), list)
                    else json.loads(p.read_text()).get("items", [])
                ))
            except Exception:
                data = json.loads(p.read_text())
                ml_all.extend(data if isinstance(data, list) else data.get("items", []))

    # dedupe by hash
    ml_map: dict[str, dict] = {}
    for r in ml_all:
        b = int(r.get("blockNumber") or 0)
        if START <= b <= END:
            ml_map[nh(r.get("hash"))] = r

    conn = sqlite3.connect(str(DB))
    conn.row_factory = sqlite3.Row
    local = [
        dict(r)
        for r in conn.execute(
            """SELECT block_number, tx_hash, kind, confidence, profit_usd, net_profit_usd, eoa
               FROM mev_ops WHERE block_number BETWEEN ? AND ?""",
            (START, END),
        )
    ]
    conn.close()
    local_map: dict[str, list[dict]] = {}
    for r in local:
        local_map.setdefault(nh(r["tx_hash"]), []).append(r)

    arb_local = {h: rows for h, rows in local_map.items() if any(x["kind"] == "arb_atomic" for x in rows)}
    # mevlive is arb+liquidation mixed; split
    ml_arb = {h: r for h, r in ml_map.items() if (r.get("type") or "").lower() == "arbitrage"}
    ml_liq = {h: r for h, r in ml_map.items() if (r.get("type") or "").lower() == "liquidation"}

    both_arb = sorted(set(ml_arb) & set(arb_local))
    only_ml_arb = sorted(set(ml_arb) - set(local_map))  # miss entirely
    only_ml_arb_as_other = sorted(set(ml_arb) & set(local_map) - set(arb_local))
    only_local_arb = sorted(set(arb_local) - set(ml_arb))

    print(f"window blocks [{START}, {END}]")
    print(f"mevlive in window: {len(ml_map)} (arb={len(ml_arb)} liq={len(ml_liq)})")
    print(f"local in window: {len(local_map)} ops")
    kinds = {}
    for rows in local_map.values():
        for r in rows:
            kinds[r["kind"]] = kinds.get(r["kind"], 0) + 1
    print(f"local kinds: {kinds}")
    print()
    print(f"arb overlap: {len(both_arb)} / mevlive_arb={len(ml_arb)}")
    print(f"mevlive arb MISSED entirely: {len(only_ml_arb)}")
    print(f"mevlive arb found as non-arb locally: {len(only_ml_arb_as_other)}")
    print(f"local arb_atomic not on mevlive: {len(only_local_arb)}")
    if ml_arb:
        print(f"arb recall vs mevlive: {len(both_arb)/len(ml_arb):.1%}")

    print("\n--- mevlive arb MISSED ---")
    for h in only_ml_arb:
        r = ml_arb[h]
        print(f"  blk={r['blockNumber']} {h} profit=${r.get('usdProfit')} sender={r.get('sender')}")

    print("\n--- mevlive arb as other kind ---")
    for h in only_ml_arb_as_other:
        r = ml_arb[h]
        kinds_l = [x["kind"] for x in local_map[h]]
        print(f"  blk={r['blockNumber']} {h} local_kinds={kinds_l} ml_profit=${r.get('usdProfit')}")

    print("\n--- overlap profit ---")
    for h in both_arb:
        ml = ml_arb[h]
        loc = next(x for x in arb_local[h] if x["kind"] == "arb_atomic")
        mlp, lp = ml.get("usdProfit"), loc.get("profit_usd")
        if mlp is not None and lp is not None:
            print(f"  {h[:14]}… ml=${float(mlp):.4f} local=${float(lp):.4f} d={float(lp)-float(mlp):+.4f}")
        else:
            print(f"  {h[:14]}… ml={mlp} local={lp}")

    print("\n--- local arb_atomic NOT on mevlive (top by |profit|) ---")
    rows = []
    for h in only_local_arb:
        loc = next(x for x in arb_local[h] if x["kind"] == "arb_atomic")
        rows.append((abs(float(loc.get("profit_usd") or 0.0)), loc, h))
    rows.sort(key=lambda t: t[0], reverse=True)
    for _, loc, h in rows[:25]:
        print(
            f"  blk={loc['block_number']} {h[:14]}… "
            f"profit=${loc.get('profit_usd')} net=${loc.get('net_profit_usd')} "
            f"conf={loc['confidence']} eoa={loc['eoa'][:12]}"
        )

    # liquidation check
    both_liq = sorted(set(ml_liq) & set(local_map))
    print(f"\nliquidation: mevlive={len(ml_liq)} overlap_any_kind={len(both_liq)}")
    for h in sorted(set(ml_liq) - set(local_map)):
        r = ml_liq[h]
        print(f"  MISSED liq blk={r['blockNumber']} {h} ${r.get('usdProfit')}")


if __name__ == "__main__":
    main()
