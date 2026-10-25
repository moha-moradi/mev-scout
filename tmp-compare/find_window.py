import urllib.request, json, time, sys

RPC_URLS = [
    "https://avalanche-c-chain-rpc.publicnode.com",
    "https://lb.drpc.live/avalanche/AlEVHe8j40WBrjJdhDUkokAKhSn-rBQR8bjj0neqFxjy",
]

def call(url, method, params):
    data = json.dumps({"jsonrpc":"2.0","method":method,"params":params,"id":1})
    req = urllib.request.Request(url, data=data.encode(),
        headers={"Content-Type":"application/json","User-Agent":"mev-scout-window"})
    with urllib.request.urlopen(req, timeout=60) as r:
        return json.loads(r.read())

def find_working_rpc():
    for u in RPC_URLS:
        try:
            res = call(u, "eth_blockNumber", [])
            if res.get("result"):
                return u, int(res["result"], 16)
        except Exception as e:
            print("rpc", u, "FAILED:", e)
    sys.exit("no RPC reachable")

RPC, head = find_working_rpc()
print("RPC:", RPC, "head block:", head, "head ts:", time.ctime(head))

def blk(num):
    res = call(RPC, "eth_getBlockByNumber", [hex(num), True])
    b = res["result"]
    return (int(b["number"],16), int(b["timestamp"],16), b["hash"])

_, head_ts, _ = blk(head)
print("head ts:", head_ts, time.ctime(head_ts))
target = head_ts - 3600
print("1h-ago ts:", time.ctime(target))

# sanity bounds
lo, hi = head-4000, head
bl, tl, _ = blk(lo)
bh, th, _ = blk(hi)
print("bounds lo=%d ts=%s  hi=%d ts=%s" % (lo, time.ctime(tl), hi, time.ctime(th)))

# binary search: last block with ts <= target
a, b = lo, hi
while b - a > 6:
    m = (a+b)//2
    _, t, _ = blk(m)
    if t <= target:
        a = m
    else:
        b = m
# a: ts<=target, b: ts>target (first too-new). b-1 is last under target.
last_under = b-1
n = last_under
_, tn, _ = blk(n)
_, tnext, _ = blk(n+1)
print("last block ts<=target: block %d ts=%s" % (n, time.ctime(tn)))
print("next block:           block %d ts=%s" % (n+1, time.ctime(tnext)))

# window: start a little earlier for safety margin, end at head-4 (confirmations)
from_block = n - 20
to_block = head - 4
print("\nPROPOSED WINDOW from=%d to=%d  (span=%d)" % (from_block, to_block, to_block-from_block+1))
print("from ts: ", time.ctime(blk(from_block)[1]))
print("to   ts: ", time.ctime(blk(to_block)[1]))

# also print the API-recent items block range for sanity: 98498599..98501866
sys.stdout.flush()
open("D:/gitlab.dte.repo/mev-scout/tmp-compare/window.txt","w").write(
    "from %d to %d\n" % (from_block, to_block)
)
