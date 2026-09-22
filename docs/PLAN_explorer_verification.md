# Verification hardening plan — explorer module

**Goal:** make explorer profit/classification claims *verifiable* — an independent
trace-derived check per tx (A), a re-exposed cross-validation report vs simulated
opportunities (C), and a repeatable real-block regression corpus (D).

---

## A — Trace verification gate (`explorer show --trace` fails loudly)

Today `job_trace_op` computes `profit_error_pct` and writes it to `details_json`, but:

- `TraceOutcome.verified` is hardcoded `true` (`core/src/jobs/trace.rs:204`)
- `cmd_show` discards the result: `let _ = job_trace_op(...)` (`cli/src/commands/explorer/show.rs:75`)
- no threshold exists anywhere.

### Changes (`core`)

1. `core/src/jobs/trace.rs`: add a pure verdict function
   `trace_verdict(exp_usd, trace_usd, err_pct, tolerance_pct, abs_usd_tol) -> TraceVerdict` where
   - `Pass` if `|err_pct| <= tol` (or, if `expected ~= 0`, `|exp - trace| <= abs_usd_tol`)
   - `Fail(reason)` if `|err_pct| > tol` (classifier over/under-estimate; positive = over-estimate)
   - `Unverifiable(reason)` when trace profit is absent (no native price, empty balance
     deltas, trace RPC failure, no USD for the op) — this is **not** a fail, it's degraded
     coverage.
   - Add `verdict` + `tolerance_pct` to `TraceOutcome`; persist
     `trace_check = "pass|fail|unverifiable"` and reason via `store.mark_trace_verified`
     (`store.rs:1885`).
2. `core/src/config/settings.rs`: add optional `ExplorerConfig.trace_tolerance_pct`
   (default **20.0**) and `trace_error_usd_tol` (default e.g. 0.50).
   Unit-test `trace_verdict` exhaustively (pass / over-est / under-est / zero-expected / no-price).

### Changes (`cli`)

3. `cli/src/commands/explorer/show.rs`: use the returned `TraceOutcome`; print the verdict.
   **Exit non-zero (via `anyhow::bail!`)** when verdict is `Fail`; `Unverifiable` prints a
   warning. Add `--tolerance-pct PCT` override flag on `ShowArgs` (`cli/src/cli.rs:109`).
4. Offline CLI test: `show --trace` needs a live RPC, so gate the CLI assertion under
   `MEV_SCOUT_E2E=1` in `cli/tests/cli_network_coverage.rs`; the core-level verdict function
   covers offline determinism.

---

## C — Re-expose `explorer validate` (read-only)

All machinery exists and is offline-safe (`validate_live` does no RPC —
`validation.rs:368`): `compute_validation` (`core/src/explorer/validate.rs:316`),
`render_validation_report` (`:750`), `job_explorer_validate`
(`core/src/jobs/explorer_validate.rs:55` — supports `--since`, `--match-window`,
`--run-id`, `--threshold-sweep`, `--emit-missing-pools`, `--review-csv`,
`--golden-causal`, `--json`). Only the CLI entry was removed.

### Changes

1. `cli/src/cli.rs`: add `Validate(ExplorerValidateArgs)` to `ExplorerCommand`; args mirror
   the job opts (`--since` / `--match-window` / `--run-id` repeatable / `--threshold-sweep`
   / `--emit-missing-pools` / `--review-csv` / `--golden-causal` / `--json`).
2. New `cli/src/commands/explorer/validate.rs` -> thin `cmd_validate` calling
   `job_explorer_validate`; wire into `cli/src/commands/explorer.rs` (mod + re-export) and
   `cli/src/commands/mod.rs` dispatch (`:77`).
3. Update `cli/tests/cli_args.rs`:
   - remove `"validate"` from the removed list (`:64`) and add it to the kept list (`:53`)
   - retarget `explorer_unknown_subcommand_fails` (`:88-92`) to a truly-unknown name
     (e.g. `explorer frobnicate`)
   - add an **offline** positive test: seed an explorer DB (modeled on
     `seed_report_fixture` at `cli_args.rs:469`, plus the `mev_ops` schema at
     `store.rs:283`) with a T1-matching op+opportunity, then
     `explorer validate --since all --json` -> expect ok + JSON report containing
     `arb_atomic`.
4. Docs: restore `explorer validate` in `docs/ARCHITECTURE.md` §4.7 command examples.

---

## D — Real-block golden corpus (WS-H2 first slice)

**Risk note:** blocks are immutable but *expected-profit numbers must be collected from a
live run first*; the harness will assert derived facts (kind present, searcher, USD band),
not exact amounts anywhere volatiles change.

1. New `core/tests/explorer_corpus.rs`:

   ```
   CorpusCase { id, chain, from_block, to_block, kind, min_ops,
                searcher: Option<Address>, profit_usd_min: Option<f64> }
   ```

   Harness gated on `MEV_SCOUT_E2E=1` + `RPC_URL` (reuse `core/tests/common/setup.rs:28`),
   ingest the window via `run_range` into an in-memory `ExplorerStore`, then assert per case.
2. **Seed-data collection step (executed during implementation):** run
   `explorer backfill --from-block ... --to-block ...` + `stats`/`show` on Polygon/Avalanche,
   hand-pick 3–5 realized ops per kind (sandwich, arb_atomic, liquidation, jit), and
   hard-code their `(chain, block_range, searcher, kind, profit_usd band)`.
3. No committed fixtures yet (per current repo convention live data is fetched at test
   time); the corpus makes that *repeatable* rather than one-off. This is the first slice
   of roadmap WS-H2 (`roadmap_to_100pct.md:261`); the synthetic per-fingerprint fixtures
   already covered by golden.rs / explorer_golden.rs stay as the offline tier.

---

## Open decisions (to confirm before implementation)

1. **A strictness:** default `show --trace` exit code non-zero on `Fail`? Recommended yes
   (assert-style), with tolerance configurable.
2. **A scope:** also add a batch `explorer verify --since 7d [--kind X] [--limit N]` that
   runs the trace check over many ops and aggregates pass/fail/unverifiable per kind?
   (RPC-expensive; ~1 debug-call per tx.) Or keep A to per-tx `show --trace` only?
3. **C default window:** `--since` default `all`, per validate's current behavior — confirm.

Suggested implementation order: **C (smallest, unblocks verification today) -> A -> D**.