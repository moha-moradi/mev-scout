var fs = require("fs");
var path = require("path");
var root = path.join(__dirname, "..");
var initSqlJs = require(path.join(root, "cache/sqljs-tmp/node_modules/sql.js/dist/sql-asm.js"));
var from = 97639634, to = 97942034;
initSqlJs().then(function (SQL) {
  var db = new SQL.Database(fs.readFileSync(path.join(root, "cache/explorer-avalanche.sqlite")));
  var c = db.exec("SELECT COUNT(*) FROM blocks_classified WHERE block BETWEEN " + from + " AND " + to)[0].values[0][0];
  var ops = db.exec("SELECT COUNT(*) FROM mev_ops WHERE block_number BETWEEN " + from + " AND " + to)[0].values[0][0];
  var mn = db.exec("SELECT MIN(block), MAX(block) FROM blocks_classified WHERE block BETWEEN " + from + " AND " + to)[0].values[0];
  var pct = (100 * c / (to - from + 1)).toFixed(1);
  var remain = (to - from + 1) - c;
  // ~started 92 min ago with unknown baseline; use rough rate from classified growth if we assume ~62k overlap already
  console.log(JSON.stringify({ window: [from, to], classified: c, ops: ops, minMax: mn, pct: pct + "%", remain: remain }, null, 2));
}).catch(function (e) { console.error(e); process.exit(1); });
