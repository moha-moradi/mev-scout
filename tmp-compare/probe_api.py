import json, urllib.request
base = "https://gateway.biglabs.eu/api/avalanche/arbitrages"
headers = {
  "User-Agent": "Mozilla/5.0",
  "Origin": "https://explorer.mev.zone",
  "Referer": "https://explorer.mev.zone/mevlive",
  "Accept": "application/json",
}
urls = [
  base + "?page=1&limit=5",
  base + "?page=1",
  base + "?limit=3&sort=-block",
  base,
]
for u in urls:
    print("\n====", u)
    try:
        req = urllib.request.Request(u, headers=headers)
        with urllib.request.urlopen(req, timeout=30) as r:
            body = r.read()
            print("status", r.status, "len", len(body), "ctype", r.headers.get("content-type"))
            text = body.decode("utf-8", "replace")
            print(text[:1500])
    except Exception as e:
        print(type(e).__name__, e)
        if hasattr(e, "read"):
            print(e.read()[:500])
