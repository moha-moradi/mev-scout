# Behavior-preserving simplification: `cli/src/commands/explorer*` + `core/src/explorer`

## Context

You pointed me at [cli/src/commands/explorer.rs](file:///D:/gitlab.dte.repo/mev-scout/cli/src/commands/explorer.rs) and [core/src/explorer](file:///D:/gitlab.dte.repo/mev-scout/core/src/explorer) and asked for a code-simplifier pass.

That module is **23,373 lines**, and the raw size is misleading: ~43% is test code. [classify/mod.rs](file:///D:/gitlab.dte.repo/mev-scout/core/src/explorer/classify/mod.rs) is 3,097 lines but only **586** are production; [store/mod.rs](file:///D:/gitlab.dte.repo/mev-scout/core/src/explorer/store/mod.rs) is 1,778 → **341** production. A blanket "simplify everything" pass over that would be reckless and would generate churn without value. So this plan targets only **confirmed, localized redundancy** where the same logic genuinely exists twice or more.

The headline finding is reassuring: this code is mostly clean. Error handling is not boilerplate-heavy (only 11 `map_err` / 11 `anyhow!` in the whole module), address/hex normalization is already centralized in [core/src/utils.rs](file:///D:/gitlab.dte.repo/mev-scout/core/src/utils.rs) and reused by 10 explorer files, and there is no meaningful drift. What remains is **a handful of specific duplicated blocks**, listed below with line evidence.

Two constraints shape every item:
- The CLI prints `comfy_table` reports; **emitted text must stay byte-identical**.
- `canonical.rs` produces IDs **persisted to SQLite** (`ON CONFLICT(canonical_id, tx_hash)`); its format strings are frozen.

## Decisions locked in

- **Scope**: CLI first, then `core/src/explorer`.
- **Risk tier**: behavior-preserving only. Explicitly **excluded** (your call): splitting inline `#[cfg(test)] mod tests` into separate files, macro-generated code, `#[cfg(...)]` gating changes, and decomposing large functions. See *Deferred* — you are turning down the single biggest navigability win here, deliberately.
- **Agent**: the `code-simplifier` plugin agent cannot run on this host — its plugin-qualified ID contains a `:`, which Windows rejects when creating the subagent metadata file (`EINVAL`, failed identically twice, retrying with an explicit name too). A `general-purpose` subagent ran with the same mandate; its findings were verified by me against source before inclusion.

## Step 0 — baseline first (do not skip)

There is no Makefile/justfile; `.github/` was deleted in `6eaa33b`, so **all gates are local** and [README.md](file:///D:/gitlab.dte.repo/mev-scout/README.md) §Validation is authoritative.

```
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --no-fail-fast 2>&1 | tee target/baseline-test.txt
```

Capture a CLI **output oracle** before editing, to diff against later. `cache/oracle-avalanche.sqlite` is gitignored — machine-local, not durable:

```
cargo run -q -p mev-scout-cli -- -f cache/oracle-avalanche.toml explorer --windows all --confidence all > target/explorer-table.txt
```

Use `--windows all` and table/csv modes only — **exclude JSON**, which emits `generated_at` at [report.rs:324](file:///D:/gitlab.dte.repo/mev-scout/cli/src/commands/explorer/report.rs#L324) and will always diff.

Known pre-existing noise: 5 wiremock pool-discovery tests fail with Squid 503 through the corporate proxy (`dexscreener.rs:315`, `geckoterminal.rs:518,564,614,702,741`). Confirm environmental by re-running once with `NO_PROXY=127.0.0.1,localhost`; allowlist them, don't count them in the delta. Env-gated/`#[ignore]` DB and RPC tests are **not** per-increment gates.

If the baseline is not green before editing, stop and report — an untrustworthy baseline invalidates every later check.

## Group A — CLI (do first)

| # | Site | Change | Lines | Risk |
|---|---|---|---|---|
| A1 | [report.rs:548-565](file:///D:/gitlab.dte.repo/mev-scout/cli/src/commands/explorer/report.rs#L548) | Replace the hand-built "Per kind" table with `out.push_str(&report_table_text(&r.by_kind, "kind")); out.push('\n');` | 17→3 | zero |
| A2 | [report.rs:347-356](file:///D:/gitlab.dte.repo/mev-scout/cli/src/commands/explorer/report.rs#L347) | `row_overview_fields` delegates to `row_fields` via a field-named `ReportRow { ..Default::default() }` instead of repeating the six expressions | 10→~8 | zero |
| A3 | [report.rs:429-436](file:///D:/gitlab.dte.repo/mev-scout/cli/src/commands/explorer/report.rs#L429) | Optional, lowest value: name the anonymous inline `padded` array as `reject_fields(&RejectCount)`, matching the `stats_fields` / `op_fields` convention | 8→2 | zero |

**A1 is verified by inspection and is the best item in this plan.** [report.rs:568](file:///D:/gitlab.dte.repo/mev-scout/cli/src/commands/explorer/report.rs#L568) *already* does `report_table_text(&r.daily, "date")` — same helper, same shape. The `by_kind` block was simply never converted. Byte-identity argument: [report.rs:162](file:///D:/gitlab.dte.repo/mev-scout/cli/src/commands/explorer/report.rs#L162) returns `format!("{table}")`; line 565 emits `format!("{t}\n")`; pushing the former plus `'\n'` is identical, and the empty-row `"-"` pad and header list match [report.rs:147-157](file:///D:/gitlab.dte.repo/mev-scout/cli/src/commands/explorer/report.rs#L147) exactly.

**A2**: [ReportRow](file:///D:/gitlab.dte.repo/mev-scout/core/src/explorer/store/mod.rs#L207) and [ReportOverview](file:///D:/gitlab.dte.repo/mev-scout/core/src/explorer/store/mod.rs#L258) share the six fields `ops, volume_usd, gross_usd, gas_usd, flash_fee_usd, net_usd`; `ReportRow` derives `Default`. Delegate with named fields — no new type, no positional args.

Do **not** merge the CSV `String::new()` pads ([stats_fields:360](file:///D:/gitlab.dte.repo/mev-scout/cli/src/commands/explorer/report.rs#L360), [op_fields:372](file:///D:/gitlab.dte.repo/mev-scout/cli/src/commands/explorer/report.rs#L372)) into a `money_cols(..., Option<f64>, ...)` helper. Five adjacent `Option<f64>` parameters let gross/gas transpose silently, and the current literal `String::new()` entries make each section's holes visually explicit. This is over-abstraction; the survey's suggestion is rejected. Likewise leave the table `"-"` pads alone (a `empty_row(n)` helper would trade self-documenting per-column dashes for a bare number).

`short_addr` at [explorer.rs:16-22](file:///D:/gitlab.dte.repo/mev-scout/cli/src/commands/explorer.rs#L16) has one definition and 2 call sites and **no equivalent exists in core** — nothing to dedupe; relocating it would be a file move (out of tier).

## Group B — core/src/explorer

| # | Site | Change | Risk |
|---|---|---|---|
| B1 | [query.rs:162, 212, 238, 592, 621, 650](file:///D:/gitlab.dte.repo/mev-scout/core/src/explorer/store/query.rs#L162) | Six identical `let mut out = Vec::new(); for r in rows {{ out.push(r?); }} Ok(out)` → `Ok(rows.collect::<Result<Vec<_>, _>>()?)`; precedent already in-file at `:566`, plus `labels.rs:74`, `schema.rs:129` | zero |
| B2 | [query.rs:204-208, 261-265, 638-642](file:///D:/gitlab.dte.repo/mev-scout/core/src/explorer/store/query.rs#L204) | `const MEV_OP_COLUMNS` for the byte-identical 23-name SELECT list. Doc must state it mirrors `map_mev_op_row`'s positional `r.get(n)` indices | zero |
| B3 | [query.rs:484, 583, 612](file:///D:/gitlab.dte.repo/mev-scout/core/src/explorer/store/query.rs#L484) | `const NET_USD_AGG` for the 3 exact copies of the loss-gated `COALESCE(SUM(CASE WHEN excluded_from_net = 0 …))` | zero |
| B4 | [query.rs:334, 383, 391, 404, 419, 438, 446](file:///D:/gitlab.dte.repo/mev-scout/core/src/explorer/store/query.rs#L334) | `parse_required<T: FromStr>` / `parse_col<T>` absorbing **seven** copies of the `"opportunities table holds unparseable {field} '{v}' (schema drift?): {e}"` message. Keep the `.unwrap_or(Address::ZERO)` / `U256::ZERO` / `0` literals at call sites — do not swap to `unwrap_or_default()` | low |
| B5 | [canonical.rs:19](file:///D:/gitlab.dte.repo/mev-scout/core/src/explorer/canonical.rs#L19) | `use crate::explorer::types::MevKind;` and shorten the **seven** fully-qualified `MevKind::` paths at `:24, :35, :45, :55, :66 (×2), :79` | zero |
| B6 | [canonical.rs:29-32, 39-42, 49-53](file:///D:/gitlab.dte.repo/mev-scout/core/src/explorer/canonical.rs#L29) | `detail_or_null(ev, key)` for three `ev.details.get(k).cloned().unwrap_or(json!(null))` copies. **Leave the Jit arm's `unwrap_or(json!(0))` at `:69-75`** — different default | zero |
| B7 | [canonical.rs:87](file:///D:/gitlab.dte.repo/mev-scout/core/src/explorer/canonical.rs#L87) | Drop the stray trailing comma in `format!("{:?|}", …,)` | zero |

**B4 needs a test before it lands.** Nothing guards it today: `opportunities_by_run` has no test callers. Add to `mod filter_tests` a round-trip via `ExplorerStore::open_in_memory()` + `insert_opportunity` + `opportunities_by_run`, plus one deliberately poisoned `pool_a` asserting the exact error string. Same for B6 — only the null path is currently pinned (`sandwich_canonical_matches_opportunity_shape`); pin the Frontrun/Backrun/Skim/Liquidation/Jit strings too, since they are persisted IDs.

## Guardrails

- `Cargo.toml` sets `warnings = "deny"`, `unwrap_used = "deny"`, `expect_used = "deny"`; dead code **fails the build**. Never split "stop calling a helper" from "delete it" across increments — each must compile clean alone.
- Doc-comments here carry task IDs (A15, A19, B11) and explain *why*. Move them verbatim; never strip one as noise.
- **Halt and return to you if**: the baseline isn't green; a change requires touching a `canonical.rs` format literal or `schema.rs`; the oracle table diff is non-empty and not provably explainable; or an edit touches the `include_str!` guard at [classify/mod.rs:3049-3096](file:///D:/gitlab.dte.repo/mev-scout/core/src/explorer/classify/mod.rs#L3049) and could make it pass vacuously.
- One commit per increment, `refactor(explorer): …` (house style per `git log`). On a failed gate, `git restore` the file and redo — never `--amend`, never `reset --hard`.
- Checkpoints after **A1+A2**, after **A3**, after **B1–B3**, after **B4–B7**, each with the full workspace suite diffed against `target/baseline-test.txt` (identical pass counts) plus a byte-compare of `target/explorer-table.txt`.

## Deferred by your choice (not forgotten)

- Splitting `classify/mod.rs` and `store/mod.rs` inline test modules into `tests.rs`, following the existing `decode/tests.rs` precedent. Removes 3,948 test lines from the two biggest production files — the largest navigability win available, and near-zero logic risk, but it's a file move you excluded.
- Macro-generating the seven hand-written enum↔`&str` pairs in [types.rs](file:///D:/gitlab.dte.repo/mev-scout/core/src/explorer/types.rs) (~120 lines → ~40, round-trip tests exist at `:649-690`).
- `#[cfg(any(test, feature = "test-util"))]`-gating `BlockContext::for_tests` at [classify/mod.rs:145](file:///D:/gitlab.dte.repo/mev-scout/core/src/explorer/classify/mod.rs#L145) — it currently compiles into release builds, and the repo already defines that feature.
- `short_addr` panics on a non-ASCII 42-byte label (`&s[..8]` cuts a char boundary). Robustness, not simplification.

## Not worth touching

`store/schema.rs` migrations `v1`–`v10` (append-only history; `migrate_v8`'s re-declared `mev_ops` table *looks* duplicated but deduping it breaks existing DBs, and idempotence is tested via `sql.contains(...)`); `store/facts.rs` `params!` lists (1:1 with `PersistedOp`); `reject.rs`'s 24-arm `as_str` and `decode/mod.rs` `log_family` (mechanically exhaustive, macro-ifying loses topic0-per-arm readability); [index.rs](file:///D:/gitlab.dte.repo/mev-scout/cli/src/commands/explorer/index.rs) (16 lines); `usd`, `row_cells`, `csv_row`, `leaderboard_limit`; `backfill.rs:36-59`'s two clock arms (only two copies, and the `Option` shapes are load-bearing for rejecting `--days` with `--from-block`); the deliberate table-vs-CSV asymmetry where `stats_table_text:173` truncates labels but `csv_lines:411` emits them raw. Decomposing `classify_atomic_arbs` (309 lines), `annotate_event_with_volume` (255) and `classify_liquidations` (245) is behavior-sensitive classifier work, not simplification.

## Separate bug found — not fixed in this pass

[canonical.rs:26](file:///D:/gitlab.dte.repo/mev-scout/core/src/explorer/canonical.rs#L26) formats `backrun:{:?}` on a `serde_json::Value`. `Debug` on `Value::Number(3)` renders `Number(3)`, while `Display` renders `3` — so persisted Sandwich canonical IDs contain `backrun:Number(3)`. The sibling arms at `:37` and `:47` correctly use `{}`. This is almost certainly unintended, but the IDs are **already written to SQLite**, so fixing it changes stored values and breaks matching against historical rows. It must not be bundled into a simplification commit. Want me to file it as its own investigation?

## Verification

After each increment (fast gate):
```
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test -p mev-scout-cli --lib commands::explorer
cargo test -p mev-scout-core --test explorer_golden
```
Per-group, also: re-run the oracle command and require a **byte-identical** `target/explorer-table.txt`; `cargo test -p mev-scout-core --test causal_fp_corpus` (offline, skips if `cache/explorer-avalanche.sqlite` is absent); and at each checkpoint the full `cargo test --workspace --no-fail-fast` diffed against the baseline pass counts.

The cheapest sufficient gate for the frozen ID surface is `explorer_golden::golden_block_persists_mev_ops_rows` ([core/tests/explorer_golden.rs:260](file:///D:/gitlab.dte.repo/mev-scout/core/tests/explorer_golden.rs#L260)), which asserts exact `canonical_id` values offline. Existing snapshot infra covers this — no new harness needed.

Expected end state: ~5 files changed, on the order of 100 lines removed net, zero emitted-text and zero persisted-ID changes. If the oracle or golden diff moves, that is a real regression signal, not test flake. The honest summary: this code needed a scalpel, not a rewrite — ten increments (A1–A3, B1–B7), and only two of them are more than mechanical.