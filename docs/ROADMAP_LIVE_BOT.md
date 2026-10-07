# ROADMAP — Simulation Fidelity

Goal: make the numbers `mev-scout live` reports mean something.
**Not a bot.** Real execution is out of scope until the simulator below earns it.

For how the system works today, see [ARCHITECTURE.md](ARCHITECTURE.md).

---

## Where the optimism comes from

The ledger books `expected_profit − gas_cost_wei` as arithmetic on every
detected opportunity (`core/src/paper/ledger.rs:196`). That claim assumes:

- the fill reverts never — `recon` can catch a revert *after the fact*
  (`core/src/paper/recon.rs:162`) but the ledger never books one
- the gas figure comes from calibrated guesses about *other people's* txs
  (`core/src/mev/detectors/two_hop.rs:141`), never from our own calldata
- state is post-tx-K, so nothing between detection and our fill moved the pool
- priority fee is free — `winning_bid_premium` defaults to `0.0`
  (`core/src/types/strategy.rs:315`)
- inventory is unlimited — the only balance modeled is the gas wallet
  (`core/src/paper/ledger.rs:94`)
- mempool-only fills that never land still book profit

Every one of these overstates P&L in the same direction.

---

## Now — three tasks

### 1. Fill by execution, not arithmetic

Replace the ledger's claim with the result of actually running the tx.

- Build calldata from an `ExecutionPlan` (a pure projection of `MevOpportunity`).
  Start with V2/V3 direct router calls behind **one ~60-line atomic executor
  contract** — no flash loan, no per-DEX adapters. Atomicity is not optional:
  two sequential swaps where the second fails still leave the first committed,
  which books profit that never happened.
- Execute against **end-of-block state**, not post-tx-K state. That is the state a
  public-mempool sender actually fills into.
- Book the `status`, `gas_used`, and native balance delta that `execute_whatif`
  already returns (`core/src/replay/whatif.rs:100`), sourced from
  `BlockReplayer::replay_to(block, last_index)` (`core/src/replay/replayer.rs:530`).

One task, four fixes: reverts, real gas, in-block state drift, slippage.

*Acceptance:* a fill whose calldata reverts books `-gas`, never `+net`.
Covered in `core/tests/paper_corpus.rs`.

### 2. Capital bound and phantom fills

- Cap each opportunity and each token against a real inventory budget; keep the
  existing gas-wallet semantics in `LedgerPolicy`.
- Mark `mempool_only` fills unsettled until a landed counterpart appears, and
  reverse them out of the session total after N blocks (`PaperFill` gains
  `settled` / `landed_block`).

*Acceptance:* a session reports gross / settled / unlanded separately instead of
one number.

### 3. Put the divergence in the `live` output

`paper::recon` already computes modeled-vs-executed per fill
(`core/src/paper/recon.rs:202`) — it just lives on a research path nobody reads
during a session. Render claimed net, executed net, and the delta as part of the
normal `live` ledger summary.

*Acceptance:* one table in the default output, no separate subcommand.

---

## Later — ordered by how much each raises fidelity

| # | Item | Why it moves the number |
|---|---|---|
| 1 | **Competition model** — win probability per opportunity, and a non-zero `winning_bid_premium` | Largest single overstatement: today every opportunity is assumed won |
| 2 | **Detect from pending** — `capture_pending` is off by default (`core/src/config/settings.rs:406`) | Removes the foresight the detector gets from confirmed blocks |
| 3 | **Restore deleted detectors** — sandwich, backrun, liquidation (gone per `ARCHITECTURE.md:660`) | Coverage is honesty: the opportunity surface today is three strategies wide |
| 4 | **Honest gas auction model** | The cheapest overstatement, worth fixing last of these |
| 5 | **Fork corpus test** | Keeps fidelity from drifting as the rest changes |
| 6 | **Record what was observable at time T** | Without a tape of heads + pending txs, any latency claim stays a guess |

---

## Deferred until the above is trustworthy

Key loading, nonce management, submission, kill switch, crash recovery,
monitoring. These are not wrong — they are the *next* phase, and their whole
purpose depends on the simulator having first been calibrated. Nothing here is
deleted; it is gated.

## Definition of done

- model-vs-executed divergence is reported on every fill and is not zero
- a would-revert fill shows up as a loss
- a session's settled P&L can be reproduced from the intent journal alone