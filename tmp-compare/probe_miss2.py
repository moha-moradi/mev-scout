import json, urllib.request
h="0x3c648059183848ab78c73fb94f6b61ac50109a78e67dc992433908c9e99451a2"
rpc="https://api.avax.network/ext/bc/C/rpc"
req=urllib.request.Request(rpc,data=json.dumps({"jsonrpc":"2.0","id":1,"method":"eth_getTransactionReceipt","params":[h]}).encode(),headers={"Content-Type":"application/json"})
rc=json.loads(urllib.request.urlopen(req,timeout=60).read())["result"]
tx=json.loads(urllib.request.urlopen(urllib.request.Request(rpc,data=json.dumps({"jsonrpc":"2.0","id":1,"method":"eth_getTransactionByHash","params":[h]}).encode(),headers={"Content-Type":"application/json"}),timeout=60).read())["result"]
sender=tx["from"].lower(); to=tx["to"].lower()
TRANSFER="ddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef"
ledger={}
def add(addr,tok,amt,sign):
    key=(addr,tok); ledger[key]=ledger.get(key,0)+sign*amt
for i,l in enumerate(rc["logs"]):
    t0=l["topics"][0][2:].lower()
    if t0!=TRANSFER: 
        print("swap",i,l["address"][-8:])
        continue
    tok=l["address"].lower()
    frm="0x"+l["topics"][1][-40:].lower()
    too="0x"+l["topics"][2][-40:].lower()
    amt=int(l["data"],16)
    add(frm,tok,amt,-1); add(too,tok,amt,+1)
    print("xfer",i,tok[-8:], frm[-8:], "->", too[-8:], amt)
print("sender",sender[-8:],"to",to[-8:])
for (a,t),v in sorted(ledger.items()):
    if a in (sender,to) and v!=0:
        print("NET",a[-8:],t[-8:],v)
print("contract+eoa usdc", ledger.get((to,"0xb97ef9ef8734c71904d8002f8b6bc66dd9c48a6e"),0)+ledger.get((sender,"0xb97ef9ef8734c71904d8002f8b6bc66dd9c48a6e"),0))