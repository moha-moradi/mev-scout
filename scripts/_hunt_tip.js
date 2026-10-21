const https = require("https");
const { URL } = require("url");
const RPC = "https://bsc-rpc.publicnode.com";
const AAVE = "0x6807dc923806fE8Fd134338EABCA509979a7e0cB";
const AAVE_LIQ = "0xe413a321e8681d831f4dbccbca790d2952b56f977908e45be37335533e005286";
const COMP_LIQ = "0x298637f684da70674f26509b10f07ec2fbc77a335ab1e7d6215a4b2484d8bb52";
const markets = [
  "0xeca88125a5adbe82614ffc12d0db554e2e2867c8","0xfd5840cd36d94d7229439859c0112a4185bc0255",
  "0x95c78222b3d6e262426483d42cfa53685a67ab9d","0x2ff3d0f6990a40261c66e1ff2017acbc282eb6d0",
  "0x151b1e2635a717bcdc836ecd6fbb62b674fe3e1d","0xa07c5b74c9b40447a954e1466938b865b6bbea36",
  "0x882c173bc7ff3b7786ca16dfed3dfffb9ee7847b","0xf508fcd89b8bd15579dc79a6827cb4686a3592c8",
  "0x57a5297f2cb2c0aac9d554660acd6d385ab50c6b","0xb248a295732e0225acd3337607cc01068e3b9c10",
  "0x5f0388ebc2b94fa8e123f404b79ccf5f40b29176","0x1610bc33319e9398de5f57b33a5b184c806ad217",
  "0x650b940a1033b8a1b1873f78730fcfc73ec11f1f","0x334b3ecb4dca3593bccc3c7ebd1a1c1d1780fbf1",
  "0xf91d58b5ae142dacc749f58a49fcbac340cb0343","0x972207a639cc1b374b893cc33fa251b55ceb7c07",
  "0xebd0070237a0713e8d94fef1b728d3d993d290ef","0x9a0af7fdb2065ce470d72664de73cae409da28ec",
  "0xec3422ef92b2fb59e84c8b02ba73f1fe84ed8d71","0x5c9476fcd6a4f9a3654139721c949c2233bbbbc8",
  "0x86ac3974e2bd0d60825230fa6f355ff11409df5c","0x26da28954763b92139ed49283625cecaf52c6f94",
];

function call(method, params) {
  const url = new URL(RPC);
  const body = JSON.stringify({ jsonrpc: "2.0", id: 1, method, params });
  return new Promise((resolve, reject) => {
    const req = https.request(
      { hostname: url.hostname, port: 443, path: url.pathname + url.search, method: "POST",
        headers: { "Content-Type": "application/json", "Content-Length": Buffer.byteLength(body) } },
      (res) => {
        let data = "";
        res.on("data", (c) => (data += c));
        res.on("end", () => {
          try {
            const j = JSON.parse(data);
            if (j.error) reject(new Error(JSON.stringify(j.error)));
            else resolve(j.result);
          } catch (e) { reject(new Error(data.slice(0, 300))); }
        });
      }
    );
    req.setTimeout(45000, () => req.destroy(new Error("timeout")));
    req.on("error", reject);
    req.write(body);
    req.end();
  });
}

(async () => {
  const tip = parseInt(await call("eth_blockNumber", []), 16);
  console.log("tip", tip);
  // find how far tip getLogs works
  for (const back of [1000, 2000, 4000, 8000, 10000, 12000, 16000]) {
    try {
      const fr = tip - back;
      const logs = await call("eth_getLogs", [{
        fromBlock: "0x"+fr.toString(16), toBlock: "0x"+tip.toString(16),
        address: AAVE, topics: [AAVE_LIQ],
      }]);
      console.log("aave window tip-"+back, "ok hits", logs.length);
    } catch (e) {
      console.log("aave window tip-"+back, "FAIL", String(e.message).slice(0,100));
    }
  }
  // dense tip scan in 2k chunks for last ~12k
  let aHits = [], vHits = [];
  let end = tip;
  const start = tip - 10000;
  while (end >= start) {
    const fr = Math.max(start, end - 1999);
    try {
      const a = await call("eth_getLogs", [{ fromBlock:"0x"+fr.toString(16), toBlock:"0x"+end.toString(16), address: AAVE, topics:[AAVE_LIQ] }]);
      const v = await call("eth_getLogs", [{ fromBlock:"0x"+fr.toString(16), toBlock:"0x"+end.toString(16), address: markets, topics:[COMP_LIQ] }]);
      aHits = aHits.concat(a); vHits = vHits.concat(v);
      console.error("scan", fr+".."+end, "a="+a.length, "v="+v.length);
    } catch (e) {
      console.error("scan fail", fr, String(e.message).slice(0,100));
    }
    end = fr - 1;
    await new Promise(r => setTimeout(r, 80));
  }
  console.log("AAVE", aHits.length, "VENUS", vHits.length);
  for (const lg of aHits.slice(0,5)) {
    const tx = await call("eth_getTransactionByHash", [lg.transactionHash]);
    console.log(JSON.stringify({kind:"aave", block:parseInt(lg.blockNumber,16), tx:lg.transactionHash, searcher:tx.from}));
  }
  for (const lg of vHits.slice(0,5)) {
    const tx = await call("eth_getTransactionByHash", [lg.transactionHash]);
    console.log(JSON.stringify({kind:"venus", block:parseInt(lg.blockNumber,16), tx:lg.transactionHash, vToken:lg.address, searcher:tx.from}));
  }
})().catch(e => { console.error(e); process.exit(1); });