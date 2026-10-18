import { readFileSync, writeFileSync } from "node:fs";
const hist = readFileSync("hist.toml", "utf8");
const urls = [...hist.matchAll(/"https?:\/\/[^"]+"/g)].map((x) => x[0]);
const out = `chain = "avalanche"
gas_model = "historical_exact"
output = "table"
priority_fee_gwei = 30
strategies = "all"
rps_limit = 40
block_concurrency = 32
rpc_rps = [
  20.0,
  20.0,
]
rpc_urls = [
  ${urls.join(",\n  ")},
]

[chains.avalanche]
chain_id = 43114

[chains.avalanche.rpc]
rpc_rps = [
  20.0,
  20.0,
]
rpc_urls = [
  ${urls.join(",\n  ")},
]

[explorer]
confirmations = 4
poll_interval_ms = 2000
`;
writeFileSync("mev-scout.phase2-avax.toml", out);
console.log("wrote clean config");
