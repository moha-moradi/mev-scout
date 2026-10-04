# mev-scout CLI — Simplification & Zero-Config Plan

Status: **implemented**, with one later default change: `discover` /
bootstrap now default to **`hybrid` + `min_tvl = 25000`** (explorer-like
ranking) instead of onchain-only. `--source onchain` remains the offline
escape hatch. Where this document and the code disagree, the code wins.
User-facing command docs live in [`ARCHITECTURE.md`](./ARCHITECTURE.md) and
[`README.md`](../README.md).

Goal: shrink the `mev-scout` command surface to the smallest set that still
delivers the project's core objective — *detect MEV opportunities, record them,
review them* — while making a bare `mev-scout` invocation produce a useful
result with **no configuration, no flags, and no network dependency beyond the
chain RPC**.

References: CLI definition in [`cli/src/cli.rs`](../cli/src/cli.rs), dispatch in
[`cli/src/commands/mod.rs`](../cli/src/commands/mod.rs), binary entrypoint in
[`cli/src/main.rs`](../cli/src/main.rs). Existing CLI docs live in
[`ARCHITECTURE.md`](./ARCHITECTURE.md).

---

## 1. Zero-config principles

1. **Single default entrypoint** — running `mev-scout` with no subcommand runs
   the core use case (`live`), once, and exits with a summary.
2. **Sensible hard defaults** — every currently-required argument becomes
   optional with a safe default (recent blocks, onchain-only, no external HTTP).
3. **Implicit bootstrap** — if the pool/token caches are missing, `live` and
   `discover` populate them automatically instead of requiring an explicit
   `tokens` / `discover` step.
4. **Offline-first** — no third-party HTTP in the default path (GeckoTerminal,
   DefiLlama, CoinGecko are all opt-in or removed).
5. **Fail loudly but locally** — a missing/bad RPC URL errors immediately with a
   single actionable message, not a cascade of "unknown chain / no pools" noise.

---

## 2. Main commands — verdicts

| Command | Verdict | Rationale |
|---|---|---|
| `live` | **Keep, minimal** | The engine: stream blocks, detect opportunities, run the paper ledger. Also the zero-config default entrypoint. |
| `discover` | **Keep, minimal** | Builds the pool set that detection depends on. Range becomes optional. |
| `report` | **Keep, thin** | Re-renders a recorded run from SQLite. Offline, already zero-config. |
| `config` | **Keep** | Cheap, prints the fully-resolved config — the primary debugging aid for defaults. |
| `tokens` | **Remove as top-level** | Auxiliary cache population. Fold into implicit bootstrap; optionally keep as `discover --tokens-only` if an explicit entry point is still wanted. |
| `explorer` | **Keep but shrink** | Forensic value is real; the subcommand set is too broad (see §5). Stays strictly opt-in. |

---

## 3. `discover` — trim aggressively

Current flags: `BlockRangeArgs` (exactly one required), `--incremental`,
`--source`, `--enrich`.

| Change | Default | Rationale |
|---|---|---|
| `--source {onchain,remote,hybrid}` → **remove** | onchain only | Remote/Hybrid add HTTP, rate limits and an extra address-space to reason about. Onchain RPC events match "scan from chain state". |
| `--enrich` (GeckoTerminal TVL/volume) → **remove** | n/a | Presentation metadata only; couples the CLI to a free 3rd-party endpoint. |
| `--days` → **remove** | n/a | Ambiguous against "confirmed tip"; `--blocks` covers the same need. |
| `--blocks`, `--block`, `--from/--to` → **keep** | `--blocks 2048` | Precise control retained; the default covers the zero-config case. |
| `--incremental` → **keep, but make it the implicit behavior** | auto when cache is non-empty | Prevents full rescans with zero user effort. |
| Range becomes **optional** | see above | `discover` with zero args must work. |

**Resolution rule when no range flag is given:**

1. Pool cache non-empty → treat as `--incremental` (resume from highest
   `creation_block`).
2. Pool cache empty → scan the chain's `pool_discovery_lookback_blocks`
   (1000 in `core/data/chains.toml`) ending at the tip. The original draft
   used a hardcoded `--blocks 2048`; the per-chain lookback is what shipped.

**Target signature**

```text
mev-scout discover [--incremental] [--blocks N | --block N | --from-block A --to-block B]
```

---

## 4. `live` — trim ledger/price flags, add bootstrap

| Flag | Verdict | Notes |
|---|---|---|
| `--loop` | **Keep** | Core toggle: continuous vs one-shot. |
| `--duration <D>` | **Keep** | `humantime` parsing stays (already in `cli/src/cli.rs`). Requires `--loop`. |
| `--max-blocks <N>` | **Keep** | Bounded runs / smoke tests. Requires `--loop`. |
| `--initial-balance <WEI>` | **Keep** | Exact, offline, maps to `[paper].starting_gas_wei`. |
| `--reserve <WEI>` | **Keep** | Risk control; keep the one-shot override. |
| `--max-fills-per-block <N>` | **Remove from CLI** | Internal tuning knob vs the hard cap of 32. Belongs in `[paper]` config. |
| `--initial-balance-usd <USD>` | **Remove** | Needs price resolution, conflicts with the wei flag, moves conversion logic into the CLI. Config-only if still needed. |
| `--native-usd <PRICE>` | **Remove** | Pure price injection; belongs in `[price]`/`[paper]` config. |

**New behaviors**

- **Implicit pool bootstrap** — if the pool cache is empty (`pool_count == 0`),
  `live` runs on-chain `discover` over the chain lookback before scanning.
  A non-empty cache is left alone, even when it is small.
- **Implicit token bootstrap** — if the token cache is empty, populate it from
  the bundled known-token list (offline). No network. There is no
  `discover --tokens-only`.
- **Single-pass default** — with no `--loop`, scan `tip − N + 1 ..= tip`
  once (`--blocks`, default 64), print the opportunity table + ledger
  summary, and exit 0. `--duration` and `--max-blocks` require `--loop`.

**Target signature**

```text
mev-scout live [--loop] [--duration D] [--max-blocks N] [--blocks N]
               [--initial-balance WEI] [--reserve WEI]
```

---

## 5. `explorer` — keep only the forensic path

Current subcommands: `index`, `stats`, `show`, `report`, `backfill`, `validate`.

| Subcommand | Verdict | Notes |
|---|---|---|
| `index [--duration D]` | **Keep** | Live, idempotent, resumable, reorg-aware ingestion. Opt-in only. |
| `backfill` | **Keep, tighten** | Required for 1d/7d/30d revenue windows. `--days` or `--from-block`+`--to-block`; no flags defaults to `--days 7`. |
| `show <TX_HASH>` | **Keep, minimal** | Drop `--tolerance-pct` (belongs in `[explorer]` config). Keep `--trace` — on-demand `debug_traceTransaction` profit recompute is high-value. |
| `report` | **Keep, minimal** | `--windows` (default `1d,7d,30d`), `--kind`, `--top` (default `10`) already have good defaults; no-arg invocation works. |
| `stats` | **Remove** | Redundant with `explorer report` for almost all users (windowed revenue + breakdowns). |
| `validate` | **Remove from CLI** | Heaviest surface (`--match-window`, repeated `--run-id`, `--threshold-sweep`, `--emit-missing-pools`, `--review-csv`, `--golden-causal`, `--json`). Excellent research tooling, orthogonal to "use the scanner". Keep the library API; hide the CLI behind a feature flag or move to a dev-only binary. |

**Explorer remains strictly opt-in** — never bootstrapped from `live`.

---

## 6. Global flags

| Flag | Verdict | Notes |
|---|---|---|
| `-f, --config <FILE>` | **Keep** | Standard. |
| `-v, --verbose` | **Keep** | Debug path; default stays `info`. |
| `--quiet` | **Keep** | Sets the tracing filter to `error`. Program tables and summaries on stdout are unchanged. |

---

## 7. Proposed final CLI surface

```text
mev-scout                                  # == live, single-pass over the last 64 blocks, exits 0
mev-scout --config FILE
mev-scout -v | --verbose
mev-scout --quiet

  discover [--incremental] [--blocks N | --block N | --from-block A --to-block B]
  live     [--loop] [--duration D] [--max-blocks N] [--blocks N]
           [--initial-balance WEI] [--reserve WEI]
  report   [--run-id ID]
  config

  explorer
    index    [--duration D]
    backfill [--days N | --from-block A --to-block B]   # no range → --days 7
    show     <TX_HASH> [--trace]
    report   [--windows 1d,7d,30d] [--kind KIND] [--top 10]
```

Removed: `tokens` (top-level), `discover --source/--enrich/--days`,
`live --max-fills-per-block/--initial-balance-usd/--native-usd`,
`explorer stats`, `explorer validate`, `explorer show --tolerance-pct`
(→ `[explorer]` config), `live --duration/--max-blocks` `requires` coupling stays.

**Net effect:** 6 top-level commands → 4 (+ the explorer nest), 6 explorer
subcommands → 4, and a zero-argument `mev-scout` that works.

---

## 8. Implementation order

All eight steps landed:

1. Implicit subcommand: `Option<Command>` plus `Cli::command_or_default()`
   builds `Command::Live(LiveArgs::default())`.
2. `LiveArgs` is single-pass unless `--loop`. `--blocks` (default 64) is the
   one-shot window. `--duration` and `--max-blocks` require `--loop`.
3. Bootstrap lives in `cli/src/commands/live.rs` (tokens from the bundled
   list, pools via on-chain discover). `discover` implies `--incremental`
   when no range is given and the pool cache is non-empty.
4. `BlockRangeArgs` is optional. Empty cache uses
   `pool_discovery_lookback_blocks`, not a hardcoded 2048.
5. Removed flags are gone from clap. `[paper].max_fills_per_block` and
   `[paper].starting_gas_wei` are the config homes for fill cap and wallet
   seed. `--initial-balance-usd` and `--native-usd` were not given new config
   keys; USD columns use a resolved native price when one is already cached
   or fetched for display. Show-gate tolerances are
   `[explorer].trace_tolerance_pct` and `[explorer].mev_tolerance_pct`.
6. `explorer stats` is gone. `explorer validate` is behind the `validate`
   Cargo feature (default off). The library API stays.
7. `docs/ARCHITECTURE.md` and `README.md` describe this surface.
8. Clippy and tests were part of the implementation; re-run them when the
   surface changes again.

---

## 9. Decisions

1. **Zero-arg behavior** — `mev-scout` is single-pass `live` over the last 64
   blocks (`tip − 63 ..= tip`), then exit.
2. **First-run discover window** — the chain's
   `pool_discovery_lookback_blocks` (1000), not 2048 and not 256–512.
3. **Single-pass** — `tip − N + 1 ..= tip` once. It does not resume from the
   last processed block. `--loop` is the continuous path.
4. **`explorer backfill` with no range** — trailing 7 days
   (`DEFAULT_BACKFILL_DAYS`).
5. **Chain default** — Polygon (`ChainName::Polygon`), with the built-in
   public RPC list from `default_chains()`. A bad or empty RPC URL fails in
   `validate_live` before bootstrap.
6. **`--quiet`** — kept.
