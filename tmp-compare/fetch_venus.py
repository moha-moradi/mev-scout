import json, urllib.request
RPC = "https://bsc-dataseed.binance.org"
markets = open("venus-markets.txt").read().split()
def rpc(method, params):
    body = json.dumps({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}).encode()
    req = urllib.request.Request(RPC, data=body, headers={"content-type": "application/json"})
    with urllib.request.urlopen(req, timeout=30) as r:
        j = json.load(r)
    if "error" in j:
        raise SystemExit(j["error"])
    return j["result"]
under = []
for m in markets:
    try:
        ret = rpc("eth_call", [{"to": m, "data": "0x6f307dc3"}, "latest"])
        u = ("0x" + ret[-40:]) if ret and len(ret) >= 66 else None
    except Exception:
        u = None
    under.append([m, u])
    print(m, u)
json.dump(under, open("venus-underlyings.json", "w"), indent=2)
print("ok", sum(1 for _, u in under if u), "/", len(under))
