# Minimal `explorer` CLI surface

## Context

`explorer` currently exposes three subcommands (`index`, `dashboard`, `purge-retired`) plus a bare rolling-window revenue report. The requested simplification is to leave only one real subcommand (`index`), make reporting and dashboarding all-time views of whatever is in the selected SQLite DB, and remove one-off maintenance and advanced tuning flags from the public CLI.

This is an intentional CLI-breaking simplification, not a compatibility-preserving refactor. Do not add aliases or fallback parsing for removed forms. Do not purge, migrate, rewrite, or otherwise modify existing explorer databases as part of this change.

## Target CLI contract

```text
mev-scout explorer [--kind KIND] [--confidence exact|inferred|all]
mev-scout explorer --dashboard [--db PATH] [--out PATH]
                   [--format html|json|csv] [--repeat DURATION]
mev-scout explorer index [--window Nh|Nd|Nw | --from-block N --to-block N]
                         [--dashboard]
```

Global `-f/--config`, `-v/--verbose`, and `--quiet` remain unchanged.

Behavioral rules:

- Bare `explorer` reports **all rows in the configured explorer DB** (`since_ts = 0`), with all leaderboard/top-op limits fixed at 10. Keep `--kind` and `--confidence`; `all`/omitted confidence means no confidence filter.
- `explorer --dashboard` renders an **all-time**, read-only dashboard. With explicit `--db`, it must remain offline and require neither live RPC nor `mev-scout.toml`, as today.
- Dashboard defaults remain `results/dash.html`, `html`; `--repeat` retains the existing atomic refresh loop.
- Remove public dashboard flags `--window`, `--max-ops`, `--evidence`, `--budget-mb`, and `--with-gaps`. Internally preserve the current CLI values: `WindowSpec::all()`, `max_ops = 2000`, `max_evidence = 500`, `budget_mb = 8`, `with_gaps = false`, `gap_block_limit = 10000`.
- `explorer index` defaults to trailing `7d`. `--window` accepts a positive `Nh`, `Nd`, or `Nw` duration up to 365 days; reject `all`, zero, malformed values, unknown suffixes, and durations over the limit before RPC initialization.
- `--window` and `--from-block/--to-block` are mutually exclusive. Exact block bounds remain inclusive and still require each other; preserve the `to < from` and resolved-range sanity checks.
- `explorer index --dashboard` runs the normal backfill first, then emits one all-time HTML dashboard from the **same config-resolved DB** to `results/dash.html`, and exits. It does not accept a separate `--db` or `--repeat`; a failed backfill emits no dashboard.
- `--kind`/`--confidence` belong only to the bare report and conflict with dashboard mode and `index`. Dashboard-only options require `--dashboard`. Parent-level report/dashboard flags must not be silently accepted alongside `index`.
- Removed forms must fail through clap/dispatch: `explorer dashboard ...`, `explorer purge-retired ...`, bare `explorer --windows/--top`, `index --hours/--days`, and all removed dashboard tuning flags.

## Implementation

### 1. Reduce the clap model

Modify `cli/src/cli.rs`:

- Reduce `ExplorerCommand` to `Index(IndexArgs)` only; delete the `Dashboard` and `PurgeRetired` variants and delete `PurgeRetiredArgs`.
- Remove `ExplorerArgs.windows`, `ExplorerArgs.top`, `resolved_windows()`, and `resolved_top()`. Remove `DEFAULT_EXPLORER_WINDOWS`; replace the remaining top-N setting with one fixed internal constant of 10.
- Keep `kind`, `confidence`, and `resolved_confidence()`. Update the report-flag validation helper so it covers only flags that still exist and any parent-level dashboard options that must not accompany `index`.
- Reshape `DashboardArgs` into the bare-`explorer` dashboard mode/options: `dashboard: bool`, `db`, `out`, `format`, and `repeat` only. Flatten/reuse it on `ExplorerArgs`; do not leave a `dashboard` subcommand. Add clap conflicts/requires so filters cannot be mixed with dashboard mode and dashboard options cannot be supplied without `--dashboard`.
- Give `IndexArgs` `window: Option<String>`, the existing exact block fields, and only a boolean `dashboard` convenience flag. Do not flatten the full dashboard options into `IndexArgs`.
- If a programmatic dashboard default is needed for `index --dashboard`, mirror the clap defaults in one small `Default` implementation, following the existing `LiveArgs::default()` pattern rather than duplicating output paths/formats at call sites.

### 2. Dispatch report, dashboard, and post-index dashboard explicitly

Modify `cli/src/commands/mod.rs` and `cli/src/commands/explorer.rs`:

- Bare `explorer`: choose `run_dashboard` when `--dashboard` is set; otherwise call the simplified all-time `cmd_explorer_report` with only kind/confidence filters.
- `ExplorerCommand::Index`: await `cmd_backfill` successfully; when its own `dashboard` flag is true, call the same dashboard path with programmatic defaults (`db = None`, all-time, HTML, `results/dash.html`, no repeat).
- Keep the existing `BarProgress` selection for `ExplorerCommand::Index` in `cli/src/main.rs`; dashboard/report remain `NoopProgress`.
- Remove the purge dispatch and exports. Keep `resolve_db`, `run_dashboard`, and the existing atomic writer unchanged apart from the reduced options.

### 3. Make the revenue report single-scope/all-time

Modify `cli/src/commands/explorer/report.rs`:

- Change `cmd_explorer_report` to accept config, kind, and confidence only. Execute one report with `since = 0` and fixed `top = 10`, reusing the existing store queries unchanged.
- Remove `ReportWindow`, rolling-window parsing/deduplication, `expected_blocks`, rolling `coverage_pct`, the under-filled warning, and now-unused imports/helpers. Move the useful duration parser to the backfill module rather than deleting its tested parsing behavior.
- Preserve the external all-time output shape: table heading `all window (all-time, ...)`; JSON root still contains `windows` with one `all` entry; CSV still uses the existing header and `window = all`. This avoids an unnecessary output-schema break while removing multi-window behavior.
- Keep `generated_at`, daily trend, quality/pricing warnings, concentration warning, classify rejects, and all report sections. The daily trend now naturally covers all persisted days.
- Update the stale warning/example that recommends `explorer index --days 30` if it survives refactoring; the rolling under-filled warning itself should normally be deleted.

### 4. Replace index hours/days with one duration window

Modify `cli/src/commands/explorer/backfill.rs`:

- Replace the `hours`/`days` parameters with the new optional window. Reuse/move the existing positive `Nh|Nd|Nw` parser from `report.rs`, add the 365-day upper bound, and default to `7d` when neither a clock window nor exact blocks are supplied.
- Resolve the trailing block span with the authoritative chain table using `duration_secs * blocks_per_day(chain) / 86400` (saturating arithmetic, minimum one block). **Do not use `block_time_secs` for this conversion**: its rounding would distort sub-second chains such as Arbitrum. The formula preserves the existing invariant that `24h == 1d` and naturally supports weeks.
- Preserve `safe_head`, confirmation-lag behavior, inclusive exact ranges, the 100,000,000-block guard, `run_range`, skip reporting, and `BackfillOutcome` unchanged.

### 5. Fix dashboard internals without reducing core library capability

Modify `cli/src/commands/explorer/dashboard.rs`:

- Remove `window`, `max_ops`, `max_evidence`, `budget_mb`, and `with_gaps` from the CLI-facing/internal request struct. Build with `WindowSpec::all()` and the fixed current CLI values listed above.
- Leave `core::explorer::dashboard::BuildOptions`, `WindowSpec::parse/trailing`, gap sampling, evidence limits, and budget enforcement intact for core callers/tests; this task only removes public CLI knobs.
- Update stale user-facing strings in `core/src/explorer/dashboard/mod.rs` and `core/data/dashboard_template.html`: disabled-gap and budget messages must no longer instruct users to pass removed `--with-gaps` or `--budget-mb` flags.
- Update the stale 1d/7d/30d comment in `core/src/explorer/ingest.rs` to describe the all-time report.

### 6. Remove the retired purge implementation completely

- Delete `cli/src/commands/explorer/purge.rs` and `core/src/explorer/store/purge.rs` (including their module-local tests).
- Remove `mod purge` / `pub use purge::PurgeReport` from `core/src/explorer/store/mod.rs`, and all CLI module/export/dispatch references.
- Do not replace this with an automatic migration. Historical retired rows remain readable and untouched; this is only removal of a one-off maintenance command.
- Update `docs/explorer_strategy_checklist.md` so it no longer claims users can remove those rows with `explorer purge-retired`; state that retired rows may remain in historical DBs and are not automatically rewritten.

### 7. Update tests as regression gates

Modify `cli/tests/cli_args.rs`:

- Help lists only `index` as an explorer subcommand and shows `--dashboard`, `--db`, `--out`, `--format`, `--repeat`, `--kind`, `--confidence`; it does not show removed flags/subcommands.
- Parse bare report defaults, both confidence behaviors, `index` default shape, valid `index --window 6h/30d/2w`, exact ranges, and `index --dashboard`.
- Reject malformed/zero/oversized/`all` index windows, window-plus-block conflicts, report/dashboard flag conflicts, dashboard options without `--dashboard`, and every removed legacy form. Add `explorer dashboard` and `explorer purge-retired` to the removed-surface gate.

Modify `cli/tests/cli_dashboard.rs`:

- Change offline HTML/JSON/CSV invocations to `explorer --dashboard ...` and remove `--window all`.
- The seeded old timestamp must still appear, proving all-time is the default rather than the former 24h window. Preserve assertions that the DB bytes are unchanged and no config/RPC is needed.
- Move dashboard help assertions to `explorer --help`; assert the retained flags and absence of `--window`, `--max-ops`, `--evidence`, `--budget-mb`, and `--with-gaps`.

Modify unit tests in `report.rs` / `backfill.rs`:

- Delete rolling-window/deduplication/leaderboard-cap/under-filled tests. Use an `all` fixture with no rolling coverage and retain CSV-section, empty-section, and warning tests.
- Add duration parsing/bounds tests in the backfill module, including `24h == 1d`, `2w`, zero/malformed/`all`/over-365d rejection. If span conversion is extracted, test the Arbitrum conversion as well.

### 8. Refresh current documentation, not historical evidence

Update all actionable examples and command maps in:

- `README.md`
- `docs/CLI.md` (command map, shared concepts, explorer flow diagram, report/index/dashboard sections, replay and baseline recipes)
- `docs/ARCHITECTURE.md` (read/write ownership and `explorer --dashboard` naming)
- `docs/LOCAL_AVALANCHE.md` and `docs/LOCAL_POLYGON.md` (index examples and window-related troubleshooting)
- `docs/explorer_strategy_checklist.md` (purge note described above)

Document that both report and dashboard are all-time views of an accumulating DB, not views limited to the most recent `index` invocation. Document `index --dashboard` as a one-shot post-backfill HTML snapshot. Leave dated benchmark/historical result logs unchanged; they record commands as originally executed.

## Verification

Run from the repository root:

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test -p mev-scout-cli --test cli_args --test cli_dashboard
cargo test --workspace
```

Then inspect the built help and offline behavior:

```bash
cargo run -p mev-scout-cli -- explorer --help
cargo run -p mev-scout-cli -- explorer index --help
```

Confirm removed forms fail and the offline integration tests produce valid HTML/JSON/CSV with `window/coverage` reflecting `all`, without mutating SQLite. If an archive RPC and disposable DB are explicitly available, optionally smoke-test one small bounded command such as `explorer index --window 1h --dashboard`; do not use production databases or broad backfills for this CLI check.

## Non-goals

- No changes to `live`, `discover`, `report`, configuration schemas, classifier/accounting logic, or explorer SQL query semantics.
- No database cleanup/migration and no execution of the removed purge against real data.
- No rewrite of the core dashboard renderer/reducer and no removal of its programmatic `BuildOptions` capabilities.
- No backwards-compatibility aliases for the removed CLI surface.