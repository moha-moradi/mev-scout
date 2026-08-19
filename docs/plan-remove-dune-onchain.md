# Remove Dune — Full On-Chain Replacement Plan

> Status: pending execution
> Date: 2026-08-19
> Scope: mev-scout (core + cli)

## Context

Dune has closed our account. The **main pipeline (`run`/`fetch`/`replay`/`report`) is already
100% RPC-based** and needs no changes. Pools and tokens already have on-chain paths
(RPC `eth_getLogs` discovery + `eth_call` metadata) and degrade gracefully when no Dune key is
configured. Only 27 of 141 SQL templates are used by automated code; 113 are interactive-only
(`dune-query` registry) and 5 are dead.

**Decision (user-confirmed):** remove Dune completely (no feature flag), replace all auxiliary
components with on-chain/RPC implementations.

## Current Dune Touchpoints (to remove/replace)

| Area | Files | Replacement |
|---|---|---|
| Dune client + SQL | `core/src/dune/{client,queries,types,consts}.rs` | Delete; new `core/src/chain/` module |
| Pool discovery via Dune | `core/src/dune/pool_discovery.rs`; `core/src/pool/discovery/mod.rs:1309` (dead wrapper); `cli/src/commands/discover.rs:217-275` | On-chain already exists (`discover_pools` mod.rs:582); delete Dune branches |
| Token cache bulk | `core/src/cache/token_cache.rs:85-138` (`fetch_from_dune`); `discover.rs:76-88` | Delete `fetch_from_dune`; see **Token strategy** below |
| Token discovery | `core/src/dune/token_discovery.rs` (QUERY_TOKENS_ALL/ACTIVE/NEW/TVL); `cli/src/commands/tokens.rs` | On-chain: tokens derived from discovered pools + RPC metadata; stats from trade scanner |

## Token strategy (user decision — replaces Dune bulk token fetch)

Goal: avoid RPC calls when resolving pool token metadata, without Dune.

**Important:** token **addresses** come for free from factory events
(`PairCreated`/`PoolCreated` emit `token0`/`token1`) during pool discovery — no extra RPC.
Dune was only used for symbol/decimals, and only symbols are display-only: **decimals are
required for MEV math** (quoting, USD conversion, pool state), so metadata cannot be skipped
entirely.

3-layer approach:
1. **Addresses** — from factory event args during pool discovery (existing path, no change).
2. **Common tokens** — bundled static data: extend `core/data/known_tokens.json` with a
   community token list (e.g. Quickswap/Uniswap/PancakeSwap tokenlists, ~500 tokens/chain).
   Covers the vast majority of pool pairs with **zero RPC**.
3. **Long-tail tokens** — lazy batched `eth_call` `symbol()`/`decimals()` for uncached tokens
   (existing path `core/src/pool/discovery/mod.rs:1120-1156` + `TokenCache::missing()`),
   one-time cost per token, persisted to SQLite forever (`token_symbols` table).

Explicitly dropped: Dune's `QUERY_ALL_TOKENS` scanned the entire token universe (~100k
tokens) — overkill; the MEV-relevant set is only tokens in discovered pools.
| Audit ground truth | `core/src/dune/audit.rs`; `cli/src/commands/audit.rs` | **DELETE** (user decision: no longer needed) |
| Monthly report | `core/src/dune/report.rs` (16 VALIDATE_*); `cli/src/commands/dune_report.rs` | Delete command; existing RPC `report` command already aggregates per-strategy results |
| Block finder | `cli/src/commands/dune_find_blocks.rs` | **DELETE** (user decision: sandwich/arbitrage block identification no longer needed) |
| Diagnostics | `cli/src/commands/dune_check.rs`, `dune_query.rs` | Replace with one `scan` command (trades/transfers/flashloans/liquidations/labels) |
| Backtest candidates | `core/tests/backtest.rs:78-183` (`dune_find_candidate_blocks`) | Sample a recent window directly from RPC (no candidate ranking needed) |
| Config/CLI plumbing | `core/src/config/settings.rs` (DuneConfig/DuneOverrides), `cli/src/cli.rs` (5 Dune arg structs), `cli/src/overrides.rs`, `mev-scout.toml` | Remove dune fields; keep coingecko_api_key + price_oracle_mode |
| Timing helpers | `core/src/dune/util.rs` (chain_timing, block_timestamp_secs, estimate_latest_block) | Move to `core/src/chain/timing.rs` (keep — not Dune-specific); drop `dune_chain_label`/`approx_block_month_min`/`render_query`/`dune_indexing_lag_blocks` |
| Tests | `core/tests/dune_api.rs` (delete), `core/tests/backtest.rs` (rework) | Replace with decoder unit tests + opt-in E2E (MEV_SCOUT_E2E pattern) |

## New module: `core/src/chain/` (on-chain, RPC-driven)

1. **`events.rs`** — event topic constants + log decoders (with unit tests):
   - ERC20 `Transfer`; Uniswap V2 `Swap`/`Sync`; Uniswap V3 + Algebra `Swap`; `PairCreated`/`PoolCreated`
   - Flash loans: Uniswap V3 `Flash`, Balancer Vault `FlashLoan`, Aave V2/V3 `FlashLoan`
   - Liquidations: Aave V3 `LiquidationCall`, Compound V3 `Absorb`
   - Curve `TokenExchange`
2. **`scanner.rs`** — chunked `eth_getLogs` range scanner (topics-only scans need no address
   lists; optional address filters for pool/token-specific scans). Chunk sizing, retry, respects
   existing RPC RPS limits; reuses `RpcClient`.
3. **`flashloans.rs`** — topic-only scan over range → `{block, tx_hash, protocol, token, amount}`.
   Protocols identified by emitting contract or topic decode.
4. **`liquidations.rs`** — topic-only scan → `{block, tx, user, liquidator, collateral_asset,
   debt_asset, collateral_amount, debt_to_cover}`.
5. **`trades.rs`** — Swap-event scanner for V2/V3/Algebra pools (replaces QUERY_TRADES_IN_RANGE).
   USD value via existing price oracle (CoinGecko/on-chain hybrid).
6. **`transfers.rs`** — ERC20 Transfer scanner (whale detection), token metadata from token
   cache, USD from price oracle.
7. **`candidate_blocks.rs`** — ~~backtest block ranking~~ **DROPPED** (user decision: no block
   identification needed). Backtest samples a recent window directly.
8. **`labels.rs`** — address labels: bundled static JSON snapshot (CEX hot wallets, DEX/bridge
   protocol contracts, MEV bot addresses — open data, e.g. DefiLlama protocol addresses) +
   optional runtime fetch of DefiLlama API (free, no key) cached to SQLite.
9. **`timing.rs`** — moved from `dune/util.rs`.

## CLI changes

- **`scan` command** (replaces `dune-query`):
  `mev-scout scan --kind trades|transfers|flashloans|liquidations|labels --from-block N --to-block M [--address X]`
  — reuses the existing table/JSON/CSV output.
- Delete `dune-check`, `dune-find-blocks`, `dune-report`, **`audit`** commands.
- `tokens` command: on-chain mode (pool-derived token set + RPC metadata; filters reworked:
  active = seen in swap scans, TVL = reserves × price).
- Expand `core/data/known_tokens.json` with community token lists (per chain).
- `discover` command: on-chain only; remove `--source dune|all`, `--min-pools` Dune semantics,
  token cache Dune warm-up.
- `cli.rs`: remove Dune arg structs + `--dune-api-key`; `overrides.rs`: drop Dune plumbing.

## Config changes

- Remove `dune_api_key` (+ commented backup keys) and any `dune_query_ids` from
  `mev-scout.toml`, `config/settings.rs` (DuneConfig), validation.
- Keep `coingecko_api_key`, `price_oracle_mode`, RPC settings.

## Verification

1. `cargo build` + `cargo test` (unit + decoder tests) — no Dune references left
   (grep `dune` case-insensitive).
2. Local smoke: `discover --chain polygon` (on-chain pools + tokens) with no Dune key — must work.
3. E2E (opt-in): `MEV_SCOUT_E2E=1 RPC_URL=... cargo test --test backtest` — candidate blocks
   from on-chain ranking, detection via existing RPC pipeline.
4. `scan --kind flashloans --from-block ... --to-block ...` returns decoded events.

## Execution phases

1. **Phase 1 — Removal:** delete `core/src/dune/` (keep timing helpers → `chain/timing.rs`),
   Dune branches in discover/token_cache, delete `audit`/`dune-*` commands (incl. `dune-find-blocks`,
   `dune_report`), delete `dune_api.rs` test, rework `backtest.rs` to sample a recent RPC window.
   Build green.
2. **Phase 2 — On-chain core:** `chain/{events,scanner,flashloans,liquidations,trades,transfers,labels}.rs`
   + unit tests.
3. **Phase 3 — CLI rewiring:** `scan` command, rework `tokens`/`discover`, delete
   `dune-*` commands.
4. **Phase 4 — Backtest + docs:** on-chain candidate ranking, `mev-scout.toml` + README
   cleanup, full verification.

## Notes / tradeoffs

- Historical USD amounts (flash loans, whale transfers) depend on price oracle accuracy;
  on-chain oracle may lack long-tail tokens → amounts shown in raw units when USD unavailable.
- Wide-range topic scans are RPC-heavy; scanner chunks + respects `rpc_rps`/`rps_limit`.
  Candidate ranking (headers only) is cheap.
- Sandwich/arbitrage ground-truth comparison disappears with Dune — **audit command deleted**
  and sandwich/arbitrage block identification removed (user decisions); our own detectors
  remain the sole detection source.
- `QUERY_TRADES_IN_BLOCK`/etc. interactive templates are replaced by the `scan` command;
  saved-query-by-ID (`execute_query_by_id`) is removed along with the client.
