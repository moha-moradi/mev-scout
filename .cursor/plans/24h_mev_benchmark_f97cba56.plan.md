---
name: MEV benchmark protocol
overview: A manual, on-demand benchmark protocol. When told "run the benchmark", re-index the frozen oracle window into a fresh DB, score local mev_ops against the BigLabs feed behind explorer.mev.zone, cross-check the vendor and the chain over plain HTTP, and emit a PASS/FAIL verdict that ratchets against the recorded baseline. Nothing is scheduled, nothing runs in CI, nothing runs by itself.
todos:
  - id: interpreter
    content: "Resolve the Python prerequisite: install 3.11+ or port scripts/compare_biglabs.py to Node; on this box `python` is the Windows Store stub and no checker script can run"
    status: pending
  - id: benchmark-runner
    content: "Add scripts/benchmark.py: the one manual entry point — preflight, fresh DB, index, coverage gate, compare, cross-checks, verdict, ledger row"
    status: pending
  - id: crosschecks
    content: "Add the HTTP cross-checks to the runner: vendor drift re-fetch of the same block range, on-chain tx truth for N matched hashes, join integrity, USD totals"
    status: pending
  - id: baseline-ledger
    content: "Write docs/oracle/benchmark-baseline.json from the first guard run on HEAD and append every later run to cache/benchmark-runs.jsonl"
    status: pending
  - id: runbook-doc
    content: "Write docs/oracle/BENCHMARK.md: the protocol, exact commands, the verdict table, and how to read each FAIL line"
    status: pending
  - id: verify-ratchet
    content: "Run the protocol on current HEAD, then against a deliberately weakened classifier, and confirm the verdict flips to FAIL"
    status: pending
  - id: audit-window
    content: "Only when explicitly asked: the wide-window audit (1h then 4h then 24h) with fetch_biglabs_day.py and benchmark_mevzone.py, speed gate as a human decision"
    status: pending
isProject: false
---

# MEV benchmark protocol (manual, run after a change)

This document is an instruction list, not a migration. It is the procedure a
human starts by saying *run the benchmark* and an agent then executes
end-to-end, finishing with a verdict table. Vendor = the BigLabs API behind
[explorer.mev.zone/mevlive](https://explorer.mev.zone/mevlive).

## 0. Ground rules

- **Manual only.** No cron, no `qoder_cron`, no CI job, no `--loop`, no
  background run. A protocol execution starts when a person asks for it and
  ends when the verdict table is printed. The repo has no workflow files
  (commit `6eaa33b ci: remove GitHub Actions workflow`) and this plan does not
  add one.
- **One run = one fresh sqlite DB.** `blocks_classified` is the resume cursor
  ([core/src/explorer/store/sync.rs:65](core/src/explorer/store/sync.rs) picks
  `b NOT IN (SELECT block FROM blocks_classified ...)`), so re-indexing an
  existing range over an existing DB silently re-uses the *old* classifier's
  answer and reports it as the new one. Any run that reuses a DB file is
  invalid, not merely stale.
- **The vendor side is a constant.** The guard snapshot was fetched once and
  committed; a checkpoint never re-fetches or re-ranges it
  ([scripts/compare_biglabs.py:19-23](scripts/compare_biglabs.py),
  [docs/explorer_strategy_checklist.md:48](docs/explorer_strategy_checklist.md)).
  With `V` fixed, every metric movement is attributable to the code under
  test — that is the whole point of the protocol.
- **Secrets stay in `cache/`.** The archive URL is read from a gitignored
  provider file and never echoed to stdout, the report, the ledger, or a
  commit. `/cache` and `*.sqlite` are already gitignored.
- **The verdict is advice to a human.** A FAIL stops the protocol and prints
  the evidence; it never auto-reverts, auto-fixes, or re-baselines anything.

## 1. The fixed guard window

Everything the per-change run needs is already committed.

| Item | Value |
| --- | --- |
| Vendor snapshot | `docs/oracle/biglabs-avalanche-98572463-98575973.json` |
| Snapshot SHA256 | `e9b06cfc518e4accb43e8098d20ff73e80b5aa40f8a3707040b232a4d19896cb` |
| Chain / range | avalanche, blocks 98,572,463 → 98,575,973 (3,511 blocks) |
| Vendor rows | 46 arbs, 46 unique hashes |
| Frozen L\V labels | `docs/oracle/adjudications-baseline.json` (validated against the snapshot hash and range; stale labels fail loudly) |
| Recorded baseline | `recall_vs_vendor` 1.0000 (46/46), 117 local rows, 5 dust `tp_vendor_miss` ([docs/explorer_strategy_checklist.md:81](docs/explorer_strategy_checklist.md)) |

Derive window lengths from `time`, never from a block count: the committed
hour spans 3,511 blocks while [core/src/chain/timing.rs](core/src/chain/timing.rs)
declares 43,200 blocks/day for Avalanche, and the payload has no `created_at`
(`sortBy=-created_at` only orders). The feed is newest-first.

## 2. The protocol

Run the steps in order. A failing step stops the run; do not continue past a
coverage or cross-check failure and print a table that looks like a checkpoint.

**Step 1 — preflight.** `python --version` is 3.11+ (see the interpreter
todo); `git rev-parse --short HEAD` and `git status --porcelain` are captured
into the run record, so every verdict states which commit produced it and
whether the tree was dirty; the snapshot SHA256 above still matches; the
provider file exists. `cargo build --release -p mev-scout-cli` so the binary
under test is the tree under test.

**Step 2 — index the guard window into a new DB.** Write
`cache/bench-run.toml` from the gitignored provider file, overriding only
`[explorer] db_path`:

```toml
chain = "avalanche"
rps_limit = 18
block_concurrency = 8

[chains.avalanche]
chain_id = 43114

[chains.avalanche.rpc]
rpc_urls = ["<from the provider file, never printed>"]
rpc_rps = [18.0]

[explorer]
db_path = "cache/bench-<sha>-<utc>.sqlite"
confirmations = 4
```

Provider cap is about 20 rps; the client bucket is `RateLimiter::new(rps, rps)`
([core/src/rpc/middleware.rs](core/src/rpc/middleware.rs)), so burst equals
rate — set **18**, not 20, never 0 (`rps_limit = 0` means unlimited,
`with_provider_rps` in [core/src/rpc/client.rs](core/src/rpc/client.rs)).
`rpc_batch_blocks` stays at 1 so one token is one POST. `block_concurrency = 8`
overrides `(min_rps * 2).clamp(5, 15)`
([core/src/config/settings.rs](core/src/config/settings.rs)
`effective_block_concurrency`). One URL only, so `effective_provider_configs`
appends no public endpoints.

```powershell
cargo run -p mev-scout-cli --release -- -f cache/bench-run.toml explorer index --from-block 98572463 --to-block 98575973
```

**Step 3 — coverage gate.** `blocks_classified` over the range must equal
3,511 and the log must show no skipped blocks; if it does, re-run the same
range into the same DB (that fill-in is the only legitimate DB reuse) and
re-check. A short count is `INCOMPLETE`, never a checkpoint — this is exactly
the exit-3 path already implemented in
[scripts/compare_biglabs.py:896](scripts/compare_biglabs.py).

**Step 4 — score.**

```powershell
python scripts/compare_biglabs.py --db cache/bench-<sha>-<utc>.sqlite --snapshot docs/oracle/biglabs-avalanche-98572463-98575973.json --chain avalanche --lo 98572463 --hi 98575973 --label <sha> --adjudications docs/oracle/adjudications-baseline.json --local-commit <sha>
```

That writes `cache/oracle-report-<sha>.json`. Keep `compare_biglabs.py`
behavior unchanged; the runner calls it, it does not fork it.

**Step 5 — cross-checks** (section 3). **Step 6 — verdict + ledger**
(section 4). Finish by pasting the printed markdown table plus the verdict
line into the reply; do not append it to
[docs/explorer_strategy_checklist.md](docs/explorer_strategy_checklist.md).

## 3. Cross-checks — all over HTTP, no browser, no hand-editing

`V ∩ L` is only a hash join, so the same hashes must also be true in two other
places before a PASS means anything.

- **C1 — vendor drift.** Re-fetch the *same block range* from
  `https://gateway.biglabs.eu/api/avalanche/arbitrages?per_page=50&sortBy=-created_at`
  by paginating until `time` falls below the window, dedupe, and diff the hash
  set against the frozen snapshot. Expected: identical. A difference is not a
  regression and not a pass — it means the yardstick moved, and section 7 is
  the only way forward. Record `vendor_drift: none | +N -M`.
- **C2 — on-chain truth.** For up to 8 hashes sampled from `V ∩ L` (deterministic
  sample: lowest block first) and 4 hashes from `L \ V`, call
  `eth_getTransactionByHash` on the archive RPC and assert the tx exists, its
  block number equals the vendor `blockNumber`, and `from` equals the vendor
  `sender`. For the `L \ V` sample also confirm the tx is not reverted. This
  catches both a hallucinated vendor row and a mis-parsed local hash, and it is
  the one check that tests the local rows nobody else lists.
- **C3 — join integrity.** For every matched hash, the local `mev_ops`
  `block_number` must equal the vendor `blockNumber`. A hash that matches while
  sitting in a different block is a data bug masquerading as a detection hit,
  and it currently passes unnoticed because the join ignores block.
- **C4 — totals.** Vendor `sum(usdProfit)` over `V` against
  `sum(profit_usd)` over `V ∩ L` and over all local rows, plus the counts
  (`vendor_rows`, `local_rows`, `intersection`, `vendor_only`, `local_only`)
  recomputed from sqlite independently of the report file. Agreement here proves
  the numbers in the table came from this DB, not from a cached report.

Gross `profit_usd` is the comparable money column. `net_profit_usd` is priced at
run-time native gas, so it is not comparable across runs
([docs/explorer_strategy_checklist.md:212](docs/explorer_strategy_checklist.md));
report it as a second column only. Liquidation gross is `collateral_usd -
debt_usd` ([core/src/explorer/store/pnl.rs](core/src/explorer/store/pnl.rs)).

## 4. Verdict — ratchet against the recorded baseline

`docs/oracle/benchmark-baseline.json` holds the reference run. A run PASSes when
every gate holds; the first failing gate names itself and prints its evidence.

| Gate | Rule | Source field |
| --- | --- | --- |
| Coverage | `blocks_classified == 3511`, no skipped blocks | step 3 |
| Recall | `recall_vs_vendor >= baseline` (baseline 1.0000; any non-empty `vendor_only` is a FAIL, with the hashes listed) | `metrics.recall_vs_vendor`, `metrics.vendor_only_hashes` |
| False positives | `fp_suspect` count `<= baseline`, and no `fp_suspect` row whose gross profit is ~0/NULL | `adjudication_counts` |
| Kind stability | the `matched_kind_labels` multiset is unchanged unless the change under test is about precedence | `metrics.matched_kind_labels` |
| PnL | median `abs(profit_delta_pct)` within `+2pp` of baseline, worst within `+10pp` | `profit_delta_pct` |
| Cross-checks | C1 `none`, C2 12/12 pass, C3 0 mismatches, C4 recomputed counts equal the report | section 3 |

Exit codes: `0` PASS, `1` FAIL (regression), `2` environment/usage, `3`
INCOMPLETE. Each run appends one JSON line to `cache/benchmark-runs.jsonl` with
`{ts, head, dirty, label, mode, metrics, gates, verdict, reasons}` — that ledger
is what the next run ratchets against, so the history of "what did this change
do" is kept without any scheduler.

Artifacts, all gitignored: `cache/bench-run.toml`,
`cache/bench-<sha>-<utc>.sqlite`, `cache/oracle-report-<sha>.json`,
`cache/benchmark-runs.jsonl`. Committed only when asked: the baseline file and
the runbook doc.

## 5. Reading a FAIL

- `vendor_only` non-empty → detection regression. Take those hashes to C2; a
  reverted or dust tx is an argument the gate is wrong, a real arb is a bug.
- `local_only` grew → new detection or an FP storm. `local_only_kind_composition`
  says which kind, `adjudication_counts` says whether it looks fake.
- `matched_kind_labels` shifted → precedence/classification change, coverage is
  intact; that is a different conversation than recall.
- PnL moved → pricing, not detection. Check gross deltas per hash before
  believing a median.
- C1 drift → the vendor changed. Do not quietly re-baseline: section 7.
- C2/C3 failure → data-integrity bug. Fix it before re-running; a PASS after a
  broken cross-check is worthless.

## 6. Fast path

`python scripts/benchmark.py` runs steps 1-6 with no arguments; defaults are the
guard window, a fresh DB name from HEAD, and the baseline ratchet. Flags exist
to narrow a run, not to skip gates: `--db <path>` (resume fill-in),
`--label <token>`, `--sample <n>`, `--skip-drift` (offline C1, marked in the
ledger), `--report-only`. Never add a `--force-pass`.

## 7. Re-baselining

Only a human decides this, and only in one of two cases: the vendor feed moved
(C1) or an intentional detection change legitimately moves recall. Procedure:
freeze a new guard window (new committed snapshot + adjudication file + range),
run the protocol on it, write the new `benchmark-baseline.json`, and state in
the same commit why the old yardstick was retired. Old baselines stay readable;
never overwrite in place.

## 8. Wide-window audit — only when explicitly asked

Not part of the per-change protocol; it costs hours and answers "is this
representative", not "did this change break". Keep the staged shape: one fixed
`window_end` (newest `time` on the first arb page, never re-anchored, so each
slice stays a prefix of the next), `scripts/fetch_biglabs_day.py` paginating
both feeds (`arbitrages` and `liquidations`, `per_page=50`, stop when a page is
empty, repeats its newest hash, or crosses the cutoff), slices at 1h → 4h → 24h
written to `docs/oracle/biglabs-avalanche-<hours>h-<lo>-<hi>.json` as
`{meta, records}` with vendor metrics in `meta` (counts, kinds, liquidations by
`protocol`, `usdProfit` sum/median/min/max, and arb notional
`(amountIn / 10^decimals) * usdPrice` — notional, not gas). One index DB per
audit, extended stage by stage (there, unlike the guard run, reuse is the point).
`scripts/benchmark_mevzone.py` scores each finished slice: recall, `V\L` and
`L\V`, kind accuracy on the intersection only (vendor `arbitrage` ↔ local
`arb_atomic`, `jit_arb` reported as compatible-but-not-strict, vendor
`liquidation` ↔ local `liquidation`; a hash hit with the wrong kind is a
detection hit and a kind miss), PnL median/p90/worst/best, and per-side money
totals.

The 24h stage runs only if a human agrees after seeing the 4h numbers, and the
speed test is what informs that choice: full `blocks_classified` coverage, no
repeated 429 skip loop, and `eta_24h = 43200 / blocks_per_sec <= 6h`. If any
fails, stop after the 4h report and say so. Do not start an index that would run
overnight.

## 9. Do not touch

[docs/oracle/biglabs-avalanche-98572463-98575973.json](docs/oracle/biglabs-avalanche-98572463-98575973.json),
[docs/oracle/adjudications-baseline.json](docs/oracle/adjudications-baseline.json),
and the baseline behavior of [scripts/compare_biglabs.py](scripts/compare_biglabs.py).
The live explorer DB is not a benchmark DB. Do not add a scheduler, a CI
workflow, or a second RPC endpoint. Do not commit artifacts unless asked.
