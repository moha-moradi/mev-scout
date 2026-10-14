#!/usr/bin/env python3
import json
import urllib.request

URL = "https://avalanche-c-chain.publicnode.com"


def rpc(method, params):
    req = urllib.request.Request(
        URL,
        data=json.dumps({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}).encode(),
        headers={
            "Content-Type": "application/json",
            "User-Agent": "mev-scout-coverage/0.1",
        },
    )
    with urllib.request.urlopen(req, timeout=30) as resp:
        return json.load(resp)


tip = int(rpc("eth_blockNumber", [])["result"], 16)
print("tip", tip)
for delta in (1_000, 5_000, 20_000, 100_000, 500_000, 1_000_000):
    b = tip - delta
    r = rpc("eth_getBlockByNumber", [hex(b), False])
    print(f"tip-{delta} ({b}): {r.get('result') is not None}")
