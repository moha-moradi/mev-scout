# mev-scout CLI — Simplification & Zero-Config Plan

Status: **proposed** (no code changed yet)

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
2. Pool cache empty → treat as `--blocks 2048` from the confirmed tip.

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

- **Implicit pool bootstrap** — if the pool cache is empty (or below a small
  threshold), `live` runs `discover` with its default range before streaming.
- **Implicit token bootstrap** — if the token cache is empty, populate it from
  the bundled known-token list (offline). No network.
- **Single-pass default** — with no `--loop`, process a bounded recent window
  once, print the opportunity table + aggregated summary, and exit 0.

**Target signature**

```text
mev-scout live [--loop] [--duration D] [--max-blocks N] [--initial-balance WEI] [--reserve WEI]
```

---

## 5. `explorer` — keep only the forensic path

Current subcommands: `index`, `stats`, `show`, `report`, `backfill`, `validate`.

| Subcommand | Verdict | Notes |
|---|---|---|
| `index [--duration D]` | **Keep** | Live, idempotent, resumable, reorg-aware ingestion. Opt-in only. |
| `backfill` | **Keep, tighten** | Required for 1d/7d/30d revenue windows. Exactly one of `--days` / `--from-block`+`--to-block`. |
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
| `--quiet` | **Consider removing** | Niche; log-level control via `RUST_LOG` already covers it. Drop if the surface must shrink further. |

---

## 7. Proposed final CLI surface

```text
mev-scout                                  # == live, single-pass, prints summary, exits 0
mev-scout --config FILE
mev-scout -v | --verbose

  discover [--incremental] [--blocks N | --block N | --from-block A --to-block B]
  live     [--loop] [--duration D] [--max-blocks N] [--initial-balance WEI] [--reserve WEI]
  report   [--run-id ID]
  config

  explorer
    index    [--duration D]
    backfill [--days N | --from-block A --to-block B]
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

1. Add the implicit subcommand (`#[command(subcommand)] Option<Command>` +
   default to `Live`) in `cli/src/cli.rs`; keep `Command::Live` unreachable as
   a *typed* default by constructing it programmatically.
2. Make `LiveArgs` single-pass the default: `--loop` gates the streaming loop;
   one-shot processes the default window and exits.
3. Add bootstrap hooks (`pool cache empty → discover`, `token cache empty →
   bundled list`) inside `commands/live.rs` / `commands/discover.rs`.
4. Make `BlockRangeArgs` optional on `DiscoverArgs` and implement the
   resolution rule (cache → incremental, else `--blocks 2048`).
5. Drop the removed flags from `cli.rs` and delete their `overrides.rs` and
   `commands/*.rs` handling; move `--max-fills-per-block`, `--native-usd`,
   `--initial-balance-usd`, `tolerance_pct` to config-only (already mapped in
   `overrides::build_overrides_from_command`).
6. Remove `explorer stats` and gate `explorer validate` behind a cargo feature
   (default off) rather than deleting the core implementation.
7. Update `docs/ARCHITECTURE.md` §CLI section + `README.md` examples.
8. Re-run `cargo clippy --workspace --all-targets` and `cargo test`.

---

## 9. Open decisions (need confirmation)

1. **Zero-arg behavior** — `mev-scout` = single-pass `live` over the last *N*
   blocks, or = catch-up-to-tip-then-stop? ("Desired initial result" differs.)
2. **First-run discover window** — `2048` blocks (better coverage, slower first
   run) or `256–512` (fast first result, thinner pool set)?
3. **Exact definition of "single-pass"** — from `tip - N` → `tip` once, or
   "resume from the last processed block up to tip, then stop"?
4. **`explorer backfill` with no range** — default to `--days 7` (zero-config
   usable) or hard error?
5. **Chain default** — which chain does the zero-config path assume, and what
   happens when no `rpc.url` is configured at all? (Should produce one clear
   error, not a degraded run.)
6. **`--quiet`** — drop, or keep as a first-class citizen?
