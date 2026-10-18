import { DatabaseSync } from "node:sqlite";
import { readFileSync, existsSync } from "node:fs";

const from = Number(process.argv[2]);
const to = Number(process.argv[3]);
if (!from || !to) {
  console.error("usage: node scripts/_phase2_1d_status.mjs FROM TO");
  process.exit(1);
}

const CHECKLIST = [
  ["baseline.arb", "kind", "arb_atomic"],
  ["baseline.liquidation", "kind", "liquidation"],
  ["baseline.sandwich", "kind", "sandwich"],
  ["baseline.frontrun", "kind", "frontrun"],
  ["baseline.backrun", "kind", "backrun"],
  ["baseline.jit", "kind", "jit"],
  ["baseline.skim", "kind", "skim"],
  ["P0.1", "tag", "flash_loan_liq"],
  ["P0.2", "kind", "liquidation"], // benqi shows as liquidation
  ["P0.4", "tag", "flash_arb"],
  ["P1.1", "flag", "interest_accrued"],
  ["P1.3", "tag", "long_tail"],
  ["P1.4", "flag", "oracle_poke_block"],
  ["P1.5", "tag", "keeper"],
  ["P3.1", "tag", "lb_bin_jit"],
  ["P3.2", "tag", "rate_provider_staleness"],
  ["P3.5", "tag", "curve_imbalance"],
  ["P3.7", "tag", "gmx_adl_arb"],
  ["P3.11", "tag", "bundler"],
  ["P3.12a", "tag", "fot_arb"],
  ["P3.12b", "tag", "rebase_arb"],
  ["P3.13", "tag", "claim_and_sell"],
  ["P3.14", "flag", "bad_debt"],
  ["P3.15", "tag", "epoch_transition"],
  ["P3.16", "tag", "savax_rate_arb"],
];

const db = new DatabaseSync("cache/explorer-avalanche.sqlite");
const total = to - from + 1;
const classified = db
  .prepare("SELECT COUNT(*) c FROM blocks_classified WHERE block BETWEEN ? AND ?")
  .get(from, to).c;
const ops = db
  .prepare("SELECT COUNT(*) c FROM mev_ops WHERE block_number BETWEEN ? AND ?")
  .get(from, to).c;
const kinds = db
  .prepare(
    "SELECT kind, COUNT(*) n, COALESCE(SUM(profit_usd),0) p FROM mev_ops WHERE block_number BETWEEN ? AND ? GROUP BY kind ORDER BY n DESC",
  )
  .all(from, to);
const rows = db
  .prepare(
    "SELECT block_number, kind, details_json, profit_usd FROM mev_ops WHERE block_number BETWEEN ? AND ?",
  )
  .all(from, to);

const tags = {};
const flags = {};
for (const r of rows) {
  let d = {};
  try {
    d = JSON.parse(r.details_json || "{}");
  } catch {
    continue;
  }
  for (const t of d.tags || []) {
    if (!tags[t]) tags[t] = { n: 0, profit: 0, sample: [] };
    tags[t].n++;
    tags[t].profit += Number(r.profit_usd || 0);
    if (tags[t].sample.length < 2) tags[t].sample.push(r.block_number);
  }
  for (const f of ["interest_accrued", "oracle_poke_block", "bad_debt"]) {
    if (d[f] === true) flags[f] = (flags[f] || 0) + 1;
  }
}

const kindMap = Object.fromEntries(kinds.map((k) => [k.kind, k.n]));
const hits = [];
const misses = [];
for (const [id, typ, key] of CHECKLIST) {
  let n = 0;
  if (typ === "kind") n = kindMap[key] || 0;
  else if (typ === "tag") n = tags[key]?.n || 0;
  else if (typ === "flag") n = flags[key] || 0;
  (n > 0 ? hits : misses).push({ id, key, typ, n });
}

const out = {
  window: [from, to],
  classified,
  total,
  pct: Number(((100 * classified) / total).toFixed(2)),
  ops,
  kinds,
  tags,
  flags,
  checklist_hits: hits,
  checklist_misses: misses,
  hit_rate: `${hits.length}/${CHECKLIST.length}`,
};

console.log(JSON.stringify(out, null, 2));

const logPath = "cache/phase2_backfill_1d.log";
if (existsSync(logPath)) {
  const t = readFileSync(logPath, "utf8");
  const done = /Backfill done/.test(t);
  console.error(done ? "LOG: backfill done" : "LOG: still running / incomplete");
}
