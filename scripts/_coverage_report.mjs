import { DatabaseSync } from "node:sqlite";
import { writeFileSync } from "node:fs";

function report(path, chain) {
  const db = new DatabaseSync(path);
  const total = db.prepare("SELECT COUNT(*) c FROM mev_ops").get().c;
  const br = db
    .prepare("SELECT MIN(block_number) a, MAX(block_number) b FROM mev_ops")
    .get();
  const kinds = db
    .prepare(
      "SELECT kind, COUNT(*) n, COALESCE(SUM(profit_usd),0) profit FROM mev_ops GROUP BY kind ORDER BY n DESC",
    )
    .all();
  const tags = {};
  const flags = {};
  let priced = 0;
  for (const r of db
    .prepare(
      "SELECT block_number, kind, details_json, profit_usd FROM mev_ops",
    )
    .all()) {
    if (r.profit_usd != null) priced++;
    let d = {};
    try {
      d = JSON.parse(r.details_json || "{}");
    } catch {
      continue;
    }
    for (const t of d.tags || []) {
      if (!tags[t]) tags[t] = { n: 0, profit: 0, blocks: [] };
      tags[t].n++;
      tags[t].profit += Number(r.profit_usd || 0);
      if (tags[t].blocks.length < 2) {
        tags[t].blocks.push(r.block_number);
      }
    }
    for (const f of ["interest_accrued", "oracle_poke_block", "bad_debt"]) {
      if (d[f] === true) {
        flags[f] = (flags[f] || 0) + 1;
      }
    }
    if (d.protocol === "benqi") {
      tags["__protocol:benqi"] = tags["__protocol:benqi"] || {
        n: 0,
        profit: 0,
        blocks: [],
      };
      tags["__protocol:benqi"].n++;
      if (tags["__protocol:benqi"].blocks.length < 2) {
        tags["__protocol:benqi"].blocks.push(r.block_number);
      }
    }
  }
  return { chain, path, total, blocks: br, kinds, tags, flags, priced };
}

const out = [
  report("cache/explorer-avalanche.sqlite", "avalanche"),
  report("cache/explorer-polygon.sqlite", "polygon"),
  report("cache/explorer-ethereum.sqlite", "ethereum"),
];
writeFileSync("cache/phase2_coverage.json", JSON.stringify(out, null, 2));
for (const r of out) {
  console.log("====", r.chain, "ops", r.total, "blocks", r.blocks, "priced", r.priced);
  console.log("KINDS", r.kinds);
  console.log("TAGS", r.tags);
  console.log("FLAGS", r.flags);
}
