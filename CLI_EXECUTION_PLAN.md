# CLI Execution Plan — Polygon Verification (run + live)

> Goal: execute and verify every `mev-scout` CLI command on Polygon, with the
> heart of the project — `run` (backtest) and `live` (real-time streaming) —
> fully exercised and verified.
>
> Status: **planned, not yet executed** (work through phases in order).

---

## 1. Block-range policy (user constraint)

| Command | Range rule | Why |
|---|---|---|
| `fetch` | `--blocks 100` max | keep cache warm-up bounded |
| `scan` | `--blocks ≤100` | event-path sanity only |
| `run` | `--blocks ≤10` | heart of project; small controlled verification |
| `replay` | single `--block` | debug tool |
| `discover` | **unbounded** | larger pool universe = more opportunities (explicit exception) |

## 2. Confirmed findings from code exploration

- Commands (10): `config`, `tokens`, `discover`, `validate-pools`, `fetch`,
  `scan`, `replay`, `run`, `live`, `report`.
- `mev-scout.toml` at repo root is auto-loaded by `main.rs`; contains 9 RPC
  providers (3× Alchemy, 2× drpc, 4× GetBlock), per-provider RPS 10–15
  (~100 RPS aggregate). All commands run without `--rpc` from repo root.
- `live` currently supports only one-shot or infinite `--loop` (Ctrl+C).
  **There is NO duration limit → must be added (Phase 4).**
- **Opportunity-loss bug** in `run_loop`
  (`cli/src/commands/live.rs:233–251`): on fetch or backtest failure the code
  does `last_block = current_tip; continue;` → failed blocks are **silently
  skipped**, losing any opportunities inside them. Must be fixed (Phase 4).
- Profit-overstatement risks: CLI default priority fee is 1 gwei (Polygon
  reality ~25–50 gwei); winning-bid premium hardcoded 0.0 → treat reported
  profits as upper bounds; mitigate via realistic fee + fact-checks.

## 3. Decisions (confirmed by user)

- **RPC:** use endpoints already in `mev-scout.toml` (no flag needed).
- **Duration format:** humantime suffixes (`90s`, `15m`, `1h`).
- **Discovery scope:** remote+enrich first (GeckoTerminal/DexScreener union,
  top ~1000 pools), then incremental on-chain pass since last cached block.
- **Priority fee:** verify with a realistic `--priority-fee 30` (not the 1
  gwei default that overstates profit).
- **Security note (out of scope but flagged):** API keys are committed in
  `mev-scout.toml`; consider env vars / gitignored local file later.

## 4. Anti-missed-opportunity & anti-overstated-profit safeguards

Not missing opportunities:
1. Broad hybrid discovery + `validate-pools` recall check before trusting
   downstream results (bigger pool universe = log-first fetch covers more).
2. Fix live-loop skip-on-failure bug so no block is silently dropped.
3. Keep `--min-profit-wei 0` during verification so nothing is filtered;
   calibrate threshold afterwards from observed dust distribution.

Profit not overstated:
1. `historical_exact` gas model for backtests (default), `live` gas model for
   live mode (default).
2. Realistic `--priority-fee 30` for all Polygon verification runs.
3. Always attach `--fact-check --evm-fact-check` on `run`.
4. Treat every reported profit as an upper bound (winning-bid premium = 0);
   optionally use core's gas-calibration system later.

---

## Phase 0 — Build & offline sanity

```powershell
cargo build -p mev-scout-cli --release
cargo test
target\release\mev-scout.exe config        # verify resolved RPCs, chain=polygon
```

## Phase 1 — Data foundation (discovery unbounded)

```powershell
mev-scout tokens --filter tvl --top 50 --cache-only
mev-scout discover --source remote --enrich --max-pools 1000
mev-scout discover --source hybrid --enrich --incremental
mev-scout validate-pools --json          # record recall before proceeding
```

## Phase 2 — Warm cache & debug tools (≤100 blocks)

```powershell
mev-scout fetch --blocks 100
mev-scout scan --kind flashloans --blocks 50
mev-scout replay --block <active-recent-block> --analyze   # single block
```

## Phase 3 — RUN verification (heart #1, ≤10 blocks)

```powershell
mev-scout run --blocks 10 --strategies all `
  --priority-fee 30 --fact-check --evm-fact-check `
  --output json --export-path results\polygon
```

Verification steps:
- `run_*.json` produced with `chain=polygon` and valid numeric block range
  (same assertions as `cli/tests/cli_e2e.rs`).
- Cross-check: `replay --block N` for each block where opportunities were
  found → compare opportunity sets between pipeline paths.

## Phase 4 — Code change: live `--duration` + missed-block fix

1. `cli/src/cli.rs` — `LiveArgs`: add
   `#[arg(long = "duration", value_name = "DURATION")] pub duration: Option<String>`
   parsed as humantime (`90s`, `15m`, `1h`); error if passed without `--loop`.
2. `cli/src/commands/live.rs` — `run_loop`: deadline-based exit
   (`Instant::now() + duration`), graceful stop at boundary, end-of-session
   summary (blocks processed, txs scanned, opportunities found, export paths).
3. **Fix skip-on-failure**: do NOT advance `last_block` on fetch/backtest
   failure — retry next tick; give up only after N consecutive failures with
   a loud warning (prevents both opportunity loss and infinite wedge).
4. Unit tests for duration parsing + quick e2e: `live --loop --duration 30s`.

## Phase 5 — LIVE verification (heart #2, timed soak)

```powershell
mev-scout live --loop --duration 15m --min-profit-wei 0 --priority-fee 30
# optional extended soak:
mev-scout live --loop --duration 1h --min-profit-wei 0 --priority-fee 30
```

Pass criteria:
- Blocks processed contiguously (no "Fetch failed" skips in output).
- Clean timed exit at duration end with session summary printed.
- JSON results accumulate under export path (`live_*.json`).
- Any live-found opportunity is reproducible via
  `replay --block <same-block>` (live path ↔ replay path consistency).
- Afterwards: `mev-scout report --output json` to inspect saved runs.

---

## Suggested execution order checklist

- [ ] Phase 0: build + offline tests + `config` sanity
- [ ] Phase 1: tokens → discover (remote) → discover (hybrid incremental) → validate-pools
- [ ] Phase 2: fetch (≤100) → scan (≤100) → replay single block
- [ ] Phase 3: run (≤10 blocks, fact-checked) + replay cross-check
- [ ] Phase 4: implement `--duration` + fix skip-on-failure + tests
- [ ] Phase 5: live 15 min soak → optional 1 h soak → report inspection
