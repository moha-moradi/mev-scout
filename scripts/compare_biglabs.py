#!/usr/bin/env python3
"""Compare an explorer index against the frozen BigLabs arbitrage snapshot.

Task M0 (explorer improvement plan): this script measures the explorer's arb
output against an *external, independent* coverage oracle.  It is deliberately
conservative about what the numbers mean:

* ``recall_vs_vendor``        = |V n L| / |V|   -- coverage of vendor-listed arbs.
* ``vendor_agreement``        = |V n L| / |L|   -- a LOWER BOUND on agreement,
  never "precision": the vendor endpoint is arb-only and blind to JIT,
  sandwich, liquidation, and to kind-precedence differences, so local rows it
  does not list are not automatically wrong.
* profit deltas               -- per-hash relative delta on V n L, in percent of
  the vendor's ``usdProfit``; rows whose vendor profit is null/zero are skipped.

The join is hash-level and kind-agnostic on our side: any local ``mev_ops`` row
on the same tx hash counts as a match, and we report which kind we assigned it.

The vendor snapshot is frozen exactly once and committed (see
``docs/oracle/biglabs-avalanche-<lo>-<hi>.json``).  Every later checkpoint
re-indexes *that same block range* and compares against *that same file* --
never re-fetch, never re-range; comparability comes from the frozen file.

Usage
-----
    python scripts/compare_biglabs.py \
        --db cache/oracle-avalanche.sqlite \
        --snapshot docs/oracle/biglabs-avalanche-98572463-98575973.json \
        --chain avalanche --lo 98572463 --hi 98575973 \
        --label baseline \
        --adjudications docs/oracle/adjudications-baseline.json

Machine output: ``cache/oracle-report-<label>.json`` (git-ignored) plus a
markdown table on stdout for the checklist doc.

Stdlib only (sqlite3/json/argparse), Python 3.11+.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import math
import sqlite3
import statistics
import sys
from pathlib import Path
from typing import Any, NoReturn

SCRIPT_VERSION = "1.1.0"
REPO_ROOT = Path(__file__).resolve().parents[1]

#: Built-in *suggestion* threshold for dust, never an A14 materiality floor.
#: A positive-profit arb below this size is still a realized cycle: the vendor
#: lists comparable dust and has no size floor, so the suggestion is
#: ``tp_vendor_miss`` (a vendor coverage gap).  A14's checkpoint criterion is
#: that genuinely fake-$0/NULL rows leave L, not that small arbs disappear.
SUGGEST_DUST_PROFIT_USD = 0.01

#: Profit at or below one raw USDC unit is rounding residue, not a price.
ZERO_PROFIT_USD = 0.000001

ADJUDICATIONS = ("tp_vendor_miss", "fp_suspect", "unknown")

#: Kinds/utility families the arb-only vendor endpoint cannot see by design.
UTILITY_PROTOCOLS = {"erc4337"}
UTILITY_TAGS = {"bundler", "erc4337", "keeper", "claim", "claim_and_sell"}
NON_ARB_KINDS = {
    "jit",
    "jit_arb",
    "sandwich",
    "frontrun",
    "backrun",
    "skim",
    "liquidation",
}

LOCAL_COLUMNS = (
    "id",
    "block_number",
    "tx_hash",
    "kind",
    "confidence",
    "eoa",
    "profit_token",
    "profit_usd",
    "net_profit_usd",
    "detector",
    "canonical_id",
    "details_json",
)


def die(message: str, hint: str | None = None) -> NoReturn:
    """Fail loudly with an actionable message instead of raising a traceback."""
    print(f"compare_biglabs: error: {message}", file=sys.stderr)
    if hint:
        print(f"compare_biglabs: hint: {hint}", file=sys.stderr)
    raise SystemExit(2)


def warn(message: str) -> None:
    print(f"compare_biglabs: warning: {message}", file=sys.stderr)


# --------------------------------------------------------------------------
# Input loading
# --------------------------------------------------------------------------


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1 << 20), b""):
            digest.update(chunk)
    return digest.hexdigest()


def norm_hash(value: Any) -> str:
    if not isinstance(value, str):
        return ""
    text = value.strip().lower()
    if not text:
        return ""
    if not text.startswith("0x"):
        text = "0x" + text
    body = text[2:]
    if len(body) != 64 or any(c not in "0123456789abcdef" for c in body):
        return ""
    return text


def short_hash(value: str) -> str:
    return f"{value[:10]}...{value[-6:]}" if len(value) > 20 else value


def display_path(value: str) -> str:
    """Repo-relative path when possible, so the markdown table is portable."""
    try:
        return Path(value).resolve().relative_to(REPO_ROOT).as_posix()
    except (ValueError, OSError):
        return value


def as_float(value: Any) -> float | None:
    if value is None or isinstance(value, bool):
        return None
    try:
        out = float(value)
    except (TypeError, ValueError):
        return None
    if math.isnan(out) or math.isinf(out):
        return None
    return out


def load_snapshot(path: Path) -> tuple[dict[str, Any], list[dict[str, Any]], dict[str, Any]]:
    if not path.is_file():
        die(
            f"snapshot not found: {path}",
            "pass the frozen vendor file committed under docs/oracle/ "
            "(freeze-once: do not re-fetch the vendor)",
        )
    try:
        raw = json.loads(path.read_text(encoding="utf-8"))
    except json.JSONDecodeError as exc:
        die(f"snapshot is not valid JSON: {path} ({exc})")

    if not isinstance(raw, dict) or not isinstance(raw.get("records"), list):
        die(
            f"snapshot has no 'records' list: {path}",
            'expected shape {"meta": {...}, "records": [{hash, blockNumber, usdProfit, ...}]}',
        )

    meta = raw.get("meta") if isinstance(raw.get("meta"), dict) else {}
    records: list[dict[str, Any]] = []
    seen: set[str] = set()
    duplicates = 0
    for index, item in enumerate(raw["records"]):
        if not isinstance(item, dict):
            die(f"snapshot record #{index} is not an object: {path}")
        tx_hash = norm_hash(item.get("hash"))
        if not tx_hash:
            die(
                f"snapshot record #{index} has a missing/malformed 'hash': {item.get('hash')!r}",
                "the frozen file is corrupt; restore it from git rather than re-fetching",
            )
        block = item.get("blockNumber")
        try:
            block_number = int(block)
        except (TypeError, ValueError):
            die(f"snapshot record #{index} ({tx_hash}) has non-numeric blockNumber: {block!r}")
        if tx_hash in seen:
            duplicates += 1
            continue
        seen.add(tx_hash)
        records.append(
            {
                "hash": tx_hash,
                "block_number": block_number,
                "usd_profit": as_float(item.get("usdProfit")),
                "type": item.get("type"),
                "time": item.get("time"),
                "sender": (item.get("sender") or "") or None,
            }
        )

    if not records:
        die(f"snapshot contains zero records: {path}")
    if duplicates:
        warn(f"snapshot had {duplicates} duplicate hash(es); kept the first occurrence")
    records.sort(key=lambda r: (r["block_number"], r["hash"]))
    return meta, records, {"duplicates_ignored": duplicates}


def open_db(path: Path) -> sqlite3.Connection:
    if not path.is_file():
        die(
            f"database not found: {path}",
            "index the frozen range first, e.g. "
            "`explorer index --config cache/oracle-avalanche.toml ...`",
        )
    uri = f"file:{path.as_posix()}?mode=ro"
    try:
        conn = sqlite3.connect(uri, uri=True)
    except sqlite3.Error as exc:
        die(f"cannot open database {path}: {exc}")
    conn.row_factory = sqlite3.Row
    tables = {
        row[0]
        for row in conn.execute("SELECT name FROM sqlite_master WHERE type = 'table'")
    }
    if "mev_ops" not in tables:
        die(
            f"{path} has no mev_ops table",
            "this does not look like an explorer index DB; point --db at the "
            "index you built for the frozen range",
        )
    return conn


def load_local(conn: sqlite3.Connection, lo: int, hi: int) -> list[dict[str, Any]]:
    columns = ", ".join(LOCAL_COLUMNS)
    query = (
        f"SELECT {columns} FROM mev_ops "
        "WHERE block_number BETWEEN ? AND ? "
        "ORDER BY block_number ASC, id ASC"
    )
    rows: list[dict[str, Any]] = []
    for row in conn.execute(query, (lo, hi)):
        record = dict(row)
        record["hash"] = norm_hash(record.get("tx_hash"))
        if not record["hash"]:
            die(
                f"mev_ops id={record.get('id')} has a malformed tx_hash: "
                f"{record.get('tx_hash')!r}"
            )
        record["profit_usd"] = as_float(record.get("profit_usd"))
        record["net_profit_usd"] = as_float(record.get("net_profit_usd"))
        record["tags"], record["protocol"] = parse_details(record.get("details_json"))
        rows.append(record)
    if not rows:
        die(
            f"mev_ops has no rows in blocks {lo}..={hi}",
            "the DB does not cover the frozen range; re-index it with the "
            "archive RPC config before comparing",
        )
    return rows


def parse_details(raw: Any) -> tuple[list[str], str | None]:
    if not raw:
        return [], None
    try:
        details = json.loads(raw)
    except (TypeError, ValueError):
        return [], None
    if not isinstance(details, dict):
        return [], None
    tags = details.get("tags")
    tag_list = [str(t) for t in tags] if isinstance(tags, list) else []
    protocol = details.get("protocol")
    return tag_list, (str(protocol) if protocol else None)


# --------------------------------------------------------------------------
# Metrics
# --------------------------------------------------------------------------


def percentile(sorted_values: list[float], pct: float) -> float:
    """Nearest-rank percentile on an ascending-sorted list.

    Index = floor(pct / 100 * (n - 1)).  Documented explicitly because the
    baseline's p90 depends on it; do not swap in an interpolating variant
    without re-freezing the baseline numbers.
    """
    if not sorted_values:
        die("cannot compute a percentile of an empty sample")
    index = int(math.floor((len(sorted_values) - 1) * pct / 100.0))
    index = max(0, min(index, len(sorted_values) - 1))
    return sorted_values[index]


def pick_local_row(rows_for_hash: list[dict[str, Any]]) -> dict[str, Any]:
    """Deterministic representative row when one hash yields several mev_ops rows."""
    return max(
        rows_for_hash,
        key=lambda r: (
            r["profit_usd"] if r["profit_usd"] is not None else float("-inf"),
            r["id"],
        ),
    )


def compute_metrics(
    vendor: list[dict[str, Any]], local: list[dict[str, Any]]
) -> dict[str, Any]:
    vendor_by_hash = {r["hash"]: r for r in vendor}
    local_by_hash: dict[str, list[dict[str, Any]]] = {}
    for row in local:
        local_by_hash.setdefault(row["hash"], []).append(row)

    vendor_hashes = set(vendor_by_hash)
    local_hashes = set(local_by_hash)
    both = sorted(vendor_hashes & local_hashes)
    local_only = sorted(local_hashes - vendor_hashes)
    vendor_only = sorted(vendor_hashes - local_hashes)

    matched_kinds: dict[str, int] = {}
    matched: list[dict[str, Any]] = []
    deltas: list[float] = []
    skipped_zero_null = 0
    skipped_local_null = 0

    for tx_hash in both:
        vendor_row = vendor_by_hash[tx_hash]
        candidates = local_by_hash[tx_hash]
        chosen = pick_local_row(candidates)
        # Unique-hash accounting: one representative row per hash everywhere,
        # so a hash with several mev_ops rows is counted once.
        matched_kinds[chosen["kind"]] = matched_kinds.get(chosen["kind"], 0) + 1

        vendor_profit = vendor_row["usd_profit"]
        local_profit = chosen["profit_usd"]
        delta_pct: float | None = None
        if vendor_profit is None or vendor_profit == 0.0:
            skipped_zero_null += 1
        elif local_profit is None:
            skipped_local_null += 1
        else:
            delta_pct = (local_profit - vendor_profit) / vendor_profit * 100.0
            deltas.append(delta_pct)

        matched.append(
            {
                "hash": tx_hash,
                "block_number": chosen["block_number"],
                "local_id": chosen["id"],
                "local_rows": len(candidates),
                "local_kind": chosen["kind"],
                "local_confidence": chosen["confidence"],
                "vendor_profit_usd": vendor_profit,
                "local_profit_usd": local_profit,
                "local_net_profit_usd": chosen["net_profit_usd"],
                "delta_pct": delta_pct,
            }
        )

    deltas.sort()
    delta_stats: dict[str, Any] = {
        "definition": "(local profit_usd - vendor usdProfit) / vendor usdProfit * 100",
        "percentile_method": "nearest-rank, index = floor(p/100 * (n-1)) on ascending deltas",
        "n": len(deltas),
        "skipped_vendor_null_or_zero": skipped_zero_null,
        "skipped_local_null": skipped_local_null,
    }
    if deltas:
        delta_stats.update(
            {
                "median": statistics.median(deltas),
                "p90": percentile(deltas, 90),
                "worst": deltas[0],
                "best": deltas[-1],
            }
        )

    # One representative row per local-only hash -- the same pick_local_row() row
    # used for the table and the delta -- so unique-hash composition cannot drift
    # from the per-row adjudication.
    local_only_rows = [pick_local_row(local_by_hash[h]) for h in local_only]
    local_only_kinds: dict[str, int] = {}
    for representative in local_only_rows:
        kind = representative["kind"]
        local_only_kinds[kind] = local_only_kinds.get(kind, 0) + 1

    return {
        "counts": {
            "vendor_rows": len(vendor),
            "vendor_unique_hashes": len(vendor_hashes),
            "local_rows": len(local),
            "local_unique_hashes": len(local_hashes),
            "intersection": len(both),
            "local_only": len(local_only),
            "vendor_only": len(vendor_only),
        },
        "metrics": {
            "recall_vs_vendor": (len(both) / len(vendor_hashes)) if vendor_hashes else 0.0,
            "recall_detail": f"{len(both)}/{len(vendor_hashes)}",
            "vendor_agreement_lower_bound": (len(both) / len(local_hashes))
            if local_hashes
            else 0.0,
            "vendor_agreement_detail": f"{len(both)}/{len(local_hashes)}",
            "vendor_agreement_label": "lower bound on agreement (vendor is a coverage oracle, not ground truth)",
            "matched_kind_labels": dict(sorted(matched_kinds.items())),
            "local_only_kind_composition": dict(
                sorted(local_only_kinds.items(), key=lambda kv: (-kv[1], kv[0]))
            ),
            "vendor_only_hashes": vendor_only,
        },
        "profit_delta_pct": delta_stats,
        "matched": matched,
        "local_only_hashes": local_only,
        "local_only_rows": local_only_rows,
        "local_by_hash": local_by_hash,
        "vendor_by_hash": vendor_by_hash,
    }


# --------------------------------------------------------------------------
# Adjudication of L \ V
# --------------------------------------------------------------------------


def auto_adjudicate(row: dict[str, Any], vendor_min_profit: float | None) -> tuple[str, str]:
    """Default per-row call for a local-only row; overridable via --adjudications.

    ``fp_suspect`` is an *observation* label, reserved for rows whose local
    profit is ~$0/NULL (a zero quote is not a price, A14) or whose net is
    negative.  Dust with positive realized profit is a ``tp_vendor_miss``: the
    vendor lists comparable dust and has no size floor, so a size threshold
    cannot explain the miss.  ``SUGGEST_DUST_PROFIT_USD`` only separates the
    suggested note wording; it is never an A14 materiality floor.
    """
    kind = row["kind"]
    profit = row["profit_usd"]
    net = row["net_profit_usd"]
    protocol = row["protocol"]
    tags = set(row["tags"])

    if kind in ("jit", "jit_arb"):
        return (
            "unknown",
            "JIT liquidity (profit NULL in baseline): the vendor endpoint is "
            "arb-only and cannot see JIT by design",
        )
    if (
        kind in NON_ARB_KINDS
        or (protocol is not None and protocol in UTILITY_PROTOCOLS)
        or tags & UTILITY_TAGS
    ):
        detail = ", ".join(
            part
            for part in (
                f"protocol={protocol}" if protocol else "",
                f"tags={','.join(sorted(tags))}" if tags else "",
            )
            if part
        )
        return (
            "unknown",
            f"utility/non-arb flow ({detail}): vendor oracle is arb-only, blind by design",
        )
    if kind == "arb_atomic":
        vendor_floor = (
            f"vendor lists comparable dust down to ${vendor_min_profit:.6f} and has no size floor"
            if vendor_min_profit is not None
            else "vendor floor unknown"
        )
        net_text = f"${net:.6f}" if net is not None else "NULL"
        if profit is None:
            return (
                "fp_suspect",
                "arb_atomic with NULL local profit: a zero/NULL quote is not a "
                "price (A14 checkpoint); observed suspect, not a proven false positive",
            )
        if profit <= ZERO_PROFIT_USD:
            return (
                "fp_suspect",
                f"single-unit/rounding residue (profit ${profit:.6f}, net {net_text}): "
                "~$0, so this is an observed suspect, not a proven false positive",
            )
        if net is not None and net < 0:
            return (
                "fp_suspect",
                f"positive gross but negative net (profit ${profit:.6f}, net {net_text}): "
                "observed suspect, not a proven false positive",
            )
        if profit < SUGGEST_DUST_PROFIT_USD:
            return (
                "tp_vendor_miss",
                f"realized dust arb (profit ${profit:.6f}, net {net_text}); "
                f"{vendor_floor}, so the miss is a vendor coverage gap, not a false positive",
            )
        return (
            "tp_vendor_miss",
            f"material realized arb (profit ${profit:.6f}, net {net_text}); "
            f"{vendor_floor}, so a size floor cannot explain the miss",
        )
    return ("unknown", f"no baseline rule for kind={kind}")


def load_adjudications(
    path: Path,
) -> tuple[dict[str, dict[str, str]], dict[str, Any]]:
    if not path.is_file():
        die(
            f"adjudications file not found: {path}",
            "create it with the frozen --label file, or drop --adjudications "
            "to use the built-in rules",
        )
    try:
        raw = json.loads(path.read_text(encoding="utf-8"))
    except json.JSONDecodeError as exc:
        die(f"adjudications file is not valid JSON: {path} ({exc})")

    rows: Any
    if not isinstance(raw, dict):
        die(
            f"adjudications file must be an object: {path}",
            'expected {"rows": {"0x<64 hex>": {"adjudication": "...", "note": "..."}}}',
        )
    if isinstance(raw.get("rows"), dict):
        rows = raw["rows"]
    elif "rows" in raw:
        die(f"adjudications file has a non-object 'rows': {path}")
    else:
        rows = {k: v for k, v in raw.items() if norm_hash(k)}

    metadata = {
        "snapshot_sha256": raw.get("snapshot_sha256"),
        "block_range": raw.get("block_range"),
        "label": raw.get("label"),
        "local_commit": raw.get("local_commit"),
    }

    out: dict[str, dict[str, str]] = {}
    for key, value in rows.items():
        tx_hash = norm_hash(key)
        if not tx_hash:
            die(f"adjudications file has a malformed hash key: {key!r}")
        if not isinstance(value, dict):
            die(f"adjudications entry for {tx_hash} must be an object, got {type(value).__name__}")
        label = value.get("adjudication")
        if label not in ADJUDICATIONS:
            die(
                f"adjudications entry for {tx_hash} has invalid adjudication {label!r}",
                f"allowed values: {', '.join(ADJUDICATIONS)}",
            )
        note = value.get("note")
        out[tx_hash] = {
            "adjudication": str(label),
            "note": str(note) if note else "",
        }
    return out, metadata


def validate_adjudications_provenance(
    path: Path,
    metadata: dict[str, Any],
    snapshot_sha256: str,
    lo: int,
    hi: int,
) -> None:
    """Fail when frozen labels were captured against a different oracle run.

    Reusing the baseline adjudications is only sound against the same frozen
    snapshot and block range; otherwise a label silently describes another
    window.  A file without provenance metadata is accepted with a warning so
    hand-written maps still work.
    """
    recorded_sha = metadata.get("snapshot_sha256")
    recorded_range = metadata.get("block_range")
    if not recorded_sha and recorded_range is None:
        warn(
            f"{path} records no snapshot_sha256/block_range; "
            "cannot prove the adjudications match this run's frozen artifact"
        )
        return
    if recorded_sha is not None:
        if not isinstance(recorded_sha, str) or recorded_sha.strip().lower() != snapshot_sha256.lower():
            die(
                f"adjudications drift: {path} was frozen against snapshot "
                f"{recorded_sha!r}, but this run's snapshot SHA256 is {snapshot_sha256}",
                "compare against the snapshot the adjudications were frozen with, "
                "or re-freeze a reviewed adjudications file for the new snapshot; "
                "stale labels must not be applied to a different artifact",
            )
    if recorded_range is not None:
        if not isinstance(recorded_range, dict):
            die(f"adjudications file has a non-object 'block_range': {path}")
        raw_lo = recorded_range.get("lo")
        raw_hi = recorded_range.get("hi")
        try:
            block_lo, block_hi = int(raw_lo), int(raw_hi)
        except (TypeError, ValueError):
            die(
                f"adjudications file has a malformed block_range: {recorded_range!r}",
                'expected {"block_range": {"lo": <int>, "hi": <int>}}',
            )
        if (block_lo, block_hi) != (lo, hi):
            die(
                f"adjudications drift: {path} was frozen for blocks "
                f"{block_lo}..={block_hi}, but this run is {lo}..={hi}",
                "the baseline is range-specific: use the adjudications frozen for "
                "this exact range, or re-freeze a reviewed file for the new range",
            )


def adjudicate_local_only(
    rows: list[dict[str, Any]],
    vendor_min_profit: float | None,
    overrides: dict[str, dict[str, str]] | None,
) -> tuple[list[dict[str, Any]], dict[str, int], list[str], list[str]]:
    adjudicated: list[dict[str, Any]] = []
    counts: dict[str, int] = {label: 0 for label in ADJUDICATIONS}
    used: set[str] = set()

    for row in rows:
        label, note = auto_adjudicate(row, vendor_min_profit)
        source = "auto"
        if overrides is not None:
            override = overrides.get(row["hash"])
            if override is not None:
                label = override["adjudication"]
                note = override["note"] or note
                source = "frozen"
                used.add(row["hash"])
        counts[label] += 1
        adjudicated.append(
            {
                "hash": row["hash"],
                "block_number": row["block_number"],
                "kind": row["kind"],
                "confidence": row["confidence"],
                "profit_usd": row["profit_usd"],
                "net_profit_usd": row["net_profit_usd"],
                "eoa": row["eoa"],
                "detector": row["detector"],
                "protocol": row["protocol"],
                "tags": row["tags"],
                "adjudication": label,
                "adjudication_source": source,
                "note": note,
            }
        )

    stale = sorted(set(overrides or {}) - used)
    missing = sorted(row["hash"] for row in adjudicated if row["adjudication_source"] == "auto")
    return adjudicated, counts, stale, (missing if overrides is not None else [])


# --------------------------------------------------------------------------
# Output
# --------------------------------------------------------------------------


def fmt_money(value: float | None) -> str:
    if value is None:
        return "NULL"
    if value == 0:
        return "$0"
    if abs(value) < 0.01:
        return f"${value:.6f}"
    return f"${value:.4f}"


def fmt_pct(value: float | None) -> str:
    return "n/a" if value is None else f"{value:+.3f}%"


def render_markdown(report: dict[str, Any]) -> str:
    counts = report["counts"]
    metrics = report["metrics"]
    stats = report["profit_delta_pct"]
    lines: list[str] = []
    label = report["label"]
    rng = report["block_range"]

    lines.append(f"<!-- compare_biglabs.py v{report['script_version']} label={label} -->")
    lines.append("")
    baseline = report.get("baseline", {})
    if baseline.get("incomplete"):
        allowed = " (allowed by --allow-incomplete)" if baseline.get("allow_incomplete") else ""
        lines.append(
            f"**INCOMPLETE: blocks classified {baseline.get('blocks_classified')}/"
            f"{rng['blocks']} - this DB does not cover the requested range, "
            f"so this table is NOT a comparable checkpoint{allowed}.**"
        )
        lines.append("")
    lines.append(f"Snapshot: `{display_path(report['inputs']['snapshot'])}`  ")
    lines.append(f"SHA256: `{report['inputs']['snapshot_sha256']}`  ")
    lines.append(
        f"Vendor fetch: fetched_at `{report['inputs']['vendor_fetch'].get('fetched_at', 'n/a')}`, "
        f"newest vendor block time `{report['inputs']['vendor_fetch'].get('newest_vendor_time', 'n/a')}`, "
        f"pages {report['inputs']['vendor_fetch'].get('page_count', 'n/a')}  "
    )
    lines.append(
        f"Local index: `{display_path(report['inputs']['db'])}` (code commit "
        f"{report['inputs'].get('local_commit') or 'unrecorded'})  "
    )
    adjudications = report["inputs"].get("adjudications")
    if adjudications:
        lines.append(f"Adjudications: frozen map `{display_path(adjudications)}`  ")
    else:
        lines.append("Adjudications: built-in suggestions (no `--adjudications` file)  ")
    lines.append(f"Block range: {rng['lo']}..={rng['hi']} ({rng['blocks']} blocks)")
    if baseline:
        lines.append(
            f"Index facts: blocks classified {baseline.get('blocks_classified')}/{rng['blocks']}, "
            f"rejected_candidates {baseline.get('rejected_candidates')}"
        )
    lines.append("")
    lines.append("| metric | value |")
    lines.append("|--------|-------|")
    lines.append(f"| vendor txs (unique hashes) | {counts['vendor_unique_hashes']} |")
    lines.append(f"| local mev_ops rows (unique hashes) | {counts['local_rows']} ({counts['local_unique_hashes']}) |")
    lines.append(
        f"| intersection V n L | {counts['intersection']} |"
    )
    lines.append(
        f"| recall_vs_vendor | {metrics['recall_vs_vendor']:.4f} ({metrics['recall_detail']}) |"
    )
    lines.append(
        f"| vendor_agreement (lower bound) | {metrics['vendor_agreement_lower_bound']:.4f} "
        f"({metrics['vendor_agreement_detail']}) |"
    )
    lines.append(f"| L\\V | {counts['local_only']} |")
    lines.append(f"| V\\L | {counts['vendor_only']} |")
    lines.append(
        "| matched rows: kind we assigned | "
        + ", ".join(f"{k}={v}" for k, v in metrics["matched_kind_labels"].items())
        + " |"
    )
    lines.append(
        "| delta on V n L: median / p90 / worst / best | "
        f"{fmt_pct(stats.get('median'))} / {fmt_pct(stats.get('p90'))} / "
        f"{fmt_pct(stats.get('worst'))} / {fmt_pct(stats.get('best'))} "
        f"(n={stats['n']}, skipped vendor null/zero={stats['skipped_vendor_null_or_zero']}, "
        f"skipped local null={stats['skipped_local_null']}) |"
    )
    lines.append("")
    lines.append("L\\V composition: " + ", ".join(
        f"{k}={v}" for k, v in metrics["local_only_kind_composition"].items()
    ))
    lines.append("")
    lines.append("Adjudication counts: " + ", ".join(
        f"`{k}`={v}" for k, v in report["adjudication_counts"].items()
    ))
    lines.append("")
    lines.append("| block | tx | kind | conf | local profit | net | adjudication | note |")
    lines.append("|-------|----|------|------|--------------|-----|--------------|------|")
    for row in report["local_only"]:
        lines.append(
            "| {block} | `{tx}` | {kind} | {conf} | {profit} | {net} | `{adj}` | {note} |".format(
                block=row["block_number"],
                tx=short_hash(row["hash"]),
                kind=row["kind"],
                conf=row["confidence"],
                profit=fmt_money(row["profit_usd"]),
                net=fmt_money(row["net_profit_usd"]),
                adj=row["adjudication"],
                note=row["note"].replace("|", "\\|"),
            )
        )
    lines.append("")
    return "\n".join(lines)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        description=(
            "Compare a local explorer index against the frozen BigLabs arb "
            "snapshot (external coverage oracle, Task M0)."
        )
    )
    parser.add_argument("--db", required=True, type=Path, help="explorer index sqlite DB (read-only)")
    parser.add_argument("--snapshot", required=True, type=Path, help="frozen vendor snapshot JSON")
    parser.add_argument("--chain", required=True, help="chain name; must match the snapshot's meta.chain")
    parser.add_argument("--lo", required=True, type=int, help="first block of the frozen window")
    parser.add_argument("--hi", required=True, type=int, help="last block of the frozen window")
    parser.add_argument("--label", required=True, help="baseline label, e.g. 'baseline' (names the report)")
    parser.add_argument(
        "--adjudications",
        type=Path,
        default=None,
        help="frozen L\\V adjudication map (JSON); rows win over the built-in rules",
    )
    parser.add_argument(
        "--out",
        type=Path,
        default=None,
        help="machine report path (default: <repo>/cache/oracle-report-<label>.json)",
    )
    parser.add_argument(
        "--local-commit",
        default=None,
        help="commit hash of the binary that built --db (recorded in the report)",
    )
    parser.add_argument(
        "--allow-incomplete",
        action="store_true",
        help="allow a DB that does not cover the whole requested range (exit 0, table marked INCOMPLETE)",
    )
    parser.add_argument(
        "--quiet",
        action="store_true",
        help="only write the machine report; skip the markdown table",
    )
    args = parser.parse_args(argv)

    if args.lo > args.hi:
        die(f"--lo ({args.lo}) is greater than --hi ({args.hi})")
    if not args.label or any(c.isspace() for c in args.label):
        die(f"--label must be a non-empty token without whitespace: {args.label!r}")

    snapshot_path = args.snapshot.resolve()
    db_path = args.db.resolve()
    out_path = (
        args.out.resolve()
        if args.out is not None
        else (REPO_ROOT / "cache" / f"oracle-report-{args.label}.json")
    )
    adjudications_path = args.adjudications.resolve() if args.adjudications else None

    snapshot_sha256 = sha256_file(snapshot_path) if snapshot_path.is_file() else ""
    meta, vendor_rows, snapshot_notes = load_snapshot(snapshot_path)

    snapshot_chain = str(meta.get("chain") or "").lower()
    if snapshot_chain and snapshot_chain != args.chain.lower():
        die(
            f"chain mismatch: snapshot is '{snapshot_chain}', --chain is '{args.chain}'",
            "compare a chain against its own frozen snapshot",
        )

    vendor_lo = min(r["block_number"] for r in vendor_rows)
    vendor_hi = max(r["block_number"] for r in vendor_rows)
    if (vendor_lo, vendor_hi) != (args.lo, args.hi):
        die(
            f"range mismatch: snapshot covers blocks {vendor_lo}..={vendor_hi}, "
            f"but --lo/--hi is {args.lo}..={args.hi}",
            "the baseline is range-specific: pass the snapshot's own min/max "
            "blockNumber (the frozen range), not a re-chosen window",
        )

    conn = open_db(db_path)
    try:
        local_rows = load_local(conn, args.lo, args.hi)
        blocks_classified = conn.execute(
            "SELECT COUNT(*) FROM blocks_classified WHERE block BETWEEN ? AND ?",
            (args.lo, args.hi),
        ).fetchone()[0]
        rejected = conn.execute(
            "SELECT COUNT(*) FROM rejected_candidates WHERE block_number BETWEEN ? AND ?",
            (args.lo, args.hi),
        ).fetchone()[0]
    except sqlite3.Error as exc:
        die(f"query failed on {db_path}: {exc}")
    finally:
        conn.close()

    result = compute_metrics(vendor_rows, local_rows)
    counts = result["counts"]
    metrics = result["metrics"]
    local_only_hashes: list[str] = result["local_only_hashes"]

    vendor_profits = [r["usd_profit"] for r in vendor_rows if r["usd_profit"] is not None]
    vendor_min_profit = min(vendor_profits) if vendor_profits else None

    overrides = None
    adjudications_meta: dict[str, Any] = {}
    if adjudications_path:
        overrides, adjudications_meta = load_adjudications(adjudications_path)
        validate_adjudications_provenance(
            adjudications_path, adjudications_meta, snapshot_sha256, args.lo, args.hi
        )
    local_only_rows = sorted(
        result["local_only_rows"], key=lambda r: (r["block_number"], r["hash"])
    )
    adjudicated, adjudication_counts, stale, missing = adjudicate_local_only(
        local_only_rows, vendor_min_profit, overrides
    )

    blocks_requested = args.hi - args.lo + 1
    incomplete = blocks_classified < blocks_requested
    if incomplete:
        detail = (
            f"blocks classified {blocks_classified}/{blocks_requested}; the DB does not "
            f"cover the requested range"
        )
        if args.allow_incomplete:
            warn(f"INCOMPLETE (allowed by --allow-incomplete): {detail}")
        else:
            warn(f"INCOMPLETE: {detail}; refusing to emit a comparable checkpoint (use --allow-incomplete to override)")

    if stale:
        warn(
            f"{len(stale)} adjudication entr(y/ies) in {adjudications_path} no longer "
            f"appear in L\\V (rows fixed, or the detector changed): {', '.join(short_hash(h) for h in stale[:5])}"
        )
    if missing:
        warn(
            f"{len(missing)} L\\V row(s) are not in {adjudications_path}; used the built-in "
            f"rules for them: {', '.join(short_hash(h) for h in missing[:5])}"
        )

    report: dict[str, Any] = {
        "script": "scripts/compare_biglabs.py",
        "script_version": SCRIPT_VERSION,
        "label": args.label,
        "chain": args.chain.lower(),
        "block_range": {"lo": args.lo, "hi": args.hi, "blocks": blocks_requested},
        "baseline": {
            "blocks_classified": blocks_classified,
            "blocks_requested": blocks_requested,
            "incomplete": incomplete,
            "allow_incomplete": bool(args.allow_incomplete),
            "rejected_candidates": rejected,
            "note": "rejected_candidates is 0 unless record_rejections was enabled at index time",
        },
        "inputs": {
            "db": str(db_path),
            "snapshot": str(snapshot_path),
            "snapshot_sha256": snapshot_sha256,
            "adjudications": str(adjudications_path) if adjudications_path else None,
            "adjudications_sha256": sha256_file(adjudications_path)
            if adjudications_path and adjudications_path.is_file()
            else None,
            "adjudications_provenance": adjudications_meta,
            "local_commit": args.local_commit,
            "vendor_fetch": {
                key: meta.get(key)
                for key in (
                    "fetched_at",
                    "url",
                    "chain",
                    "per_page",
                    "pages_fetched",
                    "page_count",
                    "window_secs",
                    "window_start",
                    "window_end",
                    "newest_vendor_time",
                    "records",
                    "block_lo",
                    "block_hi",
                )
                if key in meta
            },
            "snapshot_page_count": meta.get("page_count") or meta.get("pages_fetched"),
        },
        "counts": counts,
        "metrics": {
            key: value
            for key, value in metrics.items()
            if key not in ("vendor_only_hashes",)
        }
        | {"vendor_only": len(metrics["vendor_only_hashes"])},
        "profit_delta_pct": result["profit_delta_pct"],
        "matched": result["matched"],
        "vendor_only_hashes": metrics["vendor_only_hashes"],
        "local_only": adjudicated,
        "adjudication_counts": adjudication_counts,
        "adjudications_stale": stale,
        "adjudications_missing": missing,
        "snapshot_notes": snapshot_notes,
    }

    out_path.parent.mkdir(parents=True, exist_ok=True)
    out_path.write_text(json.dumps(report, indent=2, ensure_ascii=False) + "\n", encoding="utf-8")

    if not args.quiet:
        print(render_markdown(report))

    print(f"wrote {out_path}", file=sys.stderr)
    if incomplete and not args.allow_incomplete:
        print(
            f"compare_biglabs: INCOMPLETE: blocks classified {blocks_classified}/"
            f"{blocks_requested}; refusing to treat this table as a comparable checkpoint "
            "(re-index the full range, or pass --allow-incomplete)",
            file=sys.stderr,
        )
        return 3
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
