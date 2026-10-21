const https = require("https");
const RPC = process.env.RPC_URL;
const V3 = "0x794a61358D6845594F94dc1DB02A252b5b4814aD";
const V2 = "0x8dFf5E27EA6b7AC08EbFdf9eB090F32ee9a30fcf";
const LIQ = "0xe413a321e8681d831f4dbccbca790d2952b56f977908e45be37335533e005286";
function call(method, params) {
  const body = JSON.stringify({ jsonrpc: "2.0", id: 1, method, params });
  const u = new URL(RPC);
  return new Promise((resolve, reject) => {
    const req = https.request(
      { hostname: u.hostname, path: u.pathname + u.search, method: "POST",
        headers: { "Content-Type": "application/json", "Content-Length": Buffer.byteLength(body) } },
      (res) => {
        let d = "";
        res.on("data", (c) => (d += c));
        res.on("end", () => {
          try {
            const j = JSON.parse(d);
            if (j.error) reject(new Error(JSON.stringify(j.error)));
            else resolve(j.result);
          } catch (e) { reject(new Error(d.slice(0, 200))); }
        });
      }
    );
    req.on("error", reject);
    req.write(body);
    req.end();
  });
}
async function countRange(addr, from, to, step) {
  let n = 0;
  let sample = null;
  for (let f = from; f <= to; f += step) {
    const t = Math.min(f + step - 1, to);
    try {
      const logs = await call("eth_getLogs", [{
        fromBlock: "0x" + f.toString(16),
        toBlock: "0x" + t.toString(16),
        address: addr,
        topics: [LIQ],
      }]);
      if (logs.length) {
        n += logs.length;
        if (!sample) sample = { block: parseInt(logs[0].blockNumber, 16), tx: logs[0].transactionHash };
      }
    } catch (e) {
      console.log("fail", addr.slice(0, 10), f, t, String(e.message).slice(0, 120));
    }
  }
  return { n, sample };
}
(async () => {
  const tip = parseInt(await call("eth_blockNumber", []), 16);
  console.log("tip", tip);
  // corpus window exact
  const cFrom = 94430973, cTo = 94431566;
  const v3c = await countRange(V3, cFrom, cTo, 200);
  const v2c = await countRange(V2, cFrom, cTo, 200);
  console.log("corpus_window V3", v3c);
  console.log("corpus_window V2", v2c);
  // last 50k tip in 500-block chunks
  const tipFrom = tip - 50000;
  const v3t = await countRange(V3, tipFrom, tip, 500);
  const v2t = await countRange(V2, tipFrom, tip, 500);
  console.log("tip_50k V3", v3t);
  console.log("tip_50k V2", v2t);
})().catch((e) => { console.error(e); process.exit(1); });