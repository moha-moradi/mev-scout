var https = require("https");
var http = require("http");
var fs = require("fs");
var urls = [];
var t = fs.readFileSync("mev-scout.phase2-avax.toml", "utf8");
var m = t.matchAll(/"https?:\/\/[^"]+"/g);
for (var x of m) urls.push(x[0].slice(1, -1));
urls = Array.from(new Set(urls));
function short(u) { return u.replace(/\/[0-9a-f-]{8,}[^/]*/gi, "/...").slice(0, 72); }
function rpc(url, method, params) {
  return new Promise(function (resolve, reject) {
    var u = new URL(url);
    var lib = u.protocol === "http:" ? http : https;
    var body = JSON.stringify({ jsonrpc: "2.0", id: 1, method: method, params: params || [] });
    var req = lib.request({
      hostname: u.hostname,
      path: u.pathname + u.search,
      method: "POST",
      headers: { "content-type": "application/json", "content-length": Buffer.byteLength(body) },
      timeout: 12000,
    }, function (res) {
      var data = "";
      res.on("data", function (c) { data += c; });
      res.on("end", function () { resolve({ status: res.statusCode, text: data }); });
    });
    req.on("error", reject);
    req.on("timeout", function () { req.destroy(); reject(new Error("timeout")); });
    req.write(body);
    req.end();
  });
}
function run(i) {
  if (i >= urls.length) return;
  var u = urls[i];
  rpc(u, "eth_blockNumber", [])
    .then(function (bn) {
      if (bn.status === 429) {
        console.log(JSON.stringify({ i: i, short: short(u), ok: false, err: "429" }));
        return run(i + 1);
      }
      var j;
      try { j = JSON.parse(bn.text); } catch (e) {
        console.log(JSON.stringify({ i: i, short: short(u), ok: false, err: bn.text.slice(0, 80) }));
        return run(i + 1);
      }
      if (j.error) {
        console.log(JSON.stringify({ i: i, short: short(u), ok: false, err: j.error.message }));
        return run(i + 1);
      }
      var tip = parseInt(j.result, 16);
      var mid = "0x" + (tip - 100000).toString(16);
      return rpc(u, "eth_getBlockReceipts", [mid]).then(function (rec) {
        var rj;
        try { rj = JSON.parse(rec.text); } catch (e) { rj = { error: { message: rec.text.slice(0, 60) } }; }
        console.log(JSON.stringify({
          i: i,
          short: short(u),
          ok: true,
          tip: tip,
          receipts: !!(rj.result && !rj.error),
          receiptsErr: rj.error && rj.error.message && String(rj.error.message).slice(0, 50),
        }));
        return run(i + 1);
      });
    })
    .catch(function (e) {
      console.log(JSON.stringify({ i: i, short: short(u), ok: false, err: String(e.message || e).slice(0, 100) }));
      run(i + 1);
    });
}
run(0);
