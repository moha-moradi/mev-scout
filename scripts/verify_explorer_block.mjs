#!/usr/bin/env node
/**
 * Index one block (or range) with mev-scout explorer, then check kind/tag presence.
 *
 *   node scripts/verify_explorer_block.mjs --chain avalanche --from 97782322 --tag flash_loan_liq
 *   node scripts/verify_explorer_block.mjs --chain avalanche --from 97783552 --to 97783552 --kind arb_atomic --tag long_tail
 */
import { spawnSync } from "node:child_process";
import { DatabaseSync } from "node:sqlite";
import { existsSync } from "node:fs";
import path from "node:path";

function parseArgs(argv) {
  const out = {
    chain: "avalanche",
    from: null,
    to: null,
    tag: null,
    kind: null,
    config: null,
    skipIndex: false,
  };
  for (let i = 2; i < argv.length; i++) {
    const a = argv[i];
    const next = () => argv[++i];
    if (a === "--chain") out.chain = next();
    else if (a === "--from") out.from = Number(next());
    else if (a === "--to") out.to = Number(next());
    else if (a === "--tag") out.tag = next();
    else if (a === "--kind") out.kind = next();
    else if (a === "--config") out.config = next();
    else if (a === "--skip-index") out.skipIndex = true;
    else throw new Error(`unknown arg ${a}`);
  }
  if (out.from == null) throw new Error("--from required");
  if (out.to == null) out.to = out.from;
  return out;
}

const args = parseArgs(process.argv);
const config =
  args.config ||
  (existsSync("mev-scout.avalanche-run.toml") && args.chain === "avalanche"
    ? "mev-scout.avalanche-run.toml"
    : existsSync("mev-scout.local.toml")
      ? "mev-scout.local.toml"
      : "mev-scout.example.toml");

const exe = existsSync("target/release/mev-scout.exe")
  ? "target/release/mev-scout.exe"
  : existsSync("target/release/mev-scout")
    ? "target/release/mev-scout"
    : "mev-scout";

if (!args.skipIndex) {
  // Ensure config chain matches; for non-avalanche write a tiny override file.
  let cfgPath = config;
  if (args.chain !== "avalanche") {
    const fs = await import("node:fs");
    cfgPath = `cache/_hunt_${args.chain}.toml`;
    const rpc =
      args.chain === "polygon"
        ? "https://polygon-bor-rpc.publicnode.com"
        : "https://ethereum-rpc.publicnode.com";
    fs.writeFileSync(
      cfgPath,
      `chain = "${args.chain}"\nrpc_urls = ["${rpc}"]\nrps_limit = 8\n\n[chains.${args.chain}]\nchain_id = ${args.chain === "polygon" ? 137 : 1}\n\n[chains.${args.chain}.rpc]\nrpc_urls = ["${rpc}"]\nrpc_rps = [8.0]\n\n[explorer]\nconfirmations = 4\npoll_interval_ms = 2000\n`,
    );
  }
  console.error(
    `indexing ${args.chain} ${args.from}..${args.to} via ${exe} -f ${cfgPath}`,
  );
  const r = spawnSync(
    exe,
    [
      "-f",
      cfgPath,
      "explorer",
      "index",
      "--from-block",
      String(args.from),
      "--to-block",
      String(args.to),
    ],
    { encoding: "utf8", stdio: ["ignore", "pipe", "pipe"] },
  );
  if (r.status !== 0) {
    console.error(r.stdout);
    console.error(r.stderr);
    process.exit(r.status || 1);
  }
  console.error(r.stdout?.split("\n").slice(-8).join("\n"));
}

const dbPath = path.join("cache", `explorer-${args.chain}.sqlite`);
const db = new DatabaseSync(dbPath);
const rows = db
  .prepare(
    `SELECT block_number, kind, tx_hash, details_json, profit_usd
     FROM mev_ops
     WHERE block_number BETWEEN ? AND ?
     ORDER BY block_number, kind`,
  )
  .all(args.from, args.to);

const matched = [];
for (const row of rows) {
  let tags = [];
  let flags = {};
  try {
    const d = JSON.parse(row.details_json || "{}");
    tags = d.tags || [];
    flags = {
      interest_accrued: d.interest_accrued === true,
      oracle_poke_block: d.oracle_poke_block === true,
      bad_debt: d.bad_debt === true,
      protocol: d.protocol,
    };
  } catch {
    /* ignore */
  }
  const tagOk = !args.tag || tags.includes(args.tag);
  const kindOk = !args.kind || row.kind === args.kind;
  const flagOk =
    !args.tag ||
    !(
      args.tag === "interest_accrued" ||
      args.tag === "oracle_poke_block"
    ) ||
    flags[args.tag] === true ||
    tags.includes(args.tag);
  if (kindOk && tagOk && flagOk) {
    matched.push({
      block: row.block_number,
      kind: row.kind,
      tx: row.tx_hash,
      tags,
      flags,
      profit_usd: row.profit_usd,
      protocol: flags.protocol,
    });
  }
}

const result = {
  chain: args.chain,
  from: args.from,
  to: args.to,
  want_tag: args.tag,
  want_kind: args.kind,
  ops_in_window: rows.length,
  matched: matched.length,
  hits: matched.slice(0, 20),
  ok: matched.length > 0,
};
console.log(JSON.stringify(result, null, 2));
process.exit(result.ok ? 0 : 1);
