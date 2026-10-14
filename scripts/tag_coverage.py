#!/usr/bin/env python3
"""Report mev_ops kind + details.tags coverage from explorer-{chain}.sqlite."""
from __future__ import annotations

import argparse
import collections
import json
import sqlite3
import sys
from pathlib import Path

# Tags the classifier can emit into details.tags (not detail bool flags).
EXPECTED_TAGS = [
    "flash_arb",
    "long_tail",
    "curve_imbalance",
    "rate_provider_staleness",
    "fot_arb",
    "rebase_arb",
    "savax_rate_arb",
    "epoch_transition",
    "gmx_adl_arb",
    "flash_loan_liq",
    "bad_debt_liq",
    "buycollateral",
    "keeper",
    "automation",
    "claim_and_sell",
    "airdrop",
    "bundler",
    "erc4337",
    "lb_bin_jit",
]

# Avalanche: Morpho bad-debt fingerprint is synthetic-only.
AVAX_SKIP = {"bad_debt_liq"}

EXPECTED_KINDS = [
    "arb_atomic",
    "sandwich",
    "frontrun",
    "backrun",
    "liquidation",
    "jit",
    "jit_arb",
    "skim",
    "unknown",
]


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument(
        "--db",
        default="cache/explorer-avalanche.sqlite",
        help="path to explorer sqlite",
    )
    ap.add_argument(
        "--chain",
        default="avalanche",
        help="used only to apply AVAX_SKIP when chain=avalanche",
    )
    args = ap.parse_args()
    path = Path(args.db)
    if not path.exists():
        print(f"missing db: {path}", file=sys.stderr)
        return 2

    conn = sqlite3.connect(path)
    kinds = conn.execute(
        "SELECT kind, COUNT(*) FROM mev_ops GROUP BY kind ORDER BY 2 DESC"
    ).fetchall()
    total = conn.execute("SELECT COUNT(*) FROM mev_ops").fetchone()[0]
    br = conn.execute(
        "SELECT MIN(block_number), MAX(block_number) FROM mev_ops"
    ).fetchone()
    print(f"db={path}")
    print(f"mev_ops={total} blocks={br[0]}..{br[1]}")
    print("KINDS:")
    kind_set = {k for k, _ in kinds}
    for k, n in kinds:
        print(f"  {k}: {n}")
    for k in EXPECTED_KINDS:
        if k not in kind_set:
            print(f"  MISSING kind: {k}")

    tags: collections.Counter[str] = collections.Counter()
    flags: collections.Counter[str] = collections.Counter()
    for (dj,) in conn.execute(
        "SELECT details_json FROM mev_ops WHERE details_json IS NOT NULL"
    ):
        try:
            d = json.loads(dj)
        except json.JSONDecodeError:
            continue
        for t in d.get("tags") or []:
            if isinstance(t, str):
                tags[t] += 1
        for flag in (
            "interest_accrued",
            "oracle_poke_block",
            "absorb_paired",
            "bad_debt",
        ):
            if d.get(flag) is True:
                flags[flag] += 1

    print("TAGS:")
    for t, n in tags.most_common():
        print(f"  {t}: {n}")
    skip = AVAX_SKIP if args.chain == "avalanche" else set()
    missing = [t for t in EXPECTED_TAGS if t not in tags and t not in skip]
    present = [t for t in EXPECTED_TAGS if t in tags]
    print(f"tag_coverage={len(present)}/{len(EXPECTED_TAGS) - len(skip)} "
          f"(skip={sorted(skip)})")
    for t in missing:
        print(f"  MISSING tag: {t}")
    if flags:
        print("FLAGS:")
        for f, n in flags.most_common():
            print(f"  {f}: {n}")

    return 0 if not missing else 1


if __name__ == "__main__":
    raise SystemExit(main())
