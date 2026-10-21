const https = require("https");
const { URL } = require("url");
const RPC = "https://api-bsc-mainnet-full.n.dwellir.com/87efcc6b-d1fd-48f8-8ca3-4c53ae7b732b";
function call(method, params) {
  const url = new URL(RPC);
  const body = JSON.stringify({ jsonrpc: "2.0", id: 1, method, params });
  const t0 = Date.now();
  return new Promise((resolve, reject) => {
    const req = https.request({ hostname: url.hostname, port: 443, path: url.pathname, method: "POST",
      headers: { "Content-Type": "application/json", "Content-Length": Buffer.byteLength(body) } },
      (res) => { let d=""; res.on("data",c=>d+=c); res.on("end",()=>{ console.log(method, "ms", Date.now()-t0, "bytes", d.length); try{ const j=JSON.parse(d); if(j.error) reject(new Error(JSON.stringify(j.error))); else resolve(j.result);}catch(e){reject(new Error(d.slice(0,200)));}});});
    req.setTimeout(20000, () => req.destroy(new Error("timeout")));
    req.on("error", reject); req.write(body); req.end();
  });
}
(async () => {
  const tip = parseInt(await call("eth_blockNumber", []), 16);
  for (let i = 0; i < 5; i++) {
    const bn = tip - i;
    try {
      const r = await call("eth_getBlockReceipts", ["0x"+bn.toString(16)]);
      console.log("block", bn, "receipts", r && r.length);
    } catch (e) { console.log("fail", bn, e.message); }
  }
})().catch(e=>{console.error(e); process.exit(1);});