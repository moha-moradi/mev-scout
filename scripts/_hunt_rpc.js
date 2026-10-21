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
const COMP_LIQ = "0x298637f684da70674f26509b10f07ec2fbc77a335ab1e7d6215a4b2484d8bb52";
const AAVE_LIQ = "0xe413a321e8681d831f4dbccbca790d2952b56f977908e45be37335533e005286";
const AAVE = "0x6807dc923806fE8Fd134338EABCA509979a7e0cB";
const markets = [
  "0xeca88125a5adbe82614ffc12d0db554e2e2867c8","0xfd5840cd36d94d7229439859c0112a4185bc0255",
  "0x95c78222b3d6e262426483d42cfa53685a67ab9d","0x2ff3d0f6990a40261c66e1ff2017acbc282eb6d0",
  "0x151b1e2635a717bcdc836ecd6fbb62b674fe3e1d","0xa07c5b74c9b40447a954e1466938b865b6bbea36",
  "0x882c173bc7ff3b7786ca16dfed3dfffb9ee7847b","0xf508fcd89b8bd15579dc79a6827cb4686a3592c8",
  "0x57a5297f2cb2c0aac9d554660acd6d385ab50c6b","0xb248a295732e0225acd3337607cc01068e3b9c10",
];
const rpcs = [
  "https://bsc-rpc.publicnode.com",
  "https://bsc-dataseed.binance.org",
  "https://bsc-dataseed1.binance.org",
  "https://bsc-dataseed2.defibit.io",
  "https://bsc-dataseed3.ninicoin.io",
  "https://rpc.ankr.com/bsc",
  "https://bsc.meowrpc.com",
  "https://endpoints.omniatech.io/v1/bsc/mainnet/public",
];
(async () => {
  for (const rpc of rpcs) {
    try {
      const tip = parseInt(await call(rpc, "eth_blockNumber", []), 16);
      // try deep lookback 100k with 2k chunks - just one probe at tip-50000
      const fr = tip - 50000, to = tip - 48000;
      const logs = await call(rpc, "eth_getLogs", [{ fromBlock:"0x"+fr.toString(16), toBlock:"0x"+to.toString(16), address: AAVE, topics:[AAVE_LIQ] }]);
      console.log("DEEP OK", rpc, "tip", tip, "hits", logs.length);
    } catch (e) {
      console.log("DEEP FAIL", rpc, String(e.message).slice(0,140));
    }
  }
  // tip venus scan on publicnode
  const RPC = "https://bsc-rpc.publicnode.com";
  const tip = parseInt(await call(RPC, "eth_blockNumber", []), 16);
  let vHits = [], aHits = [];
  for (let end = tip; end > tip - 9000; end -= 1500) {
    const fr = end - 1499;
    try {
      const a = await call(RPC, "eth_getLogs", [{ fromBlock:"0x"+fr.toString(16), toBlock:"0x"+end.toString(16), address: AAVE, topics:[AAVE_LIQ] }]);
      const v = await call(RPC, "eth_getLogs", [{ fromBlock:"0x"+fr.toString(16), toBlock:"0x"+end.toString(16), address: markets, topics:[COMP_LIQ] }]);
      aHits = aHits.concat(a); vHits = vHits.concat(v);
      console.error("tipscan", fr+".."+end, "a="+a.length, "v="+v.length);
    } catch (e) {
      console.error("tipscan fail", fr, String(e.message).slice(0,120));
    }
    await new Promise(r => setTimeout(r, 100));
  }
  console.log("tip AAVE", aHits.length, "VENUS", vHits.length);
  for (const lg of vHits.slice(0,5).concat(aHits.slice(0,5))) {
    const tx = await call(RPC, "eth_getTransactionByHash", [lg.transactionHash]);
    console.log(JSON.stringify({block:parseInt(lg.blockNumber,16), addr:lg.address, tx:lg.transactionHash, searcher:tx.from, topic0:lg.topics[0].slice(0,10)}));
  }
})().catch(e=>{console.error(e);process.exit(1);});