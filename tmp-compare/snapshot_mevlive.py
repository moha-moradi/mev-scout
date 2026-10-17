"""Fetch mevlive arbitrages API into a JSON file."""
from __future__ import annotations

import json
import sys
import urllib.request
from datetime import datetime, timezone
from pathlib import Path

URL = "https://gateway.biglabs.eu/api/avalanche/arbitrages?per_page=100&sortBy=-created_at"
out = Path(__file__).resolve().parent / (sys.argv[1] if len(sys.argv) > 1 else "mevlive-snap.json")

req = urllib.request.Request(
    URL,
    headers={
        "User-Agent": "Mozilla/5.0 (compatible; mev-scout-compare/1.0)",
        "Accept": "application/json",
        "Origin": "https://explorer.mev.zone",
        "Referer": "https://explorer.mev.zone/mevlive",
    },
)
with urllib.request.urlopen(req, timeout=60) as resp:
    data = json.loads(resp.read().decode("utf-8"))

payload = {
    "fetched_at": datetime.now(timezone.utc).isoformat(),
    "count": len(data),
    "items": data,
}
out.write_text(json.dumps(payload, indent=2), encoding="utf-8")
print(f"wrote {out} ({len(data)} rows)")
if data:
    print(f"newest block={data[0].get('blockNumber')} hash={data[0].get('hash')}")
    print(f"oldest block={data[-1].get('blockNumber')} hash={data[-1].get('hash')}")
