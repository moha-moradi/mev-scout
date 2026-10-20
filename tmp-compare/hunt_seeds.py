import json, urllib.request
RPC = "https://bsc-dataseed.binance.org"
TOPIC_AAVE = "0xe413a321e8681d831f4dbccbca790d2952b56f977908e45be37335533e005286"
TOPIC_COMP = "0x298637f684da70674f26509b10f07ec2fbc77a335ab1e7d6215a4b2484d8bb52"
AAVE = "0x6807dc923806fe8fd134338eabca509979a7e0cb"
markets = open("venus-markets.txt").read().split()[:15]
def rpc(method, params):
    body = json.dumps({"jsonrpc":"2.0","id":1,"method":method,"params":params}).encode()
    req = urllib.request.Request(RPC, data=body, headers={"content-type":"application/json"})
    with urllib.request.urlopen(req, timeout=45) as r: j = json.load(r)
    if "error" in j: raise RuntimeError(j["error"])
    return j["result"]
tip = int(rpc("eth_blockNumber", []), 16)
print("tip", tip)
for f in range(tip - 9000, tip, 1000):
    t = min(f + 999, tip)
    try:
        r = rpc("eth_getLogs", [{"fromBlock":hex(f),"toBlock":hex(t),"address":AAVE,"topics":[TOPIC_AAVE]}])
        if r:
            print("aave", int(r[0]["blockNumber"],16), r[0]["transactionHash"]); break
    except Exception as e:
        print("aave err", f, e)
else:
    print("aave none")
for f in range(tip - 9000, tip, 1000):
    t = min(f + 999, tip)
    try:
        r = rpc("eth_getLogs", [{"fromBlock":hex(f),"toBlock":hex(t),"address":markets,"topics":[TOPIC_COMP]}])
        if r:
            print("venus", int(r[0]["blockNumber"],16), r[0]["address"], r[0]["transactionHash"]); break
    except Exception as e:
        print("ven err", f, e)
else:
    print("venus none")
