var fs = require("fs");
var path = require("path");
var root = path.join(__dirname, "..");
var initSqlJs = require(path.join(root, "cache/sqljs-tmp/node_modules/sql.js/dist/sql-asm.js"));

function table(db, sql) {
  var r = db.exec(sql);
  if (!r[0]) return [];
  return r[0].values;
}

initSqlJs().then(function (SQL) {
  var chains = [
    ["avalanche", "cache/explorer-avalanche.sqlite"],
    ["ethereum", "cache/explorer-ethereum.sqlite"],
    ["polygon", "cache/explorer-polygon.sqlite"],
  ];
  var out = { generated_at: new Date().toISOString(), chains: {} };
  chains.forEach(function (pair) {
    var chain = pair[0], rel = pair[1];
    var p = path.join(root, rel);
    if (!fs.existsSync(p)) {
      out.chains[chain] = { missing: true };
      return;
    }
    var db = new SQL.Database(fs.readFileSync(p));
    var total = table(db, "SELECT COUNT(*) FROM mev_ops")[0][0];
    var br = table(db, "SELECT MIN(block_number), MAX(block_number) FROM mev_ops")[0] || [null, null];
    var kinds = table(db, "SELECT kind, COUNT(*) n, COALESCE(SUM(profit_usd),0) profit FROM mev_ops GROUP BY kind ORDER BY n DESC").map(function (r) {
      return { kind: r[0], n: r[1], profit: r[2] };
    });
    var tags = {};
    var flags = {};
    var rows = table(db, "SELECT block_number, kind, details_json, profit_usd FROM mev_ops");
    rows.forEach(function (r) {
      var d = {};
      try { d = JSON.parse(r[2] || "{}"); } catch (e) { return; }
      (d.tags || []).forEach(function (t) {
        if (!tags[t]) tags[t] = { n: 0, profit: 0, blocks: [] };
        tags[t].n++;
        tags[t].profit += Number(r[3] || 0);
        if (tags[t].blocks.length < 3) tags[t].blocks.push(r[0]);
      });
      ["interest_accrued", "oracle_poke_block", "bad_debt"].forEach(function (f) {
        if (d[f] === true) flags[f] = (flags[f] || 0) + 1;
      });
    });
    var classified = null;
    try {
      classified = table(db, "SELECT COUNT(*) FROM blocks_classified")[0][0];
    } catch (e) {}
    out.chains[chain] = {
      path: rel,
      ops: total,
      block_range: { min: br[0], max: br[1] },
      classified_blocks: classified,
      kinds: kinds,
      tags: tags,
      flags: flags,
      db_bytes: fs.statSync(p).size,
    };
  });
  var dest = path.join(root, "cache/phase2_coverage.json");
  fs.writeFileSync(dest, JSON.stringify(out, null, 2));
  console.log(JSON.stringify({ wrote: dest, avalanche_ops: out.chains.avalanche && out.chains.avalanche.ops, avalanche_classified: out.chains.avalanche && out.chains.avalanche.classified_blocks }, null, 2));
}).catch(function (e) { console.error(e); process.exit(1); });
