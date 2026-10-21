import https from "node:https";
const RPC = process.env.BSC_RPC;
const call = (method, params) => new Promise((resolve, reject) => {
  const url = new URL(RPC);
  const req = https.request(
    {
      host: url.host,
      port: 443,
      path: url.pathname,
      method: "POST",
      headers: { "Content-Type": "application/json", "Host": url.host },
    },
    (res) => {
      let d = "";
      res.on("data", (c) => (d += c));
      res.on("end", () => {
        try {
          const j = JSON.parse(d);
          if (j.error) return resolve({ error: j.error.message, raw: d.slice(0, 300) });
          return resolve({ result: j.result });
        } catch (e) {
          return resolve({ bad: d.slice(0, 300) });
        }
      });
    },
    (e) => reject(e),
  );
  req.setTimeout(30000, () => req.destroy());
  req.on("error", reject);
  req.write(JSON.stringify({ jsonrpc: "2.0", id: 1, method, params }));
  req.end();
});
(async () => {
  const pool = "0x6807dc923806fE8Fd134338EABCA509979a7e0cB";
  console.log("code:", JSON.stringify(await call("eth_getCode", [pool])));
  console.log("version:", JSON.stringify(await call("eth_call", [{ from: pool, data: "0xd4f44c97" }])))
  console.log("v2liq:", JSON.stringify(await call("eth_call", [{ from: pool, data: "0x3b3e5e34" }])))
})();
