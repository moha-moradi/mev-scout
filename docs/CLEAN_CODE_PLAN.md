# Clean Code Improvement Plan — mev-scout

Status: Proposed
Scope: `core/` and `cli/` (~25k LOC Rust, workspace with 2 crates)
Method: Three parallel deep audits (error handling, duplication, architecture) + full `cargo clippy` run
Date: 2026-09-11

---

## Priorities at a glance

| # | Workstream | Type | Risk addressed | Effort |
|---|---|---|---|---|
| W1 | Remove committed API keys | Security | Critical | XS |
| W2 | Fix silent failure swallowing | Correctness | Critical | M |
| W3 | Unify arbitrage detectors | DRY | High | L |
| W4 | Split discovery module & parallelize scanners | Structure | High | M |
| W5 | Type-safety overhaul (tuples, fees, config enums) | Idiom | High | M |
| W6 | Clippy sweep + lint/CI enforcement | Enforcement | Medium | S |
| W7 | Comment hygiene | Clean Code | Medium | S |
| W8 | Test-suite cleanup | Clean Code | Medium | S |
| W9 | Latent-bug fixes | Correctness | Medium | XS |

Order: W1 → W9 → W6 → W2 → W7 → W8 → W5 → W4 → W3 (quick wins and enforcement first, so later refactors are gated).

---

## W1 — Remove committed API keys (Critical, XS)

**Problem:** Live Alchemy keys are committed in git-tracked configs while `mev-scout.example.toml` explicitly warns against it and the `${ENV_VAR}` expansion mechanism already exists (`core/src/config/settings.rs:293`).

- `mev-scout.toml:4-6` — 3 live Alchemy keys (comments attribute them to individuals)
- `mev-scout-arbitrum.toml:3` — 1 live key

**Steps:**

1. Rotate all exposed keys at the providers (committing history means rotation, not just removal).
2. Replace key-bearing URLs with `${ALCHEMY_API_KEY}`-style placeholders.
3. `git rm --cached` the live configs; add them to `.gitignore`; keep `mev-scout.example.toml` as the tracked template.
4. Purge history (`git filter-repo` or BFG) if the repo will ever be shared.
5. Also remove the committed debug artifact `core/debug-30d115.log` (not a fixture, referenced nowhere) and add `.gitignore` rules for `debug-*.log`/`*.log` so runtime logs never land in git again.

**Acceptance:** `rg "alch_|g\\.alchemy\\.com/v2/[A-Za-z0-9]{20,}"` returns nothing; live configs ignored; docs show env-var setup.

---

## W2 — Stop silent failure swallowing (Critical, M)

### 2.1 Config fallback must not be silent

- `core/src/config/settings.rs:325` — `Config::load().unwrap_or_default()`, used by `cli/src/main.rs:36-46` even for an **explicit `--config`** path. A malformed TOML silently runs with defaults (wrong chain/RPC/strategies, zero signal).
- **Fix:** Distinguish "file not found" (fallback OK, log at info) from "parse/validation error" (hard-fail with `ConfigError`).

### 2.2 "Not found" vs "broken" in the sig resolver

- `core/src/sigs/resolver.rs:68,97` — `.ok()` on `query_row` conflates "selector not in DB" with DB-corrupt/locked; then caches `None` **permanently** (lines 74, 103), silently degrading decode labels.
- **Fix:** Match `rusqlite::Error::QueryReturnedNoRows` explicitly; propagate other errors; never cache errors as misses.

### 2.3 Make discovery degradation visible

- ~15 `.ok()` sites in `core/src/pool/state/factory.rs` (578-593, 743, 904, 972-975, 1128, 1181-1182, 1480, 1550, 1559) and 8 in `core/src/pool/discovery/mod.rs:1215-1275` convert RPC failures into `None` metadata; `discovery/mod.rs:1569-1571` then silently never persists zero-token pools. A transient RPC outage permanently shrinks the pool universe.
- **Fix:** Route these through the existing `retry_call`, or at minimum count and `warn!` degraded pools per run so a flaky RPC is observable.

### 2.4 Other silent-drop sites

| Site | Problem | Fix |
|---|---|---|
| `core/src/cache/store/integrity.rs:28` | `.filter_map(\|r\| r.ok())` under-reports cache gaps | Propagate row errors |
| `cli/src/commands/validate_pools.rs:88-118`, `discover.rs:340-407` | Typo'd config address silently removes a DEX venue | Use `ConfigError::InvalidValue` (already defined, unused) |
| `core/src/explorer/store.rs:839` | Unparseable sender fabricated as `Address::ZERO`, corrupting attribution | Surface schema drift; do not fabricate |
| `core/src/explorer/store.rs:882-911`, `cache/store/mod.rs:358-383` | `.ok()` on column reads → silent `None` fields | Same treatment |
| `cli/src/commands/report.rs:120-138` | Chain of five `.ok()?` vanishes the explorer section of reports on any error | Log then skip section, or propagate |
| `core/src/mev/detectors/mempool.rs:28`, `liquidation.rs:72` | One failed RPC call silently skips the whole detection pass | Propagate or count |

### 2.5 Inconsistent panic-vs-ok on sync primitives

- `core/src/fetch/fetcher.rs:545` — `sem.acquire().await.expect(...)` **panics**
- `core/src/pool/state/factory.rs:308,563,691` — same operation `.ok()` → silently unbounded concurrency
- 15 mutex `.expect("poisoned")` sites (`sigs/resolver.rs`, `pool/state/manager.rs:155,240,274,824`, `fetch/fetcher.rs:251,602`, `cache/store/mod.rs:54`)
- **Fix:** Pick one policy. Recommended: recover from poisoned locks via `.unwrap_or_else(\|e\| e.into_inner())` (panic-while-locked must not cascade), and make closed-semaphore a typed unreachable error.

---

## W3 — Unify the arbitrage detectors (DRY, L)

**Problem:** `core/src/mev/detectors/two_hop.rs` (875 ln) and `multi_hop.rs` (1102 ln) share ~350-400 duplicated lines; a fix to one silently misses the other. They have already diverged.

**Verified duplication:**

| Concern | two_hop | multi_hop |
|---|---|---|
| `invert_monotone_quote` | :858-880 | :576-598 (byte-identical) |
| Dedup struct + `check_dedup_key` | :43-57 | :48-60 |
| FOT token filter | :148-150 | :303-305 |
| Profit normalization incl. native fallback | :172-190 | :351-366 |
| Slippage profits ±1%/±2% | :380-399 | :369-416 |
| Gas estimation + calibration blend | :890-929 | :976-1008 |
| Dominant DEX type | :918-926 | :1002-1008 |
| `MevOpportunity` construction | :195-224 | :418-447 |

Additionally, multi_hop internally repeats the "walk the path, quote pool-by-pool" loop **three times** (`quote_fn` :309-323, `eval_raw` :369-383, prefix closure :474-488).

**Plan:**

1. Extract shared module `mev/detectors/arb_common.rs`: opportunity builder (dedup + FOT filter + normalization + slippage + gas + `MevOpportunity` literal), `invert_monotone_quote`, breakpoint composer.
2. Keep **two thin front-ends** — do not merge blindly:
   - 2-hop analytical: preserve closed-form `optimal_two_hop_arb` (two_hop:245-249) and the integer spot-price prefilter `passes_spot_prefilter` (:708-736) — multi_hop has no equivalent; it eliminates ~90% of optimizer work in exact integer math.
   - N-hop numeric: Bellman-Ford cycles + `optimal_on_segments`.
3. Special-case `path.len() == 2` in the unified front-end to call the analytical solver; port the integer prefilter as an additional check for 2-pool cycles.
4. Preserve multi-token pool candidate selection (`arb_tokens`, two_hop:507-594) — multi_hop's naive `min()` non-shared-token pick is weaker.
5. Estimated net reduction: ~350 lines; one dedup/normalization/gas stack instead of two.

6. **PoolState polymorphism (kills the DEX combination matrices).** Beyond the shared helpers, push variant-specific quoting onto `PoolState` so the two God-matches collapse:
   - `two_hop.rs:244-376` `quote_path` — 14-arm `(PoolState, PoolState)` combination matrix.
   - `two_hop.rs:411-495` `two_hop_profit_at` — two stacked 8-arm matches.
   - Expose `quote_dir(shared_token)`, `reserve_pair(shared_token)`, `fee_denom()`, `token_pair()` methods on `PoolState`; a new DEX then adds one file + one method instead of new match arms.
   - Same migration retires the Law-of-Demeter chains and Feature Envy across detectors: `sandwich.rs` (~30 `pool_manager` accesses), `multi_hop.rs` (~40), `jit_arb.rs:117-119` (`pm.get(&addr).map(|p| p.info().token0)`).

7. **CLI DRY in the same pass** (and prefer `?`/`.context()` over `match { Ok => r, Err => bail }`, which destroys error chains):
   - `cli/commands/scan.rs:126-297` — the four `print_trades`/`print_transfers`/`print_flash_loans`/`print_liquidations` are ~170 duplicated lines; replace with one printer parameterized by a column spec (`json`/`csv`/`table`).
   - `cli/commands/live.rs:85-201` vs `:203-402` — `run_once`/`run_loop` duplicate ~80 lines of PoolManager/GasConfig/ResultsFile/RunManifest/persist setup; extract a shared setup fn (paired with the `LiveContext` proposal in W4).
   - `cli/display.rs:136-199,201-249` — the two near-identical table branches (`pool_manager.is_some()`, `has_pending`) → expression-driven (`let headers = if ...`); `format!("{}", x)` → `x.to_string()` (~40 sites).
   - `cli/commands/replay.rs:103-116` — five `else if t0 == keccak256(...)` chains → `match` on event topic.

**Acceptance:** golden tests from both detectors pass unchanged; a deliberately introduced profit-normalization bug fails both front-ends; the `quote_path`/`two_hop_profit_at` matrices are gone; the scanner/detector Demeter chains are collapsed.

---

## W4 — Split discovery, extract per-DEX logic, parallelize (Structure, M)

**Problem:** `core/src/pool/discovery/mod.rs` is 1826 lines with a ~630-line `discover_pools_shard` (:895-1524). Per-DEX batch scanners already exist as small files, but classify/metadata/health logic did not get extracted.

**Plan:**

1. Move into the existing per-DEX modules (`discovery/v2.rs` … `fluid.rs`), each becoming self-contained:
   - `fn classify(log) -> DexEventClass` (from `classify_dex_event`, mod.rs:602-700)
   - `fn fetch_metadata(rpc, addr) -> FetchTask` (from mod.rs:1204-1348 per-DEX match arms)
   - `fn health_probe(pool) -> (Address, Bytes)` / `fn decode_health(bytes)` (from mod.rs:1601-1736)
   - per-DEX default fees (mod.rs:1489-1504)
   - Balancer `getPool` decode (mod.rs:1027-1097), Curve `coins(i)` loop (:1099-1153)
   - Target: mod.rs shrinks to ~600-700 lines of orchestration.
2. Replace the 12 **sequential** `scan_*_batch` awaits (mod.rs:990-1001) with `join_all` — they currently serialize the whole batch on the slowest provider.
3. Replace the 7-8-parameter `scan_*_batch` signatures with a `ScanContext` struct (also resolves the clippy `too_many_arguments` sites).
4. Same treatment for other giants:
   - `cmd_discover` (cli, ~498 ln) → split along its existing numbered phases
   - `build_fallback_db` (`sigs/downloader.rs`, ~474 ln)
   - `init_from_rpc` (`pool/state/factory.rs`, ~293 ln) + `fetch_curve_state`/`fetch_balancer_state`/`refetch_pool_state` share a "decode static-call bytes into pool state" skeleton
   - `run_block` (`pipeline/runner.rs`, ~276 ln) → per-strategy dispatch
   - `retry_call_impl` (`rpc/client.rs`, ~225 ln) → retry policy vs response handling
5. **Parameter objects for the monsters (kill `#[allow(clippy::too_many_arguments)]`):**
   - `explorer/store.rs:565-588` — `insert_opportunity` (19 params) → `OpportunityRow` input struct.
   - `explorer/store.rs:391-404` — `insert_block_facts` (11 params) → `BlockFactsInput`.
   - `cli/commands/explorer.rs:614-622` — `cmd_validate`'s 7 positional params are literally the fields of `ValidateArgs` (destructured at `commands/mod.rs:115-126`); pass `&ValidateArgs`.
   - `cli/commands/live.rs:85-94,203-213` — `run_once`/`run_loop` (8-9 positional params) → shared `LiveContext`.
6. **`cli/commands/explorer.rs` (732 ln)** — split into submodules (`doctor.rs`, `index.rs`, `stats.rs`, `show.rs`, `validate.rs`, `export.rs`); several helpers sit below first use (`time_hhmmss` :320 vs :305, `block_time_secs` :253 vs :221, `block_window` :656 vs :628).
7. **Boundary: duplicated `chain_config → DiscoveryConfig` mapping** in `cli/commands/discover.rs:337-437` and `validate_pools.rs:88-122` → `DiscoveryConfig::from_chain_config()` in core.
8. **`BacktestRunner` checkpoint cost** — `pipeline/runner.rs:916,1055` deep-clones the entire `PoolManager` per block; replace with `Arc<PoolState>` snapshots or a delta undo-log (largest heap copy in the hot loop).

**Acceptance:** adding DEX #16 touches exactly one new file + one registry line; no function over ~120 lines in the touched modules; zero `#[allow(clippy::too_many_arguments)]` remain in the touched files.

---

## W5 — Type-safety overhaul (Idiom, M)

### 5.1 Named structs instead of positional tuples

| Current | Location | Replace with |
|---|---|---|
| `PoolHits = HashMap<Address, (DexType, Option<[u8;32]>, Option<(Address,Address)>, u64)>` | discovery/mod.rs:28 (re-spelled raw at :345, :857, :905) | `PoolHit { dex_type, pool_id, tokens, first_seen_block }` |
| 7-element boxed-future tuple `FetchTask` | discovery/mod.rs:1164 | `PoolMetadataFetch { addr, dex_type, token0, token1, fee, tick_spacing, first_seen_block }` |
| `classify_dex_event` return 4-tuple | discovery/mod.rs:602-604 | `DexEventClass { …, addr_override }` |
| `SwapDeclaration` 5-tuple (6 push sites, wrong-position push compiles fine) | pool/state/manager.rs:26, 368-459 | named struct |
| `Vec<(usize, u64, u64)>` provider shards | rpc/client.rs:610 | `ProviderShard { idx, from, to }` |
| `Vec<(String, Option<f64>, bool)>` provider configs | config/settings.rs:368 | `ProviderConfig { url, rps, archive }` |
| `Arc<Vec<(Address, Address, Address)>>` pairs cache | manager.rs:66 | named struct |
| `PersistenceKey` 5-tuple (rebuilt by hand at runner.rs:843-849) | pipeline/runner.rs:37 | `PersistenceKey { strategy, pool_a, pool_b, token_in, token_out }` (derive `PartialEq`/`Hash`) |
| 7-element `pool_meta` tuple (factory.rs:241-287, re-derived in every dispatch match) | pool/state/factory.rs:241-287 | reuse `PoolState::info()`; delete the tuple |
| 6-param `optimal_two_hop_arb` | pool/math/core.rs:183-190 | `PoolQuote { reserve_in, reserve_out, fee }` |

### 5.2 Fee newtype — kill the bps/ppm overload (hazard)

`PoolInfo.fee: u32` (`pool/state/pool_types.rs:80`) stores **basis points** for V2/Solidly/LB (30) but **parts-per-million** for V3/Balancer/Curve (3000); the unit is only implied by which match arm reads it (`apply.rs:72-75` vs `core.rs:148`). `fee == 0` is used as an "unset" sentinel (`discovery/mod.rs:263`, `factory.rs:542`).

**Fix:** `enum FeeTier { Bps(u32), Ppm(u32), Unset }` (or `FeeBps`/`FeePpm` newtypes). Mechanical but eliminates the most dangerous silent-corruption path in the codebase.

### 5.3 Constants cleanup

- Add `Q96` and `Q64` to `pool/math/consts.rs` (currently inline at v3.rs:87,89,125,148,170,196 and lb.rs:26,31)
- Use `PPM_DENOMINATOR` at v3.rs:230,256 (same file already uses it at :410)
- Deduplicate Newton epsilon `1e-30` (stable_swap.rs:30,78; curve.rs:240,295)
- **Bug:** `balancer.rs:66-70` — `as_limbs()[0] as f64` reads only the low 64-bit limb of U256 reserves; values > 2^64 silently misquote. Fix before any reserve grows.

### 5.4 De-stringlify config and chain metadata

- `Config.chain: String` → `ChainName` (enum exists, `types/chain.rs:45`, strum+serde ready); same for `gas_model: GasModel`, `flash_loan_provider: FlashLoanProvider`, `strategies: Vec<Strategy>`, `output: OutputFormat` — all enums already exist.
- `ChainConfig`'s 9 `Option<String>` address fields → `Option<Address>` (alloy `Address` implements serde).
- **Three disagreeing chain-timing tables** — unify into one `ChainName → timing` source:
  - `chain/timing.rs:14-29` (Polygon 1.5s)
  - `cli/commands/explorer.rs:253-258` `block_time_secs` (Polygon 2s, BSC 1s)
  - `replay/replayer.rs:76-91` hardcoded `137` at 5 sites
- `CliOverrides` mirror structs + `merge_sub!` lists (settings.rs:524-579, 636-667): a field added to both structs but forgotten in the merge list **compiles and silently never applies**. Consider a derive/macro that generates the merge from the struct definition.
- `ExplorerStore`: `ops_for_tx(&str)` → `B256`, `chain: &str` params → `ChainName`, `run_id` newtype if useful.

### 5.5 One canonical `infer_dex_type` (correctness hazard)

`dexscreener.rs:323-381` and `geckoterminal.rs:438-473` are **divergent implementations of the same classification** (e.g. dexscreener maps aerodrome→Solidly unconditionally; gecko checks v3/slipstream first). The same pool can get different `DexType`s depending on source, silently poisoning or dropping it.

**Plan:** introduce `trait RemoteSource` in `remote/mod.rs` with one shared `get_with_retry` (the two copies of 429-backoff logic differ only by error prefix), one `parse_addr`, one `is_unsupported_dex`, one **canonical** `infer_dex_type`, one pagination helper (also kills gecko's internal ~35-line self-duplication and the 5 copies of the TVL comparator). Net ~200 lines removed.

### 5.6 Visibility tightening

- `SqliteStore::conn()` (`cache/store/mod.rs:53`) publicly hands out `MutexGuard<rusqlite::Connection>` — any caller can hold it across an `.await` and stall the runtime. Encapsulate behind methods; never expose guards.
- 463 top-level `pub` items for a crate whose only consumer is `cli`; make the 13 event-topic statics (discovery/mod.rs:33-116), `pool/math/mod.rs` internal iteration-bound consts, and internal `ExplorerStore` accessors `pub(crate)` following the pattern already established elsewhere.
- **`BacktestRunner` leaks to the CLI:** `pool_manager` (runner.rs:58) and `last_processed_block` (runner.rs:80) are public fields mutated from `cli/commands/live.rs:189,352,387`. Encapsulate: private fields + `pool_manager()` accessor + `runner.advance_to(block)`.

### 5.7 Boolean blindness — replace blind bool params with enums or split methods

- `rpc/client.rs:359,382,520,540,562,575,599,1271-1276` — `archive_only: bool` → `AnyProvider`/`ArchiveOnly` enum, or split `call_at_archive()`.
- `pool/math/v3.rs:321,354,387` — `zero_for_one: bool` → `ZeroForOne`/`OneForZero` enum.
- `pool/state/manager.rs:136` — `set_use_latest(bool)` → named `use_latest()` (call sites read `set_use_latest(true)`, e.g. `cli/commands/live.rs:101,217`).
- `cli/commands/explorer.rs:614` — `cmd_validate(..., threshold_sweep: bool, emit_missing_pools: bool, json: bool)` → dies with the `&ValidateArgs` refactor (W4.5); audit remaining bare `false` args like `explorer.rs:573`.

### 5.8 `RangeSpec` for block ranges

`cli.rs:256-278` `BlockRangeArgs` is five mutually-exclusive `Option`s whose "exactly one required" invariant cannot be expressed at the type level, so every consumer calls the 5-arg positional `validation::resolve_block_range(days, blocks, block, from, to)` (`fetch.rs:26-35`, `scan.rs:17-24`, `discover.rs:176-222`). Replace with `enum RangeSpec { Days(u64), Blocks(u64), Block(u64), FromTo(u64, u64) }` + a `resolve()` method that owns the invariant.

### 5.9 Detection-path stringly literals

The `"replay"`/`"pending"` string literals that classify detection paths (`two_hop.rs:223`, `multi_hop.rs:446`, `sandwich.rs:523`, `jit.rs:332`, `jit_arb.rs:278`, `liquidation.rs:511,574`, `mempool.rs:89`, `runner.rs:609,798`) → a `const REPLAY_PATH`/`const PENDING_PATH` or a `DetectionPath` enum.

---

## W6 — Clippy sweep + enforcement (Enforcement, S)

**Current state:** 71 lib warnings + ~40 test/example; no `clippy.toml`, no `[lints]`, no CI (`.github/` absent), no `rust-toolchain.toml`, no `[workspace.dependencies]` (dep versions duplicated across crates, e.g. rusqlite declared twice).

**Steps:**

1. `cargo clippy --fix` for the ~45 auto-suggestions (needless `as u128` casts, `div_ceil`, `sort_by_key`, redundant `&*row` refs, `bool::then` closures, useless `format!`, `.first()` vs `.get(0)`).
2. Manual fixes: `too_many_arguments` (10 sites — mostly subsumed by W4/W5), `very_complex_type` (7 sites — subsumed by W5.1), `len_without_is_empty` (`chain/labels.rs:51`, `mev/detectors/liquidation.rs:95`).
3. Add to root `Cargo.toml`:
   ```toml
   [workspace.lints.rust]
   warnings = "deny"   # or allow-list first, then ratchet
   [workspace.lints.clippy]
   unwrap_used = "deny"        # scoped with #[allow] for LazyLock/actors if needed
   expect_used = "deny"
   cast_possible_truncation = "warn"
   ```
   and `[workspace.dependencies]` for shared deps.
4. `rust-toolchain.toml` pinning the toolchain; `rustfmt` already-clean (keep it that way).
5. GitHub Actions workflow: `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`.

**Rationale:** without this workstream, every fix in W2-W5 regresses.

---

## W7 — Comment hygiene (Clean Code, S)

- **36 doc comments** reference "plan §…" (e.g. `cli/display.rs:36`, `core/src/chain/events.rs:72`, `explorer/classify.rs:1`) — a design document that is **not in the repo**. Either commit the plan doc under `docs/` or rewrite comments to be self-contained.
- "on-chain verification deferred like Q6/Q10/Q11" markers (discovery/mod.rs:106-107, pool/decoders.rs:62-63, chain/events.rs:76, pipeline/scanner.rs:56) — untracked deferred-work TODOs; convert to tracked TODOs with issue refs or remove.
- `explorer/classify.rs:122-124` — dead loop iterating transfers then `let _ = addr;` discarding them; the comment promises "addresses with ≥2 swap participations also qualify" but the logic does not exist. Delete or implement.
- `explorer/validate.rs` / others: keep the good practice — most module docs are excellent; only fix the misleading ones.
- **Mojibake/encoding artifacts** in doc comments: `core/src/pool/state/factory.rs:35,1131,1331,1556` (mojibake like `�?�`, `??�??`) — repair the source encoding (UTF-8 corruption from an earlier edit).
- **Vertical distance:** constants/helpers defined far from use — `core/src/pipeline/runner.rs:30-37` (used at :834), `core/src/mev/detectors/two_hop.rs:828-829` (`BPS_FEE_DENOM` defined below first use at :819), `cli/commands/explorer.rs` (see W4.6). Move near use sites or group in a per-module `consts` block.
- **Misleading names (cosmetic):** `cli/commands/report.rs:125` — `has_ops` is inverted (`true` when ops list `is_empty()`) → rename `no_ops` or invert the check; `cli/commands/live.rs:92` — `_args` is actually used (drop the underscore); `format!("{}", x)` → `x.to_string()`.

---

## W8 — Test-suite cleanup (Clean Code, S)

**Current state:** 253 test functions (healthy), but:

- **~25 dead test helpers** generating warning noise that drowns real lints: `cli/tests/common/mod.rs` (`BIN`, `RPC_MUTEX`, `rpc_lock`, `TEST_TIMEOUT`, `NETWORK_TIMEOUT`, `HEAVY_TIMEOUT`, `combined`, `repo_config_text`, `expect_fail`, `strip_ansi`, `extract_json_array`, `first_rpc_url`, `rpc_ready`, `ensure_gate_and_rpc`, `repo_config_str`, `make_cfg`, `scout`, `expect_ok`) and `core/tests/common/setup.rs` (13 never-used fns). Delete or wire them up.
- **Tests read `mev-scout.toml` as fixture** (`cli/tests/common/mod.rs:35-40`) — the file with live keys. Decouple: tests should use the example config or inline TOML strings (works together with W1).
- Poisoned-lock handling: adopt the `rpc_lock()` pattern (`.unwrap_or_else(|e| e.into_inner())`) where tests take locks.
- Items after test modules (`manager.rs:515`, `v3.rs:654` empty line after doc comment) — hygiene.

**Acceptance:** `cargo test --workspace --no-run` produces zero warnings; no test depends on untracked config files.

---

## W9 — Latent-bug fixes (Correctness, XS)

Found by clippy's `if_same_then_else` — two dead conditions that hint at intended-but-unimplemented behavior:

1. `cli/src/commands/live.rs:39-44` — `if loop_enabled { Ok(None) } else { Ok(None) }` in `deadline_from`. Both branches identical. Determine intent: likely one branch was meant to error or set a no-deadline default differently; else collapse.
2. `core/src/pool/state/factory.rs:1075-1079` — sign-extension if/else for negative-word tick compression assigns `compressed_tick` in both branches. Either implement proper sign handling or delete the conditional.

3. `core/src/pool/discovery/mod.rs:541` — `last_err.expect(...)` is safe only while `MAX_RETRIES >= 1`; make it a typed error so a retry-loop refactor cannot panic.
4. `core/src/config/validation.rs:203` — `_ => unreachable!()` inside the provider `is_forced()` match: a fifth provider variant panics at runtime instead of surfacing `ConfigError`. Replace with an exhaustive match returning an error.
5. `cli/commands/explorer.rs:355` — `let _kind_filter = kind.and_then(MevKind::parse);` is dead code: `explorer stats --kind` is parsed but never applied, silently ignoring the user's filter. Wire it into the `cmd_stats` queries or remove the flag.
6. `cli/commands/explorer.rs:280` — `let _ = stop_flag_with_deadline(deadline);` spawns the Ctrl+C deadline tasks but the returned stop flag is never checked in `cmd_live_feed`'s loop body (288-316); Ctrl+C never stops the feed gracefully and the listener task leaks. Check the flag per iteration and `break`.
7. **Selector drift — two different ABI signatures under one name.** `core/src/pool/state/factory.rs:29` hardcodes `balances(int128)` = `[0x49,0x7b,0x66,0x78]` while `core/src/pool/discovery/mod.rs:144-148` computes the keccak of `balances(uint256)`; the same Curve pool can be quoted/classified differently depending on which module sees it. And `PENDLE_READ_STATE_SELECTOR` (`factory.rs:130-134` vs `discovery/mod.rs:119-123`) plus `INF_CL_SLOT0_SELECTOR` (`factory.rs:60-64` vs `discovery/mod.rs:75-79`) are each declared twice. Extract a single `pool/selectors.rs` with one canonical definition per ABI signature, and make the Curve call sites agree on one signature.

---

## Explicitly out of scope (recorded decisions)

- `Arc<Mutex<Connection>>` single-connection SQLite design (W5.6 fixes the API leak; a full write-channel/pool redesign is a performance project, not clean code).
- Sequential→concurrent block fetch semantics beyond W4.2 (behavior change, needs benchmarking).
- `core/data/` → `core/assets/` rename (nice-to-have; breaks include paths for little gain).
- `.gitignore`'s broad `*.txt` rule (cosmetic).

---

## Definition of done (per workstream)

- W1: no keys in tree or history; example config documented
- W2: explicit list of `.ok()`/`unwrap_or_default` sites each either propagate, count+warn, or have a written justification comment
- W3: two_hop + multi_hop golden tests pass against shared builders; line count of `arb_common.rs` + both front-ends < current two files by ≥300; `quote_path`/`two_hop_profit_at` matrices removed; CLI printers and live-setup blocks deduplicated
- W4: no function >120 lines in discovery; scanners run concurrently; DEX additions are one-file; `insert_opportunity`/`insert_block_facts`/`cmd_validate` take parameter objects; `cli/explorer.rs` split; checkpoints no longer deep-clone `PoolManager`
- W5: `rg "Option<(Address, Address)>|Vec<\(.*u64.*u64\)>" core/src` empty in touched modules; fee unit explicit at type level; three chain-timing tables are one; no blind bool params remain in touched modules; `BlockRangeArgs` is a `RangeSpec`; `BacktestRunner` fields private
- W6: `cargo clippy --workspace --all-targets -- -D warnings` green in CI
- W7: zero external-doc references; every comment describes behavior that exists
- W8: zero warnings compiling tests; fixtures self-contained
- W9: both dead branches resolved (fixed or deliberately collapsed with comment); `_kind_filter` and live-feed stop-flag fixed; `validation.rs` has no `unreachable!`; selectors deduplicated with one canonical ABI signature each
