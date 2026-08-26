# CLI E2E Test Suite — All 10 Commands, Live-Polygon (plus Phase 4 `--duration`)

> Black-box end-user tests for the real `mev-scout` binary. Networked tests run
> against live Polygon RPC behind env-var gates (extending the existing
> `cli/tests/cli_e2e.rs` pattern); pure argument-validation tests run ungated
> on every `cargo test`.
>
> Includes the Phase 4 prerequisite (`live --duration` + missed-block fix) as
> task P0 — confirmed by user ("پیاده‌سازی‌ش کن").

## Decisions (confirmed)

| Decision | Choice |
|---|---|
| Test tier | **Live-RPC only** (env-gated); no wiremock/mocked tier |
| Arg-validation tests | **Ungated**, zero-network, run on every `cargo test` |
| Layout | **Split by domain**: shared harness + 4 test binaries |
| `live --loop` bounding | Implement Phase 4 `--duration`; test uses timed graceful exit |
| Remote-API commands | Strict assertions on-chain; **tolerant** (skip w/ warning) for GeckoTerminal/DexScreener paths |
| Block ranges | Per `CLI_EXECUTION_PLAN.md`: fetch ≤100 (tests use 5–10), scan ≤100, run ≤10, replay single, discover onchain bounded in tests |
| Profit safeguards | `--priority-fee 30` on run/live invocations; `--min-profit-wei 0` on live |
| **RPC source** | **The 9 providers in `mev-scout.toml`** (user-confirmed). No `RPC_URL` env var. Networked tests run with cwd = temp workspace and `-f <repo>/mev-scout.toml`; artifacts isolated via `--db-path`/`--export-path` into the temp ws. |

## Conventions

- Gate (all networked tests): `MEV_SCOUT_E2E=1`, else print `SKIP:` and return. Tests additionally self-skip if RPCs from `mev-scout.toml` are unreachable at start (cheap probe via a 1-block scan).
- Every spawned command gets a deadline; on timeout kill child + fail. Defaults: 180 s general, 600 s for `run`/`live`, 300 s for `discover`.
- Each test works in a unique temp workspace (`%TEMP%/mev_scout_e2e_<test>_<pid>`): own `db-path`, own `export-path`; commands run with `current_dir()` = temp ws and explicit `-f <repo-root>/mev-scout.toml` so the committed provider list is used while cache/exports stay out of the repo.
- Heavy networked tests inside a binary serialize on a shared `std::sync::Mutex` to bound concurrent RPC load (up to 3 networked binaries may overlap — fine vs ~100 RPS aggregate providers).
- Assertions are structural (exit codes, stdout lines, exported JSON schemas) — no golden-path opportunity detection against live chain data (non-deterministic).
- Flaky-tip handling mirrors `core/tests/e2e.rs`: freshly-mined tip blocks may be unserved even after auto-refetch → treat as environment limitation, skip with message, not failure.

## Task P0 — Prerequisite: Phase 4 implementation

Files: `cli/src/cli.rs`, `cli/src/commands/live.rs`

1. `LiveArgs`: add
   `#[arg(long = "duration", value_name = "DURATION")] pub duration: Option<String>`
   (help heading "Live"). Parse with `humantime` (`90s`, `15m`, `1h`) — add
   `humantime = "2"` to `cli/Cargo.toml` `[dependencies]`. Error if `--duration`
   passed without `--loop` (clap `requires` won't express negation → validate
   in `cmd_live` with `anyhow::bail!`).
2. `run_loop`: compute `deadline = Instant::now() + duration` when set; check
   at top of each loop tick → break gracefully. After loop, print session
   summary: blocks processed, total txs scanned, total opportunities found,
   export path.
3. Fix skip-on-failure (`cli/src/commands/live.rs:233–251`): on fetch or
   backtest failure do **not** advance `last_block`; retry same range next
   tick. Track consecutive failures; after 5 in a row, log loud error and
   return Err (prevents both silent opportunity loss and infinite wedge).
4. Unit tests (in-file `#[cfg(test)]`): duration string parsing (`90s`,
   `15m`, `1h`, invalid input errors), duration-requires-loop validation.
   Verify with `cargo test -p mev-scout-cli`.

## Files to create

```
cli/tests/common/mod.rs            # harness
cli/tests/cli_args.rs              # UNGATED argument/error-path tests
cli/tests/cli_data_foundation.rs   # tokens, discover(onchain strict / remote tolerant), fetch, scan
cli/tests/cli_run_replay.rs        # chained: fetch → run → replay → report
cli/tests/cli_live_mode.rs         # live one-shot + live --loop --duration 30s (+ P4 pass criteria)
```

Keep existing `cli/tests/cli_e2e.rs` unchanged (still passes).

### `common/mod.rs` harness

```rust
// pub fn gate() -> Option<String>      // Some(rpc_url) if MEV_SCOUT_E2E=1 && RPC_URL set
// pub fn temp_ws(tag: &str) -> PathBuf // unique temp workspace, created
// pub fn output(base,args,&[&str], cwd) -> Command // preconfigured spawn
// pub fn run_timed(cmd, timeout) -> Result<Output, TimedOut> // kill on deadline
// pub fn wait_stdout_line(child, pred, deadline) -> String // for live loop test
// pub static RPC_MUTEX: Mutex<()>      // serializes heavy tests per binary
```

Spawn with piped stdout/stderr; always log captured streams into test failure messages.

### Test inventory

**`cli_args.rs` — ungated (no env needed, no network; run in temp cwd)**

| Test | Invocation | Assert |
|---|---|---|
| run_requires_block_range | `mev-scout run` | non-zero exit, stderr mentions block-range heading/usage |
| fetch_requires_block_range | `mev-scout fetch` | non-zero exit |
| scan_requires_block_range | `mev-scout scan` | non-zero exit |
| days_out_of_range_rejected | `run --days 400` | non-zero exit (value_parser 1..=365) |
| block_zero_rejected | `run --block 0` | non-zero exit |
| replay_requires_block | `replay` (no --block) | non-zero exit |
| unknown_subcommand | `mev-scout frobnicate` | non-zero exit |
| help_lists_all_10_commands | `mev-scout --help` | exit 0; stdout lists run, fetch, report, config, replay, discover, validate-pools, tokens, scan, live |

**`cli_data_foundation.rs` — gated**

| Test | Chain of invocations (one serialized session, shared temp ws) | Key asserts |
|---|---|---|
| discover_onchain_strict | `discover -r $RPC --source onchain --blocks 5 --json --db-path …` | exit 0; stdout parses as JSON array; each entry has address/token0/token1/dex_type |
| tokens_cache_roundtrip | `tokens -r $RPC --cache-only` then `tokens -r $RPC --output json` then `--output csv` | cache-only prints token count; JSON array items have address/symbol/decimals; CSV header exactly `address,symbol,decimals` |
| fetch_then_verify_summary | `fetch -r $RPC --blocks 5 --no-sig-resolve --db-path …` | exit 0; stdout has `Fetch complete:` + `Total blocks: 5`; db file exists; tolerate still-missing tip blocks → SKIP |
| scan_trades_json | `scan -r $RPC --kind trades --blocks 5 --output json --limit 20` | exit 0; stdout parses as JSON |
| discover_remote_tolerant | `discover -r $RPC --source remote --enrich --max-pools 50 --json` | exit-handling asserted; if 0 pools / aggregator failure → warn + PASS (service-side), else JSON array non-empty |
| validate_pools_tolerant | `validate-pools -r $RPC --json` | exit-handling asserted; tolerant to reference-source failure; if it completes, stdout parses as JSON |

**`cli_run_replay.rs` — gated, one serialized session (mirrors Phases 2–3)**

Session flow in a single `#[test]` (deterministic ordering):
1. `fetch -r $RPC --blocks 5 --no-sig-resolve --db-path ws\cache.db`
2. `run -r $RPC --blocks 5 --strategies all --priority-fee 30 --fact-check --output json --export-path ws\results --db-path ws\cache.db`
3. `replay -r $RPC --block <start_block> --analyze --db-path ws\cache.db`
4. `report --export-path ws\results --output table`, then `--output json`, then `--output csv`, and `report --output json` (latest-run default)

Assertions:
- All exit 0 (with flaky-tip SKIP escape hatch after step 1, mirroring core e2e).
- Step 2: `ws\results\run_*.json` exists; parses; `chain == "polygon"`, `start_block`/`end_block` numeric with `end >= start`, `opportunities` array present; strategies list echoes `all` expansion.
- Step 3: stdout contains `Receipt verification:` line; parse match pct; require ≥99 % else fail (replay warns below threshold — E2E enforces).
- Step 4: table shows `Run ID:`/`Chain:`; JSON re-parse equals step-2 file content (roundtrip); CSV first line is the exact header `block_number,tx_index,strategy,input_amount,expected_profit,gas_cost_wei,confidence`.
- Report error path (separate small gated test): `report --export-path <empty tmp>` → non-zero exit mentioning missing directory/files.

**`cli_live_mode.rs` — gated (heaviest; 600 s timeouts)**

| Test | Invocation | Asserts |
|---|---|---|
| live_one_shot_smoke | `live -r $RPC --priority-fee 30 --min-profit-wei 0 --output json --export-path ws\results --db-path ws\cache.db` | exit 0; stdout contains `Latest block:` and `opportunity(ies) detected`; a `live_*.json` written under export path, parses with `range_mode == "live"` |
| live_loop_duration_graceful_exit | `live -r $RPC --loop --duration 30s --poll-interval-ms 1000 --priority-fee 30 --min-profit-wei 0 --export-path ws\results --db-path ws\cache.db` | process exits on its own well before 180 s timeout; stdout contains session summary (Phase 4); **zero** `Fetch failed` / `Backtest failed` warnings (contiguity pass criterion from execution plan); ≥1 `Block …` progress line |
| live_duration_requires_loop | `live -r $RPC --duration 30s` (no --loop) | non-zero exit with clear error (P4 validation) |

## Dependencies to add

- `cli/Cargo.toml` `[dependencies]`: `humantime = "2"` (P0 duration parsing).
- No new dev-dependencies (serde_json already a normal dep; stay black-box — no rusqlite in tests).

## Risks / mitigations

- **Public-RPC flakiness (unserved fresh tip blocks)** → auto-refetch already in CLI; residual case = SKIP like core e2e, never false-red.
- **Third-party API downtime** → tolerant remote tests warn-and-pass; strict tests never touch those services.
- **Parallel binaries multiplying RPC load** → per-binary mutex; only 3 networked binaries; ranges ≤10 blocks.
- **Hung child wedges CI** → every spawn wrapped in `run_timed` deadline + kill.
- **Windows specifics** → use `Command::kill()` (no signal semantics needed); temp paths via `std::env::temp_dir()`; no shell involved.
- **P4 regression risk** → covered by new unit tests + `live_loop_duration_graceful_exit` + `live_duration_requires_loop`.

## Execution order

1. P0: implement `--duration` + retry-on-failure + unit tests → `cargo test -p mev-scout-cli` (ungated units green).
2. Add `common/mod.rs` + `cli_args.rs` → plain `cargo test` shows ungated suite passing.
3. Add `cli_data_foundation.rs` → verify once with gates set.
4. Add `cli_run_replay.rs` → verify once with gates set.
5. Add `cli_live_mode.rs` → verify once with gates set (includes P4 pass criteria).

## Validation

```powershell
cargo build -p mev-scout-cli --release
cargo test                                    # ungated cli_args + P0 unit tests green, networked SKIP
cargo test -p mev-scout-cli --test cli_args
$env:MEV_SCOUT_E2E="1"                        # RPCs come from mev-scout.toml — no RPC_URL needed
cargo test -p mev-scout-cli                   # full gated suite green against the 9 committed providers
cargo test -p mev-scout-cli --test cli_live_mode -- --nocapture
```

Out of scope (explicitly): wiremock/mocked tier, CI pipeline wiring, deeper
run↔replay opportunity-set comparison (requires core-level hooks, not
observable via CLI today), API-key secret hygiene flagged in the execution plan.
