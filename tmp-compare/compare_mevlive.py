"""Compare local explorer-avalanche.sqlite mev_ops vs mevlive API window."""
from __future__ import annotations

import json
import sqlite3
import sys
from datetime import datetime, timezone
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
DB = ROOT / "cache" / "explorer-avalanche.sqlite"
HERE = Path(__file__).resolve().parent


def load_mevlive(path: Path) -> list[dict]:
    raw = json.loads(path.read_text(encoding="utf-8"))
    if isinstance(raw, list):
        return raw
    return raw.get("items") or raw.get("data") or []


def norm_hash(h: str | None) -> str:
    if not h:
        return ""
    h = h.lower()
    return h if h.startswith("0x") else "0x" + h


def main() -> int:
    end_snap = HERE / (sys.argv[1] if len(sys.argv) > 1 else "mevlive-end.json")
    start_block = int(sys.argv[2]) if len(sys.argv) > 2 else 0

    if not DB.exists():
        print(f"missing db: {DB}")
        return 1
    if not end_snap.exists():
        print(f"missing snapshot: {end_snap}")
        return 1

    mevlive = load_mevlive(end_snap)
    # Keep only ops at/after indexer start tip window if provided.
    if start_block:
        mevlive = [r for r in mevlive if int(r.get("blockNumber") or 0) >= start_block]

    ml_by_hash = {norm_hash(r.get("hash")): r for r in mevlive}

    conn = sqlite3.connect(str(DB))
    conn.row_factory = sqlite3.Row
    q = """
      SELECT block_number, tx_hash, kind, eoa, confidence,
             profit_usd, net_profit_usd, profit_token, detector
      FROM mev_ops
      WHERE (? = 0 OR block_number >= ?)
      ORDER BY block_number DESC, id DESC
    """
    local = [dict(r) for r in conn.execute(q, (start_block, start_block)).fetchall()]
    conn.close()

    local_by_hash = {}
    for r in local:
        local_by_hash.setdefault(norm_hash(r["tx_hash"]), []).append(r)

    both = sorted(set(ml_by_hash) & set(local_by_hash))
    only_ml = sorted(set(ml_by_hash) - set(local_by_hash))
    only_local = sorted(set(local_by_hash) - set(ml_by_hash))

    # Kind breakdown local
    kinds: dict[str, int] = {}
    for r in local:
        kinds[r["kind"]] = kinds.get(r["kind"], 0) + 1

    print("=== MEV Live vs local explorer index ===")
    print(f"compared_at: {datetime.now(timezone.utc).isoformat()}")
    print(f"start_block_filter: {start_block or '(none)'}")
    print(f"mevlive_rows: {len(mevlive)}  unique_hashes: {len(ml_by_hash)}")
    print(f"local_ops: {len(local)}  unique_hashes: {len(local_by_hash)}")
    print(f"local_kinds: {kinds}")
    print()
    print(f"overlap (hash in both): {len(both)}")
    print(f"only mevlive: {len(only_ml)}")
    print(f"only local:   {len(only_local)}")
    if mevlive:
        recall = len(both) / len(ml_by_hash) if ml_by_hash else 0.0
        print(f"recall vs mevlive: {recall:.1%}  ({len(both)}/{len(ml_by_hash)})")
    if local_by_hash:
        prec = len(both) / len(local_by_hash)
        print(f"precision vs mevlive hashes: {prec:.1%}  ({len(both)}/{len(local_by_hash)})")

    def show(title: str, hashes: list[str], src: str, limit: int = 15) -> None:
        print()
        print(f"--- {title} (showing up to {limit}) ---")
        for h in hashes[:limit]:
            if src == "ml":
                r = ml_by_hash[h]
                print(
                    f"  blk={r.get('blockNumber')} {h[:12]}… "
                    f"type={r.get('type')} profit=${r.get('usdProfit')} "
                    f"token={((r.get('token') or {}).get('symbol'))} "
                    f"sender={(r.get('sender') or '')[:12]}"
                )
            else:
                for r in local_by_hash[h]:
                    print(
                        f"  blk={r['block_number']} {h[:12]}… "
                        f"kind={r['kind']} conf={r['confidence']} "
                        f"profit_usd={r['profit_usd']} net={r['net_profit_usd']} "
                        f"eoa={(r['eoa'] or '')[:12]} det={r['detector']}"
                    )

    show("ONLY mevlive", only_ml, "ml")
    show("ONLY local", only_local, "local")
    show("OVERLAP", both, "ml")

    # Profit delta on overlap
    if both:
        print()
        print("--- profit delta on overlap (mevlive usdProfit vs local profit_usd) ---")
        for h in both[:20]:
            ml = ml_by_hash[h]
            loc = local_by_hash[h][0]
            mlp = ml.get("usdProfit")
            lp = loc.get("profit_usd")
            if mlp is not None and lp is not None:
                delta = float(lp) - float(mlp)
                print(
                    f"  {h[:12]}… ml=${mlp:.4f} local=${lp:.4f} delta={delta:+.4f} "
                    f"kind={loc['kind']}"
                )
            else:
                print(
                    f"  {h[:12]}… ml={mlp} local={lp} kind={loc['kind']}"
                )

    report = {
        "start_block_filter": start_block,
        "mevlive": len(ml_by_hash),
        "local": len(local_by_hash),
        "overlap": len(both),
        "only_mevlive": only_ml,
        "only_local": only_local,
        "kinds": kinds,
    }
    (HERE / "compare-report.json").write_text(json.dumps(report, indent=2), encoding="utf-8")
    print()
    print(f"wrote {HERE / 'compare-report.json'}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
