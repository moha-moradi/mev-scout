# Avalanche pool-discovery backfill pilot (Stage 1)

**Date:** 2026-11-28  
**Status:** measured; 30-day full backfill not yet executed.  
**Harness:** `core/examples/stage1_avalanche_bench.rs`  
**Raw artifacts:** `bench/stage1/*.json`, `bench/stage1-repeat/*.json`

## Objective

Decide the simplest on-chain acquisition path that can process the pools we monitor over the past month in a few hours, using public RPC or the existing Dwellir archive endpoint, before starting Stages 2–4.

## Method

Only the three paths in the reduced plan were compared:

1. address/topic-filtered `eth_getLogs` on PublicNode and Dwellir;
2. the existing batched `eth_getBlockReceipts` scanner;
3. minimal block-polling + per-block receipt discovery.

For every run the harness froze a canonical head before scanning, then used explicit `from_block`/`to_block` ranges ending at that head. Settings were neutral: `min_tvl = 0`, `max_pools = 100_000`, `health_check = false`, `resolve_remote_metadata = false`, no remote fetch, `factory_batch_size = 1_000`, `receipt_batch_blocks = 4`, PublicNode concurrency 8. Dwellir was tested at the reported 20 RPS and at lower safe settings.

Credits/units are not observable from the client. `quota_tier` and `credits_used` are explicit inputs for provider-dashboard values and remained unset in these runs.

## Results

### PublicNode

| Path | Range | Runs | Time (median) | Throughput (median) | POSTs (median) | Failed windows | Pools |
|---|---|---:|---:|---:|---:|---:|---:|
| Filtered `eth_getLogs` | 1,000 | 1 | 6.0 s | 166 blocks/s | 2 | 0 | 0 creation events |
| Filtered `eth_getLogs` | 10,000 | 3 | 8.4 s | 1,192 blocks/s | 20 | 0 | 0 creation events |
| Batched receipts | 1,000 | 1 | 15.4 s | 64.8 blocks/s | 531 | 0 | 92 |
| Batched receipts | 10,000 | 3 | 109.5 s | 91.3 blocks/s | 3,524 | 0 | 363 |
| Poll + receipts | 1,000 | 1 | 200.5 s | 5.0 blocks/s | 3,122 | 0 | 560 |

The 10,000-block receipts runs were consistent: 85.4 s / 110.9 s / 109.5 s, 363 pools and zero failed windows each time.

### Dwellir (configured archive endpoint)

| Path | Settings | Range | Time | Throughput | POSTs | Failed windows | Pools | Result |
|---|---|---:|---:|---:|---:|---:|---:|---|
| Filtered `eth_getLogs` | 20 RPS | 1,000 | ~0.8 s | n/a | 2 | 2 | 0 | Plan rejects `eth_getLogs` with HTTP 403 |
| Batched receipts | 20 RPS, concurrency 8 | 1,000 | 40.4 s | 24.8 blocks/s | 346 | 104 | 72 | Incomplete |
| Batched receipts | 20 RPS, concurrency 4 | 1,000 | 23.6 s | 42.4 blocks/s | 128 | 172 | 44 | Incomplete |
| Batched receipts | 15 RPS, concurrency 4 | 1,000 | 8.0 s | 125.2 blocks/s | 44 | 219 | 29 | Incomplete |
| Batched receipts | 10 RPS, concurrency 2 | 1,000 | 64.8 s | 15.4 blocks/s | 284 | 38 | 83 | Incomplete on repeat |

The first 10 RPS / concurrency-2 run completed with zero failed windows in 105.8 s, but the repeat produced 38 failed windows. This variability makes the current Dwellir plan unsuitable as the primary backfill path.

### Missed-event / completeness view

There is no independent ground-truth event set in these artifacts, so failed windows are used as the observable gap signal. Zero matching creation logs in a recent 10,000-block window is a real observation, not proof that all pools were found.

| Path | Frozen range | Failed windows | Decoded pools / creation logs | Gap-free? |
|---|---:|---:|---:|---|
| PublicNode filtered logs | 1,000 | 0 | 0 creation logs | Yes |
| PublicNode filtered logs | 10,000 | 0 | 0 creation logs | Yes |
| PublicNode receipts | 1,000 | 0 | 92 pools | Yes |
| PublicNode receipts | 10,000 | 0 | 363 pools | Yes |
| PublicNode poll + receipts | 1,000 | 0 | 560 pools | Yes |
| Dwellir filtered logs | 1,000 | 2 | 0 creation logs | No (`eth_getLogs` 403) |
| Dwellir receipts, 10 RPS / 2-way repeat | 1,000 | 38 | 83 pools | No |

This is why the decision rests on the gap-free PublicNode receipts runs, not on the faster but incomplete Dwellir runs.

## Provider/quota findings

- PublicNode rejects `eth_getLogs` filters with more than 10 addresses (`-32602: A maximum of 10 addresses is allowed`). The harness now chunks configured factory addresses into groups of 10; the successful runs used two chunks.
- PublicNode accepts address/topic-filtered `eth_getLogs` for at least 10,000 blocks and returned no failed windows.
- Dwellir's current plan does not support `eth_getLogs` at all, so the reported 20 RPS cannot help that path.
- Batched receipts create two JSON-RPC calls per block (`eth_getBlockByNumber` + `eth_getBlockReceipts`) in one HTTP POST. In the repeated 10,000-block runs the client observed 3,524 attempt-POSTs, 7,048 logical calls inside batches, and ~133.7 MB of response bytes, above the theoretical 2,500 POSTs because of batch splitting/retries.
- No explicit HTTP 429 was classified in the artifacts (`rate_limit = 0` everywhere). Dwellir's 71 retryable errors in the final 1,000-block run were counted but not classified as 429 by the client parser, so quota accounting still needs dashboard verification.
- Dwellir produced retryable errors and failed windows at 20/15/10 RPS under 8-, 4-, and 2-way concurrency. The 20 RPS allowance must be checked against whether the provider counts individual JSON-RPC calls inside batches before it can be trusted.
- Filtered-log runs returned no client-visible response-byte count because the manual `getLogs` accounting does not populate the RPC client byte counter.
- No provider credit/unit counter was exposed by the RPC client; values must come from the provider dashboard.

## Decision

**Choose PublicNode batched `eth_getBlockReceipts` for the 30-day Avalanche backfill.**

Evidence:

- 10,000 blocks completed gap-free in a median 109.5 s (91.3 blocks/s).
- Extrapolation to ~1,296,000 blocks (30 days at Avalanche's ~2 s block time) is **~3.9 hours**, inside the 4–6 hour target.
- No external indexer, bloom filter, generic factory framework, or official API/subgraph adapter is needed to meet the throughput target.
- Dwellir remains a fallback only: its `eth_getLogs` plan support is absent, and its receipts path is either incomplete or too slow/variable under the tested settings.
- Filtered `eth_getLogs` is useful for targeted creation-event scans where the provider supports it, but it found no creation events in the recent 10,000-block windows and is not the inventory backbone.
- Poll + receipts is suitable for continuous discovery of new heads, not for the 30-day backfill.

## Scope note

A 30-day factory-creation scan only finds pools created in that window. The initial inventory still needs the optional bootstrap mix from the plan: existing GeckoTerminal/DexScreener candidates, simple protocol-native enumeration where already available, and longer targeted scans only where the measured 30-day result leaves a material gap. Every candidate remains unverified until Stage 4 validates it on-chain.

## Gate

Stage 1 is complete for decision purposes. The 30-day backfill itself has not been executed; execute it as the first approved action before Stage 2, using:

```bash
cargo run -p mev-scout-core --example stage1_avalanche_bench -- \
  receipts_publicnode 30d --out bench/stage1-30d \
  --config mev-scout.local.toml --repeat 1 --credits-used <dashboard-value>
```

If the full run exceeds the 4–6 hour target, record the measured bottleneck and use the least complex fallback from the plan rather than adding deferred infrastructure.
