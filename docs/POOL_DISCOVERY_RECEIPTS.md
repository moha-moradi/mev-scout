# Pool discovery without `eth_getLogs` — implementation status

Implements `docs/pool-discovery-benchmark-plan.md`. The default discovery source
remains **`hybrid`**; the receipts path is opt-in and TOML-only.

## Phase 0 — capability matrix

Probed with `core/examples/probe_rpc_capabilities.rs`; raw output:
`bench/capability-matrix.json`.

| Test | Peer | Result | Evidence |
| --- | --- | --- | --- |
| P1a `eth_blockNumber` | publicnode | OK (~0.7–1.1 s) | tip `101335035` in latest run |
| P1b `eth_getBlockReceipts(tip)` | publicnode | OK (HTTP 200) | receipt array; occasionally `null` before the tip is indexed |
| P2 `eth_getBlockReceipts(tip-30d)` | publicnode | OK (~0.4 s) | 30-day retention confirmed |
| P3 JSON-RPC batch of 4 receipt calls | publicnode | OK (~0.4 s) | 4 items in one HTTP response |
| P4 `allPoolsLength()` / `getAllPoolsLength()` @ latest | publicnode | FAIL | both selectors: `-32000 execution reverted` |
| P4 `allPoolsLength()` / `getAllPoolsLength()` @ tip-30d | publicnode | FAIL | both selectors: `-32002 request timed out` |
| P5 address-less `eth_getLogs(tip)` | publicnode | OK (~0.6 s) | single block only |
| P5 address-less `eth_getLogs(tip, tip-10)` | publicnode | FAIL | `-32701 please specify an address` |

| P1a `eth_blockNumber` | Dwellir | OK (~0.4 s) | tip `101335093` in latest run |
| P1b `eth_getBlockReceipts(tip)` | Dwellir | OK (~0.25 s) | receipt array |
| P2 `eth_getBlockReceipts(tip-30d)` | Dwellir | OK (~0.27 s) | 30-day retention confirmed |
| P3 JSON-RPC batch of 4 receipt calls | Dwellir | OK (~0.47 s) | 4 items in one HTTP response |
| P4 `allPoolsLength()` / `getAllPoolsLength()` @ latest | Dwellir | FAIL | both selectors: `-32000 execution reverted` |
| P4 `allPoolsLength()` / `getAllPoolsLength()` @ tip-30d | Dwellir | FAIL | both selectors: `-32000 execution reverted` |
| P5 address-less `eth_getLogs(tip)` | Dwellir | FAIL | HTTP 403: plan does not support `eth_getLogs` |
| P5 address-less `eth_getLogs(tip, tip-10)` | Dwellir | FAIL | HTTP 403: plan does not support `eth_getLogs` |

The initial P4 probe used the wrong selector (`0x5742b50e`); it was corrected to
`keccak256("allPoolsLength()")[..4] = 0xefde4e64` with `getAllPoolsLength()` as
a second candidate. Both selectors revert on both peers, so the failure is not
explained by a bad selector. The Dwellir API key is redacted from
`bench/capability-matrix.json` and all benchmark JSON. Factory enumeration
helpers (`allPoolsLength`/`allPairsLength`/`pool_list`) do not exist in this
checkout, so option 1 stays intentionally unwired. `eth_getLogs` cannot serve
30-day range scans on either peer: publicnode rejects address-less ranges with
`-32701`, Dwellir with 403.

## Phase 1–2 — implementation

- All factory decoders are reusable `decode_*(...)` functions shared by the
  existing `getLogs` scanner and the new receipts scanner. A regression test
  feeds one synthetic factory log per supported DEX family through the unified
  `decode_factory_creation_log` (V2, V3, Solidly, Camelot, Curve, Balancer,
  Trader Joe, Pendle, V4, Infinity, Metric, Fluid).
- `source = "receipts"` is accepted only from TOML (`[discover]`). It reuses
  `resolve_scan_window`, `lookback_scan_ranges`, the discovery watermark,
  `enrich`, health check, metadata resolution and universe persistence.
- Receipt windows are fetched with batched
  `eth_getBlockByNumber` + `eth_getBlockReceipts` (`receipt_batch_blocks`,
  default 4) and bounded by `rpc_concurrency`. `receipt_recent_blocks` pins the
  trailing blocks to provider 0 and older blocks to provider 1 (benchmark M3).
- `ProgressEvent{stage:"fetch"}` reports `done`/`total` per window. The job
  prints an explicit startup cost estimate, e.g. 30 days ≈ 1.3M blocks ≈ 325k
  batch POSTs ≈ 4–6 h on public peers at concurrency 8.
- Bundles can be cached with `receipt_cache_blocks = true` so later runs can
  reuse `get_receipts`. Off by default because a 30-day scan needs significant
  disk.
- **Watermark safety:** `failed_windows` is surfaced by `ReceiptScanResult`;
  `note_discovery_scan` is called only when it is empty. A wiremock test asserts
  that `null` receipts mark the whole window failed.
- RPC attempt/error/latency/byte counters are recorded in `RpcClient` and
  returned as `DiscoverOutcome.rpc_metrics` for the benchmark harness.

### Example configuration

```toml
[discover]
source = "receipts"
lookback_days = 30
rpc_concurrency = 8
receipt_batch_blocks = 4
receipt_cache_blocks = false
# receipt_recent_blocks = 1000  # M3 only; needs >= 2 rpc_urls
```

Validation rejects `receipt_batch_blocks = 0`, `rpc_concurrency = 0`,
`receipt_recent_blocks = 0`, and `receipt_recent_blocks` with fewer than two
providers.

## Phase 3 — benchmark harness

`core/examples/pool_discovery_bench.rs` drives the same `job_discover` for every
method with identical discovery settings (`min_tvl = 0`, `max_pools = 100000`,
`health_check = false`, `resolve_remote_metadata = false`, `enrich = false`,
`batch_size = 2000`, `rpc_concurrency = 8`).

```bash
cargo run -p mev-scout-core --example pool_discovery_bench -- M1 W1 --repeat 3
cargo run -p mev-scout-core --example pool_discovery_bench -- M2 W1 --repeat 3 \
  --rpc2 "$DWELLIR_AVALANCHE_URL"
cargo run -p mev-scout-core --example pool_discovery_bench -- M3 W1 --repeat 3 \
  --rpc1 https://avalanche-c-chain.publicnode.com --rpc2 "$DWELLIR_AVALANCHE_URL"
# 100-block phase-5 smoke:
cargo run -p mev-scout-core --example pool_discovery_bench -- M1 W1 --blocks 100
```

Each run writes `bench/pool-discovery-<method>-<window>.json` (metrics, RPC
counters, logs and discovered addresses) and prints a summary table to stdout.
M4 is recorded as `skipped` until P4 and a benchmark win exist. The Dwellir
free tier needs ~1 RPS; without provider-level throttling M2 W1 produced 220+
failed windows, so the harness applies `rpc_rps = 1.0` to Dwellir-only runs.

### W1 results (1000 blocks, 3 runs each)

| Method | Status | Pools (3 runs) | With `creation_block` | DEX families | Median time | RPC calls (3 runs) | Discovery errors |
| --- | --- | ---: | ---: | --- | ---: | ---: | ---: |
| M0 `hybrid` | ok | 27 / 27 / 27 | 0 / 0 / 0 | 3 | 85.0 s | 71 / 71 / 71 | 0 |
| M1 `receipts` (publicnode) | ok | 100 / 100 / 101 | 100 / 100 / 101 | 3 | 14.0 s | 488 / 563 / 566 | 0 |
| M2 `receipts` (Dwellir) | ok | 100 / 68 / 97 | 100 / 68 / 97 | 3 | 515.0 s | 510 / 478 / 553 | 0 |
| M3 `receipts` (mixed) | ok | 110 / 108 / 51 | 110 / 108 / 51 | 4 | 171.5 s | 565 / 627 / 452 | 0 |
| M4 `factory_state` | skipped | 0 | 0 | 0 | 0 | 0 | 0 |
| M5 `remote` | ok | 27 / 27 / 27 | 0 / 0 / 0 | 3 | 5.6 s | 1 / 1 / 1 | 0 |

M0/M5 return the same remote snapshot (no `creation_block`) because address-less
`eth_getLogs` ranges are rejected by publicnode and forbidden by Dwellir; their
on-chain coverage is effectively zero. M1–M3 return only factory-created pools
with block provenance. M3 run 1 overlapped M1 run 1 by 52/100 addresses and
found one extra family (Balancer); its third run had lower activity (51 pools)
but still zero failed windows. RPC retry counters are non-zero for M0–M3
(transient rate limits / address-less log rejections) but no discovery-window
failures were recorded after throttling.

A 100-block smoke run on publicnode produced M1 = 22 pools in 1.9 s, M0 = 27
remote-only pools in 84.7 s, M5 = 27 pools in 6.4 s, with 1/22 M1–M5 overlap.

W2/W3 were not run: at the measured rates, M1 W2 ≈ 70 min and W3 ≈ 5 h, while
Dwellir-throttled M2/M3 W2 ≈ 21 h and W3 ≈ 90 h per run. **No default change is
made:** `hybrid` stays the default, `receipts` is opt-in, and the
`factory_state` fast-path stays inactive until it proves ≥80% of M3 coverage at
<1/10 of M3 time.

With the planned pruned local node (`docs/LOCAL_AVALANCHE.md`), this split is
intentional: `receipts` is a one-time backfill while the node still lacks
history before its sync date; `hybrid` becomes the steady-state default once
the node has 30 days of address-less `eth_getLogs` coverage. No
`onchain_engine` switch was added — the source string stays `hybrid` and the
backfill lives in a separate TOML profile.

## Phase 5 — tests and rollout

- `cargo check --workspace --all-targets` — clean.
- `cargo test -p mev-scout-core --lib pool::discovery` — factory decoder and
  receipt-log conversion regressions pass.
- `cargo test -p mev-scout-core --test receipts_batch` — wiremock tests for
  one-POST multi-block batches, provider pinning and `null` receipts /
  `failed_windows`.
- 100-block smoke and W1 runs above; use `--blocks 100` before any W2/W3 run.
- Startup cost is logged before the first receipt fetch; keep
  `receipt_cache_blocks = false` unless disk capacity is explicitly provisioned.
- Dwellir's free tier should stay at `rpc_rps = 1.0`; raising concurrency
  produces `failed_windows` and prevents watermark advancement.
