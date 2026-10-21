const https = require("https");
function post(url, body, headers) {
  const u = new URL(url);
  return new Promise((resolve, reject) => {
    const req = https.request({
      hostname: u.hostname, path: u.pathname + u.search, method: "POST",
      headers: Object.assign({ "Content-Type": "application/json", "Content-Length": Buffer.byteLength(body) }, headers || {}),
    }, (res) => { let d=""; res.on("data",c=>d+=c); res.on("end",()=>resolve({status:res.statusCode, body:d})); });
    req.on("error", reject); req.write(body); req.end();
  });
}
const q = JSON.stringify({ query: `{ liquidationCalls(first: 5, orderBy: timestamp, orderDirection: desc) { id timestamp txHash liquidator user { id } } }` });
const endpoints = [
  "https://api.studio.thegraph.com/query/aave/protocol-v3-bnb/version/latest",
  "https://api.studio.thegraph.com/query/aave/protocol-v3-bsc/version/latest",
  "https://gateway.thegraph.com/api/c0f3b0d3e6b74f0f8d5f6c0e1b2a3c4d/subgraphs/id/43jbGkvSw55sMvYyF6MZieksmJbajMu3hNGF8PN9ucuP",
];
(async () => {
  for (const ep of endpoints) {
    try {
      const r = await post(ep, q);
      console.log(ep.slice(0,90), r.status, r.body.slice(0,400));
    } catch (e) { console.log("fail", ep, e.message); }
  }
  // BscScan HTML events page scrape via public API without key sometimes works for module=account
  // Try dexscreener / aave api
  const httpsGet = (url) => new Promise((resolve,reject)=>{
    https.get(url, res => { let d=""; res.on("data",c=>d+=c); res.on("end",()=>resolve({s:res.statusCode,b:d.slice(0,2000)})); }).on("error",reject);
  });
  try {
    const r = await httpsGet("https://aave-api-v2.aave.com/data/liquidity/v2?poolAddress=0x6807dc923806fE8Fd134338EABCA509979a7e0cB&chainId=56");
    console.log("aave-api", r.s, r.b.slice(0,200));
  } catch(e) { console.log("aave-api fail", e.message); }
})().catch(e=>{console.error(e);process.exit(1);});