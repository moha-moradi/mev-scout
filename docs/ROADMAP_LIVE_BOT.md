# ROADMAP — From Scanner to Real Bot (paper money first)

Status: planning document. Nothing in here is implemented.
Scope: the gap between `mev-scout live` as it exists today (a post-hoc opportunity
*scanner*) and a bot that can *act* on an opportunity, with real execution gated
behind paper-mode validation.

This document is the execution plan. For how the current system works, see
[ARCHITECTURE.md](ARCHITECTURE.md).

---

## 0. The gap map

What is already solid and should not be rebuilt:

| Area | Where |
|---|---|
| Arbitrage detection (two-hop, multi-hop BFS≤4, JIT) | `core/src/mev/detectors/` |
| Pool discovery across ~15 DEX families | `core/src/pool/discovery/` |
| Quote math (V2/V3/V4, Curve, Balancer, StableSwap, LB, Pendle) | `core/src/pool/math/` |
| Full EVM replay with receipt verification | `core/src/replay/replayer.rs` |
| Realized-MEV forensics + competitor/searcher extraction | `core/src/explorer/` |
| Paper ledger (virtual gas wallet, greedy per-block selection) | `core/src/paper/ledger.rs` |
| Paper-vs-executed reconciliation via revm | `core/src/paper/recon.rs`, `core/src/replay/whatif.rs` |
| Detector-vs-realized profit verdict | `core/src/mev/verdict.rs` |

The gap is not detection. It is the distance between **detecting** an
opportunity and **transacting** on it. That distance is entirely unbuilt, and it
is where all profit lives.

| Capability | State today | Evidence |
|---|---|---|
| Low-latency feed | HTTP polling of *mined* blocks, `poll_interval_ms = 2000` | `cli/src/commands/live.rs:145`; no `eth_subscribe` anywhere in `core/src` |
| Signing / nonce / submission | none | no `private_key` / `nonce manager` / `send_tx` outside replay & ABI decoding |
| On-chain arb contract | none | no `.sol` file in the repository |
| Capital model | gas wallet scalar only | `core/src/paper/ledger.rs:94` |
| Flash loans | fee *estimate* only | `core/src/types/strategy.rs:37` |
| Token inventory | none | no per-token balance anywhere |
| Slippage as an execution constraint | robustness *metric* only | `core/src/mev/detectors/arb_common.rs:24` |
| Risk enforcement | measured, never enforced | `core/src/paper/ledger.rs:96` |
| Kill switch | none | no halt path |
| Strategy selection | parsed, never applied | only consumed for the manifest at `core/src/jobs/live.rs:377` and `core/src/jobs/run.rs:80` |
| Competition model | fixed fee markup | `winning_bid_premium`, `core/src/types/strategy.rs:234` |

---

## 1. Ground rules

These hold for every phase and are not negotiable.

1. **Paper-first.** No phase may make a real network submission reachable by
   default. `[exec].mode` starts at `"off"` and must be set explicitly. Any code
   path that can broadcast must sit behind a mode check that fails closed.
2. **Detection changes and execution changes are separate commits.** A detector
   PR that also touches calldata is unreviewable.
3. **Every accepted fill is reconcilable.** A new decision source is not "done"
   until `paper::recon` / `mev_verdict` can adjudicate it against a revm-computed
   ground truth. If a decision cannot be adjudicated, it is a research artifact,
   not a trade.
4. **Risk gates default to the strictest value.** Missing config ⇒ no trading.
5. **No plaintext private keys.** Keys come from an env var or an encrypted
   keystore; the process refuses to start on a group/world-readable key file.
6. **CI stays green**: `cargo fmt --all -- --check`,
   `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`
   (workspace lints deny warnings).
7. **Every phase ends with a measurement**, not a claim. "Reachable in under
   X ms" must come from a benchmark, "profitable" from a fork test.

---

## 2. Phase 0 — Unblockers

**Goal:** close the cheap gaps that make later phases safe, and make
`strategies` and risk limits actually do something. No new dependencies, no
Solidity, no signing.

**Effort:** ~1 week. **Blocks:** everything.

### U0.1 — Apply `strategies` config to the detector pipeline

`config.backtest.strategies` is validated and written into the run manifest but
never reaches the detectors; `BacktestRunner` always runs all three. Add
`RunnerOpts.strategies: Option<BTreeSet<Strategy>>` and gate each detector
dispatch inside `core/src/pipeline/runner.rs` (`run_block` at line 360,
`run_range` at 862, `run_range_hybrid` at 985). Thread the set through
`jobs/live.rs` and `jobs/run.rs` from the validated config.

*Acceptance:* with `strategies = "two_hop_arb"`, a block whose only opportunity
is JIT produces zero JIT opportunities, and the manifest list matches the set of
detectors that actually ran. New test in `core/tests/mev_corpus.rs`.

### U0.2 — `ExecutionPlan`: a pure opportunity → swap-step projection

Add `core/src/exec/plan.rs` with a pure `ExecutionPlan::from_opportunity(&MevOpportunity)`
producing an ordered `Vec<SwapStep>` (pool, token_in, token_out, amount_in,
amount_out_min, dex kind, calldata template id). No I/O, no signing — this is
the seam that Phase 1 consumes and the thing Phase 2 sizes against inventory.

*Acceptance:* for a multi-hop opp the plan reproduces the same route the detector
priced, and its summed `amount_out_min` is consistent with `expected_profit`
under the configured slippage tolerance. Property test over `core/tests/arbitrage.rs` fixtures.

### U0.3 — Risk policy inside the ledger

`max_drawdown_wei` is computed and then ignored. Add
`core/src/paper/risk.rs`:

```rust
pub struct RiskPolicy {
    pub max_drawdown_wei: u128,
    pub max_session_loss_wei: u128,
    pub max_fills_per_session: usize,
    pub max_notional_wei_per_token: u128,
    pub max_consecutive_losses: usize,
}
```

`LedgerPolicy::apply` takes it and returns a `RiskHalt { reason, at_block }`
alongside `LedgerResult`; a halted ledger accepts no further fills. Add
`FillSkipReason::RiskHalt` and `FillSkipReason::NotionalCap`. Validate the
thresholds in `core/src/config/validation.rs` next to the existing
`validate_live` invariants.

*Acceptance:* a ledger whose drawdown crosses the threshold emits fills up to the
crossing, then only `RiskHalt` skips. Existing ledger tests must stay green —
`LedgerPolicy` gets the policy as an `Option`, defaulting to the old behavior.

### U0.4 — Unsettled fill states and a durable intent journal

`PaperFill` books profit for `mempool_only` opportunities that may never land.
Add `settled: bool` + `landed_block: Option<u64>` to `PaperFill`, and a new
`trade_intents` table on the explorer store (mirroring the `ensure_paper_tables`
pattern in `core/src/paper/store.rs:9`) recording every decision — accepted,
skipped, or halted — with its `canonical_id`, plan hash, and reason.

*Acceptance:* a mempool-only fill with no landed counterpart after N blocks is
marked unlanded and its `net_wei` is reversed out of the session total. Session
summary reports gross / settled / unlanded separately.

### U0.5 — `[exec]` config skeleton, failing closed

Add `ExecConfig { mode: "off"|"simulate"|"submit", contract: Option<Address>,
key_source, max_gas_price_wei, max_priority_fee_gwei, nonce_start,
confirmations }` to `core/src/config/settings.rs` and
`mev-scout.example.toml`. Default `mode = "off"`. Validation rejects any
`mode = "submit"` combination where a bound is missing or a key source is unset —
the failure must be at config validation, before any RPC work.

*Acceptance:* `mev-scout config` prints the resolved section; an invalid submit
config exits non-zero with a config error and zero RPC calls (test with
`wiremock`, following `core/tests/common/setup.rs`).

---

## 3. Phase 1 — Execution spine

**Goal:** a detected opportunity becomes real calldata that is simulated,
signed, and journaled. **No submission path is enabled by default.** This is the
single biggest unlock — before it, paper numbers are an upper bound that reality
has not touched.

**Effort:** ~3–4 weeks. **Depends on:** Phase 0 (U0.2, U0.5).

### S1.1 — Solidity executor contracts + Foundry

New `contracts/` workspace (Foundry, `forge test`), compiled in CI to ABI JSON
consumed by the Rust side. Foundry is currently absent — grep for
`forge`/`anvil`/`solc` returns nothing.

- `ArbExecutor.sol` — the single on-chain entry point:
  flash-loan callback dispatch, multi-hop swap loop, per-hop `amountOutMin`,
  final profit assertion (`require(balanceAfter > balanceBefore + minProfitWei)`),
  pull leftovers to owner, `nonReentrant`, callback restricted to the known
  providers, no `delegatecall`.
- `adapters/` — one library per pool family actually quoted by
  `core/src/pool/math/`: V2, V3 exact-input, V4, Curve, Balancer, StableSwap.
  The adapter surface must match the detector's math or the sim gate is
  comparing two different things.
- `interfaces/` — `IFlashLoanProvider` (Balancer Vault, Aave V3 Pool), ERC-20
  permit surface.
- Invariant/fuzz tests: no residual token approval left open, cannot call an
  arbitrary target, cannot drain more than the flash-loaned amount.

*Acceptance:* `forge test` green with fuzz coverage; `ArbExecutor` deployed on
anvil fork returns the expected profit for a seeded two-hop route.

### S1.2 — `core/src/exec/` — build, simulate, sign, journal

New module, wired into `core/src/lib.rs`:

| File | Responsibility |
|---|---|
| `abi.rs` | `alloy::sol!` bindings generated from the forge artifacts; checksum-verified addresses |
| `keys.rs` | key loading from env var or encrypted keystore; refuse world/group-readable key files; never log key material |
| `nonce.rs` | persistent nonce manager — atomic reservation, gap detection, crash-safe re-read from chain on restart |
| `builder.rs` | `ExecutionPlan` → unsigned tx (to, value, data, gas) |
| `signer.rs` | sign → `TxEnvelope` (raw bytes + hash); hash is the journal key |
| `sim.rs` | pre-trade gate: run the built calldata through `replay::whatif` against pending-block state |
| `submit.rs` | RPC submission + receipt wait + N-confirmation settle + reorg detection |

`sim.rs` is the important one. `replay::whatif.rs` already executes a tx through
revm and measures the native balance delta — wire it to run *our own* calldata,
not just historical txs, and make its `ExecutedNet` a hard precondition for
acceptance. A candidate whose simulated net falls below the modeled claim by more
than the tolerance band is a reject with a recorded reason, not a silent fill.

*Acceptance:* an opportunity reaches the journal with `simulated_net_wei`,
`signed_tx_hash`, and a plan hash, and its simulated net matches
`expected_profit − gas_cost_wei` within the `ReconVerdict` pass band.

### S1.3 — CLI surface, dry-run first

`mev-scout exec plan|sim|sign` — all read-only with respect to the network.
Broadcasting lives behind a separate `mev-scout exec submit` which additionally
requires `[exec].mode = "submit"` in TOML. `plan` prints the route, per-hop
`amountOutMin`, total gas estimate, and the resolved risk check for one
opportunity, so the encoding is inspectable before anything is signed.

*Acceptance:* `exec plan --canonical-id <id>` renders a full human-readable route
from a recorded run; `exec sim` exits non-zero when the sim gate rejects.

### S1.4 — Fork integration test (the phase's real deliverable)

Anvil fork at a recorded block, deploy `ArbExecutor`, run the full
detect → plan → build → sim → sign path on a fixture opportunity, assert the
signed hash matches a golden value and the simulated net lands inside the
existing tolerance band. This is the first test that exercises the entire stack
end to end and it becomes the regression net for every later phase.

*Acceptance:* green in CI; deliberately corrupting `amountOutMin` makes it fail.

---

## 4. Phase 2 — Capital and risk

**Goal:** stop treating gas as the only balance. Model inventory, funding,
exposure, and enforce the limits from U0.3 in a loop that can actually halt.

**Effort:** ~2–3 weeks. **Depends on:** Phase 1.

### K2.1 — `Portfolio` model

`core/src/portfolio/` with `Portfolio { native_wei, tokens: HashMap<Address, TokenPosition> }`
where `TokenPosition` carries amount, decimals, and a `usd_approximate` flag
(reusing the FOT/rebase handling that already exists in the explorer, per
ARCHITECTURE.md's known-bias table). Invariant: `native_wei >= reserve_wei` at
every step.

### K2.2 — Funding paths

Two funding modes, selectable per opportunity: (a) flash loan through the
on-chain providers already modeled for fee purposes in
`core/src/types/strategy.rs:37`, executed for real via the S1.1 adapters;
(b) pre-funded inventory pairs. Capital efficiency and fee drag differ
substantially between them, so the ledger must price both rather than assume
capital is free.

### K2.3 — Inventory policy and rebalancing

Which pairs to pre-fund, minimum viable size per pair, dust thresholds, and
rebalancing trades when a leg runs dry mid-route. Rebalances are themselves
trades with real slippage and must appear in the intent journal, not as a
teleport in the balance model.

### K2.4 — Enforce the risk policy

Wire U0.3's `RiskPolicy` into the live loop: daily/session loss, max drawdown,
per-token notional cap, per-pool exposure, consecutive-loss streak, total
notional cap. Every gate rejection is a journaled decision with a reason.

### K2.5 — Kill switch and flattener

Halt triggers: config file change, `SIGTERM`, an HTTP/admin toggle, and any risk
breach. The switch must be checked immediately before every send, not on a timer.
`mev-scout exec flatten` closes open inventory positions and open JIT liquidity.
Note the open problem in ARCHITECTURE.md:672 — a JIT position unwound by a reorg
cannot currently be restored, so flattening must handle an un-restorable position.

### K2.6 — Realized-vs-modeled reconciliation, continuously

`paper::recon` already adjudicates each fill against a revm ground truth. Promote
it from a research module into a running gate: rolling realized-vs-modeled
divergence per strategy, and a halt when divergence leaves the configured band.
This is the metric that says whether the bot's model is actually true.

### K2.7 — Approvals and balance pre-checks

ERC-20 allowances (Permit2 or direct), native-balance pre-checks, and a hard
refusal to build a plan whose inputs are not actually held or borrowable.

*Acceptance:* a session that breaches session loss halts, records the reason, and
refuses further fills; the session report shows realized, modeled, and the
divergence band.

---

## 5. Phase 3 — Latency and competition

**Goal:** act before the block that contains the opportunity, and stop assuming
we win every race we enter.

**Effort:** ~3–4 weeks. **Depends on:** Phases 1–2.

### L3.1 — WebSocket transport

Add a Ws provider to `core/src/rpc/` behind a cargo feature. Everything today is
HTTP. This is the single largest structural change of the phase.

### L3.2 — Subscriptions with gap recovery

`newHeads` and `newPendingTransactions`, with reconnect and gap refill that
reuses the existing gap logic (`fetch::fetcher`, `chain/timing.rs`) — a silently
missed block during a reconnect is a silent PnL hole otherwise.

### L3.3 — Streaming decision loop

Keep pool state warm in `PoolManager` and decide as the head arrives, rather than
polling a settled block and reconstructing state from scratch. Target budget:
p99 head-arrival → decision under 400 ms, measured.

### L3.4 — Submission channels

Abstraction over public send vs private bundle submission (`mev_sendBundle`).
Backrun and sandwich opportunities are strictly unviable on the public mempool —
the victim sees the tx — so channel choice is a strategy-level decision, not a
config detail.

### L3.5 — Competition and revert modeling

Replace the flat `winning_bid_premium` markup with an inclusion-probability
estimate per opportunity, informed by the searcher/sender density the explorer
already extracts, observed revert rates, and latency percentiles. Report the
modeled win probability alongside every opportunity so the ledger can discount
expected profit by it.

### L3.6 — Per-strategy capital allocation

Now that U0.1 makes strategy selection real, allocate capital per strategy and
enforce that allocation as a risk bound.

### L3.7 — Process supervision

Chain-level isolation, restart on feed loss, and a clean handoff between the
detection process and the execution process.

*Acceptance:* measured p99 latency against budget; a report showing inclusion
probability per opportunity and the resulting expected-value ranking.

---

## 6. Phase 4 — Operational hardening

**Goal:** make it safe to leave running unattended, and widen the opportunity
surface.

**Effort:** ~2–3 weeks. **Depends on:** Phase 3.

### O4.1 — Fork test suite

Extend S1.4 from one golden path to a corpus: historical blocks with known
opportunities → expected route, expected signed hash, and a profit band. Reuses
the existing fixture style in `core/tests/common/setup.rs` and
`core/tests/arbitrage.rs`.

### O4.2 — Metrics

Prometheus endpoint: per-strategy realized vs modeled PnL, decision latency
histograms, inclusion and revert rates, gas spend, feed lag, RPC per-provider
error and latency percentiles.

### O4.3 — Alerting

PnL divergence outside band, any halt trigger, feed lag, RPC degradation,
reorg detected with an open position.

### O4.4 — Crash recovery

Restart mid-flight must recover: re-read nonces from chain, look up any sent hash
in the journal, settle or mark unknown, and reconcile the portfolio against
actual balances. The intent journal from U0.4 is the input.

### O4.5 — Reorg policy

N confirmations before booking realized PnL; explicit unwind rules for open JIT
positions; pending-fill reconciliation against the reorg-aware explorer sync
state.

### O4.6 — RPC tier

Public endpoints at `rpc_rps ≈ 1.0` are the practical throughput ceiling and are
not sufficient for Phase 3's latency budget. Move to dedicated authenticated,
regionally selected endpoints with per-provider SLOs. Likely a real cost line —
worth deciding before Phase 3 starts rather than discovering mid-phase.

### O4.7 — Restore deleted detectors

Sandwich, backrun, and liquidation were removed (ARCHITECTURE.md:660–661). Each
needs to come back with a full execution path, not just detection — otherwise it
reproduces the original problem, a detector with nothing to call. Also open:
Uni V3 `Flash` handling, deferred at ARCHITECTURE.md:670.

### O4.8 — Session state machine

Explicit `init → warmup → active → degraded → halted → flattening → flat`, with
the kill switch as a first-class state rather than a boolean.

---

## 7. Critical path

```
Phase 0 (U0.1 U0.2 U0.3 U0.4 U0.5)
    │
    ▼
Phase 1 (S1.1 contracts ──┬── S1.2 exec module ── S1.3 CLI)
                          │                              │
                          └────────── S1.4 fork test ◄────┘
    │
    ▼
Phase 2 (K2.1 portfolio → K2.2 funding → K2.3 inventory → K2.4/5/6/7)
    │
    ▼
Phase 3 (L3.1 ws → L3.2 subs → L3.3 streaming → L3.4/5 channels+competition)
    │
    ▼
Phase 4 (O4.1 tests, O4.2/3 observability, O4.6 RPC, O4.7 strategies)
```

Two items are long-lead and should start early regardless of sequencing:

- **O4.6 RPC tier** — procurement and endpoint selection gate Phase 3's latency
  budget. Decide before Phase 3 begins.
- **S1.1 contracts** — the adapters must mirror `core/src/pool/math/`. Audit that
  mapping in Phase 0 so Phase 1 is not blocked on discovering a math mismatch.

## 8. Risk register

| Risk | Impact | Mitigation |
|---|---|---|
| Encoded adapter math diverges from `pool/math` | Sim gate validates the wrong thing; every later number is wrong | Adapter-by-adapter audit in Phase 0; fork test asserts contract output equals Rust quote |
| Public RPC throughput caps achievable latency | Phase 3 target unreachable | Procure endpoints before Phase 3 (O4.6) |
| Slippage tolerance too tight ⇒ nothing lands; too loose ⇒ we are the victim | Silent unprofitability | Per-hop `amountOutMin`; treat revert rate as a first-class metric, not an error count |
| Detection timing is already too late for profitable inclusion | The whole strategy set is unviable | Measure head→decision and inclusion probability in Phase 3 *before* adding capital; this is a stop-and-reassess gate, not a polish item |
| Flash-loan availability/liquidity differs per chain and provider | Fee model in `types/strategy.rs` understates cost | Price actual provider parameters on-chain; reconcile realized vs modeled (K2.6) |
| Reorg unwinds an open JIT position with no restore path | Unrecoverable loss | Explicitly noted at ARCHITECTURE.md:672; flatten rules in K2.5 |

## 9. Explicitly out of scope

Kept out so they do not creep in as "quick wins":

- Sandwiching and generalized front-running as *strategies* — they need private
  submission (L3.4) and a very different risk profile; re-adding detection ahead
  of the channel is what created the original gap.
- Cross-chain / multi-chain atomic arbitrage.
- Market making, inventory recycling strategies, CEX integration.
- Anything that requires a key at build time or in the repository.