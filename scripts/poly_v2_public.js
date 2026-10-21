const https = require("https");
const RPC = "https://polygon-bor-rpc.publicnode.com";
const V2 = "0x8dFf5E27EA6b7AC08EbFdf9eB090F32ee9a30fcf";
const V3 = "0x794a61358D6845594F94dc1DB02A252b5b4814aD";
const LIQ = "0xe413a321e8681d831f4dbccbca790d2952b56f977908e45be37335533e005286";
function call(method, params) {
  const body = JSON.stringify({ jsonrpc: "2.0", id: 1, method, params });
  const u = new URL(RPC);
  return new Promise((resolve, reject) => {
    const req = https.request({ hostname: u.hostname, path: u.pathname, method: "POST",
      headers: { "Content-Type": "application/json", "Content-Length": Buffer.byteLength(body) } },
      (res) => { let d=""; res.on("data",c=>d+=c); res.on("end",()=>{ try{ const j=JSON.parse(d); if(j.error) reject(new Error(JSON.stringify(j.error))); else resolve(j.result);}catch(e){reject(new Error(d.slice(0,200)));}}); });
    req.on("error", reject); req.write(body); req.end();
  });
}
(async () => {
  const tip = parseInt(await call("eth_blockNumber", []), 16);
  console.log("publicnode tip", tip);
  for (const [name, addr] of [["V3", V3], ["V2", V2]]) {
    let n = 0, sample = null;
    const from = tip - 20000;
    for (let f = from; f <= tip; f += 2000) {
      const t = Math.min(f + 1999, tip);
      try {
        const logs = await call("eth_getLogs", [{ fromBlock: "0x"+f.toString(16), toBlock: "0x"+t.toString(16), address: addr, topics: [LIQ] }]);
        if (logs.length) { n += logs.length; if (!sample) sample = { block: parseInt(logs[0].blockNumber,16), tx: logs[0].transactionHash }; }
      } catch (e) { console.log(name, "fail", f, String(e.message).slice(0,100)); }
    }
    console.log(name, "last20k", n, sample);
  }
})().catch(e => { console.error(e); process.exit(1); });