const https = require("https");
const { URL } = require("url");
function call(rpc, method, params) {
  const url = new URL(rpc);
  const body = JSON.stringify({ jsonrpc: "2.0", id: 1, method, params });
  return new Promise((resolve, reject) => {
    const req = https.request({ hostname: url.hostname, port: 443, path: url.pathname + url.search, method: "POST",
      headers: { "Content-Type": "application/json", "Content-Length": Buffer.byteLength(body) } },
      (res) => { let d=""; res.on("data",c=>d+=c); res.on("end",()=>{ try{ const j=JSON.parse(d); if(j.error) reject(new Error(JSON.stringify(j.error))); else resolve(j.result);}catch(e){reject(new Error(d.slice(0,200)));}});});
    req.setTimeout(30000, () => req.destroy(new Error("timeout")));
    req.on("error", reject); req.write(body); req.end();
  });
}
const AAVE = "0x6807dc923806fE8Fd134338EABCA509979a7e0cB";
const AAVE_LIQ = "0xe413a321e8681d831f4dbccbca790d2952b56f977908e45be37335533e005286";
const COMP_LIQ = "0x298637f684da70674f26509b10f07ec2fbc77a335ab1e7d6215a4b2484d8bb52";
const vUSDC = "0xeca88125a5adbe82614ffc12d0db554e2e2867c8";
const RPC = "https://bsc-dataseed.binance.org";
(async () => {
  const tip = parseInt(await call(RPC, "eth_blockNumber", []), 16);
  console.log("dataseed tip", tip);
  // try small ranges going back
  let hits = [];
  for (let end = tip; end > tip - 5000 && hits.length < 3; end -= 100) {
    const fr = end - 99;
    try {
      const logs = await call(RPC, "eth_getLogs", [{ fromBlock:"0x"+fr.toString(16), toBlock:"0x"+end.toString(16), address: AAVE, topics:[AAVE_LIQ] }]);
      if (logs.length) { hits = hits.concat(logs); console.log("AAVE HIT", parseInt(logs[0].blockNumber,16), logs[0].transactionHash); }
    } catch (e) {
      console.log("fail aave", fr, String(e.message).slice(0,100));
      break;
    }
    await new Promise(r => setTimeout(r, 200));
  }
  console.log("aave total", hits.length);
  let vhits = [];
  for (let end = tip; end > tip - 5000 && vhits.length < 3; end -= 100) {
    const fr = end - 99;
    try {
      const logs = await call(RPC, "eth_getLogs", [{ fromBlock:"0x"+fr.toString(16), toBlock:"0x"+end.toString(16), address: vUSDC, topics:[COMP_LIQ] }]);
      if (logs.length) { vhits = vhits.concat(logs); console.log("VENUS HIT", parseInt(logs[0].blockNumber,16), logs[0].transactionHash); }
    } catch (e) {
      console.log("fail venus", fr, String(e.message).slice(0,100));
      break;
    }
    await new Promise(r => setTimeout(r, 200));
  }
  console.log("venus total", vhits.length);
})().catch(e=>{console.error(e);process.exit(1);});