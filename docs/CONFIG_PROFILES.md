# Config profiles — which RPC set for which job

## First: TVL does **not** come from `eth_getLogs`

`eth_getLogs` is only for **on-chain discovery**: finding factory-creation or
DEX-activity logs so we know which pool addresses exist and at which block.

TVL and volume come from **GeckoTerminal + DexScreener over plain HTTPS**
(`fetch_remote` → `apply_explorer_ranking`). Health checks use `eth_call` at
`latest`. So:

| Need | Path | RPC method |
| --- | --- | --- |
| Discover pool addresses + `creation_block` on a mature local node | `source = "hybrid"` / `onchain` | address-less `eth_getLogs` (local only) |
| Discover pool addresses + `creation_block` on public peers | `source = "receipts"` | `eth_getBlockByNumber` + `eth_getBlockReceipts`, filtered locally |
| Current TVL-ranked list | `source = "remote"` | no log RPC; aggregator HTTPS only |
| Explorer swap backfill | `explorer index` | batched block + receipts, decoded locally |

## Profiles

All files are in the repo root. Export the variables first:

```bash
export AVAX_RPC_URL='http://127.0.0.1:9650/ext/bc/C/rpc'   # or Caddy URL from the laptop
export DWELLIR_AVALANCHE_KEY='...'                          # optional for public-only profiles
```

| Profile | Peers | Job | Discovery path | Use when |
| --- | --- | --- | --- | --- |
| `mev-scout.local-discover.toml` | local first, Dwellir archive fallback | `discover` | `hybrid` → local address-less `eth_getLogs` + remote TVL ranking | Steady state, after ~30 days of node uptime. Before that add `--blocks N`. |
| `mev-scout.local-explorer-backfill.toml` | local first, Dwellir fallback | `explorer index` | batched block + receipts, no `eth_getLogs` | Historical backfill from the node's own disk, gentle on RAM. |
| `mev-scout.local-live.toml` | local first, Dwellir fallback | `live` | address-list `eth_getLogs` | Live/pending feeds. No public equivalent works: publicnode caps 10 addresses, Dwellir returns 403. |
| `mev-scout.public-discover.toml` | publicnode + Dwellir at 1 RPS | `discover` | `receipts`, no `eth_getLogs` | One-off 30-day backfill before the local node has history. |
| `mev-scout.public-explorer-backfill.toml` | publicnode + Dwellir at 1 RPS | `explorer index` | batched block + receipts, no `eth_getLogs` | Same backfill when the local node does not exist yet. |
| `mev-scout.remote-pool-list.toml` | publicnode for startup check only | `discover` | `remote`, no log RPC | Quick current top-pool list; no `creation_block`. |

`mev-scout.local.toml` and `mev-scout.index.toml` remain as the older, narrower
profiles. `mev-scout.example.toml` is the generated multi-chain reference.

## Recommended sequence around the local-node launch

```bash
# 1. No local node yet: seed creation history via receipts (smoke first).
./target/release/mev-scout --config mev-scout.public-discover.toml discover --blocks 100
./target/release/mev-scout --config mev-scout.public-discover.toml discover

# 2. Node is up but only has recent blocks: local work inside its uptime.
./target/release/mev-scout --config mev-scout.local-discover.toml discover --blocks 2000
./target/release/mev-scout --config mev-scout.local-explorer-backfill.toml \
  explorer index --from-block $RANGE_START --to-block $RANGE_END

# 3. After 30 days: plain hybrid, incremental via watermark.
./target/release/mev-scout --config mev-scout.local-discover.toml discover

# Optional control / listing-only run at any time:
./target/release/mev-scout --config mev-scout.remote-pool-list.toml discover
```

Do not run `discover`, `live --loop` and `explorer index` against the same
SQLite file at the same time — there is a single writer.

Benchmark evidence and cost estimates: `docs/POOL_DISCOVERY_RECEIPTS.md`.
Node setup, Caddy, firewall and disk layout: `docs/LOCAL_AVALANCHE.md`.
