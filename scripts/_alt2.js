const https = require("https");
function post(url, body, headers) {
  const u = new URL(url);
  return new Promise((resolve, reject) => {
    const req = https.request({
      hostname: u.hostname, path: u.pathname + u.search, method: "POST",
      headers: Object.assign({ "Content-Type": "application/json", "Content-Length": Buffer.byteLength(body) }, headers || {}),
    }, (res) => { let d=""; res.on("data",c=>d+=c); res.on("end",()=>resolve({status:res.statusCode, body:d.slice(0,5000)})); });
    req.on("error", reject); req.write(body); req.end();
  });
}
(async () => {
  // Bitquery v1 (may need key)
  const q1 = JSON.stringify({
    query: `{ ethereum(network: bsc) { logs(options: {limit: 5, desc: "block.height"}, topic0: {is: "0xe413a321e8681d831f4dbccbca790d2952b56f977908e45be37335533e005286"}, smartContractAddress: {is: "0x6807dc923806fE8Fd134338EABCA509979a7e0cB"}) { block { height } transaction { hash } } } }`
  });
  try {
    const r = await post("https://graphql.bitquery.io", q1);
    console.log("bitquery", r.status, r.body.slice(0,800));
  } catch(e) { console.log("bitquery fail", e.message); }

  // Try llama.fi liquidations API variants
  for (const path of [
    "https://liquidations.llama.fi/liquidations/aave-v3-bsc",
    "https://liquidations.llama.fi/liquidations/venus",
    "https://api.llama.fi/liquidations/aave-v3-bsc",
    "https://api.llama.fi/protocol/aave-v3-bsc",
  ]) {
    try {
      const r = await new Promise((resolve,reject)=>{
        https.get(path, res => { let d=""; res.on("data",c=>d+=c); res.on("end",()=>resolve({s:res.statusCode,b:d.slice(0,600)})); }).on("error",reject);
      });
      console.log(path, r.s, r.b.slice(0,300));
    } catch(e) { console.log(path, e.message); }
  }

  // dwellir: try eth_getBlockReceipts on tip
  const DW = "https://api-bsc-mainnet-full.n.dwellir.com/87efcc6b-d1fd-48f8-8ca3-4c53ae7b732b";
  function call(rpc, method, params) {
    const body = JSON.stringify({jsonrpc:"2.0",id:1,method,params});
    const u = new URL(rpc);
    return new Promise((resolve,reject)=>{
      const req = https.request({hostname:u.hostname,path:u.pathname,method:"POST",
        headers:{"Content-Type":"application/json","Content-Length":Buffer.byteLength(body)}},
        res=>{let d="";res.on("data",c=>d+=c);res.on("end",()=>{try{const j=JSON.parse(d);if(j.error)reject(new Error(JSON.stringify(j.error)));else resolve(j.result);}catch(e){reject(e);}});});
      req.on("error",reject); req.write(body); req.end();
    });
  }
  const tip = parseInt(await call(DW, "eth_blockNumber", []), 16);
  console.log("dwellir tip", tip);
  for (const method of ["eth_getBlockReceipts", "eth_getTransactionReceiptsByBlockNumber"]) {
    try {
      const r = await call(DW, method, ["0x"+tip.toString(16)]);
      console.log(method, Array.isArray(r) ? ("arr len "+r.length) : typeof r);
    } catch(e) { console.log(method, "FAIL", String(e.message).slice(0,120)); }
  }
  // try receipt of a recent tx via getBlock with txs + getTransactionReceipt
  try {
    const blk = await call(DW, "eth_getBlockByNumber", ["0x"+tip.toString(16), false]);
    console.log("block txs", blk.transactions.length);
    if (blk.transactions[0]) {
      const rcpt = await call(DW, "eth_getTransactionReceipt", [blk.transactions[0]]);
      console.log("receipt logs", rcpt.logs.length, "status", rcpt.status);
    }
  } catch(e) { console.log("receipt path fail", e.message); }
})().catch(e=>{console.error(e);process.exit(1);});