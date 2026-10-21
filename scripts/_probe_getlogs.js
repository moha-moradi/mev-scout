const https = require("https");
const { URL } = require("url");

function call(rpc, method, params) {
  const url = new URL(rpc);
  const body = JSON.stringify({ jsonrpc: "2.0", id: 1, method, params });
  return new Promise((resolve, reject) => {
    const req = https.request(
      {
        hostname: url.hostname,
        port: 443,
        path: url.pathname + url.search,
        method: "POST",
        headers: { "Content-Type": "application/json", "Content-Length": Buffer.byteLength(body) },
      },
      (res) => {
        let data = "";
        res.on("data", (c) => (data += c));
        res.on("end", () => {
          try {
            const j = JSON.parse(data);
            if (j.error) reject(new Error(JSON.stringify(j.error)));
            else resolve(j.result);
          } catch (e) {
            reject(new Error(data.slice(0, 300)));
          }
        });
      }
    );
    req.setTimeout(45000, () => req.destroy(new Error("timeout")));
    req.on("error", reject);
    req.write(body);
    req.end();
  });
}

const RPCs = [
  "https://bsc-rpc.publicnode.com",
  "https://bsc.publicnode.com",
  "https://rpc.ankr.com/bsc",
  "https://1rpc.io/bnb",
];
const AAVE = "0x6807dc923806fE8Fd134338EABCA509979a7e0cB";
const AAVE_LIQ = "0xe413a321e8681d831f4dbccbca790d2952b56f977908e45be37335533e005286";

(async () => {
  for (const rpc of RPCs) {
    try {
      const tip = parseInt(await call(rpc, "eth_blockNumber", []), 16);
      const fr = tip - 500;
      const logs = await call(rpc, "eth_getLogs", [{
        fromBlock: "0x" + fr.toString(16),
        toBlock: "0x" + tip.toString(16),
        address: AAVE,
        topics: [AAVE_LIQ],
      }]);
      console.log("OK", rpc, "tip", tip, "logs", logs.length);
      if (logs.length) console.log(JSON.stringify(logs[0]));
    } catch (e) {
      console.log("FAIL", rpc, String(e.message).slice(0, 160));
    }
  }
})().catch((e) => { console.error(e); process.exit(1); });