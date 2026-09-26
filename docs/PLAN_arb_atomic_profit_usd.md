# Fix plan — `arb_atomic` absurd `profit_usd`

**Goal:** stop `arb_atomic` from reporting impossible P&L. Live run on Ethereum
block `26059586` reported **`$394,674,797,029,045.31`** for 3 atomic-arb ops in a
single block; over a 61-block window it reported **`$491,926,042,584,970.31`**.
Real atomic arbitrage on these pools is tens to low thousands of dollars.

**Status:** planned, not started. Nothing in this plan is implemented.

**Decisions already made (do not re-litigate):**

- Unpriced residual tokens → **decimals-aware and bounded, else `None`**. Prefer
  under-reporting a missing number over inventing a wrong one. This deliberately
  reuses the pattern already proven for liquidations at `core/src/explorer/store.rs:186-191`.
- Route-leg reliability **will** be addressed at the source, not only defended
  against downstream.

---

## Root cause (confirmed, not guessed)

The observed number decomposes exactly:

```
394674797029045.31  ~=  3000 * (1.3156e17 / 1e6)
                        ^^^^    ^^^^^^^^^^^^^^^
                    real leg   raw ratio, ~1.3e11 off
```

The `1e6`-vs-`1.3e17` gap is a 6-decimal vs 18-decimal mismatch. Two independent
defects compose into the amplification:

1. **Untrusted leg labels.** `attach_swap_tokens`
   (`core/src/explorer/decode.rs:923-1042`) can relabel `token_in`/`token_out`
   *after* the decoder already set `amount_in`/`amount_out`. The ±24-log proximity
   fallbacks (`:997-1018`, `:1019-1040`) bind a token from any nearby transfer with
   no guarantee it is the same side as the recorded amount. `SwapFact`
   (`core/src/explorer/types.rs:199-216`) carries no record of *how* direction was
   resolved, so nothing downstream can tell a registry fact from a guess.
2. **The pricing fallback trusts that label.** `amount_usd_realized`
   (`core/src/explorer/pricing.rs:68-103`) returns on the **first** leg mentioning
   the token — no reliability check, no bound, no best-match.

An important correction to the earlier working hypothesis: **the fallback formula
is dimensionally correct.** `out_usd * amount / amount_in` divides two raw
quantities of the *same* token, so decimals cancel and no decimals plumbing is
needed. The arithmetic is not the bug; the **inputs are untrustworthy**. Keep the
formula, fix its inputs.

Contributing (not causal) — the result is never sanity-checked anywhere:

- `core/src/explorer/store.rs:727` only checks `total > 0.0` (lower bound).
- `core/src/explorer/store.rs:785-788` computes `net_profit_usd` with no upper bound.
- No `is_finite` check on any pricing result in the repo.

### Why the fallback is hit so often

`event_tokens` (`core/src/explorer/ingest.rs:484-506`) warms prices for only the
single `e.profit_token`, while `store.rs:714-726` sums USD across **all**
`e.profit_tokens`. Every non-primary residual token is therefore *guaranteed*
unpriced and *guaranteed* to take the untrusted fallback path.

---

## Steps

### 1. Record leg direction provenance (new, additive)

- Add `pub token_source: LegSource` to `SwapFact`
  (`core/src/explorer/types.rs:199`) with variants `Registry`, `Transfer`, `Proximity`.
- Set it in `attach_swap_tokens` (`core/src/explorer/decode.rs:923-1042`):
  - `Registry` — resolved via `pool_tokens` (`:935-952`)
  - `Transfer` — bound by nearest before/after transfer (`:987-992`)
  - `Proximity` — either ±24-log fallback (`:997-1040`); also the default for
    `Balancer`, which carries explicit tokens from topics
- Emit `"token_source"` into the route JSON in
  `core/src/explorer/classify.rs:282-289`.

### 2. Make pricing trust only trustworthy legs

In `amount_usd_realized` (`core/src/explorer/pricing.rs:68-103`):

- Skip any leg whose `token_source` is absent or `Proximity`. A missing key means
  legacy/unknown, so this is backward-safe.
- Replace first-match-wins with best-match: evaluate all trusted legs and keep the
  most representative (largest trusted-leg USD), so a single dust hop cannot define
  the token's price.

### 3. Bound the estimate against the route's own scale

- Compute the total USD actually observed across trusted, priced legs of the route.
- Clamp each residual's fallback estimate to that total: a residual token cannot be
  worth more than the entire route it rode on. This makes the amplification
  structurally impossible rather than merely unlikely.
- Reject non-finite results.
- Mark a clamped result as approximate via the existing `merge_details_json`
  pattern (`store.rs:729-741`), adding a `pricing_clamped` reason.

### 4. Stop the fallback from being the common path

- Extend `event_tokens` (`core/src/explorer/ingest.rs:484-506`) to enumerate
  `e.profit_tokens` in addition to `e.profit_token`, so every residual gets a real
  external price. This alone removes most fallback invocations and is the change
  that preserves real data.
- Native residuals (`NATIVE_MARKER`) are currently filtered out of warming and so
  contribute a silent `0.0` via `.unwrap_or(0.0)`. Surface that as an explicit
  reason instead of folding it in invisibly.

### 5. Return `None` instead of guessing when untrusted

- When no trusted leg exists, return `None` and record `MULTI_ASSET_PRICING`,
  mirroring `store.rs:186-191`.
- At the `store.rs:714-726` sum, stop collapsing `None` to `0.0`. If **any**
  residual is unpriced, mark the event's USD approximate rather than silently
  under-reporting a partial sum as if it were complete.

### 6. Harden the V2 / Solidly decoders (latent, lower priority)

`core/src/explorer/decode.rs:93-94` and `:336-337` use
`amount_in = a0i.max(a1i)` and `amount_out = a0o.max(a1o)` as two independent
`max()` calls. When both `amount0In` and `amount1In` are non-zero (flash swap, or a
router doing two hops through one pool in a single tx), `amount_in` still follows
the chosen direction but `amount_out` may belong to the *opposite* side. Derive both
sides from one direction decision instead.

Note: the V3/V4/Infinity decoders (`:117-127`, `:150-159`, `:183-192`) are already
decimals-safe — they use the sign of `amount0`/`amount1`, not a magnitude
comparison. V2/Solidly are the only ones affected.

---

## Tests

**New invariants — none of these exist today:**

- `core/src/explorer/pricing.rs` — regression reproducing the blowup: an 18-decimal
  profit token with a `Proximity`/missing-source leg must yield `None`, not
  `$3.9e14`. Pin the exact `394674797029045.31` shape so the bug cannot return.
- `core/src/explorer/pricing.rs` — two trusted legs of differing size: the larger
  wins (proves best-match, not first-match).
- `core/src/explorer/pricing.rs` — clamp case: residual estimate exceeding the
  route's USD total is capped and flagged approximate.
- `core/src/explorer/decode.rs` — `token_source` is `Registry` for a known pool,
  `Proximity` for the ±24-log fallback.
- `core/src/explorer/classify.rs` — route JSON carries `token_source`.
- `core/src/explorer/ingest.rs` — `event_tokens` includes every `profit_tokens`
  residual, not just the primary.

**Existing tests that lock in current behaviour and must be updated:**

- `core/src/explorer/pricing.rs:263-311` (`realized_rate_prices_unpriced_profit_token`)
  and `core/src/explorer/store.rs:2490-2582`
  (`fot_profit_token_flagged_approximate_and_realized_rate_falls_back`) both pass
  only because their fixtures are same-scale 6-decimal, so the ratio is harmless.
  Add a cross-decimal case to each.
- `core/src/explorer/store.rs:2420-2488`
  (`multi_residual_profit_usd_sums_all_priced_tokens`) — re-check under the new
  `None`-vs-`0.0` semantics from step 5.
- `core/src/explorer/classify.rs:1650-1673`
  (`multi_residual_arb_captures_every_positive_token`) pins the spurious-residual
  behaviour that feeds the unbounded sum; keep as-is but expect a price change.

**Corpus:**

- `core/tests/explorer_corpus.rs` — extend the existing `eth-jit-v3-round-trip`
  case (block `26059586`) to also exercise the arb path.
- Add `profit_usd_max` support next to the existing `profit_usd_min` floor at
  `core/tests/explorer_corpus.rs:289-300`. **There is currently no upper-bound
  assertion anywhere in the repo**, which is exactly why this shipped unnoticed.

---

## Verification

- `cargo fmt --all -- --check`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo test --workspace`
- Live re-run on block `26059586` with `MEV_SCOUT_E2E=1` and an explicit `RPC_URL`:
  confirm arb `profit_usd` is now plausible **and** that the three ops still
  classify as `arb_atomic` — the fix must not silently drop them to `None`.
- Report the before/after USD delta over the 61-block window explicitly rather
  than folding it in silently.
- `git diff --check`, then review before committing.

---

## Risk notes

- **Steps 3 and 5 trade over-reporting for under-reporting.** Some legitimate profit
  will become `None` rather than a wrong number. That is the correct direction for a
  P&L figure, but it will change existing counts and totals, so the delta must be
  reported (see Verification).
- **Step 4 increases price lookups per block.** If that becomes a rate-limit problem
  on a public RPC, it is a config concern, not a correctness one.
- **Step 1 is additive** and only lands in `details` JSON, so existing rows simply
  lack the key and are treated as untrusted. No migration needed.
- **No decimals plumbing is required** — the fallback formula is already
  scale-correct. Do not add a decimals map to `warm_prices_for_tokens`; that is a
  larger diff for no correctness gain.

## Deferred / out of scope

- `jit` detection has no V4 coverage. V4 emits `ModifyLiquidity` rather than
  `Mint`/`Burn`, so the current detector only covers V3 and LB. Separate work.
- The `arb_atomic` profit is denominated in whatever token the transfer ledger
  shows as a positive residual, not necessarily the arb's actual profit token.
  `select_profit_token` (`core/src/explorer/profit.rs:128-150`) falls back to the
  largest *gross* `pos` amount, comparing raw integers across tokens of different
  decimals. Worth a follow-up audit, not part of this fix.
