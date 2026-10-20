"""Pin a 1800-block Avalanche window from the biglabs arbitrage feed."""
from __future__ import annotations

import json
import urllib.request
from pathlib import Path

HERE = Path(__file__).resolve().parent
BASE = "https://gateway.biglabs.eu/api/avalanche/arbitrages"
HEADERS = {
    "User-Agent": "Mozilla/5.0",
    "Origin": "https://explorer.mev.zone",
    "Referer": "https://explorer.mev.zone/mevlive",
    "Accept": "application/json",
}
WINDOW = 1800


def fetch_page(page, per_page=30):
    url = "%s?per_page=%s&page=%s&sortBy=-created_at" % (BASE, per_page, page)
    req = urllib.request.Request(url, headers=HEADERS)
    with urllib.request.urlopen(req, timeout=120) as r:
        data = json.loads(r.read())
    if isinstance(data, dict):
        data = data.get("data") or data.get("items") or data.get("results") or []
    return data if isinstance(data, list) else []


def main():
    page1 = fetch_page(1)
    if not page1:
        raise SystemExit("empty page 1")
    to_block = max(int(r["blockNumber"]) for r in page1)
    from_block = to_block - (WINDOW - 1)
    print("pinned from=%s to=%s span=%s" % (from_block, to_block, to_block - from_block + 1))
    by_hash = {}
    page = 1
    while True:
        rows = fetch_page(page)
        if not rows:
            break
        min_bn = min(int(r["blockNumber"]) for r in rows)
        for r in rows:
            bn = int(r["blockNumber"])
            if from_block <= bn <= to_block:
                by_hash[r["hash"].lower()] = r
        print("page %s: %s rows min_bn=%s collected=%s" % (page, len(rows), min_bn, len(by_hash)))
        if min_bn < from_block:
            break
        page += 1
        if page > 200:
            raise SystemExit("pagination runaway")
    rows = sorted(by_hash.values(), key=lambda r: int(r["blockNumber"]), reverse=True)
    out = HERE / "mevlive-window.json"
    meta = HERE / "window-meta.json"
    out.write_text(json.dumps(rows, indent=2), encoding="utf-8")
    meta.write_text(json.dumps({"from_block": from_block, "to_block": to_block, "feed_rows": len(rows), "newest_time": rows[0].get("time") if rows else None, "oldest_time": rows[-1].get("time") if rows else None}, indent=2), encoding="utf-8")
    print("wrote %s rows=%s" % (out, len(rows)))
    print("wrote %s" % meta)


if __name__ == "__main__":
    main()