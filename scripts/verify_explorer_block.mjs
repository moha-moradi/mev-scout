#!/usr/bin/env node
/**
 * Index one block (or range) with mev-scout explorer, then verify the persisted
 * op matches the claim being checked.
 *
 *   node scripts/verify_explorer_block.mjs -f hist.toml --chain avalanche \
 *     --from 97782322 --expect kind=arb_atomic,min-profit-usd=1
 *
 * The DB path comes from `--db`, else `[explorer].db_path` in the `-f` config,
 * else the chain-derived default. The config must already enable the chain;
 * this script never synthesizes a public-RPC config.
 */
import { spawnSync } from "node:child_process";
import { DatabaseSync } from "node:sqlite";
import { existsSync, readFileSync } from "node:fs";
import path from "node:path";

function parseArgs(argv) {
  const out = {
    chain: "avalanche",
    from: null,
    to: null,
    tag: null,
    kind: null,
    config: null,
    db: null,
    expect: [],
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
    else if (a === "--config" || a === "-f") out.config = next();
    else if (a === "--db") out.db = next();
    else if (a === "--expect") out.expect.push(next());
    else if (a === "--skip-index") out.skipIndex = true;
    else throw new Error(`unknown arg ${a}`);
  }
  if (out.from == null) throw new Error("--from required");
  if (out.to == null) out.to = out.from;
  if (!out.config) {
    throw new Error(
      "--config/-f required: pass the same config used for indexing (it names the explorer DB and chain)",
    );
  }
  return out;
}

/** Minimal section reader for the flat TOML shapes this repo uses. */
function parseConfigSections(text) {
  const sections = {};
  let current = "";
  for (const raw of text.split(/\r?\n/)) {
    const line = raw.replace(/#.*$/, "").trim();
    if (!line) continue;
    const section = line.match(/^\[(.+)\]$/);
    if (section) {
      current = section[1];
      if (!sections[current]) sections[current] = {};
      continue;
    }
    const kv = line.match(/^([A-Za-z0-9_.-]+)\s*=\s*(.+)$/);
    if (kv) {
      if (!sections[current]) sections[current] = {};
      sections[current][kv[1]] = kv[2].trim();
    }
  }
  return sections;
}

function unquote(value) {
  return value?.replace(/^"|"$/g, "");
}

function parseExpect(values) {
  const out = {};
  for (const value of values) {
    for (const pair of value.split(",")) {
      const [key, raw] = pair.split("=");
      if (!key || raw === undefined) {
        throw new Error(`bad --expect '${value}' (want key=value[,key=value])`);
      }
      if (key === "kind") out.kind = raw;
      else if (key === "searcher") out.searcher = raw.toLowerCase();
      else if (key === "min-profit-usd") out.minProfitUsd = Number(raw);
      else if (key === "confidence") out.confidence = raw;
      else if (key === "pnl-basis") out.pnlBasis = raw;
      else throw new Error(`unknown --expect key '${key}'`);
    }
  }
  if (out.minProfitUsd !== undefined && !Number.isFinite(out.minProfitUsd)) {
    throw new Error("--expect min-profit-usd must be a number");
  }
  return out;
}

const args = parseArgs(process.argv);
const expect = parseExpect(args.expect);
if (expect.kind == null) expect.kind = args.kind;

const configText = readFileSync(args.config, "utf8");
const sections = parseConfigSections(configText);
const topChain = unquote(sections[""]?.chain);
const chainSection = sections[`chains.${args.chain}`];
if (!chainSection && topChain !== args.chain) {
  throw new Error(
    `chain '${args.chain}' is not enabled in ${args.config} ` +
      `(top-level chain is '${topChain ?? "unset"}'); add [chains.${args.chain}] ` +
      `or run with --chain ${topChain ?? "<name>"}`,
  );
}

const configuredDb = unquote(sections["explorer"]?.db_path);
const dbPath =
  args.db || configuredDb || path.join("cache", `explorer-${args.chain}.sqlite`);
if (!existsSync(dbPath)) {
  throw new Error(
    `explorer DB not found: ${dbPath} (from ${args.config}); ` +
      `set --db or [explorer].db_path to the database the indexer wrote`,
  );
}

const exe = existsSync("target/release/mev-scout.exe")
  ? "target/release/mev-scout.exe"
  : existsSync("target/release/mev-scout")
    ? "target/release/mev-scout"
    : "mev-scout";

if (!args.skipIndex) {
  console.error(
    `indexing ${args.chain} ${args.from}..${args.to} via ${exe} -f ${args.config}`,
  );
  const r = spawnSync(
    exe,
    [
      "-f",
      args.config,
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

const db = new DatabaseSync(dbPath);
const rows = db
  .prepare(
    `SELECT block_number, kind, tx_hash, details_json, profit_usd, eoa, confidence
       FROM mev_ops
      WHERE block_number BETWEEN ? AND ?
      ORDER BY block_number, kind, id`,
  )
  .all(args.from, args.to);

const matched = [];
for (const row of rows) {
  let details = {};
  try {
    details = JSON.parse(row.details_json || "{}");
  } catch {
    /* keep the row; a malformed details blob fails the expectation below */
  }
  const tags = Array.isArray(details.tags) ? details.tags : [];
  const pnlBasis = details.pnl_basis ?? details.pnl?.basis ?? null;
  const tagOk =
    !args.tag || tags.includes(args.tag) || details[args.tag] === true;
  const kindOk = !args.kind || row.kind === args.kind;
  const expectKindOk = !expect.kind || row.kind === expect.kind;
  const searcherOk =
    !expect.searcher ||
    String(row.eoa || "").toLowerCase() === expect.searcher;
  const profitOk =
    expect.minProfitUsd === undefined ||
    (row.profit_usd != null && row.profit_usd >= expect.minProfitUsd);
  const confidenceOk = !expect.confidence || row.confidence === expect.confidence;
  const basisOk = !expect.pnlBasis || pnlBasis === expect.pnlBasis;
  if (
    tagOk &&
    kindOk &&
    expectKindOk &&
    searcherOk &&
    profitOk &&
    confidenceOk &&
    basisOk
  ) {
    matched.push({
      block: row.block_number,
      kind: row.kind,
      tx: row.tx_hash,
      searcher: row.eoa,
      confidence: row.confidence,
      pnl_basis: pnlBasis,
      tags,
      profit_usd: row.profit_usd,
      protocol: details.protocol,
    });
  }
}

const result = {
  chain: args.chain,
  config: args.config,
  db: dbPath,
  from: args.from,
  to: args.to,
  want_tag: args.tag,
  want_kind: args.kind,
  expect,
  ops_in_window: rows.length,
  matched: matched.length,
  hits: matched.slice(0, 20),
  ok: matched.length > 0,
};
console.log(JSON.stringify(result, null, 2));
if (!result.ok) {
  console.error(
    `no op matched ${JSON.stringify(expect)} in ${dbPath} over ${args.from}..${args.to}`,
  );
}
process.exit(result.ok ? 0 : 1);
