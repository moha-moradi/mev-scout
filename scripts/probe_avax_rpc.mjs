import { readFileSync } from "node:fs";
function extractUrls(path) {
  const t = readFileSync(path, "utf8");
  return [...new Set([...t.matchAll(/"https?:\/\/[^"]+"/g)].map((x) => x[0].slice(1, -1)))];
}
const urls = [...extractUrls("hist.toml"), ...extractUrls("mev-scout.local.toml"), ...extractUrls("mev-scout.avalanche-run.toml")];
async function rpc(url, method, params = []) {
  const r = await fetch(url, { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify({ jsonrpc: "2.0", id: 1, method, params }), signal: AbortSignal.timeout(12000) });
  return r.json();
}
for (const u of urls) {
  const short = u.replace(/\/[0-9a-f-]{8,}[^/]*/gi, "/...").slice(0, 70);
  try {
    const bn = await rpc(u, "eth_blockNumber");
    if (bn.error) { console.log(JSON.stringify({ short, err: bn.error.message })); continue; }
    const tip = parseInt(bn.result, 16);
    const mid = "0x" + (tip - 100000).toString(16);
    const rec = await rpc(u, "eth_getBlockReceipts", [mid]);
    const call = await rpc(u, "eth_call", [{ to: "0xB31f66AA3C1e785363F0875A1B74E27b85FD66c7", data: "0x70a082310000000000000000000000000000000000000000000000000000000000000001" }, mid]);
    console.log(JSON.stringify({ short, tip, receipts: !!rec.result && !rec.error, receiptsErr: rec.error && rec.error.message && rec.error.message.slice(0, 60), callOk: !!call.result && !call.error, callErr: call.error && call.error.message && call.error.message.slice(0, 60) }));
  } catch (e) { console.log(JSON.stringify({ short, fail: String(e.message || e).slice(0, 100) })); }
}
