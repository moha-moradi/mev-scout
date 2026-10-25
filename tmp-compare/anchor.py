import urllib.request, json, time, sys, gzip
from datetime import datetime, timezone

API = "https://gateway.biglabs.eu/api/avalanche/arbitrages?per_page=50&sortBy=-created_at&page={}"
RPC = "https://avalanche-c-chain-rpc.publicnode.com"
OUT_API_RAW = "tmp-compare/api_last_hour_raw.json"
OUT_API_WIN = "tmp-compare/api_window.json"
OUT_WINDOW = "tmp-compare/window.txt"

def http_json(url, timeout=60):
    h={"User-Agent":"mev-scout-anchor","Accept":"application/json","Accept-Encoding":"gzip"}
    req=urllib.request.Request(url, headers=h)
    with urllib.request.urlopen(req, timeout=timeout) as r:
        raw=r.read()
        if r.headers.get("Content-Encoding")=="gzip":
            raw=gzip.decompress(raw)
        return json.loads(raw)

def rpc(method, params):
    data=json.dumps({"jsonrpc":"2.0","method":method,"params":params,"id":1})
    req=urllib.request.Request(RPC, data=data.encode(),
        headers={"Content-Type":"application/json","User-Agent":"mev-scout-anchor"})
    return json.loads(urllib.request.urlopen(req, timeout=60).read())

# ---- head ----
head=int(rpc("eth_blockNumber",[])["result"],16)
hblk=rpc("eth_getBlockByNumber",[hex(head),True])["result"]
head_ts=int(hblk["timestamp"],16)
now_utc=datetime.fromtimestamp(head_ts,tz=timezone.utc).isoformat()
print("HEAD block=%d  ts=UTC %s"%(head, now_utc))

def bts(n):
    return int(rpc("eth_getBlockByNumber",[hex(n),True])["result"]["timestamp"],16)

target=head_ts-3600
print("1h-ago UTC: %s"%(datetime.fromtimestamp(target,tz=timezone.utc).isoformat()))
lo,hi=head-6000,head
a,b=lo,hi
while b-a>8:
    m=(a+b)//2
    t=bts(m)
    if t<=target: a=m
    else: b=m
last_under=b-1
from_block=last_under-20
to_block=head-4
print("PROPOSED WINDOW from=%d to=%d  span=%d  (from %s to %s)"%(
    from_block,to_block,to_block-from_block+1,
    datetime.fromtimestamp(bts(from_block),tz=timezone.utc).isoformat(),
    datetime.fromtimestamp(bts(to_block),tz=timezone.utc).isoformat()))

# ---- fetch API, paginate until oldest < from_block, max 30 pages ----
items={}
page=0
while page<30:
    page+=1
    try:
        batch=http_json(API.format(page))
    except Exception as e:
        print("page %d ERR: %s"%(page,e)); break
    if not isinstance(batch,list) or not batch:
        break
    for r in batch:
        items[r["hash"].lower()]=r
    oldest=min(int(x["blockNumber"]) for x in batch)
    print("page %d: blocks %d..%d (min block %d)"%(
        page, max(int(x["blockNumber"]) for x in batch),
        min(int(x["blockNumber"]) for x in batch), oldest))
    if oldest < from_block-100:
        break

# ---- filter to window ----
win=[r for r in items.values() if from_block<=int(r["blockNumber"])<=to_block]
win.sort(key=lambda x:-int(x["blockNumber"]))
print("\nAPI items in window: %d  (min block %s, max block %s)"% (
    len(win),
    min(int(r["blockNumber"]) for r in win) if win else None,
    max(int(r["blockNumber"]) for r in win) if win else None))

with open(OUT_API_RAW,"w") as f:
    json.dump(sorted(items.values(), key=lambda x:x["time"], reverse=True), f, indent=1, ensure_ascii=False)
with open(OUT_API_WIN,"w") as f:
    json.dump({"window":{"from_block":from_block,"to_block":to_block,"head":head,
        "head_ts_utc":now_utc,"target_utc":datetime.fromtimestamp(target,tz=timezone.utc).isoformat()},
        "n_in_window":len(win),"items":win}, f, indent=1, ensure_ascii=False)
with open(OUT_WINDOW,"w") as f:
    f.write("%d %d\n"%(from_block,to_block))
print("wrote",OUT_API_RAW,OUT_API_WIN,OUT_WINDOW)
print("\nall in-window (newest first):")
for r in win:
    path="->".join((t["symbol"] if "symbol" in t else "?") for t in r.get("path") or [])
    print("  blk=%d  %s  %s  %s  profit=%.6f"% (
        r["blockNumber"], r["time"], r["hash"][:12], path, float(r["usdProfit"])))
print("\ntotal API items fetched (all pages):", len(items))
