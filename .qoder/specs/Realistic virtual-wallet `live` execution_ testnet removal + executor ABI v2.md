# Realistic virtual-wallet `live` execution: testnet removal + executor ABI v2

## Context

`live` currently has three execution backends: in-process revm, a local Anvil fork,
and a testnet submitter. Testnet is economically meaningless here — plans are built
from mainnet pool addresses and fees, so Sepolia/Amoy/Fuji pools either do not exist
or hold different liquidity. Every "fill" or revert there is an artifact and pollutes
the ledger. Its only unique value was the real EIP-1559 signing path, which can be
covered locally by signed Anvil transactions against real fork state.

The goal is a `live` session that behaves like a real bot whose wallet is virtual:
an authenticated executor contract, a real deploy/send/receipt path, one wallet that
survives across blocks, and reverts/losses that actually cost gas. A Lasagna-style
generic executor (arbitrary target + calldata with output injection) is explicitly
**not** adopted: it is permissionless, unbounded, and cannot be priced or reconciled
against typed pool math. Keep typed legs and add the invariants a real bot has.

Decisions locked with the user:
- Executor ABI v2: operator-only `execute`, immutable withdrawer, `minProfit` + `deadline`.
- Anvil signs EIP-1559 locally and sends raw txs (no impersonation).
- Delete testnet completely.

## Working-tree note

`docs/CLI.md` and `docs/ARCHITECTURE.md` already have uncommitted edits (explorer /
dashboard audit). Rebase the changes below onto those edits; do not revert them.
`.cursor/plans/live_explorer_ops_guide_*.plan.md` is unrelated and untracked.

---

## Step 1 — Executor ABI v2 (`contracts/src/FlashArbExecutor.sol`)

Keep typed legs; no raw-call kind.

```solidity
contract FlashArbExecutor {
    uint8 public constant ABI_VERSION = 2;

    address public immutable operator;
    address public immutable withdrawer;
    address public immutable weth;

    address internal activeVault;
    address internal expectedV3Pool;
    uint256 internal borrowBalanceBefore;

    struct Leg { address pool; address tokenIn; address tokenOut; uint8 kind; uint24 fee; }
    struct Plan {
        address vault;
        address borrowToken;
        uint256 amount;
        uint256 minProfit;   // borrow-token units, not USD/wei
        uint256 deadline;    // unix seconds, checked against block.timestamp
        Leg[] legs;
    }

    constructor(address operator_, address withdrawer_, address weth_) // all non-zero
}
```

- `execute(Plan)`: guards in this order — `msg.sender == operator` (`NotOperator`),
  `block.timestamp <= deadline` (`Expired`), `activeVault == 0` (`Reentrant`),
  `amount != 0`, `vault != 0`, `legs.length != 0`. Then set
  `borrowBalanceBefore = IERC20(plan.borrowToken).balanceOf(address(this))`,
  set `activeVault`, call `flashLoan`, clear `activeVault` and
  `borrowBalanceBefore` after it returns.
- `receiveFlashLoan` decodes `(Leg[] legs, uint256 minProfit)`. Run legs, repay
  **first** (unchanged), then compute
  `left = balance - repay; net = left > borrowBalanceBefore ? left - borrowBalanceBefore : 0`.
  `if (net < minProfit) revert MinProfit();`. If `net == 0` return; else send to
  `withdrawer` (unwrap first when `borrowToken == weth`).
- Remove `profitTo` and plan-supplied `weth`; both are immutable/spoofable otherwise.
- Add `sweep(address token)` and `sweepEth()` — `operator` only, always to
  `withdrawer`. `receive()` stays for `IWETH.withdraw` and receives swept ETH.
- No owner transfer / operator rotation / pause: immutable roles are simpler and
  stricter for a bot. A deployed executor is replaced, not mutated.
- Delete the now-unused `EthForwardFailed` path only if `sweepEth` reuses it; keep it.

### Forge tests (`contracts/test/FlashArbExecutor.t.sol`)

- Constructor args everywhere; extend the local `Vm` shim with `warp` and `prank`.
- Update the selector/version test to ABI v2.
- Add: `test_only_operator` (stranger reverts), `test_expired_reverts`,
  `test_min_profit_reverts`, `test_profit_goes_to_withdrawer` (caller = operator,
  withdrawer ≠ caller, withdrawer native/token balance rises),
  `test_sweep_operator_only`.
- Update the fork test and `_plan` helper to the new tuple.

---

## Step 2 — Regenerate embedded artifacts

- Run `"$HOME/.foundry/bin/forge" build` in `contracts/`.
- Copy `out/FlashArbExecutor.sol/FlashArbExecutor.json`:
  `.bytecode.object` → `core/src/exec/artifacts/FlashArbExecutor.creation.hex`,
  `.deployedBytecode.object` → `...deployed.hex` (strip `0x`, newline at end).
- `core/src/exec/artifacts.rs`: `ABI_VERSION = 2`, test asserts `2`.
- Never inject `deployed.hex` directly anymore: immutables are zeroed at compile
  time; the executor must be deployed from `creation.hex` + constructor args.

Regeneration command (Node, run from repo root) is recorded in Step 9 docs:

```bash
node -e "const j=require('./contracts/out/FlashArbExecutor.sol/FlashArbExecutor.json'),f=require('fs');f.writeFileSync('core/src/exec/artifacts/FlashArbExecutor.creation.hex',j.bytecode.object.replace(/^0x/,'')+'\n');f.writeFileSync('core/src/exec/artifacts/FlashArbExecutor.deployed.hex',j.deployedBytecode.object.replace(/^0x/,'')+'\n')"
```

---

## Step 3 — Rust plan + calldata v2

`core/src/exec/plan.rs`
- `ExecPlan` drops `profit_to` and `weth`; adds `min_profit: U256`, `deadline: u64`.
- `plan_from_opportunity(opp, pools, vault, provider, min_profit, deadline)`.
- Keep strategy/provider/path/leg checks unchanged.

`core/src/exec/calldata.rs`
- New signature:
  `execute((address,address,uint256,uint256,uint256,(address,address,address,uint8,uint24)[]))`
- Head = 6 words: `vault`, `borrow_token`, `amount`, `min_profit`, `deadline`,
  `legsOffset = 192`; then array length + 5 words per leg.
- Update `selector_matches_abi_v1` → v2 and the length assertion
  `4 + 32 + 32*6 + 32 + 32*5`.

`core/src/exec/mod.rs`
- Delete `EXECUTOR_ADDRESS` (no fixed injection address after real deploy).
- Delete `Testnet` from `ExecMode` and its `as_str`/`parse` arms.
- Keep `ANVIL_DEFAULT_KEY` / `ANVIL_DEFAULT_ADDRESS` for virtual signing.

Session-side values: `min_profit = U256::from(1)` (the ledger already requires net > 0;
a real deployment would configure a threshold) and
`deadline = opp.timestamp.saturating_add(12)`. Always use chain time, never wall clock.

---

## Step 4 — revm: real CREATE deploy + correct state positioning

### 4a. `core/src/replay/whatif.rs`
- Add `pub created: Option<Address>` to `WhatIfRun`; set it in both the success and
  degraded branches (`None`) from
  `ExecutionResult::Success { output: Output::Create(_, Some(addr)), .. }`.
- Make `execute_whatif` generic over `DB: DatabaseRef + DatabaseCommit` so the deploy
  path can be unit-tested with `CacheDB<EmptyDB>`; update all callers (type inference
  keeps them unchanged).

### 4b. `core/src/exec/revm_backend.rs`
- `RevmBlock { state_block, exec_env: BlockEnv, db, executor: Address, nonce: u64 }`.
- `open_revm_block(replayer, opp, searcher, withdrawer, weth, balance_wei)`:
  1. Choose the state reference (4c) and replay positioning txs.
  2. Fund `searcher` with `balance_wei`, **preserving** its nonce/code
     (`db.basic_ref` first; never rebuild `AccountInfo` with `nonce: 0`).
  3. Deploy via a real CREATE:
     `creation_bytecode() + ABI words [searcher, withdrawer, weth]`, a `TxEnv`
     (`TxKind::Create`, gas 3M, nonce from db, gas price = basefee + 1 gwei),
     through `execute_whatif`; assert `run.created` equals
     `searcher.create(deploy_nonce)`; store `executor` and the next nonce.
- `exec_plan_revm` calls `block.executor` (not `EXECUTOR_ADDRESS`), uses
  `block.exec_env`, reads the nonce from `db.basic_ref(searcher)` for the synthetic
  hash, and increments after a committed run.
- Delete `SEARCHER_FUND_WEI` and `inject_executor`.

### 4c. State reference / intra-block position
`core/src/replay/replayer.rs`
- `tx_data_to_tx_env`: `gas_price: tx.gas_price.unwrap_or(tx.max_fee_per_gas)`,
  `gas_priority_fee: tx.max_priority_fee_per_gas.or(tx.gas_price)`.
- Add `build_next_block_env(prev: &BlockData) -> BlockEnv`: `number + 1`,
  `timestamp + 12`, `basefee * 1.125` (saturating), same beneficiary/gas limit,
  spec-derived blob fields.
- In `open_revm_block`:
  - `pending = opp.mempool_only || matches!(opp.detection_path, Some(DetectionPath::Pending))`.
  - Replay-detected (`!pending`): `what_if_state(N)` (pre-block N), replay
    `txs[0..=opp.tx_index]` with `execute_whatif(db, cfg(N), env(N), ...)` and keep
    the returned db; `exec_env = build_block_env(block N)`. This models "my arb is
    the next tx after the anchor".
  - Pending: `what_if_state(N + 1)` (state after block N), `exec_env =
    build_next_block_env(block N)`.
  - If block data/txs are absent (log-only), fall back to pre-block N without
    positioning replay and log once.

---

## Step 5 — Anvil: signed EIP-1559 sender

`core/src/exec/anvil.rs` (absorb the signing code from `testnet.rs` before deleting it)

```rust
pub struct AnvilSender { url: String, chain_id: u64, signer: PrivateKeySigner,
                         pub from: Address, nonce: u64 }
impl AnvilSender {
    pub async fn connect(url: &str, key: &str) -> Result<Self>; // eth_chainId + pending nonce
    pub async fn fund(&mut self, wei: u128) -> Result<()>;      // anvil_setBalance, loopback only
    pub async fn deploy_executor(&mut self, args: &[Address]) -> Result<Address>;
    pub async fn send_exec(&mut self, to: Address, data: Bytes) -> Result<ExecAttempt>;
}
```

- `send()` is the deleted `testnet.rs:136` logic: `TxEip1559`,
  `max_fee = eth_gasPrice * 2`, priority 1 gwei, `sign_hash_sync`,
  `encoded_2718`, `eth_sendRawTransaction`, increment nonce after send.
- `fund` replaces `impersonate_and_fund`; `AnvilNode::attach` already enforces
  loopback, so `anvil_setBalance` cannot touch a remote chain.
- `deploy_executor` appends constructor address args to `creation_bytecode()`
  (shared `append_address_args` helper in `artifacts.rs`; do not add alloy
  `sol-types`/`contract` features just for `SolValue` — they are not enabled).
- Remove `impersonate_and_fund`, `deploy_executor` free fn, `send_exec` free fn,
  `send_tx`, and the impersonation test; keep `rpc`, `reset_fork`,
  `wait_receipt` helpers.
- Use `exec_key` when configured, else `ANVIL_DEFAULT_KEY`. The sender address is
  the operator/withdrawer, matching `searcher_address`.

---

## Step 6 — Session wiring + result cache (`core/src/exec/session.rs`)

`ExecContext`: keep `mode, chain_id, vault, weth, searcher, provider, fork_url,
exec_key, anvil_bin, anvil, anvil_executor, anvil_block`; delete
`testnet`, `testnet_executor`, `executor`; rename `submitted` → `cache`.

- `execute_ledger` dispatch: `None` bails; `Revm` → `run_revm`; `Anvil` → `run_anvil`.
- `run_revm`: prepare one `RevmBlock` per `(block_number, pending_kind)`, opening it
  with the wallet value supplied by the ledger callback for that block's first
  candidate. Pass `exec_env` and the deployed executor address.
- `run_anvil` per prepared block:
  `reset_fork(N)` → `AnvilSender::connect` (nonce is read *after* reset, so no
  separate resync) → `fund(wallet_before)` → `deploy_executor([searcher,
  withdrawer, weth])` → `send_exec`. Keep `anvil_executor/anvil_block` so later
  candidates in the same block reuse the deployment.
- Per-pass cache (from the prior plan): both backends consult `ctx.cache` keyed by
  `submit_key(opp)` (`canonical_id`, else the composite fallback). Insert after an
  attempt reached the backend (`tx_hash.is_some() || status || fail ==
  Some(ExecRevert)`); rename `remember_broadcast` → `remember_attempt`. Safe because
  `session_opps` only appends, per-block ordering is deterministic, and historical
  state is immutable.

---

## Step 7 — Delete testnet

- `core/src/exec/testnet.rs`: delete the file, `mod testnet;`, and the re-export.
- `core/src/jobs/live.rs`: drop `assert_testnet_chain`, `TestnetSender` imports;
  remove the chain-id check, `Testnet && key.is_none()` bail, and the
  `ExecMode::Testnet` arm of `ensure_exec_ready`; update the `--exec` error text to
  `expected 'none', 'revm', or 'anvil'`.
- `core/src/config/settings.rs`: remove `executor_address` (it was testnet-only and
  ABI v2 immutables make an externally deployed executor unusable unless its roles
  match); update `exec` docs and `exec_mode()` error. Unknown TOML keys are ignored,
  so existing configs keep loading.
- `mev-scout.example.toml`, `cli/src/cli.rs` help text, and `README.md`.

---

## Step 8 — Persistent virtual wallet, revert costs, schema v13

`core/src/paper/ledger.rs`
- Callback becomes `FnMut(&MevOpportunity, u128 /* wallet_before */) -> ExecAttempt`;
  call `run(opp, wallet)` at the existing call site.
- Keep the claimed-`gas_cost_wei` gate against `wallet - reserve`; add an early
  `wallet <= reserve_wei` → `InsufficientGas` skip.
- On `fail == Some(ExecRevert)` or `status == false` with `gas_wei > 0`: debit
  `wallet = wallet.saturating_sub(gas_wei)`, update peak/drawdown, accumulate
  `revert_gas_wei`.
- On a successful attempt with `net <= 0`: apply
  `wallet = wallet.saturating_add_signed(net)`, update drawdown, accumulate
  `exec_loss_wei`, then push the `NonPositiveNet` skip.
- Extract a small `apply_wallet_delta(...)` helper so peak/drawdown logic is shared.
- `apply` / `apply_simulated` initialize the two new totals to zero.

`core/src/paper/types.rs`
- `ExecAttempt::reverted(gas_used, gas_wei, tx_hash)` (status false,
  `fail = Some(ExecRevert)`); use it for receipt timeouts. Real revert receipts keep
  `ran(false, …)`.
- `LedgerResult`: `#[serde(default)] revert_gas_wei: u128`,
  `#[serde(default)] exec_loss_wei: u128`.
- `PaperSession`: optional string mirrors of both.

`core/src/explorer/store/schema.rs` + `core/src/paper/store.rs`
- `migrate_v13_paper_exec_cost`: `ensure_column` on `paper_sessions` for
  `revert_gas_wei TEXT`, `exec_loss_wei TEXT`; append to `MIGRATIONS`
  (`SCHEMA_HEAD` follows automatically).
- Add both to `ensure_paper_tables` DDL, `insert_paper_session` (INSERT + params),
  `paper_session`, `recent_sessions`, `row_to_session`.
- `core/src/jobs/live.rs` summary: append `| revert gas X | loss Y` when non-zero.

Wallets are recomputed from `starting_gas_wei` plus booked deltas each pass, so the
backend always receives the same deterministic block-start balance. Executor deploy
gas is setup overhead (per block in Anvil, per prepared block in revm) and is not
booked as a fill; document this.

---

## Step 9 — Docs and tests

Docs:
- `docs/CLI.md`: `--exec <none|revm|anvil>`; delete the testnet row; document signed
  Anvil (`exec_key`, default dev key), executor roles/minProfit/deadline, revert gas
  and `exec_loss_wei`, and the new `paper_sessions` columns.
- `docs/ARCHITECTURE.md`: `exec — FlashArb plan · revm/anvil`; artifact row now says
  “deploy from creation hex + constructor args” and records the regeneration command.
- `README.md`, `mev-scout.example.toml`: remove testnet and `executor_address`; note
  `exec_key` now also selects the Anvil signer.
- `core/src/exec/artifacts.rs` module doc: update ABI v2 + regeneration command.

Rust tests:
- `calldata`: v2 offsets/length/selector.
- `artifacts`: `ABI_VERSION == 2`.
- `revm_backend`: new unit test with `CacheDB<EmptyDB>` + generic `execute_whatif`
  verifying `created`, `Address::create(searcher, nonce)`, and nonce increment.
- `ledger`: update `apply_executed` closures to the wallet parameter; add a revert
  gas debit test and a persistent-wallet test.
- `anvil`: update deploy/send tests to constructor args + signed sends; keep the
  existing skip when `anvil`/`forge` are missing; add a non-operator revert case.
- `session`: `submit_key` / `remember_attempt` rules.
- `cli/tests/cli_args.rs`: assert `--help` no longer mentions `testnet`, and
  `live --exec testnet` fails.

---

## Verification

1. `cargo fmt --all` then `cargo test --workspace` (forge/anvil-dependent tests
   self-skip when the binaries are absent).
2. Contract: `cd contracts && "$HOME/.foundry/bin/forge" build && "$HOME/.foundry/bin/forge" test`.
3. Regenerate the two hex files (command above); `cargo test -p mev-scout-core exec::artifacts`
   must confirm ABI v2 and non-empty bytecode.
4. Offline config: `cargo run -p mev-scout-cli -- config` still resolves.
5. Negative: `cargo run -p mev-scout-cli -- live --exec testnet --blocks 1` fails with
   the new error, and `live --help` does not list testnet.
6. revm end-to-end: `cargo run -p mev-scout-cli -- -f <config> live --blocks 8 --sim state --exec revm`.
   Expect a run header, `exec: revm … ok/revert`, `recon:` line, and nonzero
   `revert gas`/`loss` only when those cases occurred.
7. Anvil end-to-end (fork-capable RPC or `[paper].anvil_url` loopback fork):
   `... live --blocks 8 --exec anvil`. Verify `exec: anvil`, real signed txs,
   `wallet:` changes between blocks, and `report` reprints the same session.
8. Sanity: no testnet chain ids remain (`grep -rn "testnet|Testnet" core cli docs README.md`),
   and no code path can broadcast to a remote chain (`anvil_setBalance`/reset and
   signed sends only target the loopback node; revm is in-process).

## Risks / traps

- ABI v2 and artifact hex must land in the same change; the selector test and
  `ABI_VERSION` test catch drift.
- `deployed.hex` has zero immutables: deploy from `creation.hex` + args only.
- Anvil `reset` rewinds the nonce; `connect` must happen after `reset_fork`.
- Deadlines must use chain `block.timestamp` (historical for revm), never wall clock.
- `minProfit` is denominated in borrow-token units (WETH units for the normal path).
- Removing `executor_address` is a config-surface break; unknown keys are ignored.
- Deploy gas is not booked in the ledger (setup overhead), by design.
- Keep the existing uncommitted docs edits; the doc changes above are additive.