const https = require("https");
function get(url) {
  return new Promise((resolve, reject) => {
    https.get(url, (res) => {
      let d = "";
      res.on("data", c => d += c);
      res.on("end", () => resolve({ status: res.statusCode, body: d.slice(0, 4000) }));
    }).on("error", reject);
  });
}
function post(url, body) {
  const u = new URL(url);
  return new Promise((resolve, reject) => {
    const req = https.request({ hostname: u.hostname, path: u.pathname + u.search, method: "POST",
      headers: { "Content-Type": "application/json", "Content-Length": Buffer.byteLength(body) } },
      (res) => { let d=""; res.on("data",c=>d+=c); res.on("end",()=>resolve({status:res.statusCode, body:d.slice(0,5000)})); });
    req.on("error", reject);
    req.write(body); req.end();
  });
}

(async () => {
  // Aave official subgraph candidates
  const queries = JSON.stringify({
    query: `{ liquidations(first:5, orderBy: timestamp, orderDirection: desc) { id timestamp txHash liquidator collateralAmount } }`
  });
  const endpoints = [
    "https://api.thegraph.com/subgraphs/name/aave/protocol-v3-bsc",
    "https://api.studio.thegraph.com/query/24143/aave-v3-bsc/version/latest",
    "https://gateway.thegraph.com/api/subgraphs/id/Cd2gEDVdxKQTuqqubenyRtK61CvDk4Ea299mRYPdTNNm",
  ];
  for (const ep of endpoints) {
    try {
      const r = await post(ep, queries);
      console.log("SUB", ep.slice(0,80), r.status, r.body.slice(0,300));
    } catch (e) { console.log("SUB fail", ep, e.message); }
  }

  // DefiLlama liquidations
  try {
    const r = await get("https://yields.llama.fi/liquidations/bsc");
    console.log("LLAMA", r.status, r.body.slice(0,500));
  } catch (e) { console.log("LLAMA fail", e.message); }

  // 1rpc 50-block scan over last 20k
  const RPC = "https://1rpc.io/bnb";
  function call(method, params) {
    const body = JSON.stringify({ jsonrpc:"2.0", id:1, method, params });
    const u = new URL(RPC);
    return new Promise((resolve, reject) => {
      const req = https.request({ hostname:u.hostname, path:u.pathname, method:"POST",
        headers:{"Content-Type":"application/json","Content-Length":Buffer.byteLength(body)}},
        (res)=>{ let d=""; res.on("data",c=>d+=c); res.on("end",()=>{ try{ const j=JSON.parse(d); if(j.error) reject(new Error(JSON.stringify(j.error))); else resolve(j.result);}catch(e){reject(e);} }); });
      req.on("error", reject); req.write(body); req.end();
    });
  }
  const AAVE = "0x6807dc923806fE8Fd134338EABCA509979a7e0cB";
  const AAVE_LIQ = "0xe413a321e8681d831f4dbccbca790d2952b56f977908e45be37335533e005286";
  const tip = parseInt(await call("eth_blockNumber", []), 16);
  console.log("1rpc tip", tip);
  let hits = [];
  for (let end = tip; end > tip - 30000 && hits.length < 5; end -= 50) {
    const fr = end - 49;
    try {
      const logs = await call("eth_getLogs", [{ fromBlock:"0x"+fr.toString(16), toBlock:"0x"+end.toString(16), address:AAVE, topics:[AAVE_LIQ] }]);
      if (logs.length) {
        hits = hits.concat(logs);
        console.log("HIT", fr, end, logs.length, parseInt(logs[0].blockNumber,16), logs[0].transactionHash);
      }
    } catch (e) {
      if (!String(e.message).includes("limited")) console.log("1rpc fail", fr, String(e.message).slice(0,120));
    }
    if ((tip - end) % 2000 === 0) console.error("1rpc progress tip-"+(tip-end), "hits", hits.length);
    await new Promise(r => setTimeout(r, 40));
  }
  console.log("1rpc aave hits", hits.length);
})().catch(e => { console.error(e); process.exit(1); });