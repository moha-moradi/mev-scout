import json, urllib.request
h="0x3c648059183848ab78c73fb94f6b61ac50109a78e67dc992433908c9e99451a2"
rpc="https://api.avax.network/ext/bc/C/rpc"
req=urllib.request.Request(rpc,data=json.dumps({"jsonrpc":"2.0","id":1,"method":"eth_getTransactionReceipt","params":[h]}).encode(),headers={"Content-Type":"application/json"})
rc=json.loads(urllib.request.urlopen(req,timeout=60).read())["result"]
txreq=urllib.request.Request(rpc,data=json.dumps({"jsonrpc":"2.0","id":1,"method":"eth_getTransactionByHash","params":[h]}).encode(),headers={"Content-Type":"application/json"})
tx=json.loads(urllib.request.urlopen(txreq,timeout=60).read())["result"]
sender=tx["from"].lower()
to=(tx.get("to") or "").lower()
print("from",sender,"to",to)
USDC="0xb97ef9ef8734c71904d8002f8b6bc66dd9c48a6e"
BTC="0x152b9d0fdc40c096757f570a51e494bd4b943e50"
TRANSFER="0xddf252ad1be2c89b069c2c784c0669f15ba976d55f2bfc423923f2c8b436b9dd"
ledger={}
def add(addr, tok, amt, sign):
    key=(addr.lower(), tok.lower())
    ledger[key]=ledger.get(key,0)+sign*amt
for l in rc["logs"]:
    if not l["topics"] or l["topics"][0].lower()!=TRANSFER:
        continue
    tok=l["address"].lower()
    frm="0x"+l["topics"][1][-40:]
    too="0x"+l["topics"][2][-40:]
    amt=int(l["data"],16)
    add(frm,tok,amt,-1)
    add(too,tok,amt,+1)
    print("xfer", tok[-8:], frm[-8:], "->", too[-8:], amt)
print("--- nets ---")
for (a,t),v in sorted(ledger.items()):
    if v!=0:
        print(a[-8:], t[-8:], v)
print("feed delta", 147250550-147207300)
print("searcher usdc", ledger.get((sender,USDC),0), ledger.get((to,USDC),0))