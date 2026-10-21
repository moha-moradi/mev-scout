const https = require("https");
const { URL } = require("url");
const RPC = process.env.BSC_RPC || "https://bsc-rpc.publicnode.com";
const AAVE = "0x6807dc923806fE8Fd134338EABCA509979a7e0cB";
const AAVE_LIQ = "0xe413a321e8681d831f4dbccbca790d2952b56f977908e45be37335533e005286";
const COMP_LIQ = "0x298637f684da70674f26509b10f07ec2fbc77a335ab1e7d6215a4b2484d8bb52";
const markets = [
  "0xeca88125a5adbe82614ffc12d0db554e2e2867c8",
  "0xfd5840cd36d94d7229439859c0112a4185bc0255",
  "0x95c78222b3d6e262426483d42cfa53685a67ab9d",
  "0x2ff3d0f6990a40261c66e1ff2017acbc282eb6d0",
  "0x151b1e2635a717bcdc836ecd6fbb62b674fe3e1d",
  "0xa07c5b74c9b40447a954e1466938b865b6bbea36",
  "0x882c173bc7ff3b7786ca16dfed3dfffb9ee7847b",
  "0xf508fcd89b8bd15579dc79a6827cb4686a3592c8",
  "0x57a5297f2cb2c0aac9d554660acd6d385ab50c6b",
  "0xb248a295732e0225acd3337607cc01068e3b9c10",
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
    req.setTimeout(60000, () => req.destroy(new Error("timeout")));
    req.on("error", reject);
    req.write(body);
    req.end();
  });
}

async function scan(label, address, topics, lookback, chunk, maxHits, tip) {
  const hits = [];
  let end = tip, c = chunk;
  const start = Math.max(0, tip - lookback);
  while (end >= start && hits.length < maxHits) {
    const fr = Math.max(start, end - c + 1);
    try {
      const filter = { fromBlock: "0x"+fr.toString(16), toBlock: "0x"+end.toString(16), topics };
      if (address) filter.address = address;
      const batch = await call("eth_getLogs", [filter]);
      hits.push.apply(hits, batch);
      if (batch.length || (end % (chunk * 20) < chunk)) {
        console.error("["+label+"] "+fr+".."+end+" +"+batch.length+" total="+hits.length);
      }
      end = fr - 1;
    } catch (e) {
      if (c <= 100) { console.error("["+label+"] hard fail "+e.message); break; }
      c = Math.max(100, Math.floor(c / 2));
      console.error("["+label+"] fail shrink="+c+" "+String(e.message).slice(0,120));
    }
    await new Promise((r) => setTimeout(r, 50));
  }
  return hits;
}

(async () => {
  const tip = parseInt(await call("eth_blockNumber", []), 16);
  console.log("tip", tip, "rpc", RPC);
  const aave = await scan("aave", AAVE, [AAVE_LIQ], 2000000, 5000, 20, tip);
  console.log("AAVE hits", aave.length);
  for (let i = 0; i < Math.min(5, aave.length); i++) {
    const lg = aave[i];
    const tx = await call("eth_getTransactionByHash", [lg.transactionHash]);
    console.log(JSON.stringify({ kind:"aave", block:parseInt(lg.blockNumber,16), tx:lg.transactionHash, searcher:tx.from }));
  }
  const venus = await scan("venus", markets, [COMP_LIQ], 500000, 3000, 20, tip);
  console.log("VENUS hits", venus.length);
  for (let i = 0; i < Math.min(5, venus.length); i++) {
    const lg = venus[i];
    const tx = await call("eth_getTransactionByHash", [lg.transactionHash]);
    console.log(JSON.stringify({ kind:"venus", block:parseInt(lg.blockNumber,16), tx:lg.transactionHash, vToken:lg.address, searcher:tx.from }));
  }
})().catch((e) => { console.error(e); process.exit(1); });