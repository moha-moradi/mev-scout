var fs = require("fs");
var path = require("path");
var root = path.join(__dirname, "..");
var initSqlJs = require(path.join(root, "cache/sqljs-tmp/node_modules/sql.js/dist/sql-asm.js"));
initSqlJs().then(function (SQL) {
  var db = new SQL.Database(fs.readFileSync(path.join(root, "cache/explorer-avalanche.sqlite")));
  var from = 97562463, to = 97864863;
  var c = db.exec("SELECT COUNT(*) FROM blocks_classified WHERE block BETWEEN " + from + " AND " + to);
  var ops = db.exec("SELECT COUNT(*) FROM mev_ops WHERE block_number BETWEEN " + from + " AND " + to);
  var kinds = db.exec("SELECT kind, COUNT(*) n FROM mev_ops WHERE block_number BETWEEN " + from + " AND " + to + " GROUP BY kind");
  console.log(JSON.stringify({
    classified: c[0].values[0][0],
    ops: ops[0].values[0][0],
    kinds: kinds[0] ? kinds[0].values : [],
    dbSize: fs.statSync(path.join(root, "cache/explorer-avalanche.sqlite")).size
  }, null, 2));
}).catch(function (e) { console.error(e); process.exit(1); });
