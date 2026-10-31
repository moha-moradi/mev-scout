# Explorer Module Improvements — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fix all correctness bugs, refactor the explorer module's hot spots, remove performance bottlenecks, and close CLI/test/doc gaps identified in the 2026-10-25 three-way code review (classify/decode, ingest/store, CLI/tests/docs) — plus the fifteen additional verified findings and eight in-place corrections from the 2026-10-25 **second-pass** review (money correctness, kind precedence, chain isolation, gate honesty).

**Architecture:** Four phases. Phase A (correctness; changes emitted numbers and must land first), Phase B (refactors — but see the phase-contract correction: B1, B3, B8 and B10 also change numbers), Phase C (performance, behavior-preserving), Phase D (CLI/tests/docs/polish). **Within a phase, independence is the exception, not the rule:** see the dependency graph in Self-Review Notes for the authoritative execution order, and note that nine tasks edit `core/src/explorer/ingest.rs` and must run strictly sequentially.

**Tech Stack:** Rust 2021, alloy 2, rusqlite 0.31 (bundled), tokio, clap. Workspace crates: `mev-scout-core`, `mev-scout-cli`.

**Spec:** The review findings in this plan's tasks are the spec. Each task cites exact `file:line` for every change. Every line reference in this plan was re-read against the working tree on 2026-10-25; two first-pass claims were **withdrawn after verification** (see the note in Self-Review Notes) rather than carried forward, and any step that says "read and confirm" is a deliberate request to verify rather than an unresolved TODO.

## Global Constraints

- Workspace lints (root `Cargo.toml`): `warnings = "deny"`, clippy `unwrap_used = "deny"`, `expect_used = "deny"`. Never add `unwrap()`/`expect()`; never leave a new warning.
- Verify after every task: `cargo test -p mev-scout-core && cargo test -p mev-scout-cli`.
- Lint gate before each commit: `cargo clippy --workspace --all-targets`.
- Golden/corpus gates: `cargo test -p mev-scout-core --test explorer_golden` (static, always runs) plus the explicit live commands in **Canonical gate commands** below. A plain `--test explorer_corpus --test explorer_tag_coverage` run leaves both live halves skipped.
- **Gate honesty (added 2026-10-25 review — read before trusting any gate above).** As written, two of the three gate targets assert nothing on a default machine:
  - `core/tests/explorer_corpus.rs:903-906` — `real_block_corpus_matches_derived_facts` prints "Skipping: MEV_SCOUT_E2E/RPC_URL not set" and `return`s, **without `#[ignore]`**. `cargo test` reports it green while verifying nothing. Same shape at `:929-930` ("no corpus chain reachable").
  - `core/tests/explorer_tag_coverage.rs:44-45` is correctly `#[ignore = "live RPC — set MEV_SCOUT_COVERAGE=1"]`, so the gate line above never runs it (it needs `--ignored` plus the env var), and even when run it only `WARN`s unless `MEV_SCOUT_COVERAGE_STRICT=1` (`:121-130`).
  - There is **no CI in the repo at all** (no `.github/`, no justfile/Makefile), while `Cargo.toml:7-11` sets `warnings = "deny"` and `unwrap_used`/`expect_used` are `deny`. The lint policy this plan's Global Constraints relies on is currently unenforced.
  Consequence: **Task A0 must land first.** Until it does, every "expected: PASS" in Phase A/B/C is partly an illusion, and the "inspect the golden diff" rule below cannot be honoured for the corpus half.
- **Canonical gate commands (A0 Step 3 — read every later "PASS" under this rule).** There are two gates, not one:
  - **Static (always):** `cargo test -p mev-scout-core --test explorer_golden` — the only gate that asserts on a default machine.
  - **Live corpus (opt-in):** `MEV_SCOUT_E2E=1 RPC_URL=<archive-rpc> cargo test -p mev-scout-core --test explorer_corpus -- --ignored`
  - **Live tag coverage (opt-in, strict):** `MEV_SCOUT_COVERAGE=1 MEV_SCOUT_COVERAGE_CONFIG=<path> MEV_SCOUT_COVERAGE_STRICT=1 cargo test -p mev-scout-core --test explorer_tag_coverage -- --ignored`

  A bare `--test explorer_corpus` still runs the binary's **static** tests and leaves the live test `ignored`; it is not evidence about real-chain behavior. Any later step that names `explorer_corpus` or `explorer_tag_coverage` as its live gate carries the marker `[live half: canonical commands]` — without the env prefix and `--ignored`, that half did not run. Do **not** append `-- --ignored` to a combined `--test explorer_golden --test explorer_corpus` command: `--ignored` applies to every selected target and would skip all golden tests.
- **Phase A warning:** Tasks A1, A4, A9, A10, A11, A12, **A13, A14, A16, A17, A19** intentionally change realized P&L / classification output. Golden baselines may shift. If a golden test fails after those, inspect the diff: if it matches the intended behavior change, update the baseline deliberately and say so in the commit message. Never edit a baseline to make a failure disappear without understanding it.
- **Phase-contract correction (2026-10-25 review; amended for B10/B11 and the re-baseline discipline).** Phase B is labelled behavior-preserving, but **B1** (`.round()` vs `as u128` changes every gas figure, ~1.7% understatement today), **B3** (adding the defensive `pool_legs` sort to sandwich), **B8** (making `TxClaims` the authority over tx_index exclusivity), **B10** (upsert semantics on re-index) and **B11** (the `unknown` kind label becomes `utility` on every keeper/claim/bundler row) all change emitted output. Treat these as correctness work, and do not use "identical output" as their acceptance test — the plan states the expected diff direction per task, and baselines update per the re-baseline discipline in Self-Review Notes.
- **A1 and B8 are the same invariant.** B8 subsumes A1's mechanism. Either let A1 land as the fix and reduce B8 to wiring + registry (no re-baseline expected), or skip A1 and do it once in B8. Do not re-baseline goldens twice for one invariant.
- **One-file sequencing track (`core/src/explorer/ingest.rs`).** Tasks A6, A8, A21, C4, C7, C8, C9, D10 and D12 all edit `ingest.rs`. The claim "tasks within a phase are mostly independent" does **not** hold for that file. Execute them in exactly this order, one at a time: **A6 → A8 → A21 → C4 → C7 → C8 → C9 → D10 → D12**. C8 moves the async/store boundary that A8, A21 and D10 all touch, so landing it late avoids rework.
- Commit style: conventional commits (`fix:`, `refactor:`, `perf:`, `test:`, `docs:`), matching repo history.
- **Working-tree hygiene (re-verified 2026-10-25; supersedes the earlier dirty-tree note).** The in-flight `block_indexer`/`QiRateCache` work the first draft warned about landed in commit `4b5da5e`; at re-verification the tree was clean apart from this plan file. Do not trust any snapshot — including this one — for long: **run `git status` before starting each task and again before each commit**, and reconcile with any new in-flight work before proceeding. The standing rule survives regardless: **never use `git add -A`/`git add .`** — every task stages explicit paths, so unrelated work can never be swept into a `fix(explorer):` commit.
- Do not `cd`; run every command from the repository root the session was started in. Do not hardcode an absolute repo path into a command, script, or doc — this plan was drafted on a machine where the checkout lived at `D:\gitlab.dte.repo\mev-scout` and the path has since moved, so any command or recorded output carrying that prefix is historical.

---

## Phase M — External coverage oracle (measurement harness; M0 before Phase A, M1 at audit checkpoints)  *(added 2026-10-26)*

**Why this phase exists — the one gap every other gate in this plan shares:** goldens are self-generated baselines (a bug present in both the old and the new code passes every golden), and the A15 census is an internal query. Nothing else in this plan validates emitted rows against an independent external source. The 2026-10-25 manual comparison against explorer.mev.zone's backend (recall 7/7 vs the vendor, vendor agreement 50%, matched-hash profit deltas ≤ 0.61%) proved the method; these two tasks make it reproducible and turn "the plan should improve accuracy" into a measured claim.

**Oracle:** `https://gateway.biglabs.eu/api/avalanche/arbitrages?per_page=50&sortBy=-created_at` (backend of explorer.mev.zone mevlive). Verified quirks (2026-10-25): no `created_at` field is returned despite the sort parameter — `time` equals the block timestamp, so window filtering is by `time`/`blockNumber`; `sender` is the initiating EOA. Other chains swap the path segment (`/api/ethereum/arbitrages`, `/api/polygon/arbitrages`, …).

**Standing caveats for every metric below (do not let the harness imply more than it measures):**
- The vendor is a **coverage oracle, not ground truth**. It misses large real arbs — relayer-routed bots whose profit stays in the contract are invisible to it (the 50% agreement means half our rows were vendor-unseen). Therefore: a vendor row we miss is a real bug (recall is a lower bound on ours); a local row the vendor lacks is *not* a false positive until adjudicated by hand.
- The endpoint is `/arbitrages` only — it is **blind to sandwich/liquidation/JIT classification** and to kind-precedence changes (B8) and the `Unknown`→`Utility` rename (B11). Those ride the A15 census and the goldens. The oracle's lane is: arb recall, and the P&L math tasks (A9, A13, A14, A16, B1).
- Vendor profit figures are not truth either — "delta shrank" is weak evidence. The strong check is the plan's own predicted direction per task (see M1).

### Task M0: Freeze the baseline before Phase A  *(runs alongside A0; Steps 1–2 must complete before the first Phase A task lands)*

**Files:**
- Create: `scripts/compare_biglabs.py` (explicit `--db`, `--snapshot`, `--chain`, `--lo`, `--hi`; no hardcoded DB path — D15's rule applies to this script too)
- Create: `docs/oracle/biglabs-avalanche-<LO>-<HI>.json` (frozen vendor snapshot, committed — the API will not serve this window again, so the file is the only durable copy)
- Modify: `docs/explorer_strategy_checklist.md` (new "External coverage oracle — baseline" section holding the metrics table)

**Non-negotiable design point — freeze once, never re-fetch.** "Last 1 hour" taken before implementation and again after implementation are two *different* windows; the comparison would be meaningless, and the API only serves recent data anyway. So: fetch the vendor's last hour **once, now**; derive the fixed block range `[LO, HI]` from the snapshot; commit the raw JSON. Every later checkpoint re-indexes *that same range* and compares against *that same snapshot*. Comparability comes from the frozen snapshot, not from re-calling the API.

- [x] **Step 1: Fetch and freeze.** Paginate the API until `time` falls below the window start (1 hour of Avalanche ≈ 1,800 blocks); save the raw pages as `docs/oracle/biglabs-avalanche-<LO>-<HI>.json` with fetch metadata (`fetched_at`, URL, page count). Derive `[LO, HI]` from the snapshot's min/max `blockNumber`.
- [x] **Step 2: Index the same range with the current (pre-plan) code** into a dedicated DB (`cache/oracle-avalanche.sqlite`) via the CLI backfill path with the archive endpoints from `hist.toml` — never the live `explorer-avalanche.sqlite`, and never publicnode (D15 documents why it cannot serve historical blocks). Record the commit hash of the binary that indexed it next to the metrics; a baseline of unknown code version is not a baseline.
- [x] **Step 3: Write the comparison script.** Hash-level join, kind-agnostic on our side (any local `mev_ops` row on the same tx hash counts as a match — a vendor arb that we label `Unknown`/`Frontrun` is a finding, not a miss). Prior art: the deleted `tmp-compare/compare_mevlive.py` (`git show 5ad9630^:tmp-compare/compare_mevlive.py`) already implements the hash join and the profit-delta core — resurrect it rather than rewriting. Report:
  - `recall_vs_vendor = |V ∩ L| / |V|` (V = vendor tx hashes in range, L = local tx hashes in range), plus the kind label we assigned each matched vendor row
  - `vendor_agreement = |V ∩ L| / |L|` — labelled *lower-bound agreement*, never "precision"
  - per-hash relative profit deltas on `V ∩ L` (median / p90 / max)
  - the `L \ V` list with a per-row adjudication column: `tp_vendor_miss` | `fp_suspect` | `unknown` — adjudicated by hand in this baseline run; every M1 checkpoint re-adjudicates it, and a row that disappears must map to a predicted task effect or it is a regression flag
  - machine output `cache/oracle-report-<label>.json` + a markdown table for the checklist doc
- [x] **Step 4: Record the baseline** in `docs/explorer_strategy_checklist.md`: block range, both code versions (our commit, vendor fetch time), all metrics, the adjudicated `L \ V` table, and the snapshot's SHA256. This table is the "before" every M1 run is judged against.
- [x] **Step 5: Commit** — `git commit -m "test(explorer): freeze biglabs coverage-oracle baseline (recall/agreement/profit deltas) before improvement plan"`

---

### Task M1: Re-run the oracle at each audit checkpoint — did the plan actually move the numbers?

**Files:**
- Modify: `docs/explorer_strategy_checklist.md` (one metrics table per checkpoint, next to the M0 baseline)

**Checkpoints (three re-runs; each re-indexes the same frozen `[LO, HI]` from scratch into `cache/oracle-avalanche.sqlite` — recreate the DB each run, because A21/B10 reorg and upsert semantics differ across checkpoints and a reused DB would mix them):**
1. **End of Phase A**, attached to the Phase-A audit commit the re-baseline discipline already defines. Phase A is where the money tasks land (A9, A13, A14, A16), so this is the run that matters most.
2. **End of Phase B** — B1 (~1.7% gas), B3, B8, B10, B11 are declared number-movers by the phase-contract correction.
3. **Final run**, closing the plan — the before/after table in the checklist doc is the plan's externally visible evidence.

**What each run checks — predicted direction, not "closer to vendor":**
- A14: fake-`$0`-quote rows vanish from `L` (watch them leave `L \ V` with `fp_suspect` → gone).
- A13: net profit on flash-loan rows moves *up* by (once) the fee amount. B1: net moves *down* by the gas understatement (~1.7% of gas). A16: `NULL`-profit rows in range drop to zero.
- Recall is the one hard gate: `recall_vs_vendor` may not drop below the M0 baseline at any checkpoint — this is stop-the-line, everything else is evidence.
- The oracle is blind to B8/B11 kind changes — say so in the table rather than implying coverage.

- [ ] **Step 1 (per checkpoint):** re-index `[LO, HI]` with the checkpoint's code, run the script, append the table (label: `phase-a`, `phase-b`, `final`).
- [ ] **Step 2 (per checkpoint):** diff the `L \ V` set against the previous run's adjudicated table — every appeared/disappeared row gets a one-line cause tied to a task ID, or a `REGRESSION?` flag.
  *Progress: checkpoint `phase-a` done (Steps 1–2, commit `f0efc23`; table in `docs/explorer_strategy_checklist.md`). `phase-b` and `final` pending — boxes stay open until all three runs exist.*
- [ ] **Step 3 (final run only, optional sanity):** one *fresh* last-hour fetch (a new window) to confirm recall also holds on live data — comparison is recall-only on that window, never profit, and it never replaces the frozen-window runs.

---

## Phase A — Correctness (P0)

### Task A0: Make the verification gates real (CI + honest skips) — lands first

**Files:**
- Create: `.github/workflows/ci.yml`
- Modify: `core/tests/explorer_corpus.rs:903-906` and `:929-930`
- Modify: `core/tests/explorer_tag_coverage.rs` (env-var docs only; the `#[ignore]` at `:45` is already correct)
- Modify: `docs/CLI.md` (or a short `## CI` section in `README.md`) documenting which gates run where

**Problem:** Every later task in this plan is gated on `cargo test -p mev-scout-core --test explorer_golden --test explorer_corpus --test explorer_tag_coverage`, but two of those targets are silent no-ops by default (`explorer_corpus.rs:903` `return`s without `#[ignore]`; `explorer_tag_coverage.rs:45` is `#[ignore]`d and only `WARN`s without `MEV_SCOUT_COVERAGE_STRICT=1`). There is also no CI enforcing `warnings = "deny"` / `unwrap_used = "deny"` (`Cargo.toml:7-11`, `clippy.toml`), so a task can land red and nothing catches it. The plan's safety net is currently a mirror.

- [x] **Step 1: Make the corpus skip honest.** Replace the early-return-with-`Ok` at `explorer_corpus.rs:903-906` with an explicit skip that cannot read as a pass. Preferred shape: keep the test, add `#[ignore = "live RPC — set MEV_SCOUT_E2E=1 and RPC_URL"]` on the `#[tokio::test]`, and inside the body `anyhow::bail!` if `MEV_SCOUT_E2E=1` was set but no RPC resolved (that is the "I asked for it and you silently skipped" case). Same treatment for the `checked == 0` path at `:929-930`: when an RPC URL was supplied, zero reachable chains is a failure, not a shrug.
- [x] **Step 2: Add the workflow** — `.github/workflows/ci.yml`:

```yaml
name: ci
on:
  push: { branches: [main] }
  pull_request:
jobs:
  static:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
        with: { components: rustfmt, clippy }
      - run: cargo fmt --all --check
      - run: cargo clippy --workspace --all-targets -- -D warnings
      - run: cargo test -p mev-scout-core --test explorer_golden
  live:
    runs-on: ubuntu-latest
    if: ${{ vars.RPC_URL != '' }}
    env:
      MEV_SCOUT_E2E: "1"
      RPC_URL: ${{ vars.RPC_URL }}
      MEV_SCOUT_COVERAGE_STRICT: "1"
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
      - run: cargo test -p mev-scout-core --test explorer_corpus -- --ignored
      - run: cargo test -p mev-scout-core --test explorer_tag_coverage -- --ignored
```

  The `live` job's `if:` is the point: an explicitly-requested live gate that gets skipped must be visible as a skipped job, never as a green check.
- [x] **Step 3: Re-baseline the plan's own gate lines.** Append to Global Constraints (done above) and to each later task's verify step where the corpus is load-bearing: `MEV_SCOUT_COVERAGE_STRICT=1` and `--ignored` where a live gate is intended, so "PASS" means the assertions ran.
- [x] **Step 4: Verify locally** — `cargo test -p mev-scout-core 2>&1 | grep -E "explorer_corpus|ignored"`: the corpus test must now report as `ignored`, not `ok`. Commit body must state how many tests are ignored by default so no future reader mistakes silence for success.
- [x] **Step 5: Commit** — `git commit -m "ci(explorer): enforce fmt/clippy/test and make live-RPC tests skip loudly instead of passing silently"`

---

### Task A1: Sandwich legs must supersede ArbAtomic on the same tx_index

**Files:**
- Modify: `core/src/explorer/classify/mod.rs:265-282`
- Test: `core/src/explorer/classify/mod.rs` (new `#[cfg(test)]` test at file end)

**Interfaces:**
- Consumes: `classify_sandwiches` output `Vec<MevEvent>` with `details["backrun_tx_index"]: u64`, `kind_procedence()` at `classify/mod.rs:297`.
- Produces: unchanged public API; invariant "one tx_index never carries both a `Sandwich` event and an `ArbAtomic`/`Unknown` event".

**Problem:** `superseder` (mod.rs:265) is built only from frontruns/backruns. A tx that is a sandwich front-run (or its back-run tx) *and* also produced a closed arb cycle keeps its `ArbAtomic` event through the `retain` at mod.rs:276 — double-counting P&L on one tx_index, exactly what the load-bearing comment (mod.rs:217-223) forbids.

**Scope note (added after review).** Verified: `consumed` (mod.rs:249-255) is only handed to `classify_frontruns`/`classify_backruns`, and `classify_arbs` runs at mod.rs:207 — *before* sandwiches exist at `:243` — so the hole is real, and the fix does not drop the sandwich itself because `sandwiches` is appended at `:282` outside the `retain`. Two constraints: (1) this fix only covers `Sandwich` vs `ArbAtomic|Unknown`; the identical hole exists for Sandwich vs `Jit`/`JitArb`/`Skim`/`Liquidation` and is closed structurally only by B8. (2) A1 and B8 implement one invariant twice — decide up-front whether B8 re-baselines again or merely absorbs A1's mechanism, and say which in the A1 commit body. Prefer: A1 lands the fix, B8 lands the authority and must expect **zero** golden drift.

- [x] **Step 1: Write the failing test** — append to `core/src/explorer/classify/mod.rs`:

```rust
#[cfg(test)]
mod supersede_tests {
    use super::*;
    use crate::explorer::test_fixtures::builders::*;

    /// A sandwich front-run tx that ALSO closes an atomic-arb cycle must not
    /// keep its ArbAtomic event: P&L is attributed once, to the Sandwich.
    #[test]
    fn sandwich_front_supersedes_arb_on_same_tx() {
        // Build via the scenario fixtures: one searcher tx that is a closed
        // 2-pool cycle AND the front-run of a victim swap, plus the back-run.
        // Reuse the pattern of an existing sandwich scenario in
        // core/src/explorer/scenarios.rs (search `fn sandwich_`) for the
        // transfer/swap layout, but add a second pool hop to the front tx so
        // find_closed_cycle also matches. If composing from raw builders is
        // simpler, do that; the assertion below is the contract:
        let input = /* BlockInput with the tx layout described above */;
        let out = classify_block(input);
        let mut by_tx: std::collections::HashMap<u64, Vec<MevKind>> = Default::default();
        for e in &out.events {
            by_tx.entry(e.tx_index).or_default().push(e.kind);
        }
        for (tx, kinds) in by_tx {
            let has_sandwich = kinds.contains(&MevKind::Sandwich);
            let has_arb = kinds.contains(&MevKind::ArbAtomic) || kinds.contains(&MevKind::Unknown);
            assert!(
                !(has_sandwich && has_arb),
                "tx {tx} carries both Sandwich and Arb/Unknown: {kinds:?}"
            );
        }
    }
}
```

(The `/* BlockInput ... */` site is the one place the executor must design the fixture: use the builders in `core/src/explorer/test_fixtures.rs` — read that file first; it has `tx`, `swap`, `transfer`, `block` helpers with `effective_gas_price_gwei: 30.0` defaults. Model the front tx on an existing `Sandwich` scenario in `core/src/explorer/scenarios.rs` and add one extra swap edge through a second pool so the tx also closes a cycle. Acceptance criterion: before the fix, this test FAILS with "tx N carries both Sandwich and Arb/Unknown"; after, it PASSES.)

- [x] **Step 2: Run it, verify it fails** — `cargo test -p mev-scout-core supersede_tests`
Expected: FAIL with the double-count assertion (if it unexpectedly passes, the fixture does not reproduce the bug — strengthen it before proceeding; do NOT proceed with a test that never failed).

- [x] **Step 3: Implement the fix** — in `classify_block` at `core/src/explorer/classify/mod.rs`, extend the `superseder` population (currently lines 265-275) so sandwich legs claim their tx_indices *before* the `retain` at line 276:

```rust
    let mut superseder: HashMap<u64, MevKind> = HashMap::new();
    for s in &sandwiches {
        superseder.insert(s.tx_index, MevKind::Sandwich);
        if let Some(b) = s.details.get("backrun_tx_index").and_then(|v| v.as_u64()) {
            superseder.insert(b, MevKind::Sandwich);
        }
    }
    for e in frontruns.iter().chain(backruns.iter()) {
        // unchanged: frontruns/backruns never collide with sandwich legs
        // because `consumed` already excluded them at classify time.
        superseder.entry(e.tx_index).or_insert(e.kind);
        if e.kind == MevKind::Frontrun {
            if let Some(c) = e.details.get("close_tx_index").and_then(|v| v.as_u64()) {
                superseder.entry(c).or_insert(MevKind::Frontrun);
            }
        }
    }
```

(Sandwich entries use `insert` — highest precedence per `kind_precedence` — while the existing `entry().or_insert()` chain stays below them.)

- [x] **Step 4: Run the test and the full gates**

Run: `cargo test -p mev-scout-core supersede_tests && cargo test -p mev-scout-core --test explorer_golden --test explorer_corpus`
Expected: new test PASS. Golden/corpus: PASS, or fail with diffs showing ArbAtomic events disappearing from txs that are also sandwich legs — in that case update those baselines deliberately.

- [x] **Step 5: Commit**

```bash
git add core/src/explorer/classify/mod.rs
git commit -m "fix(explorer): sandwich legs supersede ArbAtomic on the same tx_index to prevent P&L double-counting"
```

---

### Task A2: Fix hour/second unit mixup in `native_price_near`

**Files:**
- Modify: `core/src/explorer/store/jit_positions.rs:53-64`
- Test: new `#[cfg(test)] mod tests` at end of `core/src/explorer/store/jit_positions.rs`

**Interfaces:**
- Produces: same signature `pub fn native_price_near(&self, ts: u64) -> anyhow::Result<Option<f64>>`; behavior fix only (now returns the price at the hour bucket nearest `ts/3600`, within ±24h, instead of the globally arbitrary-nearest row).

**Problem:** `native_price_near` orders by `ABS(hour - ?1)` where `hour` is an hour bucket (`ts/3600`) but `?1` is bound to raw `ts` seconds (jit_positions.rs:59). `ABS(100 - 361800)` vs `ABS(200 - 361800)` — the second is always smaller, so it returns the *latest* cached price, not the one nearest the queried timestamp. This silently mis-prices the detector-vs-realized USD→wei conversion.

- [x] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::explorer::store::ExplorerStore;

    #[test]
    fn native_price_near_returns_bucket_nearest_ts() {
        let store = ExplorerStore::open_in_memory().expect("open store");
        let hour_a = 100u64;
        let hour_b = 200u64;
        store.put_price(Address::ZERO, hour_a, 10.0, "test").unwrap();
        store.put_price(Address::ZERO, hour_b, 99.0, "test").unwrap();
        // ts inside hour_a: 100*3600 + 1800
        let got = store.native_price_near(100 * 3600 + 1800).unwrap();
        assert_eq!(got, Some(10.0), "must pick the hour bucket containing ts, not the latest row");
        // ts nearest hour_b
        let got = store.native_price_near(199 * 3600 + 3500).unwrap();
        assert_eq!(got, Some(99.0));
    }
}
```

(Verified, so the hedging here can be dropped: `ExplorerStore::open_in_memory()` **does** exist at `store/schema.rs:21-26` and runs the same `initialize()` as the file path, and `put_price(token, hour, usd, source)` has exactly the argument order the snippet uses (`jit_positions.rs:37-43`). One caveat to carry into B6: the in-memory path skips the `journal_mode`/`synchronous` pragma batch that `open()` runs at `schema.rs:15`, so tests exercise a different pragma set than production — B6 should make both paths share one setup.)

- [x] **Step 2: Run it, verify it fails** — `cargo test -p mev-scout-core native_price_near`
Expected: FAIL — first assertion returns `Some(99.0)` (wrong bucket).

- [x] **Step 3: Fix** — replace the body of `native_price_near`:

```rust
    pub fn native_price_near(&self, ts: u64) -> anyhow::Result<Option<f64>> {
        let bucket = ts / 3600;
        let mut stmt = self.conn.prepare(
            "SELECT usd FROM prices
             WHERE lower(token) IN ('0x0000000000000000000000000000000000000000', '0x0')
               AND hour BETWEEN ?1 - 24 AND ?1 + 24
             ORDER BY ABS(hour - ?1) ASC LIMIT 1",
        )?;
        let mut rows = stmt.query([bucket as i64])?;
        Ok(match rows.next()? {
            Some(row) => Some(row.get::<_, f64>(0)?),
            None => None,
        })
    }
```

(The ±24h bound matches `price_at`'s staleness policy at jit_positions.rs:27; without it a store with only ancient prices would happily return them.)

- [x] **Step 4: Run the *real* consumers too** — `cargo test -p mev-scout-core native_price_near && cargo test -p mev-scout-core explorer_golden && cargo test -p mev-scout-core --test mev_corpus`
Expected: PASS. Note the blast radius is narrower than the gate line in Global Constraints suggests: the **only** production callers of `native_price_near` are `core/tests/mev_corpus.rs:466` and `:485` — a target this plan did not previously run at all. Add it to this task's gate.
- [x] **Step 4b: Predict the diff direction before you inspect it.** Two verified facts change what "correct" looks like here: (1) the unit fix converts today's *always returns the newest cached row* into *returns the nearest bucket within ±24h*, so for a historical `ts` in a backfilled DB the result may become `None` rather than a wrong number — a corpus verdict that previously compared against a mis-priced USD will now compare against no price. That is the honest outcome, not a regression; do not "fix" it by widening the bound. (2) The `prices` table already carries **two different hour conventions**: token prices are keyed by block hour (`ingest.rs:751` `pricing::hour_bucket(ts)` with the block's `ts`) while native prices are keyed by fetch-time hour (every `native_price_cached` call site passes `crate::utils::epoch_secs()` — `ingest.rs:1065`, `:1110`, `:1179`). A2 does not resolve that, and the deferred section at the end of this plan (historical native pricing) is what would. Record the convention mismatch in the `native_price_near` doc comment so the next reader does not assume block-hour keying.

- [x] **Step 5: Commit**

```bash
git add core/src/explorer/store/jit_positions.rs
git commit -m "fix(explorer): native_price_near compares hour buckets, not raw epoch seconds"
```

---

### Task A3: Reject oversized ABI arrays instead of silent truncation at 64

**Files:**
- Modify: `core/src/explorer/decode/jit.rs:129-148` (`decode_abi_u256_array`) and `:153-175` (`decode_lb_amounts_xy`)
- Test: `core/src/explorer/decode/tests.rs` (append)

**Interfaces:**
- Produces: `decode_abi_u256_array(data: &[u8], head_word: usize) -> Option<Vec<U256>>` and `decode_lb_amounts_xy(data: &[u8]) -> Option<(U256, U256)>` — both now return `None` when the declared array length exceeds 64.

**Problem:** Both functions iterate `0..len.min(64)`, so a Trader Joe LB position spanning >64 bins silently yields wrong min/max bin ids (and wrong tick range → wrong JIT in-range validation) instead of being rejected.

- [x] **Step 1: Write the failing test** — append to `core/src/explorer/decode/tests.rs`:

```rust
#[test]
fn abi_u256_array_rejects_over_64_entries() {
    // head word = relative offset 32, then length 65, then 65 zero words.
    let mut data = vec![0u8; 32]; // offset word (rel = 0x20)
    data.extend_from_slice(&U256::from(65usize).to_be_bytes::<32>()); // len = 65
    data.extend_from_slice(&[0u8; 65 * 32]);
    assert!(
        decode_abi_u256_array(&data, 0).is_none(),
        "arrays > 64 entries must be rejected, not truncated"
    );
    // exactly 64 still decodes
    let mut ok = vec![0u8; 32];
    ok.extend_from_slice(&U256::from(64usize).to_be_bytes::<32>());
    ok.extend_from_slice(&[0u8; 64 * 32]);
    assert_eq!(decode_abi_u256_array(&ok, 0).map(|v| v.len()), Some(64));
}
```

(If `decode_abi_u256_array` is private to `decode/jit.rs`, make it `pub(super)` and re-export for the test module the same way other decode fns are tested in `decode/tests.rs`. `to_be_bytes::<32>()` is alloy's `U256::to_be_bytes` — if the API differs in alloy 2, use `U256::from(65usize).to_be_bytes()` with the appropriate array size or build the word with `U256::to_be_bytes_vec()` sliced to 32.)

- [x] **Step 2: Run it, verify it fails** — `cargo test -p mev-scout-core abi_u256_array_rejects`
Expected: FAIL (returns `Some(64 items)` for the 65-entry case).

- [x] **Step 3: Fix** — in `decode_abi_u256_array` (decode/jit.rs:138-147):

```rust
    let len = u256_to_usize(U256::from_be_slice(&data[rel..rel + 32]))?;
    if len > 64 {
        return None;
    }
    let mut out = Vec::with_capacity(len);
    for i in 0..len {
        let start = rel + 32 + i * 32;
        if data.len() < start + 32 {
            break;
        }
        out.push(U256::from_be_slice(&data[start..start + 32]));
    }
    Some(out)
```

Apply the identical `if len > 64 { return None; }` guard and remove the two `min(64)`s in `decode_lb_amounts_xy` (decode/jit.rs:164-167).

- [x] **Step 4: Run tests** — `cargo test -p mev-scout-core decode`
Expected: PASS.

- [x] **Step 5: Commit**

```bash
git add core/src/explorer/decode/jit.rs core/src/explorer/decode/tests.rs
git commit -m "fix(explorer): reject ABI arrays over 64 entries instead of silent truncation"
```

---

### Task A4: Aggregate flash-loan repay detection across split transfers

**Files:**
- Modify: `core/src/explorer/classify/arb.rs:64-77`
- Test: `core/src/explorer/classify/arb.rs` (new `#[cfg(test)] mod repay_tests` at file end)

**Interfaces:**
- Consumes: `FlashLoanFact { token, amount, fee, initiator, provider, recipient }` (see `core/src/explorer/types.rs`), `DeltaLedger::from_transfers`, `net_flash_loan`.
- Produces: no signature change; `repaid` is now `repaid_total >= fl.amount` over all matching transfers.

**Problem:** `arb.rs:64-69` requires a *single* transfer `t.amount >= fl.amount` to the provider. Balancer-style split/partial repays (two transfers of half each) fail the check, so `net_flash_loan` subtracts principal the ledger already netted — silently erasing real profit and rejecting the op as `NetZeroSearcher`.

- [x] **Step 1: Write the failing test** — append to `classify/arb.rs`:

```rust
#[cfg(test)]
mod repay_tests {
    use super::*;
    use crate::explorer::classify::BlockInput;
    use crate::explorer::test_fixtures::builders::*;

    /// Two half-amount repayments to the provider must count as repaid, so
    /// `net_flash_loan` must NOT subtract the principal again.
    #[test]
    fn split_flash_repay_is_detected() {
        // Fixture: one tx with a flashloan of 1000 WETH (fee 0) and two
        // transfers of 500 WETH each from the borrower to the provider.
        // Use test_fixtures builders (read test_fixtures.rs first for the
        // exact helper names: tx(), transfer(), block(), plus the
        // FlashLoanFact literal from core/src/explorer/types.rs).
        // Build BlockInput exactly as existing arb tests in this file do
        // (search `mod tests` in classify/arb.rs for the pattern).
        let input = /* BlockInput per existing arb-test pattern */;
        let (events, rejects) = classify_arbs(&input, &Default::default(), &test_arb_tag_opts(&input));
        assert!(
            events.iter().any(|e| e.kind == MevKind::ArbAtomic),
            "split-repay flash arb must classify: rejects were {rejects:?}"
        );
    }
}
```

(Reuse the local test helpers already present in `classify/arb.rs`'s existing `#[cfg(test)] mod tests` — read it first; it will show how `BlockInput` and `ArbTagOpts` are built in this file's tests. Copy that setup and add the split-repay transfer layout. Acceptance: FAIL before the fix with a `NetZeroSearcher`-style reject, PASS after.)

- [x] **Step 2: Run it, verify it fails** — `cargo test -p mev-scout-core split_flash_repay`
Expected: FAIL (op rejected, no ArbAtomic event).

- [x] **Step 3: Fix** — replace arb.rs:64-69 with an aggregate:

```rust
            let repaid_total = tx
                .transfers
                .iter()
                .filter(|t| {
                    t.token == fl.token
                        && (t.from == borrower || t.from == tx.from)
                        && (t.to == fl.provider || t.to == fl.recipient)
                })
                .fold(U256::ZERO, |acc, t| acc.saturating_add(t.amount));
            let repaid = repaid_total >= fl.amount;
```

(Do not include `fee` in the threshold — the original single-transfer check compared against `fl.amount` only; keep semantics minimal. Note the aggregate may now over-match unrelated same-token transfers to the provider in the same tx, but those are exactly the transfers the ledger nets anyway, which is the direction the load-bearing comment at arb.rs:47-51 wants.)

- [x] **Step 4: Run full gates**

Run: `cargo test -p mev-scout-core split_flash_repay && cargo test -p mev-scout-core --test explorer_golden --test explorer_corpus`
Expected: PASS; any golden shift must show *newly classified* ops, not lost ones — inspect before updating baselines.

- [x] **Step 5: Commit**

```bash
git add core/src/explorer/classify/arb.rs
git commit -m "fix(explorer): aggregate split flash-loan repays instead of requiring one full transfer"
```

---

### Task A5: Typed enum filters instead of string-formatted SQL

**Files:**
- Modify: `core/src/explorer/store/query.rs:14-23` and every `*_filtered`/`report_*` fn signature that takes `kind: Option<&str>, confidence: Option<&str>` (query.rs and `core/src/explorer/store/mod.rs:334-340` leaderboard fns)
- Modify: call sites `cli/src/commands/explorer/report.rs:144-155, 184-191`
- Test: `core/src/explorer/store/query.rs` (extend existing tests if present; otherwise add `#[cfg(test)] mod tests`)

**Interfaces:**
- Consumes: `crate::explorer::types::{MevKind, Confidence}` — both already have `parse`/`as_str` (MevKind::parse is used at report.rs:147).
- Produces: store query fns take `kind: Option<MevKind>, confidence: Option<Confidence>`; SQL fragments use `kind.as_str()`. The store is `pub`, so arbitrary strings can no longer reach SQL at all.

**Problem:** `report_filter_clause` (query.rs:14-23) and `ops_in_range` (query.rs:205-217) interpolate `kind`/`confidence` strings into SQL via `format!`. Safe today only because the CLI pre-validates; the store is a public API and this is one refactor away from an injection bug.

- [x] **Step 1: Write the failing test** — a compile-level change; write the API-shape test first in `query.rs`:

```rust
#[cfg(test)]
mod filter_tests {
    use super::*;
    use crate::explorer::types::{Confidence, MevKind};

    #[test]
    fn filter_clause_takes_enums_not_strings() {
        // Compile-time contract: if this compiles, arbitrary &str can no
        // longer reach SQL. Runtime check: fragment contains the canonical
        // snake_case kind spelling.
        let s = report_filter_clause(Some(MevKind::Sandwich), Some(Confidence::Exact));
        assert!(s.contains("kind = 'sandwich'"));
        assert!(s.contains("confidence = 'exact'"));
    }
}
```

(First read `core/src/explorer/types.rs` for the exact `Confidence` variants and `as_str` spellings — the test asserts whatever `as_str()` returns; if `Confidence` has no `as_str`, add one mirroring the DB spelling. Adjust the assertion to the real strings.)

- [x] **Step 2: Change signatures** — mechanical refactor:
  1. `fn report_filter_clause(kind: Option<MevKind>, confidence: Option<Confidence>) -> String` using `format!("AND kind = '{}' ", k.as_str())`.
  2. Every store query fn in `query.rs` and the leaderboard fns in `store/mod.rs` (`top_grouped`, `top_competitors_filtered`, `top_pools_filtered`, `top_senders_filtered`): change `kind: Option<&str>` → `Option<MevKind>`, `confidence: Option<&str>` → `Option<Confidence>`, pass through.
  3. `cli/src/commands/explorer/report.rs`: keep the CLI-side parse (it owns the friendly error), pass `Some(parsed)` (the enum) instead of `Some(parsed.as_str())`; delete `confidence_str` indirection and pass `Confidence::parse(...)` — check `cli/src/cli.rs:118-121` for the existing confidence validation to reuse.
  4. Grep for other callers: `grep -rn "report_by_kind\|top_competitors_filtered\|top_pools_filtered\|top_senders_filtered\|ops_in_range" --include=*.rs core cli` and update all.

- [x] **Step 3: Run gates** — `cargo test -p mev-scout-core && cargo test -p mev-scout-cli && cargo clippy --workspace --all-targets`
Expected: PASS, no warnings (workspace denies them).

- [x] **Step 4: Commit**

```bash
git add core/src/explorer/store/query.rs core/src/explorer/store/mod.rs cli/src/commands/explorer/report.rs
git commit -m "refactor(explorer): store filters take MevKind/Confidence enums so strings never reach SQL"
```

---

### Task A6: `run_live` survives transient indexing errors

**Files:**
- Modify: `core/src/explorer/ingest.rs:979-996`

**Problem:** `run_live` propagates any single `index_block` error with `?` (ingest.rs:992) and any `native_price_cached` failure (ingest.rs:979), killing the live loop until restart. A transient Llama/price blip or RPC hiccup must not stop indexing.

- [x] **Step 1: Implement tolerance** — read `core/src/explorer/ingest.rs:900-1005` fully first, then change the poll body:

```rust
            let native_price = match native_price_cached(cfg, store, crate::utils::epoch_secs()).await {
                Ok(p) => p,
                Err(e) => {
                    warn!(error = %e, "native price fetch failed; skipping this poll");
                    tokio::time::sleep(std::time::Duration::from_millis(poll_ms)).await;
                    continue;
                }
            };
            for block in cursor..=safe {
                let indexed = match index_block(IndexBlockRequest {
                    rpc,
                    store,
                    cfg,
                    pools,
                    block_number: block,
                    native_price,
                    token_prices: None,
                    savax_rates: &mut savax_rates,
                    qi_rates: &mut qi_rates,
                })
                .await
                {
                    Ok(i) => i,
                    Err(e) => {
                        // Transient failure: do not advance the cursor; retry
                        // this block on the next poll.
                        warn!(block, error = %e, "live index_block failed; will retry next poll");
                        next_block = Some(block);
                        break;
                    }
                };
                blocks_session += 1;
                ops_session += indexed.ops as u64;
                emit_progress(blocks_session, ops_session, false);
            }
```

(Leave the reorg-check `?`s at ingest.rs:962-977 untouched for this task — reorg verification failing hard is a different, defensible policy. **Do not settle the `set_sync_state` question by reading.** The snippet's `next_block = Some(block)` + `break` is meant to prevent `store.set_sync_state(cfg.chain_id, safe, safe)` at :997 from recording a completed range that was never indexed; if the break does *not* bypass it, the gap becomes invisible to resume logic — a silent data-loss mode worse than the crash being removed. Pin it with Step 2b instead, and if the break does not bypass the call, use a `let completed = …` flag.)

- [x] **Step 2b: Add the consecutive-failure cap and pin the cursor invariant.** Two additions the review found missing: (1) an unbounded retry loop is a new failure mode — if the provider is down, this shape polls forever at `poll_ms` with no backpressure. Add a `consecutive_failures` counter reset on success, and after a threshold (name it: `const MAX_CONSECUTIVE_LIVE_FAILURES: u32 = 10;`) `bail!` with the last error and the stuck block number, so a dead RPC ends the run loudly instead of spinning. (2) A test that asserts the invariant the fix depends on: after a simulated failed `index_block` for block N, `get_sync_state`/`indexed_to` must remain `< N` and `block_classified(N)` must be false. If no mock-provider harness exists (check `core/tests/common/`), extract the poll body's error branch into a small pure fn (`fn live_outcome(result, cursor, safe) -> (Option<u64>, bool /*advance sync*/, u32 /*failures*/)`) and unit-test *that* — testing the decision is as good as testing the loop and does not need the network.

- [x] **Step 2: Verify** — `cargo test -p mev-scout-core ingest`
Expected: PASS (existing ingest tests; no unit test can easily simulate a mid-loop RPC failure without a mock provider — if `core/tests` has a mock-RPC harness, consider a test there; otherwise manual review + clippy is the gate).

- [x] **Step 3: Commit**

```bash
git add core/src/explorer/ingest.rs
git commit -m "fix(explorer): live indexer retries transient block/price failures instead of dying"
```

---

### Task A7: Make `unwind_from` atomic

**Files:**
- Modify: `core/src/explorer/store/sync.rs:70-108`
- Test: `core/src/explorer/store/sync.rs` (new `#[cfg(test)] mod tests`)

**Problem:** `unwind_from` issues 9 separate DELETEs with no transaction (sync.rs:71-106). A crash mid-unwind leaves `mev_ops`/`transfers`/`txs` inconsistent with `blocks_classified`, silently corrupting the reorg recovery the function exists to provide.

- [x] **Step 1: Write the failing-style test** (atomicity can't be crash-tested; assert behavior + that an error inside rolls everything back). Read how tests construct a store (check `core/src/explorer/store/mod.rs` bottom or `core/tests/explorer_golden.rs` for the open pattern) first. The test must also insert a `rejected_candidates` row for the forked block and assert it is gone after `unwind_from` — that deletion is new in this task and would otherwise land untested:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unwind_from_removes_all_traces_of_forked_blocks() {
        let store = /* in-memory or tempfile store per existing test pattern */;
        store.mark_block_classified(100, 0).unwrap();
        store.unwind_from(100).unwrap();
        assert!(!store.block_classified(100).unwrap());
        // and idempotent:
        store.unwind_from(100).unwrap();
    }
}
```

- [x] **Step 2: Run** — `cargo test -p mev-scout-core unwind_from`
Expected: PASS already (this is a safety refactor; the test pins the behavior).

- [x] **Step 3: Wrap in one transaction** — rusqlite 0.31's `Connection::unchecked_transaction()` works on `&self` (no `&mut` needed, so `ExplorerStore`'s `conn` field stays non-mut):

```rust
    pub fn unwind_from(&self, fork_block: u64) -> anyhow::Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        tx.execute("DELETE FROM blocks_classified WHERE block >= ?1", [fork_block as i64])?;
        tx.execute("DELETE FROM mev_ops WHERE block_number >= ?1", [fork_block as i64])?;
        tx.execute("DELETE FROM transfers WHERE block_number >= ?1", [fork_block as i64])?;
        tx.execute("DELETE FROM swaps WHERE block_number >= ?1", [fork_block as i64])?;
        tx.execute("DELETE FROM txs WHERE block_number >= ?1", [fork_block as i64])?;
        tx.execute("DELETE FROM blocks WHERE block_number >= ?1", [fork_block as i64])?;
        tx.execute("DELETE FROM jit_open_positions WHERE opened_block >= ?1", [fork_block as i64])?;
        tx.execute("DELETE FROM oracle_answers WHERE block_number >= ?1", [fork_block as i64])?;
        tx.execute("DELETE FROM reserve_rates WHERE block_number >= ?1", [fork_block as i64])?;
        tx.execute(
            "DELETE FROM rejected_candidates WHERE block_number >= ?1",
            [fork_block as i64],
        )?;
        tx.commit()?;
        Ok(())
    }
```

(If `unchecked_transaction` is not available in this rusqlite version, fall back to one `self.conn.execute_batch(&format!("BEGIN IMMEDIATE; {deletes} COMMIT;"))` with the numeric `fork_block` inlined — safe since it is a `u64`.)

**Amendment (2026-10-25 review): the current function is missing a table, not just a transaction.** Verified against the schema: `rejected_candidates` has `block_number INTEGER NOT NULL` and `chain TEXT` (`store/schema.rs:205-224`, indexed `rejected_candidates(chain, block_number)`), so it is unwindable by block — but today's `unwind_from` never deletes it. After a reorg, reject rows from the abandoned chain keep feeding the reject counts that the revenue report prints (and that Task D3 exports to CSV), so the denominators in the "why is this kind empty" analysis become fiction. The code block above adds it. Also decide and document explicitly: `opportunities` (`schema.rs:178-201`) and `paper_sessions`/`paper_fills` are *detector-run* records, not reorg-derived chain state — do **not** unwind them, and say why in the doc comment so nobody "completes" the list later. Finally: `unwind_from` deletes 10 tables by block number, but the fact tables carry **no chain column** (see Task A18) — in a shared DB this deletes the other chain's rows. A18's config guard must land before or with A7 for that to be safe.

- [x] **Step 4: Run** — `cargo test -p mev-scout-core unwind_from && cargo test -p mev-scout-core explorer_golden`
Expected: PASS.

- [x] **Step 5: Commit**

```bash
git add core/src/explorer/store/sync.rs
git commit -m "fix(explorer): reorg unwind runs in a single transaction"
```

---

### Task A8: Backfill skips failed chunks instead of aborting the run

**Files:**
- Modify: `core/src/explorer/ingest.rs` — `run_range` pipeline loop at ~:1140-1233 (read `run_range` fully from its doc comment at :1015 before editing), plus the range-outcome struct it fills (`outcome.blocks_processed`, `outcome.ops` — find its definition in ingest.rs)

**Problem:** ingest.rs:1216 (`Some(Err(e)) => return Err(e)`) aborts a multi-hour backfill on the first failed chunk fetch. The fetch path already rides `retry_call` with backoff (`rpc/client.rs`), so an error here means even retries failed — the right move is to skip that chunk, log loudly, and keep going; the range is idempotent and gap-resumable, so a re-run repairs skipped blocks.

- [x] **Step 1: Implement skip-tracking** — after reading `run_range`:

  1. Add a `skipped_blocks: Vec<u64>` field to the range-outcome struct (public struct in ingest.rs; update its constructor sites and any CLI consumer — grep `blocks_processed` to find them; the CLI progress printer may want a line for skipped blocks).
  2. Make each in-flight fetch carry its block list so failures are attributable. Find where fetch futures are spawned (`spawn_one` at ~:1140) and change the spawned future's `Ok`/`Err` value to `(Vec<u64>, Result<…>)` — i.e. move the chunk's block numbers out of the closure capture into the returned pair (or, simpler if the closure shape fights this: keep a `HashMap<u64 /*first block*/, Vec<u64>> chunk_of` map alongside `inflight`).
  3. In the pipeline loop, replace:

```rust
                Some(Err(e)) => return Err(e),
```

with:

```rust
                Some((chunk_blocks, Err(e))) => {
                    warn!(
                        blocks = ?chunk_blocks,
                        error = %e,
                        "backfill chunk fetch failed after retries; skipping blocks \
                         (re-run the same range later to fill the gap)"
                    );
                    skipped.extend(chunk_blocks.iter().copied());
                }
```

  4. In the persist loop head (`while next_persist < missing.len()`), before the `ready.remove(&need)` check:

```rust
            if skipped.contains(&need) {
                next_persist += 1;
                continue;
            }
```

  5. Record `outcome.skipped_blocks = skipped;` before `store.set_sync_state(...)` at :1227. Also check ingest.rs:1048 (`unwrap_or(to_block)`) — per the review, on head-fetch failure it must not clamp to `to_block` past the confirmation lag; change it to `bail!` on head-fetch failure (the head fetch failing is not transient-skippable — nothing can be indexed without a safe tip).

- [x] **Step 2: Verify** — `cargo test -p mev-scout-core ingest && cargo test -p mev-scout-cli && cargo clippy --workspace --all-targets`
Expected: PASS.

- [x] **Step 3: Commit**

```bash
git add core/src/explorer/ingest.rs cli/
git commit -m "fix(explorer): backfill skips failed chunks and reports the gap instead of aborting"
```

---

### Task A9: Flag unpriced flash-loan fees instead of treating them as $0

**Files:**
- Modify: `core/src/explorer/store/facts.rs:338-341` (read `insert_block_facts` region :300-380 first)
- Modify: `core/src/explorer/store/query.rs` `ReportQuality` counting (find where `NATIVE_UNPRICED`-style pricing reasons are counted — read `facts.rs:236-307` and the `ReportQuality` definition in `store/mod.rs`)
- Test: extend the store tests pattern used in Task A2's file or `core/tests/explorer_golden.rs`

**Problem:** When a flash-loan fee token is unpriced, `flashloan_fee_usd` is `None` and net is computed as `g - gg - 0.0` — the fee silently becomes $0, overstating net with no pricing-reason flag.

- [x] **Step 1: Read** `core/src/explorer/store/facts.rs:236-380` to see how `NATIVE_UNPRICED`/`MULTI_ASSET_PRICING` details flags and the `ReportQuality` counters work.
- [x] **Step 2: Implement** — where the flash fee is priced (facts.rs:338-341): when the fee is non-zero in token terms but `usd` lookup fails, append `"FLASH_FEE_UNPRICED"` to the event's pricing-reason details (same mechanism as `NATIVE_UNPRICED`), and add an `ops_flash_fee_unpriced` counter to `ReportQuality` surfaced in `report.rs`'s pricing-reasons warning line (report.rs:326-334 — add `| {n} flash-fee-unpriced` when non-zero). Net math stays as-is (still excludes the unknown fee) but it is now visible and countable. Test: build a persisted op with an unpriced fee token via the golden-test store pattern and assert the detail flag + quality counter.
>
> **Cross-refs (2026-10-25 review).** Land **A14 before A9**: today a `0.0` quote is accepted as a valid price (`pricing.rs:151-152` has no `> 0.0` guard), so an "unpriced fee" can already be misreported as a priced-at-zero one, and this task's flag would then be describing the wrong condition. And coordinate with **A13**, which changes the same `facts.rs:338-341` region for the opposite reason (the fee counted *twice* when it was already netted into the ledger): A13 decides how many times the fee is subtracted, A9 decides what to say when it cannot be priced at all. Do them in that order so the second one builds on settled arithmetic.
- [x] **Step 3: Verify** — `cargo test -p mev-scout-core && cargo test -p mev-scout-cli`
- [x] **Step 4: Commit** — `git commit -m "fix(explorer): flag unpriced flash-loan fees in pricing reasons instead of netting them as zero"`

---

### Task A10: Persist lossy sandwiches with a flag instead of dropping forensics

**Files:**
- Modify: `core/src/explorer/store/facts.rs:347-353` (lossy sandwich gate)
- Modify: `core/src/explorer/store/query.rs` (net aggregations must exclude flagged rows)
- Test: `core/tests/explorer_golden.rs` (extend) or store unit test

**Problem:** The persist-time gate drops provably unprofitable sandwiches entirely (facts.rs:347-353). Once dropped they are unrecoverable forensics — the report can never show how many sandwiches lost money.

- [x] **Step 1: Read** `facts.rs:340-360` (the gate) and how `mev_ops.net_profit_usd` / details are stored and aggregated in `query.rs`.
- [x] **Step 2: Implement** — instead of skipping the insert, insert the row with `details['loss_gate'] = 'dropped_from_net'` (or a dedicated integer column `excluded_from_net INTEGER NOT NULL DEFAULT 0` if the schema is being touched in Task B6 anyway — prefer the column, it makes the SQL filter trivial and indexable):
  - Schema: add the column (Task B6's migration machinery lands the DDL; if B6 has not run yet, use `ensure_column` per the existing pattern in `schema.rs`).
  - `insert_block_facts`: set the flag instead of `continue`.
  - Every net/gross aggregation in `query.rs` (`report_window_overview`, `report_by_kind`, `report_daily`, leaderboards, `top_ops`): add `WHERE excluded_from_net = 0` to net sums; keep op *counts* inclusive so totals still reflect reality. For `top_ops`, keep excluded rows out of "top by net" ordering.
  - Report: show a `loss-gated ops: N` line in `render_window` (report.rs:300-312 area).
- [x] **Step 3: Test** — golden-style: persist a sandwich with negative net, assert the row exists with the flag and that overview net excludes it while op count includes it.
- [x] **Step 4: Verify + commit**

```bash
cargo test -p mev-scout-core --test explorer_golden --test explorer_corpus && cargo test -p mev-scout-cli
git commit -m "fix(explorer): persist loss-gated sandwiches with excluded_from_net flag; report shows the count"
```

[live half: canonical commands]

---

### Task A11: JIT mint→burn pairing must be ordered and one-shot

**Files:**
- Modify: `core/src/explorer/classify/jit.rs:54-61` (read the pairing loop first)
- Test: `core/src/explorer/classify/jit.rs` (new `#[cfg(test)]` tests)

**Problem:** The doc claims "Mint before Burn" but the pairing never checks `burn_tx > mint.tx_index`, and `find` lets one Burn satisfy multiple identical Mints (duplicate JIT events).

- [x] **Step 1: Read** `classify/jit.rs:30-80` and its existing tests for the fixture pattern.
- [x] **Step 2: Write failing tests** — two cases: (1) Burn in tx0, Mint in tx1 (same pool/owner/ticks) must NOT pair; (2) two identical Mints (tx1, tx2) and one Burn (tx3) must produce exactly one JIT event, not two. Build with `test_fixtures` builders per the existing jit tests.
- [x] **Step 3: Fix** — in the pairing: require `burn_tx_index > mint.tx_index`, and consume burns (e.g. collect matched burn tx indices into a `HashSet<u64>` and skip already-consumed burns).
- [x] **Step 4: Verify + commit** — `cargo test -p mev-scout-core jit && cargo test -p mev-scout-core --test explorer_golden --test explorer_corpus` (goldens may shift if any seeded block had duplicate pairing — inspect diffs), then `git commit -m "fix(explorer): JIT mint→burn pairing requires ordering and one-shot burn consumption"`. [live half: canonical commands]

---

### Task A12: Small classifier correctness fixes (batch)

**Files:**
- Modify: `core/src/explorer/classify/mod.rs:443-448` (`has_unresolved_tokens`)
- Modify: `core/src/explorer/classify/liquidation.rs:168` (and :239 area if same pattern)
- Modify: `core/src/explorer/classify/skim.rs:75-76`
- Modify: `core/src/explorer/classify/causal.rs:226-229` (frontrun gas over window)

- [x] **Step 1: `has_unresolved_tokens`** (mod.rs:443) — it checks sentinels only on `token_in`, diverging from `decode::sentinels::is_unresolved_token` (checks both). Replace the body with a delegation: `s.token_in ... || s.token_out ...` matching `is_unresolved_token`'s both-sides check (read `core/src/explorer/decode/sentinels.rs` — 11 lines — and call or mirror it).
- [x] **Step 2: Liquidation `profit_token: Some(Address::ZERO)`** (liquidation.rs:168, Compound V3 Absorb / Morpho path) — `Some(Address::ZERO)` forces downstream pricing to special-case zero. Change to `None` (use `.filter(|a| !a.is_zero())` on the collateral asset) and verify the persist path tolerates `profit_token = NULL` (read `facts.rs` where `profit_token` is read; it already handles `Option`).
- [x] **Step 3: Skim dead fallback** (skim.rs:75-76) — `.unwrap_or((Address::ZERO, U256::ZERO))` is unreachable (`profit_tokens.is_empty()` returns earlier at :66-68). Remove the `unwrap_or`; use `expect`-free pattern: restructure to `if let Some((tok, amt)) = profit_tokens.first() { … } else { continue; }` or keep the early-return and index directly.
- [x] **Step 4: Frontrun gas consistency** (causal.rs:226-229) — the frontrun P&L sums transfers across `front..=close` but charges gas for the front tx only, while sandwich (sandwich.rs:99-117) charges both legs. Read both regions, then sum `tx_gas_cost_wei` (from Task B1 if already landed; otherwise the local expression) over every tx in `front..=close`.
- [x] **Step 5: Tests** — each fix gets a targeted unit test next to its classifier's existing tests where feasible; at minimum the golden/corpus gates must pass: `cargo test -p mev-scout-core --test explorer_golden --test explorer_corpus --test explorer_tag_coverage`. [live half: canonical commands]
- [x] **Step 6: Commit** — `git commit -m "fix(explorer): classifier batch — two-sided sentinel check, no zero profit_token, skim dead code, frontrun window gas"`

---

---

### Task A13: Do not subtract the flash-loan fee twice  *(added 2026-10-25 review)*

**Files:**
- Modify: `core/src/explorer/store/facts.rs:338-341`
- Modify: `core/src/explorer/classify/arb.rs:52-82` (the flash-loan netting block)
- Modify: `core/src/explorer/types.rs` (event carries the netting signal)
- Test: `core/src/explorer/store/mod.rs` (persist-level, next to the existing gas-subtraction tests)

**Interfaces:**
- Produces: the persisted `net` for an event whose ledger already absorbed the fee equals `profit_usd - gas_usd` (fee counted once); `flashloan_fee_usd` stays populated for display.

**Problem:** Two verified code paths both remove the same fee. `net_flash_loan` at `profit.rs:294-296` does `*e = e.saturating_add(borrowed.saturating_add(fee))` into `neg` — so the ledger residual the classifier reports is *already* fee-netted — and then `store/facts.rs:339` computes `Some(g - gg - flashloan_fee_usd.unwrap_or(0.0))`. Fires on the `!repaid` branch (`arb.rs:70`), i.e. exactly the Aave/UniV3-callback arbs where the repay is not a visible `Transfer`. Second defect in the same block: `arb.rs:72-76` nets `borrowed + fee` against **both** `borrower` and `tx.to` when they differ, so a principal held by the receiver contract can be stripped twice. Note A4 does not cover this: A4 only widens repay *detection*; when repay is genuinely absent the double subtraction survives.

- [x] **Step 1: Write the failing test** — a persist-level test in `store/mod.rs`: build an event with `profit_usd` derived from a ledger that was flash-netted (assert the arithmetic directly: given a ledger residual already reduced by the fee, the stored `net_profit_usd` must not subtract it again). Prefer expressing it as: same inputs, `fee_netted_in_ledger = true` ⇒ `net` is `fee` higher than today. Before the fix the test fails by exactly `flashloan_fee_usd`.
- [x] **Step 2: Implement the signal** — set a flag on the event at the point of netting (in `arb.rs`, alongside `flashloan_fee_wei`) rather than guessing downstream: e.g. `details["flash_fee_netted_in_ledger"] = true` plus a typed field if `MevEvent` already carries such markers. In `facts.rs:339`, use it: `let fee_for_net = if netted { 0.0 } else { flashloan_fee_usd.unwrap_or(0.0) };` and keep writing `flashloan_fee_usd` into the row so reports still show the fee.
- [x] **Step 3: Decide the double-net** — read `arb.rs:72-76` and pick one owner for the borrow (sender vs receiver contract) instead of both. If both must stay, justify it in the commit body and add a regression test for a two-holder fixture. Do not change this silently.
- [x] **Step 4: Gates** — `cargo test -p mev-scout-core && cargo test -p mev-scout-core --test explorer_golden --test explorer_corpus`. Expected diff direction: flash-loan ops' `net_profit_usd` **rises** by one fee; sandwich gates near zero may flip from dropped to kept. [live half: canonical commands]
- [x] **Step 5: Commit** — `git commit -m "fix(explorer): count the flash-loan fee once (ledger netting already removes it at persist time)"`

---

### Task A14: Treat a zero USD quote as "no price", not as a valid $0 quote  *(added 2026-10-25 review; must land before A9 and A10)*

**Files:**
- Modify: `core/src/explorer/pricing.rs:84-93` (`amount_usd_realized` cache path) and `:151-153` (`finite_quote`)
- Modify: `core/src/explorer/store/jit_positions.rs:37-49` (`put_price` — refuse to persist a non-positive quote)
- Modify: `core/src/explorer/ingest.rs:760-775` (Llama results pushed to cache without validation)
- Test: `core/src/explorer/pricing.rs` `#[cfg(test)]` block

**Problem:** The two pricing paths disagree on what a valid quote is. The route-leg path requires a positive price — `consider_leg` at `pricing.rs:158` rejects `leg_usd <= 0.0` — but the cached-price path at `pricing.rs:90-92` returns `finite_quote(usd, false)`, and `finite_quote` (`:151-152`) only checks `is_finite()`. A cached `0.0` is therefore a *successful* price. It gets there silently: `ingest.rs:772` writes whatever Llama returns via `put_price` (`jit_positions.rs:44-47`, `INSERT OR REPLACE` keyed by `(hour, token)`), so one bad response poisons the whole hour bucket. Downstream, a zero-priced token contributes `0` to the multi-residual sum **without** raising the "pricing incomplete" flag, so a partial total is stored as if it were complete — and for a sandwich that fabricated partial can make `net <= 0`, at which point `facts.rs:347-353` drops the row with no reject record and no log. **Sequencing:** A10 persists loss-gated ops with a flag. If A10 lands before A14, A10 faithfully stores losses that were manufactured by a zero quote. Land A14 first.

- [x] **Step 1: Write the failing test** — in `pricing.rs` tests: `token_prices` contains `(token → TokenUsd { usd: 0.0, decimals: 18 })`, non-zero `amount`, and a `details` route with one trusted priced leg. Today `amount_usd_realized` returns `Some(0.0)`; after the fix it must fall through to the route path and return the route-derived quote (or `None` if no usable leg exists), and the caller must see the "no external price" branch.
- [x] **Step 2: Fix both ends** — (a) guard the cache path: only trust a cached price when it is finite **and** `> 0.0`; (b) stop persisting junk: `put_price` returns `Ok(())` without writing when `!usd.is_finite() || usd <= 0.0`, and the caller emits a `warn!` naming the token and hour. Keep `finite_quote` semantics for the route path unchanged (it already excludes non-positive legs) so only the two wrong behaviors move.
- [x] **Step 3: Gates** — `cargo test -p mev-scout-core && cargo test -p mev-scout-core --test explorer_golden --test explorer_corpus`. Expected diff direction: ops previously priced at a fake `$0` either become `profit_usd = NULL` (visible, countable) or pick up the realized-route price. Net totals move in either direction — that is the point — so enumerate the changed rows in the commit body.
- [x] **Step 4: Commit** — `git commit -m "fix(explorer): a 0.0 USD price is not a quote; reject it at cache write and price-resolution time"`

---

### Task A15: Kind-collision census — measure before B8 hard-codes a precedence  *(added 2026-10-25 review; blocks B8)*

**Files:**
- Create: `core/tests/explorer_kind_census.rs` (query-only, runs against an already-indexed DB path from config)
- Modify: `docs/explorer_strategy_checklist.md` (new "Kind collision census" section holding the result table)

**Problem:** `kind_precedence` (`classify/mod.rs:297-309`) assigns `ArbAtomic = 3` and `Liquidation = 6`, `Jit = 5`, `Skim = 7`. It exists today only as a sort key (`mod.rs:287`) plus four ordering asserts (`mod.rs:2150-2154`) — it has never decided which event survives, so its relative ranking of Liquidation/Jit/Skim has no evidence behind it. B8 promotes it into the authority for tx_index exclusivity. Verified current behavior: the empty guard at `arb.rs:35-39` means a liquidation tx whose liquidator also closes a swap cycle emits **both** `Liquidation` and `ArbAtomic`; the `retain` at `mod.rs:276-279` only matches `ArbAtomic | Unknown`, so `Liquidation`/`Jit`/`Skim` co-labeling is unrestrained. Applying today's ordering as a rule would make an *inferred* arb win over an *explicit* `LiquidateBorrow` event and delete the liquidation label — and every `P0.*`/`polygon.*`/`bsc.*` row in the checklist is a Liquidation case.

- [x] **Step 1: Write the census** — one read-only query over the explorer DB(s) already on disk:

```sql
SELECT chain_hint, kind_a, kind_b, count(*) AS pairs
FROM (
  SELECT block_number, tx_index,
         MIN(kind) AS kind_a,
         CASE WHEN count(DISTINCT kind) > 1 THEN MAX(kind) END AS kind_b,
         count(DISTINCT kind) AS nk
  FROM mev_ops GROUP BY block_number, tx_index
)
... -- expand to ordered pairs per tx_index, group by the unordered kind pair
```

  (The exact SQL is yours to write — the contract is: emit one row per **unordered kind pair** that co-occurs on the same `(block_number, tx_index)`, with counts, per chain. Add a second query for the same-tx *P&L* impact: sum `gross_profit_usd` of the lower-precedence member of each pair, so the cost of each precedence choice is in USD, not in row counts.)
- [x] **Step 2: Record reality** — paste the table into `docs/explorer_strategy_checklist.md` under a new heading, with the window(s) and DB path it was computed from. If the census is empty on every chain, that is a legitimate, valuable result: it means B8 is a no-op on today's corpus and the precedence question can be answered on principle. If it is not empty, the pairs with real USD at stake must be settled explicitly before B8.
- [x] **Step 3: Decide the order (do not defer to B8)** — annotate the checklist table with a decision per non-trivial pair, e.g. "`Liquidation` vs `ArbAtomic`: liquidation wins because it is anchored on an explicit `LiquidateBorrow` event; the arb P&L is attributed to the liquidation event's `details`" — and only then edit `kind_precedence`. Keep the numbers in `kind_precedence` as the single source, with a comment per contested pair citing this census.
- [x] **Step 4: Make it a permanent guard** — add a `#[test]` asserting the census query itself runs and that no `(block_number, tx_index)` in a *post-B8* database still carries two P&L-bearing kinds. This is the fitness function for B8's invariant. **Skip honestly per A0's convention:** the guard needs an indexed DB, so mark it `#[ignore = "needs an indexed explorer DB — set MEV_SCOUT_E2E=1"]` and `bail!` when the env var is set but no DB resolves — otherwise this guard becomes the next silent green.
- [x] **Step 5: Commit** — `git commit -m "test(explorer): kind-collision census on real indexed data; precedence decisions recorded before the claims registry"`

---

### Task A16: Price native-denominated profit instead of storing NULL  *(added 2026-10-25 review)*

**Files:**
- Modify: `core/src/explorer/store/facts.rs:236-255` (the multi-residual USD sum) and `:325-335` (where `native_price` is already in scope for gas)
- Test: `core/src/explorer/store/mod.rs` (near the existing `unpriced → NULL` test)

**Problem:** `facts.rs:240-243` hits the `NATIVE_MARKER` token in `ev.profit_tokens`, sets `native_unpriced = true` and `continue`s — so a native-denominated residual contributes nothing to `profit_usd`. A native price is *right there*: the same function converts gas with `wei_to_usd(ev.gas_cost_wei, np)` at `facts.rs:329-330`. Result: the op persists with `profit_usd = NULL`, which every report aggregate silently omits (`SUM(profit_usd)` ignores NULL), so real ETH-denominated MEV disappears from totals rather than being flagged. Compounding, and to be documented as a known limit rather than fixed here: `profit.rs:73-76` only **debits** `tx.value` into the ledger — there is no credit path and no internal-trace decoding anywhere in the module (logs-only by design, per `explorer/mod.rs:2-3`), so native *received* (unwrap-to-beneficiary, builder/MEV-share kickbacks paid coinbase→searcher) is invisible, and an ETH-funded cycle keeps its gross token profit un-reduced.

- [x] **Step 1: Write the failing test** — an event whose `profit_tokens` is a single `NATIVE_MARKER` residual, with a native price available. Today: `profit_usd` is `NULL` and the pricing reason is `native_unpriced`. Expected after: `profit_usd = wei_to_usd(amount, native_price)` and no unpriced reason.
- [x] **Step 2: Implement** — inside the residual loop, when `*tok == NATIVE_MARKER`, use `native_price_usd` if present (`wei_to_usd`) and only fall back to `native_unpriced = true` when there genuinely is no native price. Do not double-count: verify `gas_usd` is subtracted once for these ops (it already is, at the `net` step) and that `profit_amount`/`profit_tokens` semantics stay as the classifier wrote them.
- [x] **Step 3: Document the credit gap** — extend the `explorer/mod.rs` module doc (or `profit.rs`'s doc) with two sentences stating that native inflows are not observable without traces, so a reader does not read a NULL/low native figure as "no profit". Do **not** add trace calls here — that is the deferred design.
- [x] **Step 4: Gates** — `cargo test -p mev-scout-core && cargo test -p mev-scout-core --test explorer_golden --test explorer_corpus`. Expected diff direction: native-denominated ops gain `profit_usd`, report totals can only rise or stay; `ReportQuality` unpriced counts fall. [live half: canonical commands]
- [x] **Step 5: Commit** — `git commit -m "fix(explorer): price native residuals with the in-scope native price instead of persisting NULL profit"`

---

### Task A17: `is_wrap_noise` must require the paired native flow it documents  *(added 2026-10-25 review)*

**Files:**
- Modify: `core/src/explorer/profit.rs:34-40` (`is_wrap_noise`) and its call site in `DeltaLedger::from_transfers` (`profit.rs:55-78`)
- Test: `core/src/explorer/profit.rs` `#[cfg(test)]` (the delta-netting tests already live there)

**Problem:** The doc comment at `profit.rs:34-38` says the filter applies to "a transfer from/to the wrapped-native contract **in the same tx as native-value movement**". The predicate at `:39` is only `t.token == wrapped_native && (t.from == Address::ZERO || t.to == Address::ZERO)`, and `from_transfers` applies it to **every** transfer for **every** address — so a plain `WETH deposit` mint is dropped unconditionally. Asymmetric suppression: a searcher that wraps and then spends loses the incoming credit but keeps the outgoing spend, producing a phantom inventory spend. That is precisely the signal the arb pass's deficit/inventory vetoes consume — grep `has_inventory_spend` and `has_material_deficit` in `classify/arb.rs` and read them before editing, because this fix changes what those vetoes see.

- [x] **Step 1: Write the failing test** — two cases: (a) wrap-then-spend (mint 1 WETH, one spend of 1 WETH, `tx.value == 0`): assert today's asymmetric netting (the address shows a net −1 WETH delta) and after the fix a net 0; (b) genuine mid-cycle wrap/unwrap with matching native movement: the noise filter must still apply, so the existing tests keep passing.
- [x] **Step 2: Implement the minimal honest version** — thread the tx's native value (already available at the `from_transfers` call, `(tx.from, tx.value)`) into the filter and suppress wrapper mints/burns **only** when a corresponding native flow exists to offset them; otherwise keep them as real deltas. If that turns out to break legitimate cycle-closing behavior, the smaller correct step is to pair mint↔burn per address and suppress only the matched amount — say which you did and why.
- [x] **Step 3: Gates** — `cargo test -p mev-scout-core && cargo test -p mev-scout-core --test explorer_golden --test explorer_corpus`. Expect classification *changes* (ops previously vetoed as "open inventory spend" may now classify, and vice versa). This is a correctness task despite living in `profit.rs`'s quiet corner — list the flipped ops in the commit body. [live half: canonical commands]
- [x] **Step 4: Commit** — `git commit -m "fix(explorer): wrap-noise suppression requires the paired native flow instead of dropping every wrapper mint/burn"`

---

### Task A18: Chain isolation — refuse a shared explorer DB (schema fix is B6's job)  *(added 2026-10-25 review)*

**Files:**
- Modify: `core/src/config/settings.rs` (validation in `load`/`load_or_default`, next to the existing merge logic)
- Modify: `mev-scout.example.toml` (document the constraint on `explorer.db_path`)
- Test: `core/src/config` tests (or a new `#[cfg(test)]` if none)

**Problem:** `effective_explorer_db_path` (`settings.rs:575-582`) returns `explorer.db_path` **verbatim** for every chain when it is non-empty, and `./cache/explorer-{chain}.sqlite` only when it is empty. The forensic tables carry no chain column at all — verified in `store/schema.rs`: `blocks` (`:30`), `txs` (`:40`), `transfers` (`:54`), `swaps` (`:67`), `mev_ops` (`:83`), `prices` (`:123`), `sync_state` (`:131`, keyed by `chain_id` — the one exception), `blocks_classified` (`:138`), `jit_open_positions` (`:147`), `oracle_answers` (`:161`), `reserve_rates` (`:169`) — while `opportunities` (`:180`), `rejected_candidates` (`:207`) and `paper_sessions` (`:231`) do carry `chain TEXT`. So with a shared file and two chains: chain A marking block N classified makes chain B skip block N forever (the skip lives in the `blocks_classified` check on the ingest path), and `unwind_from` (Task A7) deletes the other chain's rows. Note `hist.toml` already sets `db_path`, so this is not hypothetical for anyone who reuses that config.

- [x] **Step 1: Write the failing test** — load a config with `[explorer] db_path = "..."` and more than one enabled chain; assert `Config::load`/validation fails with an actionable message naming the offending key and the chains involved.
- [x] **Step 2: Implement the guard** — validate at config load: non-empty `explorer.db_path` **and** `chains.len() > 1` ⇒ hard error (not a warning; a silent cross-chain skip is worse than a failed start). Keep the single-chain case working so existing `hist.toml` usage is unaffected.
- [x] **Step 3: File the real fix in B6's scope, do not half-do it here** — adding `chain INTEGER NOT NULL DEFAULT <id>` to the eleven tables plus composite keys is a *migration*, and B6 is the task that builds migration machinery. B6 therefore owns it; A18 owns the immediate safety. Cross-reference both ways in the commit bodies.
- [x] **Step 4: Gates** — `cargo test -p mev-scout-core && cargo test -p mev-scout-cli && cargo test -p mev-scout-cli -- --nocapture` for the CLI error-path test if you add one.
- [x] **Step 5: Commit** — `git commit -m "fix(config): reject a shared explorer.db_path across multiple chains (fact tables are not chain-keyed)"`

---

### Task A19: `Confidence` must describe the evidence behind the number  *(added 2026-10-25 review)*

**Files:**
- Modify: `core/src/explorer/types.rs` (`Confidence`, plus a `pnl_basis` marker on `MevEvent`)
- Modify: `core/src/explorer/classify/sandwich.rs:95-97, 190-197`
- Modify: `core/src/explorer/classify/liquidation.rs:165-180` (and the sibling site near `:240` if it shares the pattern)
- Modify: `core/src/explorer/classify/arb.rs` (bundler/keeper/claim sites; see Step 2)
- Modify: `core/src/explorer/store/facts.rs` (persist `pnl_basis`) and `store/query.rs` / `cli/.../report.rs` (surface it)

**Problem:** `Confidence` is emitted per pass, not per evidence, and today's report lets `--confidence exact` (validated in `cli/src/cli.rs:120`) select numbers whose basis is a gross amount difference. Three verified sites: (1) `sandwich.rs:97` `let profit = back.amount_out.saturating_sub(front.amount_in);` — two pool-log amounts, never ledger-netted, no deficit/inventory veto — published with `confidence: Confidence::Exact` at `:195`, with `flashloan_fee_wei: None` right above it. (2) `liquidation.rs:169` `profit_amount: Some(liq.collateral_amount)` — gross seized collateral, not `collateral − debt_to_cover` — with `confidence: Confidence::Exact` at `:176`. (3) The ERC-4337 bundler path books the sender's gas cost as profit (grep `actual_gas_cost` in `arb.rs` and confirm the current shape before editing). Read the module promise at `explorer/mod.rs:16-17` ("Leftover profitable patterns → `unknown` (inferred)") — there is no residual pass, so `Unknown` never fires as documented; that is part of the same honesty problem.

- [x] **Step 1: Add the basis, do not just demote** — introduce `pnl_basis` on `MevEvent` (persisted): `LedgerResidual` (netted deltas − gas, deficit-vetoed), `GrossAmountDiff`, `SeizedCollateral`, `GasCostAsProfit`, `ForwardedResidual`. `Exact` then means specifically "ledger residual", which is what a consumer trusts it to mean. Demote `sandwich.rs:195` and `liquidation.rs:176` to `Inferred` **or** keep `Exact` only where a ledger residual exists — pick one policy, state it in the `Confidence` doc comment, and make the report print the basis column.
- [x] **Step 2: Reconcile-failure must demote** — `liquidation.rs:48-52` records a `TRANSFER_MISMATCH` reason into `details` JSON but the event still ships `Exact`. Any event with a recorded reconcile failure is `Inferred` by definition. Same rule for `jit.rs`: if `fees_estimated.confidence` says `"inferred"` while the event says `Exact` (read `jit.rs:190-230` and confirm), reconcile them to the weaker of the two.
- [x] **Step 3: Zero-profit exemptions** — check whether the unresolved/zero-profit filter exempts keeper/bundler/claim tags and thereby persists `Exact` events with `profit_amount = None` (read the filter around `classify/mod.rs:455-480` before changing it). If so, those become `Inferred` too, or gain an explicit "no P&L basis" reason.
- [x] **Step 4: Gates** — `cargo test -p mev-scout-core && cargo test -p mev-scout-core --test explorer_golden --test explorer_corpus --test explorer_tag_coverage`. Expect `exact` op counts to **drop** and `inferred` to rise. That is the intended outcome; the checklist's "ETH DB verified" rows must be re-read against the new basis, and `docs/explorer_strategy_checklist.md` updated in the same commit if any row's confidence claim changes. [live half: canonical commands]
- [x] **Step 5: Commit** — `git commit -m "fix(explorer): confidence reflects the P&L basis; gross-diff and unreconciled events are no longer Exact"`

---

### Task A20: Every silent drop in classify must emit a reject reason  *(added 2026-10-25 review)*

**Files:**
- Modify: `core/src/explorer/reject.rs` (new variants + retire the placeholder)
- Modify: `core/src/explorer/classify/causal.rs` (25 bare `continue;`), `skim.rs` (6), `legs.rs` (2), `sandwich.rs` (2), `liquidation.rs` (1), `jit.rs`, `mod.rs` (3)
- Modify: `core/src/explorer/store/facts.rs` (persist reject rows for every pass, not just arb)
- Test: one reason-emitting test per pass

**Problem:** Verified counts of `RejectedCandidate {` construction sites: `classify/arb.rs` = 2 (plus the reject-return plumbing that feeds it), and **0 in every other pass** — `causal.rs`, `jit.rs`, `legs.rs`, `liquidation.rs`, `sandwich.rs`, `skim.rs`, `mod.rs`. Bare `continue;` drop sites, by contrast: `causal.rs` 25, `arb.rs` 24, `skim.rs` 6, `mod.rs` 3, `legs.rs` 2, `sandwich.rs` 2, `liquidation.rs` 1. `reject.rs`'s classify_* variants (`UnresolvedCycle`, `MixedFlowOwner`, `NetZeroSearcher`, `MaterialDeficit`, `OpenInventory`, `BuilderTip`) were reverse-engineered from the one file that emits them, and `reject.rs:22-26` itself admits `StateStale` is "a taxonomy placeholder; not emitted by the runner". Most consequentially, `legs.rs`'s `tx_cycle_profit` returning `None` is the single gate that decides whether a Frontrun or Backrun exists, and it is invisible in the reject stream — which is exactly why the open question in `docs/explorer_strategy_checklist.md:3` ("0 frontrun/backrun/skim across 963 ETH and 5001 AVAX blocks; classifier not loosened") cannot be answered from data: you cannot distinguish "it did not happen" from "it was filtered". Task D3's CSV `classify_rejects` section will therefore print an honest-looking near-empty table.

- [x] **Step 1: Extend the taxonomy before emitting** — add reasons with names that carry the *decision*, e.g. `CausalWindowTooWide` (the `CAUSAL_WINDOW_TXS` gate at `legs.rs:42`), `CausalPriceMoveBelowFloor` (`CAUSAL_PRICE_MIN_PCT` at `legs.rs:57`), `CycleNotClosed`, `NoCounterpartyLeg`, `SkimPoolNotV2Like`, `JitPairingUnmatched`, `LiquidationNotReconciled`, `VictimSetEmpty`. Delete or document `StateStale` — an unemitted variant in an enum that a report iterates is a lie waiting to be counted.
- [x] **Step 2: Emit at the load-bearing gates, not everywhere.** Instrument in this order (stop when the "why is this kind empty" question is answerable): `legs.rs::tx_cycle_profit` (all three of its return-`None` reasons, with `detail` naming which), `causal.rs` window/price-move gates, `sandwich.rs` leg-walk rejections, `skim.rs` pool-set rejections, `jit.rs` mint→burn pairing misses, `liquidation.rs` reconcile failures.
- [x] **Step 3: Bound the volume** — a reject per non-qualifying tx per block is write amplification (a 400-tx block would emit ~400 rows). Rule: emit only for candidates that already had a plausible anchor (a victim set, a matched pair, a swap cycle ≥2 edges), and aggregate the rest into one row per block per reason with a count. Say which rule you used in the doc comment on `RejectedCandidate`.
- [x] **Step 4: Tests** — one per instrumented pass asserting the specific reason string appears for a near-miss fixture, plus one asserting the *absence* of a reason is a failure (i.e. no new silent `continue` may land in these files). Add the reject-count table to `docs/explorer_strategy_checklist.md` next to the census from A15 so the two answer the same question together.
- [x] **Step 5: Gates + commit** — `cargo test -p mev-scout-core && cargo test -p mev-scout-core --test explorer_golden --test explorer_corpus && cargo test -p mev-scout-cli`; `git commit -m "feat(explorer): reject reasons for causal/skim/jit/sandwich/liquidation drops — silent continue is no longer acceptable"` [live half: canonical commands]

---

### Task A21: Unwind from the true fork point, and stop reading a failed probe as "no reorg"  *(added 2026-10-25 review)*

**Files:**
- Modify: `core/src/explorer/ingest.rs:629-683` (`check_reorg`, `verify_indexed_tip`)
- Modify: `core/src/explorer/ingest.rs` (call sites around `:962-977`; and confirm whether the backfill path `run_range` performs any reorg verification at all)
- Modify: `core/src/explorer/store/sync.rs` (an `unwind_from` variant, or keep the signature and pass the discovered fork)
- Test: `core/src/explorer/store/sync.rs` (fork-point selection can be unit-tested with a seeded hash chain, no RPC needed)

**Problem (verified):** both reorg paths unwind **from the block they probed**, not from the fork. `check_reorg` (`:631`) compares the stored hash at `block` against chain (`:636-647`) and calls `store.unwind_from(block)` at `:649`; `verify_indexed_tip` (`:660`) does the same at `:674`. If the fork actually began at `X < block`, every row between `X` and `block` from the abandoned chain stays in the DB — and because `blocks_classified` still marks them classified, the indexer will never re-index them. The rows keep feeding reports, which is precisely the failure reorg handling exists to prevent. Two secondary findings in the same code: (1) `check_reorg` fetches a full block **and receipts** (`:639`, `get_block_and_receipts_batch`) only to read `block_data.hash` at `:646`, while `verify_indexed_tip` already uses the header-only `get_block_hash` (`:668`) — that is 32 heavy fetches per sweep where 32 hash calls would do; (2) on fetch failure both return `Ok(None)` (`:641-644`, `:678-681`), i.e. "provider unreachable" and "no reorg" are the same value.

- [x] **Step 1: Write the pin test** — seed a store with blocks `N-4..=N` and hashes `h0..h4`, then simulate a new canonical chain that diverged at `N-2`. Assert `unwind_from` is invoked with `N-2`, not with the probe block `N`. Test the hash-walk helper directly (pure function over `(stored_hash, live_hash)` results) so no RPC is involved.
- [x] **Step 2: Walk back to the fork** — when a probe mismatches, walk backwards while stored hash ≠ live hash (bounded: `const MAX_REORG_LOOKBACK_BLOCKS: u64 = <confirmations + sweep width>;`), then unwind from the first matching parent. Reuse the already-fetched hashes rather than re-fetching; the sweep in the live loop already touches 32 blocks, so the walk is cheap once the probe is header-only.
- [x] **Step 3: Make failure distinguishable from clean** — return an explicit tri-state (`NoReorg` / `Forked(u64)` / `ProbeFailed`) or `bail!` on probe failure in the live loop. A live indexer that cannot verify the tip should say so — and per A6's policy decision, a *reorg verification* failure is the one place failing hard is defensible, so coordinate the two: A6 leaves these `?`s alone, A21 decides what they return.
- [x] **Step 4: Switch `check_reorg` to header-only** (`rpc.get_block_hash`), keeping the receipts fetch only where data is actually needed. Note this in C9's measurement run so the two perf changes do not get conflated.
- [x] **Step 5: Check the backfill path** — grep `check_reorg|verify_indexed_tip` for call sites and determine whether `run_range` verifies reorgs at all while advancing `set_sync_state` at the end of a range. If it does not, decide and document: a backfill of historical ranges cannot reorg (depth >> reorg window) *except* at its live-adjacent head — so either check only the last `confirmations + margin` blocks of a range, or state the assumption in the `run_range` doc. Do not leave it implicit.
- [x] **Step 6: Gates + commit** — `cargo test -p mev-scout-core && cargo test -p mev-scout-core --test explorer_golden --test explorer_corpus`; `git commit -m "fix(explorer): reorg unwind walks back to the fork point; probe failure is not 'no reorg'"` [live half: canonical commands]

---

## Phase B — Refactors (see the phase-contract correction in Global Constraints: B1, B3, B8, B10 and B11 change emitted output)

### Task B1: One `tx_gas_cost_wei` helper for all 11 gas-cost sites

**Files:**
- Create: `core/src/explorer/classify/gas.rs`
- Modify: `core/src/explorer/classify/mod.rs` (add `mod gas;` + re-export for sibling modules)
- Modify (replace the 11 sites): `classify/arb.rs:315, 371, 424, 464`, `classify/sandwich.rs:114`, `classify/causal.rs:101, 228`, `classify/jit.rs:154`, `classify/liquidation.rs:173, 240`, `classify/skim.rs:110`

**Interfaces:**
- Produces:

```rust
use alloy::primitives::U256;
use super::TxInput;

/// Gas cost in wei for a tx: gas_used × effective gas price.
/// Rounds the f64 gwei price to the nearest wei before multiplying, so
/// fractional-gwei prices are not silently truncated.
pub(super) fn tx_gas_cost_wei(tx: &TxInput) -> U256 {
    let price_wei = (tx.effective_gas_price_gwei * 1e9).round() as u128;
    U256::from(tx.gas_used).saturating_mul(U256::from(price_wei))
}
```

**Problem:** The formula `U256::from(gas_used).saturating_mul(U256::from((tx.effective_gas_price_gwei * 1e9) as u128))` is copy-pasted 11×, and the bare `as u128` truncates fractional gwei (a 30.5 gwei price becomes 30 gwei — cost understated ~1.7%).

> **Reclassified (2026-10-25 review): this is a correctness task wearing a `refactor:` label.** `.round()` changes the gas cost — and therefore `net` and any sandwich-gate decision — on every fractional-gwei block. The helper extraction is the refactor; the rounding is a fix. Do them in one commit but split the message prefixes (`refactor(explorer): …; also fix gas truncation`) and run the re-baseline alongside Phase A, not after it. The 11 call sites are worth listing in the commit body so a reviewer can confirm none were missed (`grep -n "effective_gas_price_gwei \* 1e9" core/src`).

- [x] **Step 1: Create the helper** (code above) and wire `pub(super) mod gas;` into `classify/mod.rs`'s module list.
- [x] **Step 2: Replace all 11 sites** — each becomes `gas::tx_gas_cost_wei(tx)` (adjust the receiver name per site: `tx`, `t`, `b_tx`). Grep to confirm none remain: `grep -n "effective_gas_price_gwei \* 1e9" core/src` → expect only `classify/gas.rs` and the fixtures.
- [x] **Step 3: Unit test** in `gas.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::explorer::test_fixtures::builders::*;

    #[test]
    fn rounds_fractional_gwei_up() {
        let mut tx = test_tx(); // per test_fixtures builders; default 30.0 gwei
        tx.effective_gas_price_gwei = 30.5;
        // 30.5 gwei = 30_500_000_000 wei; × gas_used
        let gas_used = tx.gas_used as u128;
        assert_eq!(
            tx_gas_cost_wei(&tx),
            U256::from(gas_used).saturating_mul(U256::from(30_500_000_000u128))
        );
    }
}
```

- [x] **Step 4: Gates** — `cargo test -p mev-scout-core && cargo clippy --workspace --all-targets` (golden numbers may shift microscopically upward for fractional-gwei blocks — inspect diffs; they should be gas-only).
- [x] **Step 5: Commit** — `git commit -m "refactor(explorer): single tx_gas_cost_wei helper; round fractional gwei instead of truncating"`

---

### Task B2: Split the 460-line `classify_arbs` into four passes

**Files:**
- Modify: `core/src/explorer/classify/arb.rs` (read the whole file first)
- Modify: `core/src/explorer/classify/mod.rs:207` (call site)

**Interfaces:**
- Produces (all `pub(super)` in `arb.rs`, same reject/event shapes):

```rust
pub(super) fn classify_atomic_arbs(input: &BlockInput, arb_tag_opts: &ArbTagOpts<'_>) -> (Vec<MevEvent>, Vec<RejectedCandidate>);
pub(super) fn classify_keepers(input: &BlockInput) -> Vec<MevEvent>;
pub(super) fn classify_claim_and_sell(input: &BlockInput) -> Vec<MevEvent>;
pub(super) fn classify_bundlers(input: &BlockInput) -> Vec<MevEvent>;
```

- `classify_arbs` remains as a 10-line orchestrator calling the four in order and concatenating (keeps mod.rs:207 unchanged, or update mod.rs to call the four directly — prefer keeping `classify_arbs` as orchestrator for a smaller diff).

**Problem:** One function runs four unrelated passes (atomic arb w/ ledger machinery, keeper, claim-and-sell, ERC-4337 bundler) sharing one loop and one `events` vec.

- [x] **Step 1: Read `arb.rs` fully** (816 lines) and identify the four pass boundaries (they are the `// ──`-commented sections after the atomic-arb section ends).
- [x] **Step 2: Extract each pass** into its own function; move shared per-tx prerequisites (the `tx.success` / `tx.swaps.is_empty()` guards, ledger construction) into the atomic-arb pass only — check each other pass's actual data needs (keepers likely only need `tx.keepers`, claim-and-sell needs `buy_collaterals` + swaps, bundlers needs `user_ops`) and give each its own minimal guards. Delete the dead `liq_txs` parameter and the empty `if liq_txs.contains(...)` block at arb.rs:35-39 in the same change (empty body = the guard was never implemented; mod.rs:197-201's `liq_txs` construction then becomes dead too — remove it and its comment, noting in the commit that the intended guard was never implemented).
- [x] **Step 3: Tests** — existing arb tests must pass unchanged: `cargo test -p mev-scout-core arb && cargo test -p mev-scout-core --test explorer_golden --test explorer_corpus`. Behavior must be byte-identical (same events, same order); if order shifts, preserve the original concatenation order in the orchestrator. [live half: canonical commands]
- [x] **Step 4: Commit** — `git commit -m "refactor(explorer): split classify_arbs into atomic-arb/keeper/claim-sell/bundler passes"`

---

### Task B3: Extract `searcher_candidates`; sandwich reuses `pool_legs`

**Files:**
- Modify: `core/src/explorer/classify/legs.rs` (add the helper; read `legs.rs:107-158` first)
- Modify: `core/src/explorer/classify/arb.rs:84-102` (use the helper)
- Modify: `core/src/explorer/classify/sandwich.rs:23-45` (reuse `pool_legs`; read `legs.rs:107-135` and `sandwich.rs` fully first)

**Interfaces:**
- Produces in `legs.rs`:

```rust
/// Searcher-attribution candidates for a tx: EOA sender, its receiver-side
/// contract, plus any address with ≥2 swap participations via transfers.
/// Order-stable (sender first) for deterministic downstream matching.
pub(super) fn searcher_candidates(tx: &TxInput) -> Vec<Address> {
    let mut candidates: Vec<Address> = vec![tx.from];
    if let Some(to) = tx.to {
        candidates.push(to);
    }
    let mut participation: std::collections::HashMap<Address, usize> = Default::default();
    for t in &tx.transfers {
        *participation.entry(t.from).or_insert(0) += 1;
        *participation.entry(t.to).or_insert(0) += 1;
    }
    let qualified: Vec<Address> = participation
        .into_iter()
        .filter(|(addr, count)| *count >= 2 && !candidates.contains(addr))
        .map(|(addr, _)| addr)
        .collect();
    candidates.extend(qualified);
    candidates
}
```

**Problem:** (1) arb.rs:91-102 and legs.rs:146-158 duplicate the participation-count candidate scan (both with the O(n²) `candidates.contains` — acceptable at tx scale, keep it in one place). (2) sandwich.rs:23-45 re-implements `pool_legs` *without* the defensive sort, so its correctness silently depends on `input.txs` insertion order.

> **Reclassified (2026-10-25 review).** Part (2) is a latent-bug fix, not a refactor: if any golden shifts, the shift *is* the finding (a sandwich whose legs were assembled in insertion order rather than sorted order). Predict it in the commit body rather than treating it as a failed behavior-preservation check. Also worth doing here while you are in the file: `searcher_candidates` is the right home for the `>= 2` participation rule, but note there are currently **two copies of that rule with different receivers** — `arb.rs:99` and `legs.rs:155` — so unify both into the helper and state in the doc comment whether `>= 2` is intended to be chain-configurable (it is the knob Task D12 exists for).

- [x] **Step 1: Add `searcher_candidates` to legs.rs** (code above), replace both duplicated scans with calls.
- [x] **Step 2: Sandwich walk** — read `sandwich.rs` fully; replace its local leg-walk with a call to `pool_legs` (legs.rs:107-135). Verify the sort does not change any golden output; if a golden shifts, investigate — the shift means the walk was order-dependent (a real latent bug the sort fixes).
- [x] **Step 3: Gates** — `cargo test -p mev-scout-core && cargo test -p mev-scout-core --test explorer_golden --test explorer_corpus` [live half: canonical commands]
- [x] **Step 4: Commit** — `git commit -m "refactor(explorer): shared searcher_candidates; sandwich reuses sorted pool_legs walk"`

---

### Task B4: Topic0 dispatch table for log decoding

**Files:**
- Modify: `core/src/explorer/classify/mod.rs:352-435` (`decode_tx_logs_with_aliases` — read fully first)
- Modify: `core/src/explorer/decode/swap.rs:75-321` (`decode_swap` — read fully first)

**Problem:** `decode_tx_logs_with_aliases` calls 15 decoders sequentially per log (each re-comparing topic0), and `decode_swap` is a 250-line topic0 if-chain. A dispatch table makes adding venues a one-line change.

- [x] **Step 1: Read both regions fully**, plus `decode/mod.rs` (49 lines) for the decoder signatures.
- [x] **Step 2: Refactor `decode_tx_logs_with_aliases`** into a `match log.topics()[0]` dispatch (a `const`-style match is preferable to a `HashMap` here — compile-time exhaustiveness, zero allocation):

```rust
    match log.topics().first() {
        Some(&t) if t == decode::V2_SWAP_TOPIC0 => { /* stamp + push swap fact */ }
        Some(&t) if t == decode::V3_SWAP_TOPIC0 => { /* … */ }
        // … one arm per decoder, in the same order as today
        _ => {}
    }
```

Extract the repeated `tx_index/log_index` stamping into a small local closure `let mut stamp = |fact: &mut dyn …|` or per-arm helper so each arm is 1-3 lines. Keep topic0 constants where they live today (`decode/swap.rs` etc. — re-export via `decode/mod.rs` if private).
- [x] **Step 3: Refactor `decode_swap`** the same way: hoist the topic0 comparisons into the same match style (the function itself can stay; just replace the if-chain with match arms and per-AMM handlers).
- [x] **Step 4: Gates** — decode tests are the safety net: `cargo test -p mev-scout-core decode && cargo test -p mev-scout-core --test explorer_golden --test explorer_corpus`. Output must be identical. [live half: canonical commands]
- [x] **Step 5: Commit** — `git commit -m "refactor(explorer): topic0 match dispatch for log decoding and swap decoders"`

---

### Task B5: Extract P&L annotation out of `insert_block_facts`

**Files:**
- Modify: `core/src/explorer/store/pnl.rs` (add pure annotation fns; read the file — 96 lines — first)
- Modify: `core/src/explorer/store/facts.rs:125-436` (read fully first; slim to SQL only)

**Interfaces:**
- Produces in `pnl.rs`:

```rust
/// Everything `insert_block_facts` needs to persist one event, with all
/// business logic (PnL annotation, sandwich gating from Task A10, volume
/// pricing, flash-fee conversion) already applied. Pure: no `Connection`.
pub struct PersistedOp { /* mirrors the mev_ops INSERT column list */ }
pub fn annotate_event(/* event + tx row + price inputs */) -> PersistedOp;
```

**Problem:** `insert_block_facts` (facts.rs:125) is 310 lines mixing business logic into the SQL layer — untestable without a database.

- [x] **Step 1: Read facts.rs fully** (445 lines) and `pnl.rs`.
- [x] **Step 2: Move each logic block** (PnL annotation at :236-307, sandwich gate at :347-353 → now the A10 flag, volume pricing at :358-371, flash-fee conversion at :338-341) into pure functions in `pnl.rs` taking owned/borrowed data and returning the values the INSERT needs. `insert_block_facts` becomes: build `Vec<PersistedOp>`, then run the inserts.
- [x] **Step 3: Unit-test the pure fns** — the existing pricing-edge tests (per the review, tests cover the cross-decimal blowup) move/become pure tests without a store: `cargo test -p mev-scout-core pnl`.
- [x] **Step 4: Gates + commit** — `cargo test -p mev-scout-core && cargo test -p mev-scout-core --test explorer_golden`; `git commit -m "refactor(explorer): pure PnL/annotation in pnl.rs; facts.rs is SQL only"`

---

### Task B6: `PRAGMA user_version` migrations

**Files:**
- Modify: `core/src/explorer/store/schema.rs` (read fully first)

**Interfaces:**
- Produces:

```rust
/// Ordered migrations; `Vn` applied when `user_version < n`. Never edit a
/// landed migration — append a new one.
const MIGRATIONS: &[&str] = &[
    /* V1 */ SCHEMA_V1, // the current full CREATE TABLE set, verbatim
    /* V2 */ "ALTER TABLE mev_ops ADD COLUMN excluded_from_net INTEGER NOT NULL DEFAULT 0;", // if A10 used ensure_column instead
    // …future migrations append here
];

pub(super) fn migrate(conn: &rusqlite::Connection) -> anyhow::Result<()> {
    let mut v: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap_or(0);
    for (i, sql) in MIGRATIONS.iter().enumerate() {
        let target = (i + 1) as i64;
        if v < target {
            conn.execute_batch(sql)?;
            conn.pragma_update(None, "user_version", target)?;
            v = target;
        }
    }
    Ok(())
}
```

**Problem:** Schema evolution today is `CREATE IF NOT EXISTS` + ad-hoc `ensure_column` + a hand-rolled table rebuild for the `kind` CHECK (schema.rs:27-335) — no version tracking, bespoke code per change.

> **Queue of columns this plan creates — B6 owns all of them (2026-10-25 review).** By the time B6 lands, four earlier tasks each want a new column or flag: A10 (`excluded_from_net`), A14 (nothing — but it changes what may be `NULL`), A16 (`native_unpriced` reason semantics), A19 (`pnl_basis`), and A18's option-2 (the `chain` column on the eleven unchained tables). Route **every** one of them through `MIGRATIONS` and delete the corresponding `ensure_column` calls, otherwise B6 ships a versioned system that coexists with the unversioned one and the next reader cannot tell which is authoritative. Test that an existing DB file on disk (a copy of `cache/explorer-*.sqlite`, or a fixture built with the V1 DDL) migrates to the head version and that a second open is a no-op.
>
> **Pragmas belong here too.** Verified at `store/schema.rs:15`: the open path sets only `PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;` — no `busy_timeout` and no `foreign_keys`. Two consequences: (1) with WAL plus a `BEGIN IMMEDIATE` writer (`store/facts.rs:141`), a concurrent live indexer and backfill on the same file get `SQLITE_BUSY` and abort the block; add `PRAGMA busy_timeout=<ms>` (name it, e.g. `5000`) in `open`. (2) There is not one `FOREIGN KEY` clause in the whole schema and `foreign_keys` is never enabled, so orphan `txs`/`swaps`/`mev_ops` rows under a missing `blocks` row are undetectable. Enabling FKs on an existing DB with orphans will fail the migrate — so make that an explicit, optional, *later* migration with a pre-flight orphan count printed, not a silent break. Also fix the `open_in_memory` path (`schema.rs:21-26`): it skips the WAL batch entirely, meaning tests exercise a different pragma set than production.

> **One clarification useful for A2's test:** `ExplorerStore::open_in_memory()` does exist (`schema.rs:21`), so the A2 snippet's `ExplorerStore::open_in_memory().expect("open store")` is valid — the hedging note in A2 Step 1 can be dropped once you confirm it (note the workspace denies `expect_used`; `clippy.toml` re-allows it under tests, so keep such calls inside `#[cfg(test)]`).

- [x] **Step 1: Read schema.rs fully.** Identify the existing open path (where the CREATE set + `ensure_column` calls run).
- [x] **Step 2: Introduce `migrate`** — snapshot today's DDL as `SCHEMA_V1` (existing DBs already match it; set their `user_version` to 1 inside `migrate` when it is 0 but tables exist — detect with `SELECT count(*) FROM sqlite_master WHERE type='table' AND name='blocks_classified'`). Replace the ad-hoc `ensure_column` calls: each existing ensure becomes migration V2+ (idempotent `ALTER TABLE … ADD COLUMN` guarded by the version check; SQLite has no `IF NOT EXISTS` for columns, so keep a `column_exists` helper for the V1→V2 legacy-DB path inside the migration itself).
- [x] **Step 3: Test** — open a store on a fresh temp file, assert `user_version == MIGRATIONS.len()`; if any test fixture in the repo has a pre-existing DB file, verify it migrates: `cargo test -p mev-scout-core && cargo test -p mev-scout-core --test explorer_golden`.
- [x] **Step 4: Commit** — `git commit -m "refactor(explorer): PRAGMA user_version ordered schema migrations"`

  *Progress: V1–V6 are the CREATE set, `flashloan_fee_usd`, `volume_usd`, `excluded_from_net`, the skim CHECK rebuild, and the rejected-candidate natural key. Both open paths set `busy_timeout=5000`. `foreign_keys` stays off (deferred). **Not in this migration:** A18's chain column / composite keys on the fact tables. The config guard is still the live safety; a column nothing reads would not isolate two chains, and the store API has no chain id to write. That rebuild is the next schema migration.*

---

### Task B7: rusqlite `Transaction` API + one leaderboard query builder

**Files:**
- Modify: `core/src/explorer/store/facts.rs:141, 425` (hand-rolled `BEGIN IMMEDIATE`/`COMMIT` strings → `unchecked_transaction()` per the Task A7 pattern — read both sites first)
- Modify: `core/src/explorer/store/mod.rs:326` + `store/query.rs:94-155` (three near-identical leaderboard fns)
- Modify: `core/src/explorer/results.rs:25-57` (wrap `persist_opportunities_to_explorer` inserts in one transaction)

**Problem:** String-managed transactions have no panic safety; the three leaderboard fns (`top_grouped`, `top_competitors_filtered`, `top_pools_filtered`) are copy-paste SQL that must stay in sync by hand; `persist_opportunities_to_explorer` autocommits per row.

> **Add these three autocommit loops to the same sweep (2026-10-25 review).** They are the write paths that are *not* inside the block transaction and each has a distinct failure shape: (1) the rejection loop on the ingest path (around `ingest.rs:528-532`) writes rejects before/outside `insert_block_facts`, so if the fact insert then fails, the rejects are already durable — and `rejected_candidates` has **no primary key and no unique constraint** (`schema.rs:205-224`), so a re-indexed block appends a second full set of reject rows. That directly corrupts the reject denominators A20 is being written to create, so order A20 *after* this task or give `rejected_candidates` a natural unique key here. (2) The JIT open/close writes (`ingest.rs:~599-619` → `store/jit_positions.rs:66-83`) are one statement per position — C4 batches them, so coordinate: B7 owns "inside a transaction", C4 owns "fewer statements". (3) The label loop runs *after* `COMMIT` and propagates `?` per call (`store/facts.rs:426-428`) — a label failure therefore returns `Err` for a block whose facts are already committed, which makes the caller believe the block is unindexed while `blocks_classified` says otherwise. Decide explicitly: labels are best-effort (warn + continue) or they belong inside the transaction. Pick one and write the reason in the doc comment.

- [x] **Step 1: Read** facts.rs transaction sites, mod.rs:300-360, query.rs:80-160, results.rs.
- [x] **Step 2: Transactions** — replace `execute("BEGIN IMMEDIATE")` / `execute("COMMIT")` pairs with `let tx = self.conn.unchecked_transaction()?; … tx.commit()?;` (A7 pattern).
- [x] **Step 3: Leaderboard builder** — collapse the three fns into one private helper:

```rust
fn top_by(
    &self,
    since: i64,
    group_col: &str,   // "competitor" | "sender" | "pool" — internal const, never user input
    limit: usize,
    kind: Option<MevKind>,
    confidence: Option<Confidence>,
) -> anyhow::Result<Vec<StatsRow>>
```

with `group_col` chosen by the public wrappers from a closed set (no string escapes user space after Task A5). Keep the three public fn signatures so CLI is untouched.
- [x] **Step 4: `results.rs`** — collect rows, insert inside one `unchecked_transaction`.
- [x] **Step 5: Gates + commit** — `cargo test -p mev-scout-core && cargo test -p mev-scout-cli && cargo clippy --workspace --all-targets`; `git commit -m "refactor(explorer): transactional persist paths and shared leaderboard query builder"`

---

### Task B8: Structural kind-claim registry (replaces three ad-hoc mechanisms)

**Files:**
- Create: `core/src/explorer/classify/claims.rs`
- Modify: `core/src/explorer/classify/mod.rs:211-289` (orchestrator), `classify/jit.rs` call site if needed

**Problem:** Tx-index exclusivity is enforced by three spread-out mechanisms: the JitArb upgrade (mod.rs:230-239), the `superseder` map (mod.rs:265-279, extended in A1), and `kind_precedence` ordering (mod.rs:297). The A1 bug happened *because* the invariant is comment-enforced, not structural.

> **Prerequisites and scope corrections (2026-10-25 review).**
> 1. **B8 depends on A15.** `TxClaims` turns `kind_precedence` into binding law, and that ordering has never been validated as an exclusion rule — as written it makes `ArbAtomic` (3) beat `Liquidation` (6), `Jit` (5) and `Skim` (7), i.e. an inferred cycle outranks an explicit `LiquidateBorrow`. Land A15 first and encode the *decided* order, with one comment per contested pair citing the census.
> 2. **B8 absorbs A1 — expect zero golden drift.** A1 already fixed the Sandwich/Arb hole. If B8 shifts any baseline, the shift is either the Jit/Skim/Liquidation collisions (which A15 told you exist) or a mistake. State which in the commit body. This is the one task where "identical output" *is* the right acceptance test, precisely because the decisions were taken upstream.
> 3. **Keep the JitArb upgrade path visible.** The load-bearing comment at `mod.rs:217-223` says the upgrade and its paired `retain` must both survive because `JitArbDetector` was removed from the execution path for lacking an honest P&L model. `TxClaims` must not silently re-order `Jit → JitArb → ArbAtomic` into a state where the arb wins; add a unit test that reproduces that exact three-way case, not just the two-kind cases in the snippet.
> 4. **Do the 13-literal cleanup in the same file sweep.** Thirteen hand-built `MevEvent { … }` literals across `classify/` (none shared) are why A19's `pnl_basis` and A20's reasons will each require touching every pass. Add one constructor/builder for the common shape in `types.rs` (or `classify/mod.rs`) as Step 0 of B8 so later tasks are one-line changes.
> 5. **The `Unknown` residual pass is still missing.** `explorer/mod.rs:16-17` promises "Leftover profitable patterns → `unknown` (inferred)" but every `MevKind::Unknown` site is `Exact` and no residual scan exists. Once `TxClaims` knows the winner per tx_index, the leftover set falls out for free — either implement the residual pass here or delete the promise from the module doc. Leaving documented-but-absent behavior is how the next reviewer re-files this as a bug.

**Interfaces:**
- Produces in `claims.rs`:

```rust
/// Claims a tx_index for a kind. Higher-precedence kinds (lower
/// `kind_precedence`) always win; a lower-precedence claim is a no-op.
/// Feeding all classifier passes through this makes "one tx_index, one
/// P&L-bearing event" structural.
pub(super) struct TxClaims { claimed: std::collections::HashMap<u64, MevKind> }
impl TxClaims {
    pub fn claim(&mut self, tx_index: u64, kind: MevKind) -> bool; // true if this claim won
    pub fn winner(&self, tx_index: u64) -> Option<MevKind>;
    pub fn is_claimed(&self, tx_index: u64) -> bool;
}
```

- [x] **Step 1: Write `claims.rs`** with the struct above (precedence via `crate::explorer::classify::kind_precedence`); unit tests: claim Sandwich then ArbAtomic on same tx → winner is Sandwich; reverse order → same result.
- [x] **Step 2: Rewire `classify_block`** — read mod.rs:180-290. Order of operations becomes: run all passes (they may over-claim); feed every produced event + every "suppressed" tx_index (JitArb's arb suppression, frontrun close-tx claims, A1's sandwich legs) through `TxClaims::claim`; then `events.retain(|e| claims.winner(e.tx_index) == Some(e.kind) || !claims.is_claimed(e.tx_index))`… — precisely: keep an event iff it is the winner for its tx_index. The `consumed`/`back_consumed` pre-filtering passes use for input pruning can stay (they are an optimization), but the *authority* is now `TxClaims`.
- [x] **Step 3: Port A1's test + the existing golden/corpus suites** — all must pass with identical output. This is the riskiest refactor in the plan; if output drifts, stop and reconcile against the **A15-decided** order, not the numbers currently in the `kind_precedence` body: `Sandwich > Frontrun > Backrun > JitArb > Liquidation > Jit > Skim > ArbAtomic > Unknown`. The decided order is recorded in the `kind_precedence` doc comment (`core/src/explorer/classify/mod.rs:338-357`) and in `docs/explorer_strategy_checklist.md` → "Kind collision census → Decided order per contested pair"; the body (`:358-369`) still holds the pre-decision numbering where `ArbAtomic`(3) outranks the three explicitly-anchored kinds, and demoting it is this task's job. Because the census measured **zero** co-labeling on today's corpus, that demotion must still land with **zero** golden drift — any drift means a new co-label appeared or the registry is wrong, and either way stop.
- [ ] **Step 4: Gates + commit** — full: `cargo test -p mev-scout-core && cargo test -p mev-scout-cli && cargo test -p mev-scout-core --test explorer_golden --test explorer_corpus --test explorer_tag_coverage && cargo clippy --workspace --all-targets`; `git commit -m "refactor(explorer): TxClaims registry makes tx_index exclusivity structural"` [live half: canonical commands]

---

### Task B9: Group `BlockInput` registries into `BlockContext`

**Files:**
- Modify: `core/src/explorer/classify/mod.rs:90-119` (struct), every constructor site (grep `BlockInput {`), `core/src/explorer/test_fixtures.rs`, `core/src/explorer/golden.rs:90-185`, `core/src/explorer/ingest.rs` (the builder that fills it)

**Problem:** `BlockInput` is a 20-field god-struct mixing per-block data with registries/config (`chainlink_feeds`, `savax`, `gmx_event_emitters`, `tip_sinks`, …), making fixture-building in tests verbose and the module boundary unclear.

- [ ] **Step 1: Read mod.rs:90-119** and list which fields are per-block (`block`, `ts`, `txs`, …) vs registry/config. Grep constructors: `grep -rn "BlockInput {" core/src core/tests`.
- [ ] **Step 2: Extract** the registry/config fields into `pub struct BlockContext<'a> { … }` (borrowed registries), add `pub ctx: &'a BlockContext` to `BlockInput`. Update all constructors. In tests, provide a `BlockContext::for_tests()` default in `test_fixtures.rs` so most tests don't spell out registries.
- [ ] **Step 3: Gates** — full suite + goldens: `cargo test -p mev-scout-core && cargo test -p mev-scout-core --test explorer_golden --test explorer_corpus && cargo clippy --workspace --all-targets`. Pure mechanical move — any output change is a mistake. [live half: canonical commands]
- [ ] **Step 4: Commit** — `git commit -m "refactor(explorer): BlockContext groups registry/config out of BlockInput"`

---

### Task B10: Make `mev_ops` idempotent and revisable  *(added 2026-10-25 review)*

**Files:**
- Modify: `core/src/explorer/store/schema.rs` (`mev_ops` keys — `:83-111`, and the rebuild path at `:294+`)
- Modify: `core/src/explorer/store/facts.rs:374` (the plain `INSERT INTO mev_ops`)
- Create: `core/src/explorer/store/reprice.rs` (or a method on `ExplorerStore`) — update-in-place for the pricing-derived columns
- Test: `core/src/explorer/store/mod.rs` + a migration test alongside B6's

**Problem:** Facts use `INSERT OR IGNORE` on natural keys, but `mev_ops` is a plain `INSERT` with `AUTOINCREMENT id` and **no unique constraint**; `canonical_id` is indexed but not unique (`schema.rs:111`), and the only `ON CONFLICT` in the whole module is `sync.rs:24` / `labels.rs:21`. So re-index safety rests entirely on the `blocks_classified` bookkeeping — if that row is ever missing or deleted selectively, re-indexing duplicates every event. Worse, a persisted op can never be *revised*: A14 (price validation), A16 (native pricing), A19 (`pnl_basis`) and the deferred historical-native-pricing work all want to change `profit_usd` / `net_profit_usd` / pricing-reason columns on rows that already exist, and today the only route is delete-and-reindex from the chain, which needs RPC.

- [x] **Step 1: Add the natural key** — `UNIQUE(canonical_id, tx_hash)` (confirm that pair is actually one event by reading how `explorer_canonical_id` is built in `canonical.rs`; if it needs the kind or the leg set, include those and say why). Land it as a B6 migration with a pre-flight duplicate report: on an existing DB, `SELECT canonical_id, tx_hash, count(*) … HAVING count(*) > 1` must be printed, and the migration must refuse to add the constraint while duplicates exist rather than silently picking a winner.
- [x] **Step 2: Switch to `ON CONFLICT DO UPDATE`** for the classification-derived columns, keeping `id` stable so nothing that references an op row by id is orphaned. Define which columns are *revisable* (pricing-derived, `excluded_from_net`, `pnl_basis`) and which are immutable facts (block, tx_index, kind, gas) — write that split into the doc comment, because A10/A14/A16/A19 all depend on it and none of them currently say.
- [x] **Step 3: A reprice entry point** — one function that recomputes USD columns for a block range from stored prices, with no RPC. This is what makes A14/A16/A19's deferred follow-up ("historical native pricing") a recompute instead of a re-index; note the dependency in the deferred section rather than duplicating the design here.
- [ ] **Step 4: Gates + commit** — `cargo test -p mev-scout-core && cargo test -p mev-scout-core --test explorer_golden`; `git commit -m "refactor(explorer): mev_ops keyed and upserted so re-index is idempotent and USD columns are revisable"`

---

### Task B11: `Unknown` is three known utility families — rename the kind, add `subkind`, make the docs honest  *(added 2026-10-25, third pass; rides B6's migration machinery, lands after A19, before D5)*

**Files:**
- Modify: `core/src/explorer/types.rs` (`MevKind::Unknown` variant, `as_str` at :44, `parse` at :58)
- Modify: `core/src/explorer/classify/arb.rs:362, 415, 455` (the three emission sites)
- Modify: `core/src/explorer/classify/mod.rs:16` (the "Leftover profitable patterns → `unknown`" doc promise), `:277` (superseder match), `:307` (`kind_precedence`), `:453-479` (`filter_unresolved`), test sites at `:953, :1001, :1888`
- Modify: `core/src/explorer/mod.rs:15-21` (the pipeline doc lists every pass except the three utility ones)
- Modify: `core/src/explorer/store/schema.rs:89-90, 300-301` (the `kind` CHECK — reuse the existing table-rebuild path as a B6 migration) and `store/facts.rs` (persist `subkind`)
- Modify: `core/src/explorer/scenarios.rs:686, 915`, `core/tests/explorer_corpus.rs` (`ALL_KINDS` + case kind expectations)
- Test: guard test next to `filter_unresolved`

**Interfaces:**
- Produces: `MevKind::Utility` with `as_str() = "utility"`; `mev_ops.subkind TEXT` (nullable) populated from the existing `details["tags"]` (`keeper` | `bundler` | `claim_and_sell`); a migration widening the CHECK (schema.rs:89-90) and rewriting existing rows `kind = 'unknown'` → `'utility'`. D5's `MevKind::ALL` and the CLI `--kind` list pick the renamed variant up automatically.

**Problem (verified):** the kind named `Unknown` contains no unclassified events. It is emitted by exactly three fingerprint passes — keeper executions (arb.rs:357-378), airdrop claim-and-sell (arb.rs:410-441), ERC-4337 bundler (arb.rs:450-465) — all `Confidence::Exact`, all already self-labelled in `details["tags"]`. `filter_unresolved` (classify/mod.rs:453-479) keeps only those three tag families, so no "misc residual" bucket exists at all. Meanwhile the classify module doc (classify/mod.rs:16) promises "Leftover profitable patterns → `unknown` (inferred)" — a residual pass that was never coded (A19 records the same honesty gap) — and the pipeline doc at `explorer/mod.rs:15-21` lists every pass except the three utility ones. Net effect: the report merges keeper fees, airdrop claims and 4337 gas payments into one meaningless row, the label answers "what exactly is in `unknown`?" with a falsehood ("three known things"), and a future residual pass would have no honest bucket left to use. The name is the bug.

- [x] **Step 1: Rename the variant.** `MevKind::Unknown` → `MevKind::Utility`, `as_str()` → `"utility"`, `parse` maps `"utility"`. Grep `MevKind::Unknown` across the workspace and update every site: the three emission sites (arb.rs:362, 415, 455), `kind_precedence` (classify/mod.rs:307), the `superseder` retain match (classify/mod.rs:277), `filter_unresolved` (classify/mod.rs:457) and its doc comment, scenario assertions (scenarios.rs:686, 915), classify test fixtures (classify/mod.rs:953, :1001, :1888), and the corpus `ALL_KINDS`/case expectations. If D5 has not landed, also update the hardcoded CLI kind list at `report.rs:149` now (D5 replaces it with `MevKind::ALL` later). Before dropping `"unknown"` from `parse`, grep the old string across `results/`, `scripts/` and the corpus for live consumers; keep a `parse("unknown") → Utility` alias only if one exists, and note the decision in the commit body. Acceptance: `grep -rn "MevKind::Unknown" core/ cli/` returns zero hits.
- [x] **Step 2: Migrate the schema (B6 machinery, appending after B10's migration and preserving its unique key).** The `mev_ops.kind` CHECK (schema.rs:89-90; rebuild copy at :300-301) is why the hand-rolled table rebuild exists — reuse that rebuild as migration `V<n>`: widen the CHECK to include `'utility'`, run `UPDATE mev_ops SET kind = 'utility' WHERE kind = 'unknown'`, and add `subkind TEXT` in the same migration. Grep `'unknown'` in SQL strings across `store/` for any other literal (label maps, report queries) and migrate them too.
- [x] **Step 3: Persist and surface `subkind`.** Extract the tag→subkind mapping (`keeper` | `bundler` | `claim_and_sell`) into one `fn` shared by `filter_unresolved` and `insert_block_facts` so the two cannot drift. Populate `subkind` on insert; surface it in `top_ops` output and JSON. `report_by_kind` stays grouped by kind — do not restructure the report here.
- [x] **Step 4: Make the docs honest.** (a) `classify/mod.rs:16`: replace the residual promise with reality — "`Utility` covers exact-fingerprint operational executions (keeper, airdrop claim-and-sell, ERC-4337 bundler); no residual-inference pass exists, and if one is ever added it gets its own kind, never this bucket." (b) `explorer/mod.rs:15-21`: add the three utility passes to the pipeline list and `Utility` to the priority list. Grep `docs/` and `README.md` for kind references to `unknown` and fix them in this commit or hand the list to D8 explicitly.
- [x] **Step 5: Guard against bucket-rot.** Unit test next to `filter_unresolved`: every emitted `Utility` event carries exactly one of the three subkind tags — a future pass that dumps an untagged event into the kind must fail this test instead of silently recreating the mystery bucket.
- [ ] **Step 6: Gates + commit** — `cargo test -p mev-scout-core && cargo test -p mev-scout-cli && cargo test -p mev-scout-core --test explorer_golden --test explorer_corpus`. Expected diff: kind labels on keeper/claim/bundler rows change to `utility`; P&L figures unchanged; `subkind` appears in JSON. Reconcile per the re-baseline discipline. `git commit -m "fix(explorer): rename Unknown to Utility with subkind — the bucket held three known families and the docs promised a residual pass that does not exist"` [live half: canonical commands]

---

## Phase C — Performance (behavior-preserving; note `warnings = "deny"` means an unused import left by a perf edit breaks the build)

### Task C1: `causal.rs` — kill the double `tx_cycle_profit` and the per-pair clone storm

**Files:**
- Modify: `core/src/explorer/classify/causal.rs:43-84` and `:290-348` (read the whole file first — 348 lines)

**Problem:** (1) `tx_cycle_profit(input, b_tx)` is computed twice per candidate (full ledger + cycle walk + clone each time). (2) `frontrun_cross_tx_profit` clones every transfer in the `front..=close` window for *each* (front, close-candidate) pair → O(candidates × window × transfers) clones per block.

- [x] **Step 1: Read causal.rs fully.**
- [x] **Step 2: Bind the first `tx_cycle_profit` result** into a variable and reuse it at the second call site (:43 vs :84 per the review).
- [x] **Step 3: Restructure `frontrun_cross_tx_profit`** — hoist the per-front window clone out of the close-candidate loop: build `Vec<&TransferFact>` (borrowed, no clone) for the window once per front tx, then for each close candidate extend a small owned ledger from the borrowed slice (`DeltaLedger` API permitting; if `DeltaLedger::from_transfers` requires owned values, build the ledger once per front and clone the *ledger* per candidate instead of the transfer vec — read `DeltaLedger` in arb.rs/legs.rs first and choose whichever avoids the per-pair transfer clone).
- [ ] **Step 4: Gates** — goldens are the correctness net: `cargo test -p mev-scout-core --test explorer_golden --test explorer_corpus`. Identical output required. Optionally measure: index one seeded range before/after (`explorer index` on a corpus seed) and note the delta in the commit body. [live half: canonical commands]
- [ ] **Step 5: Commit** — `git commit -m "perf(explorer): causal pass reuses cycle profit and borrows transfer windows"`

---

### Task C2: `attach_swap_tokens` — index transfers by counterparty

**Files:**
- Modify: `core/src/explorer/decode/attach.rs:42-186` (read fully first)

**Problem:** O(swaps × transfers × 3) with a pairing pass plus two ±24 proximity passes per swap.

- [x] **Step 1: Read attach.rs fully.**
- [x] **Step 2: Pre-index once per tx**:

```rust
let mut by_counterparty: std::collections::HashMap<Address, Vec<&TransferFact>> = Default::default();
for t in &tx.transfers {
    by_counterparty.entry(t.from).or_default().push(t);
    by_counterparty.entry(t.to).or_default().push(t);
}
```

then each swap's candidate set is `by_counterparty.get(&pool).into_iter().flatten()`. Name the proximity window: `const PROXIMITY_LOG_WINDOW: usize = 24; // ±24 logs: calibrated on corpus — see decode/attach.rs history` (pull the real calibration note from the file's comments if one exists).
- [x] **Step 3: Gates** — `cargo test -p mev-scout-core decode && cargo test -p mev-scout-core --test explorer_golden --test explorer_corpus` — identical output required. [live half: canonical commands]
- [ ] **Step 4: Commit** — `git commit -m "perf(explorer): attach_swap_tokens via counterparty index; named proximity window"`

---

### Task C3: Batched, prepared inserts in `insert_block_facts`

**Files:**
- Modify: `core/src/explorer/store/facts.rs:157-214` (read fully first)

**Problem:** Per-row `conn.execute` INSERTs re-prepare every statement.

- [ ] **Step 1:** In the (post-B5, SQL-only) `insert_block_facts`, prepare each INSERT once per call with `conn.prepare_cached(...)` and loop `stmt.insert(...)`/`stmt.execute(rusqlite::params![...])`. Same for the txs/swaps/transfers bulk inserts.
- [ ] **Step 2: Gates** — `cargo test -p mev-scout-core && cargo test -p mev-scout-core --test explorer_golden`. Optionally note before/after timing on a corpus seed range in the commit body.
- [ ] **Step 3: Commit** — `git commit -m "perf(explorer): prepare_cached batched inserts in insert_block_facts"`

---

### Task C4: Batch JIT position writes; prune only when dirty

**Files:**
- Modify: `core/src/explorer/ingest.rs:606-619` (`persist_jit_positions` — read the region first)

**Problem:** Positions are written one Mint at a time (`record_jit_open(&[one])`), one DELETE per Burn, and the prune runs on every block even with zero JIT facts.

- [ ] **Step 1: Read the region.** Collect all opens into one `Vec<OpenPosition>` and one `record_jit_open(&all)`; collect all closes and run them after; prune only when the block actually produced JIT facts (`if opens + closes > 0`).
- [ ] **Step 2: Gates** — `cargo test -p mev-scout-core && cargo test -p mev-scout-core --test explorer_golden --test explorer_corpus`. [live half: canonical commands]
- [ ] **Step 3: Commit** — `git commit -m "perf(explorer): batch JIT position persist; prune only on JIT-active blocks"`

---

### Task C5: `prior_oracle_answers` single query

**Files:**
- Modify: `core/src/explorer/store/facts.rs:40-55` (read first)

**Problem:** One query per feed (N+1).

- [ ] **Step 1:** Replace with a single groupwise-max query:

```sql
SELECT feed, block_number, answer FROM oracle_answers o
WHERE block_number <= ?latest AND feed IN (?, ?, …)
  AND block_number = (SELECT MAX(block_number) FROM oracle_answers
                      WHERE feed = o.feed AND block_number <= ?latest)
```

(With per-feed cutoffs it may need a per-feed `latest`; read the current loop to see whether the cutoff is uniform — if uniform, the query above with one bound `latest` works. If per-feed, use a CTE `WITH cutoffs(feed, latest) AS (VALUES …)`.)
- [ ] **Step 2: Gates** — `cargo test -p mev-scout-core explorer_golden` (oracle lookback is exercised by liquidation goldens).
- [ ] **Step 3: Commit** — `git commit -m "perf(explorer): prior_oracle_answers in one grouped query"`

---

### Task C6: Windowed `unclassified_blocks`

**Files:**
- Modify: `core/src/explorer/store/sync.rs:51-68`

**Problem:** The recursive CTE materializes the full range (hundreds of thousands of rows for a 30d backfill).

- [ ] **Step 1:** Replace with chunked anti-joins — iterate in windows of 4096 blocks:

```rust
    pub fn unclassified_blocks(&self, from: u64, to: u64) -> anyhow::Result<Vec<u64>> {
        const WINDOW: u64 = 4096;
        let mut out = Vec::new();
        let mut lo = from;
        while lo <= to {
            let hi = lo.saturating_add(WINDOW - 1).min(to);
            let mut stmt = self.conn.prepare(
                "WITH RECURSIVE seq(b) AS (
                   SELECT ?1 UNION ALL SELECT b+1 FROM seq WHERE b < ?2
                 )
                 SELECT b FROM seq
                 WHERE b NOT IN (SELECT block FROM blocks_classified
                                 WHERE block BETWEEN ?1 AND ?2)",
            )?;
            for row in stmt.query_map(rusqlite::params![lo as i64, hi as i64], |r| r.get::<_, i64>(0))? {
                out.push(row? as u64);
            }
            lo = hi + 1;
        }
        Ok(out)
    }
```

- [ ] **Step 2: Test** — extend the sync.rs test module from A7: mark a middle block classified, assert the gap list is exactly the others across a >WINDOW range.
- [ ] **Step 3: Gates + commit** — `cargo test -p mev-scout-core && cargo test -p mev-scout-cli`; `git commit -m "perf(explorer): unclassified_blocks scans in 4096-block windows"`

---

### Task C7: Batched price warm-up

**Files:**
- Modify: `core/src/explorer/ingest.rs:760, 728` (`warm_prices_for_tokens` — read the region first)

**Problem:** One `price_at` point query per token per block, plus a full `TokenCache` clone per call.

- [ ] **Step 1: Read the region and the `TokenCache` type.** Add a store method `prices_at(tokens: &[Address], hour: u64) -> anyhow::Result<HashMap<Address, Option<(f64, String)>>>` doing one query with `token IN (…)` grouped per token; call it once per block. Cache the `TokenCache` handle (it is behind a `OnceLock` per the review — borrow, don't clone).
- [ ] **Step 2: Gates** — `cargo test -p mev-scout-core && cargo test -p mev-scout-core --test explorer_golden`.
- [ ] **Step 3: Commit** — `git commit -m "perf(explorer): batched price warm-up; no per-call TokenCache clone"`

---

### Task C8: Keep SQLite off the async runtime threads

**Files:**
- Modify: `core/src/explorer/ingest.rs` (the `index_block`/`index_block_fetched` → store call sites), possibly `core/src/explorer/store/mod.rs` (expose a `Handle` that spawns blocking)

**Problem:** `ExplorerStore` holds one blocking `rusqlite::Connection` invoked inline inside async tasks (store/mod.rs:94-96) — every insert stalls a tokio worker.

- [ ] **Step 1: Read** the store call sites in ingest.rs and decide the smallest safe change: wrap the per-block persist call in `tokio::task::spawn_blocking` with the store moved/borrowed via an `Arc` (check whether `ExplorerStore` is already behind an `Arc` at call sites). Do NOT introduce a connection pool — single-writer SQLite gains nothing from it; the goal is only to unblock the runtime.
- [ ] **Step 2: Document the single-writer constraint** in the `ExplorerStore` doc comment (store/mod.rs).
- [ ] **Step 3: Gates** — full: `cargo test -p mev-scout-core && cargo test -p mev-scout-cli && cargo test -p mev-scout-core --test explorer_golden --test explorer_corpus`. Watch for ordering regressions: persists must remain in ascending block order (the pipeline depends on it per the run_range doc at ingest.rs:1020-1022) — spawn_blocking must await in order, not race. [live half: canonical commands]
- [ ] **Step 4: Commit** — `git commit -m "perf(explorer): persist via spawn_blocking; document single-writer store"`

---

### Task C9: Kill the per-block clone storm and the O(n²) price lookup in the persist path  *(added 2026-10-25 review)*

**Files:**
- Modify: `core/src/explorer/ingest.rs:495-520` (per-block registry clones), `:795-815` (`usd_of`), `:715-735` (token-cache clone per call), `:631-675` (reorg probe)
- Modify: `core/src/explorer/store/facts.rs:355-371` (`volume_usd` re-scan)
- Test: existing ingest/golden tests; output must be byte-identical

**Problem (all verified):** (1) `ingest.rs:501-515` clones registry-sized sets into every block's inputs — `cfg.profit_token_priority.clone()`, `pools.v2_like.clone()`, `cfg.chainlink_feeds.clone()`, `pools.epoch_venue.clone()`, `cfg.gmx_event_emitters.clone()`, and `cfg.tip_sink_addresses.clone()` plus a mutation at `:515` — for a backfill that is one full copy of the pool/registry universe *per block*. (2) `usd_of` at `ingest.rs:802` is `priced.iter().find(|(p, _)| p == a)` called from inside a per-token loop (`:806`) — quadratic in the block's priced-token count. (3) `check_reorg` (`ingest.rs:631`) is on the live path; `get_block_hash` already exists (`:668`, used by `verify_indexed_tip` at `:660`) — read `check_reorg` and, if it fetches a full block *and* receipts to compare one hash, switch it to the hash-only call. (4) `store/facts.rs:355-371` re-scans `swaps` per event to build `volume_usd`.

- [ ] **Step 1: Measure first** — before changing anything, capture a baseline on a seeded range: time `explorer index --from-block … --to-block …` on a corpus window and record blocks/sec plus peak RSS in the commit body. Several C-tasks claim wins with no number attached; this one should not.
- [ ] **Step 2: `Arc` the immutable universes.** Wrap the registry/pool/fee-set collections that are read-only per run in `Arc` (built once in the range/live driver, cloned by reference into each block's inputs). The one that is *mutated* per block (`:515`, the tip-sink set) needs a real answer, not a clone: either keep a base `Arc` plus a small per-block `Vec` of additions, or move the union into the classifier where it is consumed. Do not hide a mutation behind an `Arc` clone and call it a perf win.
- [ ] **Step 3: Index `priced` once.** Build a `HashMap<Address, f64>` before the loop at `:806` and make `usd_of` a map lookup. Same treatment for any other `.iter().find(...)` inside a per-token or per-event loop you pass while editing (`grep -n "iter().find" core/src/explorer/ingest.rs`).
- [ ] **Step 4: Settle the reorg probe and the volume scan** — apply (3) and (4) above only if the reads confirm the shape; if `check_reorg` is already hash-only, say so in the commit body and skip. Do not invent the change to fit the plan.
- [ ] **Step 5: Gates** — `cargo test -p mev-scout-core && cargo test -p mev-scout-core --test explorer_golden --test explorer_corpus`; then re-run Step 1's range and report the delta. Output must be identical — this task is honestly behavior-preserving. [live half: canonical commands]
- [ ] **Step 6: Commit** — `git commit -m "perf(explorer): Arc per-block registry sets, hash-indexed price lookup, cheaper reorg probe"`

---

### Task C10: Indexes that match the report predicates, and deterministic report ordering  *(added 2026-10-25 review)*

**Files:**
- Modify: `core/src/explorer/store/schema.rs` (new `CREATE INDEX IF NOT EXISTS` lines + the migration path from B6)
- Modify: `core/src/explorer/store/query.rs` (`ORDER BY` tiebreaks; `details_json LIKE` → real columns)
- Test: `core/src/explorer/store/query.rs` unit test asserting stable ordering across repeated runs

**Problem (verified against the schema and the query file):** the index set does not match what the report queries actually filter on.
- `mev_ops` has `mev_ops_block`, `mev_ops_sender`, `mev_ops_kind_ts(kind, ts)`, `mev_ops_canonical` (`schema.rs:108-111`). Every report window uses `WHERE ts >= {since_ts}` (`query.rs:108`, `:435`, `:513`, `:541`, `:569`) — with `kind` leading and often absent from the predicate, `mev_ops_kind_ts` is unusable for those, so each report is a full `mev_ops` scan, three windows deep. There is **no index on `ts` alone**.
- `query.rs:163` looks ops up by `tx_hash` and there is no `mev_ops(tx_hash)` index (`schema.rs:108-111` has none).
- `opportunities` is indexed `(chain, block_number)` and `(canonical_id)` (`schema.rs:200-203`), but run-scoped reads filter by `run_id` — unindexed.
- `prices` has no index of its own; `price_at`/`put_price` (`jit_positions.rs:11-34`, `:44-47`) look up by `(token, hour)`, while `native_price_near` (`:53-63`) adds `lower(token)` — a function on the column, which no plain index can serve, on top of the `ORDER BY ABS(hour - ?)` the A2 task fixes.
- `query.rs:431-434` counts pricing-quality flags with four `LIKE '%…%'` scans over `details_json` — unindexable by construction, and it silently breaks if the JSON serialization of a flag ever changes shape.
- Ordering: `query.rs:110` and `:145` order by `gross_usd DESC`, `:515` by `gross DESC`, `:543` by `label DESC` — no `id` tiebreak anywhere in those, so ties reshuffle between runs (`:570` is the one that does it right: `net_profit_usd DESC, id DESC`). `:218` orders by `block_number, tx_index` where `tx_index` is nullable and has no tiebreak, and `ops_in_range` has no `LIMIT`/cursor (a caller does `ops_in_range(0, u64::MAX)`).

- [ ] **Step 1: Add the indexes** (as B6 migrations, not raw `CREATE`s, so existing DBs get them): `mev_ops(ts)`, `mev_ops(tx_hash)`, `opportunities(run_id)`, `prices(token, hour)`. For `native_price_near`, either store a normalized `token_lc` column or key the native row at a single canonical spelling and drop `lower(token)` from the predicate — pick one and note it, because leaving `lower()` there makes `prices(token, hour)` useless for that query.
- [ ] **Step 2: Promote the pricing flags out of JSON.** Add integer columns for `usd_approximate`, `native_unpriced`, `multi_asset_pricing`, `pricing_clamped` (these are exactly the flags A9, A14 and A16 write) and populate them on insert; keep `details_json` for humans but stop computing reports from substring matching. Gate: the `ReportQuality` numbers must be unchanged on an existing corpus before you switch the reads.
- [ ] **Step 3: Deterministic ordering.** Append `, id DESC` (or `tx_hash` where `id` is not selected) to every `ORDER BY` used by a report or leaderboard, and add a repeated-run test asserting identical row order. Reports that reshuffle between two identical runs cannot be diffed, which quietly defeats the golden-diff discipline in this plan's Global Constraints.
- [ ] **Step 4: Optional, only if measured** — a covering index for the window aggregate (`mev_ops(ts, kind, gross_profit_usd, net_profit_usd)`) if `EXPLAIN QUERY PLAN` still shows a scan after Step 1. Run `EXPLAIN QUERY PLAN` on the five report queries and paste the before/after into the commit body; that output is the evidence this task worked.
- [ ] **Step 5: Gates + commit** — `cargo test -p mev-scout-core && cargo test -p mev-scout-cli && cargo test -p mev-scout-core --test explorer_golden`; `git commit -m "perf(explorer): indexes aligned to report predicates; report ordering made deterministic"`

---

## Phase D — CLI, tests, docs, polish

### Task D1: `--top` applies to every leaderboard; order-preserving window dedup

**Files:**
- Modify: `cli/src/commands/explorer/report.rs:175, 189-190` and `:157-171`
- Test: `cli` unit tests (find the existing arg/report test module — grep `mod tests` in `cli/src`)

- [ ] **Step 1:** report.rs:189-190 — pass `top` instead of the hardcoded `10` to `top_senders_filtered` and `top_pools_filtered`.
- [ ] **Step 2:** report.rs:171 — `window_list.dedup()` only removes *consecutive* duplicates. Replace with order-preserving seen-set dedup:

```rust
    let mut seen = std::collections::HashSet::new();
    window_list.retain(|w| seen.insert(w.clone()));
```

- [ ] **Step 3: Test** — assert `["1d", "7d", "1d"]` input yields a two-window report and that `--top 3` caps searchers/pools lists (extract the window-parsing into a testable `fn parse_windows(windows: &[String]) -> anyhow::Result<Vec<String>>` if it is not already a fn).
- [ ] **Step 4: Gates + commit** — `cargo test -p mev-scout-cli && cargo clippy --workspace --all-targets`; `git commit -m "fix(explorer): --top caps all leaderboards; dedup windows order-preserving"`

---

### Task D2: Generic window parsing (`Nd`/`Nh`/ISO date)

**Files:**
- Modify: `cli/src/commands/explorer/report.rs:36-44, 157-171, 183` (`expected_blocks`, validation, `since_ts` use)
- Modify: `cli/src/commands/explorer.rs:17-29` (`since_ts` — remove the now-unreachable unknown arm and the duplicated `86_400` constants)
- Modify: `cli/src/cli.rs:103` (flag help text for `--windows`)
- Test: cli unit tests
- Docs: `docs/CLI.md` §3.6 (explorer report), `README.md:107`

- [ ] **Step 1:** Introduce a parsed window type:

```rust
/// A report window: either a rolling duration (`1d`, `12h`, `30d`) or the
/// all-time span.
#[derive(Debug, Clone, PartialEq)]
enum ReportWindow {
    Rolling { label: String, secs: u64 },
    AllTime,
}

fn parse_window(s: &str) -> anyhow::Result<ReportWindow> {
    if s == "all" {
        return Ok(ReportWindow::AllTime);
    }
    let secs = parse_duration_secs(s)?; // accepts `Nh`/`Nd` (and `Nw`); reject 0
    Ok(ReportWindow::Rolling { label: s.to_string(), secs })
}
```

Write `parse_duration_secs` (suffix match `h|d|w`, `u64` value, `secs = v * unit`) with unit tests including rejects (`"0d"`, `"1x"`, `"d"`).
- [ ] **Step 2:** `expected_blocks` derives from `secs` directly (`Some(secs / block_time_secs(chain) as u64)`), deleting the duplicated `86_400` literals. Validation at report.rs:165 becomes `parse_window(p)?`. The stored `window` label stays the raw string so JSON keys are unchanged for `1d/7d/30d/all` (back-compat for any consumer).
- [ ] **Step 3:** Update `cli/src/commands/explorer.rs` `since_ts` to take the parsed window (removing its unreachable unknown-value arm per the review) and the help text + `docs/CLI.md` + `README.md` to document `--windows 1d,12h,7d,all`.
- [ ] **Step 4: Gates + commit** — `cargo test -p mev-scout-cli && cargo test -p mev-scout-core && cargo clippy --workspace --all-targets`; `git commit -m "feat(explorer): generic --windows durations (Nh/Nd/Nw) with derived coverage"`

---

### Task D3: CSV output carries every section

**Files:**
- Modify: `cli/src/commands/explorer/report.rs:239-247`
- Test: cli unit test rendering a `WindowReport` fixture to CSV

- [ ] **Step 1:** Extend the CSV arm to emit all sections with a `section` column, keeping the existing header shape for the overview/by-kind rows (consumers may parse them):

```rust
        OutputFormat::Csv => {
            println!("window,section,label,ops,volume_usd,gross_usd,gas_usd,flash_fee_usd,net_usd");
            for r in &reports {
                print_csv_row(&r.window, "overview", "total", &row_overview_fields(&r.overview));
                for k in &r.by_kind {
                    print_csv_row(&r.window, "by_kind", &k.label, &row_fields(k));
                }
                for d in &r.daily {
                    print_csv_row(&r.window, "daily", &d.label, &row_fields(d));
                }
                for s in &r.top_competitors {
                    print_csv_row(&r.window, "top_competitors", &s.label, &stats_fields(s));
                }
                for s in &r.top_senders {
                    print_csv_row(&r.window, "top_senders", &s.label, &stats_fields(s));
                }
                for s in &r.top_pools {
                    print_csv_row(&r.window, "top_pools", &s.label, &stats_fields(s));
                }
                for op in &r.top_ops {
                    print_csv_row(&r.window, "top_ops", &op.tx_hash, &op_fields(op));
                }
                for c in &r.classify_rejects {
                    print_csv_row(&r.window, "classify_rejects", &c.reason, &vec![c.count.to_string()]);
                }
            }
        }
```

Add the small `stats_fields`/`op_fields` helpers mirroring `row_fields` (fields must match the JSON names semantically: ops, gross, net for stats; volume, gross, gas, net for ops — pad missing columns with empty strings so the row width matches the header).
- [ ] **Step 2: Test** — build a `WindowReport` with one row per section, render, assert the CSV lines.
- [ ] **Step 3: Gates + commit** — `cargo test -p mev-scout-cli`; `git commit -m "feat(explorer): CSV report emits daily/leaderboards/top-ops/rejects sections"`

---

### Task D4: `test-util` feature; collapse the three fixture sets

**Files:**
- Modify: `core/Cargo.toml` (add `[features] test-util = []`)
- Modify: `core/src/explorer/mod.rs:32-46` (gate `golden`, `scenarios`, `test_fixtures`, and `scenario_targets` behind `#[cfg(any(test, feature = "test-util"))]`)
- Modify: `core/tests/explorer_golden.rs`, `core/tests/explorer_corpus.rs` (enable the feature for these tests via `core/Cargo.toml` `[[test]]` sections or move shared fixture use to the gated modules)
- Modify: `core/src/explorer/golden.rs:90-185` (reuse `test_fixtures` builders instead of re-implementing `swap/transfer/tx/block`)
- Docs: note the feature in `docs/ARCHITECTURE.md` if it documents the explorer test layout

**Problem:** `scenario_targets.rs` (hunt recipes, seed blocks, operator notes) compiles into the release library but only integration tests consume it; fixture builders exist three times (golden.rs, test_fixtures.rs, explorer_golden.rs hand-mapping).

- [ ] **Step 1:** Add the feature and re-gate the four modules. Verify release build excludes them: `cargo build -p mev-scout-core --release` then `cargo tree`/size sanity; run `cargo test -p mev-scout-core --all-features` to confirm integration tests still compile (they need `test-util` — add `required-features` on the `[[test]]` targets or enable via dev-dependencies self-feature: in `core/Cargo.toml` add `[dev-dependencies] mev-scout-core = { path = ".", features = ["test-util"] }` — check the crate can self-dev-depend; if not, use `[[test]] required-features = ["test-util"]` and run with `--features test-util`).
- [ ] **Step 2:** Point `golden.rs`'s builders at `test_fixtures` (parameterize the block number/timestamp that differ today — golden's `block()` uses 42_000 per the review — via small wrappers, do not silently change golden block numbers).
- [ ] **Step 3:** In `core/tests/explorer_golden.rs:263-304`, replace the hand-mapped `TxInput → TxRow/SwapRow/TransferRow` mapping with a `#[cfg(feature = "test-util")] pub fn rows_from_indexed_block(...)` exposed from `ingest.rs` (the same mapping it does at ingest.rs:342-380) so a schema change fails the golden test instead of silently diverging.
- [ ] **Step 4: Gates + commit** — `cargo test -p mev-scout-core --features test-util && cargo test -p mev-scout-cli && cargo clippy --workspace --all-targets`; `git commit -m "test(explorer): test-util feature gates test modules; single fixture source"`

---

### Task D5: `MevKind::ALL` — non-tautological kind-coverage test

**Files:**
- Modify: `core/src/explorer/types.rs` (add `ALL` to `MevKind`)
- Modify: `core/tests/explorer_corpus.rs:782-792, 970-990`

- [ ] **Step 1:** In types.rs, add to `impl MevKind`:

```rust
    /// Every emittable kind, in canonical order. Single source of truth for
    /// coverage tests and CLI error messages.
    pub const ALL: &[MevKind] = &[
        MevKind::Sandwich, MevKind::Frontrun, MevKind::Backrun, MevKind::ArbAtomic,
        MevKind::JitArb, MevKind::Jit, MevKind::Liquidation, MevKind::Skim, MevKind::Utility,
    ];
```

(Read `types.rs`'s `MevKind` first and list the actual variants — use the real set. B11 renames `Unknown` to `Utility` before this lands: include the renamed variant — it *is* reported, so it stays in `ALL`.)
- [ ] **Step 2:** In `explorer_corpus.rs`, change the coverage test to derive from the enum:

```rust
    let expected: Vec<&str> = MevKind::ALL.iter().map(|k| k.as_str()).collect();
    assert_eq!(ALL_KINDS, expected, "ALL_KINDS must mirror MevKind::ALL");
```

so a new variant fails the test until a corpus case covers it. Also fix the tautology at :970-990 per the review: the `every kind is reported` loop should assert against `MevKind::ALL`, not against `ALL_KINDS` compared to itself.
- [ ] **Step 3:** Use `MevKind::ALL` to build the `--kind` error message in `cli/src/commands/explorer/report.rs:149` (delete the hardcoded list).
- [ ] **Step 4: Gates + commit** — `cargo test -p mev-scout-core --test explorer_corpus && cargo test -p mev-scout-cli`; `git commit -m "test(explorer): MevKind::ALL drives kind-coverage and CLI error message"` [live half: canonical commands]

---

### Task D6: Corpus/catalogue cross-check covers every named block; fix aave doc drift

**Files:**
- Modify: `core/tests/explorer_corpus.rs:993-1017` (cross-check)
- Modify: `docs/explorer_strategy_checklist.md:11, 16`
- Modify: `core/src/explorer/scenario_targets.rs:61-66, 127` (hunt text)

- [ ] **Step 1:** Read the cross-check test and the `STRATEGY_SCENARIO_TARGETS` catalogue. Extend the cross-check from "Avalanche `Seeded` rows only" to: for every catalogue entry of status `Seeded`/`Verified` that names a chain + block, assert a `CorpusCase` exists covering that block (any window containing it). The known failure: `scenario_targets.rs:61-66` claims sandwich "ETH verified @ 26089508" but no case covers block 26089508. Resolution: either transcribe the 26089508 seed into a `CorpusCase` (follow the transcribe guidance documented in the corpus test header) or downgrade the catalogue row to `Hunt` and fix the checklist — pick based on whether the seed data exists in `results/`; if the block was never actually recorded as a case, fixing the *claim* is correct.
- [ ] **Step 2:** Fix the P0.1 doc drift: `docs/explorer_strategy_checklist.md:16` says "Aave V2 flash+liq … protocol=aave_v2" but the passing test expects `aave_v3` (`explorer_corpus.rs:660`, comment at :477-478), and `scenario_targets.rs:127` says "AVAX seed uses Aave V2 pool". Update both docs to `aave_v3`.
- [ ] **Step 3: Gates + commit** — `cargo test -p mev-scout-core --test explorer_corpus --test explorer_tag_coverage`; `git commit -m "test(explorer): catalogue↔corpus cross-check for all seeded blocks; fix aave_v2/v3 doc drift"` [live half: canonical commands]

---

### Task D7: Snapshot tests for report rendering

**Files:**
- Create: `cli/tests/explorer_report_render.rs` (or extend an existing `cli/tests` module — check what exists)
- Modify: `cli/src/commands/explorer/report.rs` (extract pure renderers)

- [ ] **Step 1:** Refactor `render_window` (report.rs:275-386) so the pure parts are testable: extract `fn window_report_rows(r: &WindowReport) -> Vec<Vec<String>>` (or render to a `String` instead of printing — change `render_window` to build a `String` and let the caller `println!` it).
- [ ] **Step 2:** Snapshot-test: construct a `WindowReport` fixture (hand-built rows — no store needed), render, and assert the full output string (coverage warning path <0.7, concentration warning >40%, rejects line, empty-section "(no data)" rows).
- [ ] **Step 3: Gates + commit** — `cargo test -p mev-scout-cli`; `git commit -m "test(explorer): snapshot tests for window report rendering"`

---

### Task D8: Documentation drift batch

**Files:**
- Modify: `core/src/explorer/scenario_targets.rs:4`, `core/src/explorer/scenarios.rs:2` (doc links)
- Modify: `core/src/explorer/interest_attr.rs:126-127` (leftover design-note comment)
- Modify: `core/src/explorer/store/query.rs:156, 82, 121` (doc comments referencing removed `stats`/`show` commands)
- Modify: `README.md:138` (BSC row), `README.md:107`, `docs/CLI.md:99` (missing `--confidence`)
- Modify: `core/src/explorer/ingest.rs:584-588` (misleading `events` field count — see also D10)

- [ ] **Step 1:** Module docs point to `docs/explorer_strategy_scenarios.md` and `docs/explorer_strategy_tracking_plan.md` which do not exist; point both at `docs/explorer_strategy_checklist.md` (verify what the checklist file actually covers first — read its headings).
- [ ] **Step 2:** `interest_attr.rs:126-127`: read the comment ("Wait - original returned false…"), confirm the code below it is the final behavior, delete the design-note lines.
- [ ] **Step 3:** `query.rs` docstrings referencing `show <TX>` / `stats --kind` (commands removed per `docs/CLI.md:42`): rewrite to describe current callers; check whether `ops_for_tx` still has any non-test caller (review says only `core/tests/mev_corpus.rs:442`) — if test-only, move it behind the `test-util` feature from D4 or mark the doc "test support".
- [ ] **Step 4:** `README.md:138` BSC row links `docs/LOCAL_BSC.md` + `mev-scout.local-bsc.toml`, both missing: ask the repo (git log) whether BSC support was removed — if yes, delete the row; if the files were never committed, delete the row (do not fabricate the missing files).
- [ ] **Step 5:** Document `--confidence` in `README.md:107` and `docs/CLI.md:99` explorer report flag lists.
- [ ] **Step 6: Gates + commit** — `cargo test -p mev-scout-core && cargo test -p mev-scout-cli` (docs in doc-comments compile); `git commit -m "docs(explorer): fix dead links, stale command references, and flag documentation"`

---

### Task D9: Dead code and cleanup batch

**Files:**
- Modify: `core/src/explorer/classify/causal.rs:169` (`*vt - *ftx == 0` dead check — verified dead because `skip(fi + 1)` guarantees `vt > ftx`; read and confirm, then delete)
- Modify: `core/src/explorer/classify/sandwich.rs:63-65` (`let Some(f) = front else { continue; }` unreachable — guarded by `!walk.victims.is_empty()`; read and confirm, then simplify)
- Modify: `cli/src/commands/explorer.rs:24-28` (`since_ts` unreachable unknown arm — superseded by D2 if landed; if D2 not landed, delete the arm per the review)
- Modify: `core/src/explorer/ingest.rs:91` (`Box::leak` per `IngestConfig::from_chain` call — replace the leaked `&'static str` label with a `String` field or a `OnceLock` per label; read the struct first and pick the smaller change)
- Modify: `core/src/explorer/classify/mod.rs:150-163` (oracle/reserve facts collected twice — collapse the Vec-of-refs then clone into one `flat_map(...).cloned().collect()` each)
- Modify: `core/src/explorer/decode/aggregator.rs:197-249` (name the magic tolerances: read the region, then `const AGG_EDGE_TOLERANCE_BPS_CLOSE: u64 = 200;` / `…_MID: u64 = 400;` with a one-line rationale copied from any calibration comment present, or "calibrated on KyberSplit regression corpus" if the history comment at arb.rs:111 is the origin)
- Modify: `core/src/explorer/classify/sandwich.rs:141-150` — **added 2026-10-25 review.** `let reverse_backrun = opposite_dir(front, back);` pushes `"REVERSE_DIRECTION"` into the reason list unconditionally when true. Check whether `fold_sandwich` is reachable only behind the *same* `opposite_dir` test earlier in the walk (grep `opposite_dir` in the file — if the guard is upstream, the flag is constant noise in every persisted sandwich's `details`, and the reason list is then misleading rather than informative). If confirmed constant, delete the flag; if reachable both ways, keep it and add a test pinning which case produces it. Apply the identical question to `contract_mediated` just below (`:121`) — verify it is not near-always-true for any contract call before trusting it as a signal.
- Modify: `core/src/explorer/classify/arb.rs:35-39` — if Task B2 has not landed yet, the empty `if liq_txs.contains(&tx.tx_index) { /* comment */ }` block belongs in this batch: it is verified dead (`liq_txs` is built at `mod.rs:197-201` and consumed nowhere else). Do not delete the *intent* with the code: record in `docs/explorer_strategy_checklist.md` (via A15/A20) that the Liquidation-vs-Arb guard was never implemented, so B8's precedence decision can see it was an open hole and not a design choice.

- [ ] **Step 1:** Apply each cleanup after reading its region; each deletion must be *verified dead* by reading the guarding logic — if any is not actually dead, leave it and note why in the commit body.
- [ ] **Step 2: Gates** — `cargo test -p mev-scout-core && cargo test -p mev-scout-cli && cargo clippy --workspace --all-targets` (workspace `warnings = deny` will catch anything still referenced).
- [ ] **Step 3: Commit** — `git commit -m "refactor(explorer): remove verified-dead code paths; name magic tolerances"`

---

### Task D10: Ingest/store polish batch

**Files:**
- Modify: `core/src/explorer/ingest.rs:584-588` (`IndexedBlock.events` counts `txs.len()` — return the decoded event count or rename the field; read the struct and its consumers first, prefer the rename `txs_indexed` if the count semantics are load-bearing somewhere)
- Modify: `core/src/explorer/ingest.rs:946-955` (tip-jump overwrites `sync_state.indexed_to`, orphaning prior historical gaps — preserve `min(existing, tip-1)` semantics: read `get_indexed_to`, and when jumping to tip, keep `indexed_to = existing.min(safe.saturating_sub(1))` and log a warning naming any gap left behind)
- Modify: `core/src/explorer/store/facts.rs:426-428` (`label_competitor` failure aborts the loop after facts are committed — warn-and-continue per call; idempotent backfill exists per report.rs:178)
- Modify: `core/src/explorer/store/facts.rs:358-371` (volume folds unpriced swap legs in as 0 with no flag — add a `VOLUME_PARTIAL` pricing-reason detail flag when any leg was unpriced, mirroring A9's mechanism)
- Modify: `core/src/explorer/ingest.rs:164-209` (`build_profit_priority` hardcodes stablecoin addresses despite `known_tokens.json`/chain config — read `ChainConfig` in `core/src/chain/` and move the per-chain stable list there; keep existing values verbatim)

- [ ] **Step 1:** Read each region; apply the five changes independently, each with a targeted test where a behavior is observable (tip-jump gap preservation and volume partial flag are testable via the store/golden patterns used earlier).
- [ ] **Step 2: Gates** — `cargo test -p mev-scout-core && cargo test -p mev-scout-cli && cargo test -p mev-scout-core --test explorer_golden --test explorer_corpus && cargo clippy --workspace --all-targets`. [live half: canonical commands]
- [ ] **Step 3: Commit** — `git commit -m "fix(explorer): ingest/store polish — gap-preserving tip jump, volume partial flag, config-owned stablecoins"`

---

### Task D11: Externalize the JIT fee estimate (config-shaped constant)

**Files:**
- Modify: `core/src/explorer/classify/jit.rs:163-172` (`POOL_FEE_BPS = 30` hardcoded)
- Modify: `core/src/explorer/classify/mod.rs` (`BlockInput`/`BlockContext` gains `jit_pool_fee_bps: u64`) and the constructor in `core/src/explorer/ingest.rs`
- Modify: config plumbing if a chain-level knob exists (read `core/src/config/` and the `mev-scout.example.toml` to see where per-chain explorer knobs live; if none exist yet, default the field to 30 in the constructor and do NOT invent a new TOML key — the constant becomes overridable in code, config wiring is a separate decision)

- [ ] **Step 1:** Move the constant to a `BlockInput`/`BlockContext` field with default 30, document it as an estimate ("assumes the 30 bps tier; 5/100 bps V3 pools will be mis-estimated until per-pool fee decoding lands").
- [ ] **Step 1b (review correction): the type is `u32`, not `u64`.** Verified: `classify/jit.rs:162` declares `const POOL_FEE_BPS: u32 = 30;`. Match the field type to the constant or the diff will smuggle in casts. And note the real fix is not a knob: per-pool `fee` is already known to the cache layer (`core/src/cache/store/pools.rs:23` per the store review — confirm before relying on it), so decode the actual tier for the minted range and keep the constant only as the fallback when the fee is unknown. Prefer landing that as a follow-up ticket rather than freezing 30 bps into config as if it were policy.
- [ ] **Step 1c:** fold this task into **D12** — both are "make classify thresholds explorer config", and D12 owns the plumbing, the defaults-equal-today rule, and the per-chain section of `mev-scout.example.toml`. Land them as one config change so there is exactly one commit that can shift classification by configuration.
- [ ] **Step 2: Gates + commit** — `cargo test -p mev-scout-core && cargo test -p mev-scout-core --test explorer_golden --test explorer_corpus`; `git commit -m "refactor(explorer): JIT pool-fee estimate moves to block context with documented default"` [live half: canonical commands]

---

### Task D12: Classification thresholds become explorer config (defaults identical to today)  *(added 2026-10-25 review; absorbs D11)*

**Files:**
- Modify: `core/src/config/settings.rs` (`ExplorerConfig`, near `:217-243`)
- Modify: `mev-scout.example.toml` (documented per-chain section)
- Modify: `core/src/explorer/classify/legs.rs:42,57`, `classify/jit.rs:162`, `ingest.rs:50`, `interest_attr.rs:18`
- Modify: `core/src/explorer/classify/mod.rs` (thread through `BlockContext` from B9)
- Test: settings default-deserialization test + one test proving an unset config reproduces today's constants

**Problem:** `ExplorerConfig` (`settings.rs:217-243`) exposes only `db_path`, `confirmations`, `poll_interval_ms`, `mev_tolerance_pct`, `mev_error_usd_tol`, `tip_sink_addresses`. **Not one classification threshold is overridable.** Verified constants: `CAUSAL_WINDOW_TXS: u64 = 8` (`legs.rs:42`), `CAUSAL_PRICE_MIN_PCT: f64 = 0.5` (`legs.rs:57`), `POOL_FEE_BPS: u32 = 30` (`jit.rs:162`), `JIT_WINDOW_BLOCKS: u64 = 1_000` (`ingest.rs:50`), `LOOKBACK_BLOCKS: u64 = 300` (`interest_attr.rs:18`). The two causal ones are the gates that decide whether a Frontrun or Backrun exists at all — and `legs.rs:35-56` documents them as "not yet calibrated" while reporting zero matched ops in the windows it cites. So the project's currently-open question (`docs/explorer_strategy_checklist.md:3`: 0 frontrun/backrun/skim across 963 ETH and 5001 AVAX blocks, "Classifier not loosened") is blocked on a knob that does not exist: to test a hypothesis about the window you must recompile.

- [ ] **Step 1: Add the keys with today's values as defaults.** `explorer.causal_window_txs`, `explorer.causal_price_min_pct`, `explorer.jit_window_blocks`, `explorer.interest_lookback_blocks`, `explorer.jit_pool_fee_bps` — each `#[serde(default = "…")]` returning the current constant, so an unset config is bit-identical to today. Type each key to match the existing constant (`u64`/`f64`/`u32`), per D11 Step 1b.
- [ ] **Step 2: Thread through `BlockContext` (B9), not through globals.** The classifier takes `BlockInput`/`BlockContext` and is pure — keep it that way. No `static mut`, no `OnceLock` reading env vars inside `classify/`.
- [ ] **Step 3: Document what tuning them means.** In `mev-scout.example.toml`, next to each key, one line: the observable consequence of raising/lowering it and which reject reason (from A20) grows when it is tightened. Add the standing rule from the checklist: *thresholds are never loosened to make a kind appear* — a lower window that produces frontruns is evidence for a hypothesis, not verification, and the checklist row must be marked by a pinned seed block, not by a count.
- [ ] **Step 4: Gates + commit** — `cargo test -p mev-scout-core && cargo test -p mev-scout-cli && cargo test -p mev-scout-core --test explorer_golden --test explorer_corpus` (with no config set, output must be identical — that is the test for Step 1); `git commit -m "feat(explorer): per-chain causal/JIT thresholds move into explorer config with today's values as defaults"` [live half: canonical commands]

---

### Task D13: Take live RPC keys out of tracked config  *(added 2026-10-25 review — security, adjacent to this module)*

**Files:**
- Modify: `mev-scout.avalanche-run.toml:13-16,34-37`, `hist.toml:6-7`
- Modify: `.gitignore` (currently ignores `mev-scout.toml` only, per `:9-10`)
- Modify: `mev-scout.example.toml` (show the env-interpolation form), `README.md` (correct the "secrets come from environment variables" claim)

**Problem (verified):** tracked files embed live provider credentials in the URL path — `mev-scout.avalanche-run.toml:13-16` and `:34-37` (dwellir UUID, drpc token path, getblock key path) and `hist.toml:6-7`. `git ls-files` confirms both are committed. The mechanism to avoid this already exists: `Config::expand_env_secrets` runs on the load path (`settings.rs`, called from `Config::load`), so this is a transcription problem, not a design problem. Out of scope for this task and to be raised separately to the user: **the keys are already in git history**, so removing them from the working tree does not revoke them — rotation is the actual fix.

- [ ] **Step 1: Convert to env interpolation** using the existing `${VAR}` convention; keep each file's structure otherwise untouched so the hunt recipes in the checklist still work.
- [ ] **Step 2: Add the two files to `.gitignore`** and untrack them (`git rm --cached`), leaving them on disk. Confirm the local run still works before committing, and say plainly in the commit body that history rewrite/rotation is not included here.
- [ ] **Step 3: Ask the user before doing anything about rotation** — this is an external action on a paid provider account (revoking/issuing keys) and must not be folded into a code change. Report which keys were exposed and where.
- [ ] **Step 4: Gates + commit** — `cargo test -p mev-scout-cli && cargo run -p mev-scout-cli -- --help` sanity; `git commit -m "security(config): move live RPC keys out of tracked TOML into env interpolation"`

---

### Task D14: The strategy checklist and the seed catalogue stop being two sources of truth  *(added 2026-10-25 review)*

**Files:**
- Create: `core/tests/explorer_checklist_sync.rs`
- Modify: `docs/explorer_strategy_checklist.md` (stable table column order — it already has `id|label|priority|status|chain|seed_from|seed_to|notes`)
- Modify: `core/src/explorer/scenario_targets.rs` (if it needs accessors for the test)

**Problem:** The checklist is the project's highest-value evidence — it records which strategy was seen on which chain at which block, and the header paragraph at `docs/explorer_strategy_checklist.md:3` is a prose log of 5001-block hunts — but nothing reads it. `scenario_targets.rs` holds the machine copy of the same seed blocks, `explorer_corpus.rs` holds a third copy as `CORPUS`, and D6's cross-check covers only corpus↔catalogue. So a row can say `verified @ 26089508` while no code references that block (D6 Step 1 already documents one instance of exactly that). Additionally `scenario_targets.rs:324-346` asserts only *shape* (`assert!(n >= 25)` at `:331`, `!t.hunt.is_empty()` at `:344`), and its `syn_test` strings name `scenarios.rs` test functions that nothing verifies exist — rename a scenario test and the catalogue silently points at a phantom.

- [ ] **Step 1: Fix the dangling `syn_test` names first** (cheap, no new infrastructure): one `#[test]` that `include_str!`s `core/src/explorer/scenarios.rs` and asserts every `syn_test` value in the catalogue appears as a `fn <name>` in that file.
- [ ] **Step 2: Parse the markdown table** and diff it against `STRATEGY_SCENARIO_TARGETS` on `id`, `status`, `chain`, `seed_from`, `seed_to`. Rule for conflicts: the code catalogue is authoritative for seeds, the checklist is authoritative for `status` prose — so the test should compare *seed/chain* strictly and treat `status` as a warning that names the row. Extend D6 rather than duplicating it if by then D6 has landed.
- [ ] **Step 3: Gates + commit** — `cargo test -p mev-scout-core --test explorer_checklist_sync --test explorer_corpus`; `git commit -m "test(explorer): checklist table, seed catalogue and scenario test names are cross-checked"` [live half: canonical commands]

---

### Task D15: Stop the verify/hunt scripts from lying about the DB they read  *(added 2026-10-25 review)*

**Files:**
- Modify: `scripts/verify_explorer_block.mjs:60-95` (hardcoded DB path at `:95`, temp-config generation at `:60-75`)
- Modify: `scripts/tag_coverage.py:13` and `core/tests/explorer_tag_coverage.rs:20` (two copies of `EXPECTED_TAGS`; a third lives in `scenario_targets.rs`)
- Modify: `scripts/hunt_strategy.mjs` / `scripts/hunt_strategy.py` (duplicated implementations with env-var RPC defaults in both)

**Problem (verified):** `verify_explorer_block.mjs:95` hardcodes `path.join("cache", "explorer-${args.chain}.sqlite")`, while `effective_explorer_db_path` honours `explorer.db_path` — and `hist.toml:18` actually sets `db_path = "cache/hist-explorer-avalanche.sqlite"`. Index with that config, then verify: the script reads a stale or non-existent file and exits 1 with no hint of the mismatch. That is the single most likely way to lose an afternoon in this workflow. Second: for any chain other than avalanche, `:60-75` writes a throwaway config `cache/_hunt_${chain}.toml` with hardcoded publicnode URLs (`:63-64`), discarding the archive endpoints you configured — and publicnode has no archive depth for historical seeds, so the backfill returns 0 blocks and the assertion fails for the wrong reason. Third: `ok = matched.length > 0` asserts *presence only* — not kind, not profit, not searcher — so a seed can be "verified" while the classifier regressed its P&L to nonsense.

- [ ] **Step 1: One DB path source of truth.** Add `--db` and, when absent, parse `db_path` (and the chain-derived default) from the `-f` config file. Never hardcode the pattern in a second language.
- [ ] **Step 2: Stop generating configs.** Require an explicit config (`-f`) and fail with a clear message if the requested chain is not enabled in it, instead of silently substituting publicnode URLs. If a synthesized config is genuinely needed, print its path and the endpoint chosen so it is visible in the log.
- [ ] **Step 3: Make the assertion as strong as the checklist's claim.** Support `--expect kind=frontrun,searcher=0x…,min-profit-usd=N` and fail when the persisted op's confidence/basis does not match what the checklist row claims (after A19 lands, include `pnl_basis`). `matched.length > 0` is not verification.
- [ ] **Step 4: One tag list.** Generate or export `EXPECTED_TAGS` from a single source (e.g. `scenario_targets.rs` written to `cache/tags.json` by a cargo test, consumed by both `tag_coverage.py` and the Rust test), so the three copies cannot disagree — today a new tag can be added in one place and be silently un-enforced in the other two.
- [ ] **Step 5: Verify by use, then commit** — run the script against a real seeded block for a chain whose config sets `db_path`, and confirm it now reads the right file and fails loudly when given the wrong one; `git commit -m "fix(scripts): verify/hunt scripts honor configured explorer db and assert kind/profit, not mere presence"`

---

### Task D16: Typing and column hygiene in the store  *(added 2026-10-25 review)*

**Files:**
- Modify: `core/src/explorer/store/jit_positions.rs:107-140` (the `liquidity` round-trip)
- Modify: `core/src/explorer/store/facts.rs:145-210` (the columns bound but never populated)
- Modify: `core/src/explorer/store/query.rs` (`ops_in_range` bounding)
- Modify: `core/src/explorer/store/schema.rs` + B6 (any column dropped or retyped)

**Problem:** (1) `jit_open_positions.liquidity` is stored as TEXT (`p.liquidity.to_string()` at `jit_positions.rs:78`) and read back with `liq.parse::<u128>().unwrap_or(0)` at `:135` — an unparseable or overflowed value becomes **liquidity 0**, which then feeds JIT in-range checks and P&L silently instead of erroring. (2) The INSERT lists at `facts.rs:145` (`producer`), `:181-182` (`dex`, `sender`) and `:201-202` (`is_native`) bind columns that the review found written as constant `NULL`/`0`; read the bound values and decide per column — populate it from the decoded fact or drop it, do not carry dead schema. (3) `ops_in_range` has no `LIMIT`/cursor; grep its callers (`store/mod.rs` tests use narrow ranges; `core/tests/explorer_tag_coverage.rs` may ask for a whole window) and add a bounded variant or a `LIMIT` parameter so a report path cannot load every op into RAM. (4) USD is `REAL` throughout, computed as f64 (`pricing.rs:42-46`) and `SUM`ed in SQL (`query.rs:417-434`) — this plan is not going to migrate money to integer cents, but the choice must be **written down** with its consequence (sub-cent drift in aggregates, non-reproducible sums across ordering) in the `mev_ops` schema comment, so nobody reads `net_profit_usd` as exact-to-the-wei.

- [ ] **Step 1: Fail loud on liquidity** — `parse` error becomes `Err` (or a counted, warned skip that keeps the position out of range math), never `0`.
- [ ] **Step 2: Dead columns** — per column: populate or drop via a B6 migration; state which in the commit body.
- [ ] **Step 3: Bound the read** — add `LIMIT`/pagination to `ops_in_range` and update callers.
- [ ] **Step 4: Document the float-money decision** — one schema comment; no behavior change.
- [ ] **Step 5: Gates + commit** — `cargo test -p mev-scout-core && cargo test -p mev-scout-cli`; `git commit -m "fix(explorer): store typing — liquidity errors instead of zeroing, dead columns resolved, bounded op reads"`

---

### Task D17: Close the test holes this plan keeps stepping on  *(added 2026-10-25 review)*

**Files:**
- Modify: `core/src/explorer/golden.rs` (the single test + the count pin at `:783`)
- Modify: `core/src/explorer/classify/*.rs`, `core/src/explorer/decode/*.rs`, `core/src/explorer/store/*.rs` (targeted unit tests)
- Test: the new tests themselves are the deliverable

**Problem (verified):** 29 files in the explorer module contain **zero `#[test]`** — every `classify/` pass except `mod.rs` (`arb.rs`, `causal.rs`, `jit.rs`, `legs.rs`, `liquidation.rs`, `sandwich.rs`, `skim.rs`), every `decode/` file except `tests.rs`, and every `store/` file except `mod.rs` (`facts.rs`, `query.rs`, `sync.rs`, `pnl.rs`, `schema.rs`, `labels.rs`, `jit_positions.rs`), plus `reject.rs`, `results.rs`, `types.rs`. Meanwhile `golden.rs` is 787 lines of fixtures behind exactly **one** `#[test]`, and it ends with `assert_eq!(score.cases, 11)` at `:783` — a count snapshot that breaks on every legitimate fixture addition and tells you nothing about *which* case regressed. This matters for this plan specifically: A13/A14/A17/A19/A20 add behavior in files that have no test module at all, so without this task each of those tasks invents its own harness placement and several will settle for a golden-diff instead of a unit assertion.

- [ ] **Step 0 (do this before Phase A tasks land):** establish the `#[cfg(test)] mod tests` convention per file — one module at the bottom of the file under test, using `test_fixtures` builders. Note the good news the review found: `classify_block(&BlockInput) -> ClassifyOutput` is pure and `test_fixtures.rs` has no network references, so every one of these is testable without RPC.
- [ ] **Step 1: Replace the count pin** — `assert_eq!(score.cases, 11)` becomes an assertion on the *set of case identities* (per-chain/per-block/kind expectations), so adding a fixture extends the list instead of breaking a magic number, and removing one is visible by name.
- [ ] **Step 2: Minimum viable coverage per A-task** — each of A13 (fee counted once), A14 (zero quote rejected), A16 (native residual priced), A17 (wrap pairing), A19 (basis/confidence), A20 (reason emitted) must land its unit test in the file that owns the logic, not in `store/mod.rs`, so the file gains its first `#[cfg(test)]` and the next change there has a net.
- [ ] **Step 3: The two untested load-bearing helpers** — `store/query.rs` (`report_filter_clause`, the window aggregates A5/C10 both rewrite) and `store/sync.rs` (`unwind_from` from A7/A21, `unclassified_blocks` from C6). Both currently have no test at all and both are being edited by this plan; add a test per touched function as part of those tasks rather than as a follow-up.
- [ ] **Step 4: Gates + commit** — `cargo test -p mev-scout-core && cargo clippy --workspace --all-targets`; `git commit -m "test(explorer): per-file test modules for classify/decode/store paths this plan modifies"`

---

## Explicitly deferred (out of scope for this plan — needs its own design)

- **Historical native pricing for backfill gas** (ingest.rs:979, 1110): backfill prices gas at *current* native price refreshed every 256 blocks. Correct fix (fetch Llama historical native price keyed to block ts) touches the price-fetch layer and changes every historical gas figure — deserves its own plan with golden re-baselining across chains. Do not fold into this plan.
  **Cross-references added by the 2026-10-25 review:** this is *not* the same gap as **A16** (A16 prices a native *profit residual* with the price already in scope; this is about which price is in scope for historical blocks), and it depends on **B10** existing — without a reprice path, this work can only be done by re-indexing from RPC. B10's `ExplorerStore::reprice_range` rewrites `profit_usd` / `net_profit_usd` / `excluded_from_net` from the `prices` table and a caller-supplied decimals map. It does not reprice `gas_cost_usd`: gas wei is not a `mev_ops` column, so historical gas still needs its own design. It also inherits **A2**'s open convention problem: token prices are keyed by block hour (`ingest.rs:751`) while native prices are keyed by fetch hour (`ingest.rs:1065`, `:1110`, `:1179` all pass `epoch_secs()`), so "historical native price" first requires deciding what `prices.hour` means.
- **Internal transfers / traces for native credits** — the module is logs-only by design (`explorer/mod.rs:2-3`) and `profit.rs:73-76` debits `tx.value` with no credit path, so native *received* (unwrap-to-beneficiary, builder/MEV-share kickbacks) is unobservable. A21/A16 document the limit; lifting it means `trace_*`/`internal_tx` support and a new ingestion mode, which is a plan of its own.
- **Per-pool V3 fee decoding instead of the 30 bps estimate** — D11/D12 make the estimate overridable, which is not the same as correct. The real fix reads each pool's actual fee tier (the cache layer already stores per-pool `fee`); fold it into the pool-decoding work, not into this plan.
- **Money as floats** — `REAL` columns and f64 arithmetic throughout (`pricing.rs:42-46`, `SUM` in `query.rs:417-434`). D16 Step 4 documents the decision; migrating USD to integer basis points is a schema rewrite plus a report rewrite and should be its own plan if the sub-cent drift ever matters.
- **Foreign-key enforcement** — B6 may add `PRAGMA foreign_keys=ON` but must not silently fail on pre-existing orphan rows; a real backfill-and-cleanup of orphans is deferred work.
- **Provider-side hardening** — this plan fixes how ingest *reacts* to failures (A6, A8, A21). Whether the RPC layer's cooldown/retry policy waits out short cooldowns for non-archive scopes, and whether receipt/tx alignment is validated before classification, are `rpc/`-module questions outside the explorer's scope; file them separately if the A6/A8 changes surface them.
- **Rotating the exposed RPC keys** — D13 removes them from the working tree; revoking/rotating live provider credentials is an external action that needs the user's explicit go-ahead, not a code change.
- **`spawn_blocking` alternative designs** (connection pool, async SQLite): rejected in C8 for cause (single-writer); revisit only if profiling shows persist is a bottleneck after C3/C4.

## Self-Review Notes

- **Coverage (corrected on the 2026-10-25 second review).** The first version of this plan claimed all 40+ findings mapped to A1-A12 / B1-B9 / C1-C8 / D1-D11. That was overstated: the second review verified **fifteen** items with no task anywhere in the plan. They are now: **A0** (CI + honest live-test skips), **A13** (flash-loan fee counted twice), **A14** (a `0.0` USD quote accepted as valid), **A15** (kind-collision census before precedence becomes law), **A16** (native profit priced as `NULL`), **A17** (wrap-noise filter with no pairing check), **A18** (shared `explorer.db_path` across chains), **A19** (`Confidence::Exact` from gross/unreconciled evidence), **A20** (silent drop sites outside `arb.rs`), **A21** (unwind from the probe instead of the fork), **B10** (`mev_ops` neither idempotent nor revisable), **C9** (per-block clone storm + O(n²) price lookup), **C10** (indexes that miss every report predicate + nondeterministic ordering), **D12** (classification thresholds not configurable), **D13** (live RPC keys in tracked TOML), **D14** (checklist ↔ catalogue never compared), **D15** (verify/hunt scripts reading a hardcoded DB path), **D16** (typing + dead columns + unbounded reads), **D17** (29 files with zero tests, `golden.rs` count pin). Amended in place: **A1** (scope vs B8), **A2** (blast radius + hour-convention caveat), **A6** (failure cap + pinned cursor invariant), **A7** (`rejected_candidates` deletion + `opportunities` decision), **B1**/**B3** (reclassified as correctness), **B6** (column queue, pragmas, `open_in_memory`), **B7** (three autocommit write loops), **B8** (A15 dependency, absorbs A1, `MevEvent` builder, missing `Unknown` residual pass), **D9** (constant `REVERSE_DIRECTION`, dead `liq_txs`), **D11** (`u32` type, folded into D12). A third pass the same day (kind-taxonomy question) added **B11**: the `Unknown` kind is three known utility families (keeper / claim-and-sell / ERC-4337 bundler) behind exact fingerprints — rename to `Utility`, persist a `subkind` column, and fix the doc promise of a residual pass that was never coded.
- **Two items in the first review were corrected by verification, not accepted.** `report_filter_clause`'s string-built SQL (A5) is **not** an injection today — clap restricts `--confidence` to `exact|inferred|all` at `cli/src/cli.rs:120` — so A5 is an API-hardening task, not a security fix, and should be described that way. And `explorer_tag_coverage.rs` *is* properly `#[ignore]`d (`:45`), so A0's problem there is that the plan's gate line never runs it, not that it fakes a pass; only `explorer_corpus.rs:903` fakes a pass.
- **Ordering / dependency graph (the authoritative execution sequence, replacing "phases are mostly independent").**
  - **A0 first, unconditionally** — every "expected: PASS" below it is unverifiable otherwise.
  - **M0 alongside A0; its Steps 1–2 (vendor fetch + baseline index) must complete before the first Phase A task lands** — the baseline must capture pre-plan code or there is no "before" to compare against. **M1** rides the Phase-A audit commit, a post-Phase-B run, and one final run; it adds no new timeline.
  - **A14 → A9 → A10** (a fake `$0` price manufactures the loss A10 would faithfully persist).
  - **A15 → B8**; **A1 → B8** (B8 must show zero golden drift because A15 and A1 already settled the invariant).
  - **A18 → A7** (a shared DB makes A7's multi-table DELETE delete another chain's rows).
  - **A13 → A19** (both change `net`; settle the fee counting before re-labelling confidence bases).
  - **B6 → B10 → {A10 column, A18 chain column, A19 `pnl_basis`, C10 indexes}** — every schema addition is a migration, so the machinery lands before the payloads.
  - **B6 → B11** — the `kind` CHECK rewrite and the `subkind` column are migrations; they ride B6's rebuild path, never a raw `ALTER`.
  - **A19 → B11 → D5** — B11 renames the kind that A19's basis labels sit on and that D5's `MevKind::ALL` must list; do not interleave A19 and B11 edits on the three emission sites, and land B11 before D5 so `ALL` contains `Utility` exactly once.
  - **B7 → A20** (`rejected_candidates` needs a unique key before the reject stream gets wider, or re-indexing duplicates it).
  - **B9 → D12** (thresholds travel through `BlockContext`).
  - **ingest.rs single track: A6 → A8 → A21 → C4 → C7 → C8 → C9 → D10 → D12.**
  - **D11 → D12** (one config change), **D14 → D6** or the reverse, but one commit that makes checklist/corpus/catalogue agree.
  - **D13** and **D15** are independent of everything above; they are the two with the highest immediate operator cost per hour lost, so front-load them if execution is opportunistic.
- **Re-baseline discipline (corrected — the original wording had a tension).** Deferring every baseline update to a single end-of-Phase-A pass would make the accumulated golden diff unattributable, which defeats the "predicted diff" rule. Instead: after each number-moving task (A1, A4, A9-A14, A16, A17, A19, B1, B3, B8, B10, B11), run the goldens *in that task*, capture the diff, and update the baseline in that task's commit with a `PENDING-REBASELINE(<task>)` note in the commit body recording the observed shift and the predicted direction. One audit commit at the end of Phase A then reconciles the accumulated shifts task-by-task against each prediction, and records the A15 census table, the C1/C9 blocks-per-second numbers, and the **M1 end-of-Phase-A oracle run** (recall floor + adjudicated `L \ V` diff against the M0 baseline) alongside. A golden diff you predicted is evidence; a golden diff you "updated" is a lost finding — the per-task note is what keeps the audit honest.
- **Riskiest tasks.** B8 (structural claims — gated by A15 evidence), C8 (async/store boundary), B9 (wide mechanical rename), A21 (reorg semantics — changes *when* data is deleted), A17 (changes what the arb pass's inventory vetoes see), D13/D15 (touch operator workflow, not code paths). Each may be deferred individually except A0, which nothing else may be deferred behind.
- **Still open after this plan (deliberately):** trace-level native credits, per-pool V3 fee truth, money-as-float, FK enforcement, key rotation, provider cooldown policy, and receipts/tx-count validation on the fetch side — see the deferred section for why each is a design task rather than a checkbox.
