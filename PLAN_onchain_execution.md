# On-Chain Execution — Plan

## Overview

Add Solidity smart contracts and extend the Rust live mode so that `mev-scout live` can **broadcast real transactions** instead of simulating them virtually. The bot will detect MEV opportunities (arbitrage, sandwich, liquidation, JIT liquidity), construct calldata for deployed executor contracts, sign, and broadcast via private relays.

### Current limitations addressed
- Live mode (`mev-scout live`) currently simulates all trades in-memory — no real on-chain activity
- No Solidity contracts exist in the project
- Virtual wallet tracking has no capital efficiency (no flash loans)
- No private key management, signing, or tx broadcast

---

## Phase 1 — Solidity Smart Contracts

### 1.1 Project structure

```
contracts/
├── foundry.toml                # Foundry config
├── lib/
│   └── forge-std/              # Submodule (foundry standard library)
├── src/
│   ├── executors/
│   │   ├── FlashLoanArbitrage.sol
│   │   ├── SandwichExecutor.sol
│   │   ├── LiquidationExecutor.sol
│   │   └── JitLiquidityExecutor.sol
│   ├── interfaces/
│   │   ├── IFlashLoanProvider.sol
│   │   ├── IDEXRouter.sol
│   │   └── IExecutorFactory.sol
│   └── factory/
│       └── ExecutorFactory.sol
├── script/
│   └── Deploy.s.sol
└── test/
    ├── FlashLoanArbitrage.t.sol
    ├── SandwichExecutor.t.sol
    ├── LiquidationExecutor.t.sol
    └── JitLiquidityExecutor.t.sol
```

### 1.2 FlashLoanArbitrage.sol

**Purpose**: Execute multi-hop arbitrage funded by flash loans.

**Flash loan support** (in priority order per `FlashLoanProvider` in `core/src/types/strategy.rs`):
- Balancer V2 (0% fee, `flashLoan()`)
- Aave V3 (0.09% fee, `flashLoanSimple()`)
- Uniswap V2/V3 (~0.10% fee, `swap()`)

**Interface**:
```solidity
import "@openzeppelin/contracts/security/ReentrancyGuard.sol";

contract FlashLoanArbitrage is ReentrancyGuard {
    error NotOwner();
    error FlashLoanFailed();
    error SwapFailed();
    error NotProfitable(uint256 profit, uint256 minProfit);

    address public owner;

    event ArbitrageExecuted(
        address indexed tokenIn,
        address indexed tokenOut,
        uint256 inputAmount,
        uint256 profit,
        FlashLoanProvider provider
    );

    constructor() {
        owner = msg.sender;
    }

    modifier onlyOwner() {
        if (msg.sender != owner) revert NotOwner();
        _;
    }

    /// Execute arbitrage using the best available flash loan provider.
    /// `FlashLoanProvider.Auto` is resolved to a concrete provider by the
    /// Rust backend before building calldata — the contract does NOT try
    /// providers in order; it uses whatever `provider` is passed in.
    /// @param provider         Which flash loan provider to use
    /// @param tokenIn          Token to borrow and swap from
    /// @param tokenOut         Token to end with (profit token)
    /// @param amountIn         Amount of tokenIn to borrow
    /// @param minProfit        Minimum profit in tokenOut (revert if below)
    /// @param swapPath         Encoded swap route (router addresses + fees)
    function executeArbitrage(
        FlashLoanProvider provider,
        address tokenIn,
        address tokenOut,
        uint256 amountIn,
        uint256 minProfit,
        bytes calldata swapPath
    ) external nonReentrant;

    /// Withdraw accumulated profits (owner only).
    function withdraw(address token, uint256 amount) external onlyOwner;

    /// Change owner (owner only).
    function transferOwnership(address newOwner) external onlyOwner;
}
```

**Execution flow**:
1. Request flash loan from the chosen provider
2. In the provider-specific callback:
   - **Balancer V2**: `receiveFlashLoan(IERC20[] tokens, uint256[] amounts, uint256[] feeAmounts, bytes calldata userData)`
   - **Aave V3**: `executeOperation(address[] assets, uint256[] amounts, uint256[] premiums, address initiator, bytes calldata params)`
   - **Uniswap V2**: `uniswapV2Call(address sender, uint256 amount0, uint256 amount1, bytes calldata data)`
   - **Uniswap V3**: `uniswapV3SwapCallback(int256 amount0Delta, int256 amount1Delta, bytes calldata data)`
   
   Inside the callback:
   a. Swap borrowed token through the encoded swap path
   b. Verify `outputAmount > amountIn + flashLoanFee + gasReserve`
   c. Approve flash loan repayment
   d. Return control to flash loan provider
3. Emit event with profit details

### 1.3 SandwichExecutor.sol

**Purpose**: Execute sandwich attacks (buy before victim, sell after victim pushes price).

**Interface**:
```solidity
import "@openzeppelin/contracts/security/ReentrancyGuard.sol";

contract SandwichExecutor is ReentrancyGuard {
    error NotOwner();
    error FrontRunFailed();
    error BackRunFailed();
    error NotProfitable();

    address public owner;

    constructor() {
        owner = msg.sender;
    }

    modifier onlyOwner() {
        if (msg.sender != owner) revert NotOwner();
        _;
    }

    /// Execute a two-phase sandwich attack.
    /// Phase 1 (front-run): buy tokenIn before victim tx
    /// Phase 2 (back-run): sell tokenOut after victim tx
    /// Both phases must be in the same block with sequential nonces.
    /// NOTE: Requires a bundle relay (Flashbots/MEV-Share); Public mode
    /// cannot guarantee atomic multi-tx ordering in the same block.
    /// @param tokenIn         Token to buy
    /// @param tokenOut        Token to sell back to
    /// @param amountIn        Amount of tokenIn to buy
    /// @param pool            Pool where the swap occurs
    /// @param router          Router contract to use
    /// @param minProfit       Minimum profit to accept
    function executeSandwich(
        address tokenIn,
        address tokenOut,
        uint256 amountIn,
        address pool,
        address router,
        uint256 minProfit
    ) external nonReentrant returns (uint256 profit);

    /// Withdraw accumulated profits (owner only).
    function withdraw(address token, uint256 amount) external onlyOwner;
}
```

**Execution flow** (two separate tx with sequential nonces, same block):
1. **Front-run tx**: Swap `tokenOut → tokenIn` via DEX router
2. **Victim tx** (not ours, monitored from mempool)
3. **Back-run tx**: Swap `tokenIn → tokenOut` at the inflated price

**Broadcast requirement**: Must use `submit_bundle()` via Flashbots or MEV-Share.
`TxBroadcaster` in `Public` mode should reject sandwich strategy with an error.

### 1.4 LiquidationExecutor.sol

**Purpose**: Execute Aave V3 liquidations with flash-loan-funded capital.

**Interface**:
```solidity
import "@openzeppelin/contracts/security/ReentrancyGuard.sol";

contract LiquidationExecutor is ReentrancyGuard {
    error NotOwner();
    error LiquidationCallFailed();
    error SwapFailed();

    address public owner;

    constructor() {
        owner = msg.sender;
    }

    modifier onlyOwner() {
        if (msg.sender != owner) revert NotOwner();
        _;
    }

    /// Liquidate an Aave V3 position. Borrows liquidation capital via
    /// flash loan, calls Aave's liquidationCall(), then swaps seized
    /// collateral to repay the flash loan.
    /// @param user            User to liquidate
    /// @param debtToken       Token in which the user is insolvent
    /// @param collateralToken Token to seize
    /// @param debtToCover     Amount of debt to repay
    /// @param aavePool        Aave V3 pool address
    /// @param flashLoanProvider Flash loan source
    /// @param minProfit       Minimum profit in seized collateral
    function executeLiquidation(
        address user,
        address debtToken,
        address collateralToken,
        uint256 debtToCover,
        address aavePool,
        FlashLoanProvider flashLoanProvider,
        uint256 minProfit
    ) external nonReentrant;

    /// Withdraw (owner only).
    function withdraw(address token, uint256 amount) external onlyOwner;
}
```

**Execution flow**:
1. Borrow `debtToCover` via flash loan
2. Approve Aave pool to spend debt token
3. Call `AavePool.liquidationCall(debtToken, collateralToken, user, debtToCover, receiveAToken)`
4. Receive seized collateral (with liquidation bonus)
5. Swap seized collateral to debt token
6. Repay flash loan
7. Keep surplus (liquidation bonus minus fees)

### 1.5 JitLiquidityExecutor.sol

**Purpose**: Provide just-in-time liquidity to Uniswap V3 pools around a large swap.

**Interface**:
```solidity
import "@openzeppelin/contracts/security/ReentrancyGuard.sol";

contract JitLiquidityExecutor is ReentrancyGuard {
    error NotOwner();
    error MintFailed();
    error BurnFailed();
    error SwapFailed();

    address public owner;

    constructor() {
        owner = msg.sender;
    }

    modifier onlyOwner() {
        if (msg.sender != owner) revert NotOwner();
        _;
    }

    /// Provide JIT liquidity: mint a concentrated position before a
    /// large swap, collect fees during the swap, burn after.
    /// NOTE: Requires a bundle relay (Flashbots/MEV-Share); Public mode
    /// cannot guarantee atomic multi-tx ordering in the same block.
    /// @param pool           Uniswap V3 pool
    /// @param tickLower      Lower tick of the position (int24, ABI-encoded as int256 by Rust)
    /// @param tickUpper      Upper tick of the position (int24, ABI-encoded as int256 by Rust)
    /// @param amount0Desired Amount of token0 to provide
    /// @param amount1Desired Amount of token1 to provide
    /// @param swapRouter     Router to swap profits back to native
    function executeJit(
        address pool,
        int24 tickLower,
        int24 tickUpper,
        uint256 amount0Desired,
        uint256 amount1Desired,
        address swapRouter
    ) external nonReentrant returns (uint256 profit);

    /// Withdraw (owner only).
    function withdraw(address token, uint256 amount) external onlyOwner;
}
```

**Execution flow** (two tx, sequential nonces, same block):
1. **Mint tx**: Call `pool.mint()` to add liquidity in the target tick range
2. **Victim swap tx** (passes through, paying fees)
3. **Burn tx**: Call `pool.burn()` to remove liquidity + collect fees

**Broadcast requirement**: Must use `submit_bundle()` via Flashbots or MEV-Share.
`TxBroadcaster` in `Public` mode should reject JIT strategy with an error.

### 1.6 ExecutorFactory.sol

**Purpose**: Single deploy-and-register contract so the Rust backend only needs one factory address per chain.

```solidity
contract ExecutorFactory {
    event ExecutorDeployed(
        ExecutorType indexed kind,
        address indexed executor,
        address indexed deployer
    );

    enum ExecutorType {
        FlashLoanArbitrage,
        Sandwich,
        Liquidation,
        JitLiquidity
    }

    /// Deploy a new executor contract (permissionless or owner-restricted).
    /// `initParams` is passed to the executor's constructor.
    function deployExecutor(ExecutorType kind, bytes calldata initParams)
        external returns (address executor);

    /// Register an already-deployed executor address.
    /// Useful for upgrading or when the factory is deployed after the executors.
    function registerExecutor(ExecutorType kind, address executor) external;

    /// Get the latest deployed/registered executor address for a given type.
    function getExecutor(ExecutorType kind) external view returns (address);
}
```

### 1.7 Interface contracts

```solidity
// IFlashLoanProvider.sol
interface IFlashLoanProvider {
    enum ProviderType { BalancerV2, AaveV3, UniswapV2, UniswapV3 }
}

// IDEXRouter.sol
interface IDEXRouter {
    function swapExactTokensForTokens(
        uint amountIn, uint amountOutMin,
        address[] calldata path, address to, uint deadline
    ) external returns (uint[] memory amounts);

    function exactInput(
        bytes calldata path, address to, uint deadline
    ) external returns (uint amountOut);
}

// IExecutorFactory.sol
interface IExecutorFactory {
    enum ExecutorType { FlashLoanArbitrage, Sandwich, Liquidation, JitLiquidity }
    function deployExecutor(ExecutorType kind, bytes calldata initParams) external returns (address);
    function registerExecutor(ExecutorType kind, address executor) external;
    function getExecutor(ExecutorType kind) external view returns (address);
}
```

---

## Phase 2 — Rust Backend Changes

### 2.1 New module: `core/src/execution/`

> **Note**: Consider placing this under `core/src/mev/execution/` instead of
> `core/src/execution/`, since `live.rs` already lives there and all execution
> concerns would be co-located.

```
core/src/execution/
├── mod.rs
├── signer.rs           # Private key management, nonce tracking
├── tx_builder.rs       # Build calldata for each executor contract
├── broadcaster.rs      # tx submission (flashbots, MEV-share, public RPC)
└── config.rs           # ExecutionConfig struct
```

### 2.2 Signer & Nonce Management

```rust
// core/src/execution/signer.rs
pub struct ExecutionSigner {
    signer: PrivateKeySigner,
    chain_id: u64,
    nonce_manager: NonceManager,
}

impl ExecutionSigner {
    pub fn from_private_key(key: &str, chain_id: u64) -> Result<Self>;
    pub async fn next_nonce(&mut self) -> Result<u64>;
    pub async fn sign_tx(&self, tx: TransactionRequest) -> Result<Signature>;
    pub fn address(&self) -> Address;
}
```

### 2.3 Transaction Builder

```rust
// core/src/execution/tx_builder.rs
pub struct TxBuilder {
    executor_factory: Address,
    chain_defaults: ChainConfig,
}

impl TxBuilder {
    /// Build a fully populated TransactionRequest with calldata, value,
    /// EIP-1559 fee fields (max_fee_per_gas, max_priority_fee_per_gas
    /// fetched from the chain via RPC), nonce, chain_id, and gas limit.
    fn build_base_tx(&self, to: Address, data: Bytes) -> Result<TransactionRequest>;

    /// Build calldata for flash loan arbitrage.
    pub fn build_arbitrage_tx(
        &self,
        opp: &MevOpportunity,
        provider: FlashLoanProvider,
        min_profit: U256,
    ) -> Result<TransactionRequest>;

    /// Build calldata for sandwich attack (front-run + back-run).
    pub fn build_sandwich_txs(
        &self,
        opp: &MevOpportunity,
        pool: Address,
    ) -> Result<(TransactionRequest, TransactionRequest)>;

    /// Build calldata for liquidation.
    pub fn build_liquidation_tx(
        &self,
        opp: &MevOpportunity,
        user: Address,
        aave_pool: Address,
    ) -> Result<TransactionRequest>;

    /// Build calldata for JIT liquidity.
    /// `tick_lower` / `tick_upper` must be ABI-encoded as int256 (32 bytes)
    /// even though the Solidity function uses int24. The alloy ABI encoder
    /// handles this automatically when using `SolCall` derive macros.
    pub fn build_jit_txs(
        &self,
        opp: &MevOpportunity,
        pool: Address,
        tick_lower: i32,
        tick_upper: i32,
    ) -> Result<(TransactionRequest, TransactionRequest)>;
}
```

### 2.4 Broadcaster (Private Relay Integration)

**Chain-specific relay defaults**:
- Ethereum: Flashbots, MEV-Share, and Public all available
- Polygon, BSC, Arbitrum, Avalanche, Base, Optimism: Public only;
  Flashbots/MEV-Share selection falls back to Public with a warning

```rust
// core/src/execution/broadcaster.rs
pub enum BroadcastMode {
    /// Submit via Flashbots relay (Ethereum only).
    Flashbots,
    /// Submit via MEV-Share (Ethereum only).
    MevShare,
    /// Submit via public RPC (eth_sendRawTransaction).
    Public,
    /// Submit via a custom private relay URL.
    CustomRelay(String),
}

impl BroadcastMode {
    /// Returns the effective mode for the given chain.
    /// Non-Ethereum chains fall back to Public if Flashbots/MEV-Share is selected.
    pub fn for_chain(self, chain_id: u64) -> Self;
}

pub struct TxBroadcaster {
    mode: BroadcastMode,
    rpc: RpcClient,
    flashbots_relay: Option<FlashbotsClient>,
    mevshare_relay: Option<MevShareClient>,
}

impl TxBroadcaster {
    /// Simulate a tx via eth_call before broadcasting (saves gas on reverts).
    pub async fn simulate_tx(&self, tx: &TransactionRequest) -> Result<SimulatedReturn>;

    /// Submit a single tx. Returns tx hash.
    pub async fn submit_tx(&self, tx: TransactionRequest) -> Result<FixedBytes<32>>;

    /// Submit a bundle of txs (for sandwich: front-run + back-run).
    /// Panics if mode is Public (not supported).
    pub async fn submit_bundle(&self, txs: Vec<TransactionRequest>) -> Result<FixedBytes<32>>;

    /// Wait for tx confirmation (poll receipt up to `max_blocks`).
    pub async fn wait_for_confirmation(
        &self, tx_hash: FixedBytes<32>, max_blocks: u64
    ) -> Result<TransactionReceipt>;
}
```

### 2.5 Updated LiveRunner

Modify `execute_mempool_opportunity()` in `core/src/mev/execution/live.rs`:

```rust
// New field on LiveRunner:
pub struct LiveRunner {
    // ... existing fields ...
    execution_signer: Option<ExecutionSigner>,  // None = simulation mode
    tx_builder: Option<TxBuilder>,
    broadcaster: Option<TxBroadcaster>,
    execution_config: Option<ExecutionConfig>,
}

// Updated execute_mempool_opportunity():
fn execute_mempool_opportunity(&mut self, opp: MevOpportunity, pending: &PendingBlockCapture) {
    if let Some(ref signer) = self.execution_signer {
        // ── REAL ON-CHAIN EXECUTION ─────────────────────────────────
        let tx = self.tx_builder
            .as_ref()
            .unwrap()
            .build_arbitrage_tx(&opp, FlashLoanProvider::Auto, self.config.min_profit_threshold_wei)
            .unwrap();
        // Pre-simulate via eth_call (saves gas on reverting txs)
        let simulation = self.broadcaster.as_ref().unwrap().simulate_tx(&tx).await;
        if simulation.is_err() {
            tracing::warn!("Pre-simulation failed, skipping broadcast");
            return;
        }
        let tx_hash = self.broadcaster.as_ref().unwrap().submit_tx(tx).await;
        let receipt = self.broadcaster.as_ref().unwrap().wait_for_confirmation(tx_hash, 5).await;
        // Update wallet from receipt: deduct gas cost, add profit
        let gas_cost = receipt.effective_gas_price * receipt.gas_used;
        self.wallet.native_balance_wei = self.wallet.native_balance_wei.saturating_sub(gas_cost);
        // Record execution
        // NOTE: do NOT also call process_settled_opportunities() for this block,
        // otherwise profits will be double-counted (once from receipt, once from replayed block).
    } else {
        // ── SIMULATION (existing logic) ─────────────────────────────
        self.execute_virtual(opp);
    }
}
```

### 2.6 Config additions

The `ExecutionConfig` struct lives in `core/src/execution/config.rs` (not in `settings.rs`)
and is referenced by `SettingsConfig` via a new `execution` field. This keeps the
execution config co-located with the execution module.

```rust
// core/src/execution/config.rs
pub struct ExecutionConfig {
    pub private_key: Option<String>,       // MEV_SCOUT_PK env var
    pub broadcast_mode: BroadcastMode,
    pub executor_factory: Option<Address>, // deployed factory address
    pub flashbots_relay_url: Option<String>,
    pub mevshare_relay_url: Option<String>,
    pub confirmation_blocks: u64,
    pub gas_limit_multiplier: f64,          // e.g. 1.2 for 20% buffer
}

impl Default for ExecutionConfig {
    fn default() -> Self {
        ExecutionConfig {
            private_key: None,
            broadcast_mode: BroadcastMode::Public,
            executor_factory: None,
            flashbots_relay_url: Some("https://relay.flashbots.net".into()),
            mevshare_relay_url: Some("https://mev-share.flashbots.net".into()),
            confirmation_blocks: 1,
            gas_limit_multiplier: 1.2,
        }
    }
}
```

### 2.7 CLI argument additions

In `cli/src/cli.rs` and `cli/src/commands/live.rs`:

| Flag | Description | Default |
|------|-------------|---------|
| `--wallet-key` or env `MEV_SCOUT_PK` | Private key for signing | — |
| `--broadcast-mode` | `public`, `flashbots`, `mevshare`, `custom` | `public` |
| `--executor-factory` | Deployed ExecutorFactory address | — |
| `--relay-url` | Custom relay URL (for `custom` mode) | — |
| `--gas-multiplier` | Gas limit multiplier (safety buffer) | 1.2 |

---

## Phase 3 — Build & Deployment

### 3.1 Foundry setup

```toml
# contracts/foundry.toml
[profile.default]
src = "src"
out = "out"
libs = ["lib"]
remappings = [
    "forge-std/=lib/forge-std/src/",
]
solc_version = "0.8.28"
optimizer = true
optimizer_runs = 1_000_000
```

**Init**:
```bash
cd contracts
forge init --no-commit
forge install foundry-rs/forge-std
```

### 3.2 Deployment script

```solidity
// contracts/script/Deploy.s.sol
contract DeployScript is Script {
    function run() external {
        uint256 deployerPrivateKey = vm.envUint("DEPLOYER_PK");
        vm.startBroadcast(deployerPrivateKey);

        // Deploy all executors
        FlashLoanArbitrage arb = new FlashLoanArbitrage();
        SandwichExecutor sandwich = new SandwichExecutor();
        LiquidationExecutor liq = new LiquidationExecutor();
        JitLiquidityExecutor jit = new JitLiquidityExecutor();

        // Deploy factory
        ExecutorFactory factory = new ExecutorFactory();
        factory.registerExecutor(ExecutorFactory.ExecutorType.FlashLoanArbitrage, address(arb));
        factory.registerExecutor(ExecutorFactory.ExecutorType.Sandwich, address(sandwich));
        factory.registerExecutor(ExecutorFactory.ExecutorType.Liquidation, address(liq));
        factory.registerExecutor(ExecutorFactory.ExecutorType.JitLiquidity, address(jit));

        console.log("Factory deployed at:", address(factory));

        vm.stopBroadcast();
    }
}
```

### 3.3 Deployment config per chain

In `core/src/config/defaults.rs`, add:

```rust
pub fn default_executor_addresses() -> HashMap<String, HashMap<ExecutorType, Address>> {
    // Maps chain_name -> executor_type -> deployed address
    // Populated after initial deployment
}
```

---

## Phase 4 — Testing

### 4.1 Solidity tests (Forge)

| Test File | What It Tests |
|-----------|---------------|
| `FlashLoanArbitrage.t.sol` | Flash loan callback, swap path encoding, profit verification |
| `SandwichExecutor.t.sol` | Sequential nonce ordering, front-run/back-run, revert on unprofitable |
| `LiquidationExecutor.t.sol` | Aave V3 liquidation call, collateral swap, flash loan repayment |
| `JitLiquidityExecutor.t.sol` | Mint/burn in same block, fee collection, profit extraction |

### 4.2 Rust integration tests

| Test | What It Tests |
|------|---------------|
| `test_tx_builder_arbitrage()` | Correct calldata encoding for FlashLoanArbitrage |
| `test_tx_builder_sandwich()` | Two sequential tx calldata with linked nonces |
| `test_signer_nonce_management()` | Nonce increment, persistence across restarts |
| `test_broadcaster_flashbots_submit()` | Bundle format, relay URL construction |
| `test_full_pipeline_anvil()` | Spawn Anvil fork, deploy executors, detect MEV, execute |

---

## Files to Create / Modify

| File | Action |
|------|--------|
| `contracts/foundry.toml` | **Create** |
| `contracts/src/executors/FlashLoanArbitrage.sol` | **Create** |
| `contracts/src/executors/SandwichExecutor.sol` | **Create** |
| `contracts/src/executors/LiquidationExecutor.sol` | **Create** |
| `contracts/src/executors/JitLiquidityExecutor.sol` | **Create** |
| `contracts/src/interfaces/IFlashLoanProvider.sol` | **Create** |
| `contracts/src/interfaces/IDEXRouter.sol` | **Create** |
| `contracts/src/interfaces/IExecutorFactory.sol` | **Create** |
| `contracts/src/factory/ExecutorFactory.sol` | **Create** |
| `contracts/script/Deploy.s.sol` | **Create** |
| `contracts/test/FlashLoanArbitrage.t.sol` | **Create** |
| `contracts/test/SandwichExecutor.t.sol` | **Create** |
| `contracts/test/LiquidationExecutor.t.sol` | **Create** |
| `contracts/test/JitLiquidityExecutor.t.sol` | **Create** |
| `core/src/execution/mod.rs` | **Create** |
| `core/src/execution/signer.rs` | **Create** |
| `core/src/execution/tx_builder.rs` | **Create** |
| `core/src/execution/broadcaster.rs` | **Create** |
| `core/src/execution/config.rs` | **Create** |
| `core/src/mev/execution/live.rs` | **Modify** — add real tx path |
| `core/src/types/strategy.rs` | **Modify** — add `BroadcastMode`, `ExecutorType` |
| `core/src/config/settings.rs` | **Modify** — add `execution: ExecutionConfig` field |
| `core/src/config/defaults.rs` | **Modify** — add executor addresses per chain |
| `core/src/lib.rs` | **Modify** — add `pub mod execution;` |
| `cli/src/cli.rs` | **Modify** — add wallet/relay CLI args |
| `cli/src/commands/live.rs` | **Modify** — dispatch with new args |

---

## Risk & Mitigation

| Risk | Impact | Mitigation |
|------|--------|------------|
| **Failed tx = wasted gas** | Financial loss | Conservative `min_profit_threshold`, pre-simulation with revm before broadcast |
| **Flash loan callback reverts** | No principal loss, but gas wasted | Test all paths on fork (Anvil) before mainnet |
| **MEV competition (front-running)** | Execution fails | Use Flashbots/MEV-Share for privacy; bundle tx ordering |
| **Private key compromise** | Total loss of funds | Use dedicated hot wallet; withdraw profits regularly; env var only, never in code |
| **Contract bugs (reentrancy, etc.)** | Loss of executor funds | OpenZeppelin `ReentrancyGuard`; rigorous forge tests; third-party audit before mainnet use |
| **Nonce mismatch (sandwich)** | Tx ordering fails | `NonceManager` tracks nonce per chain; retry with updated nonce |
| **Chain differences** | Relay not available | Graceful fallback: `Flashbots → Public` for Ethereum; `Public` only for Polygon/BSC |

---

## Out of Scope (Future Iterations)

- Real-time P&L dashboard with DeFi protocol positions (Uniswap V3 LP, Aave deposits)
- Multi-instance / multi-chain concurrent operation
- Telegram / Discord alerts on executed trades
- MEV-Share bid optimization (dynamic bid price based on opportunity size)
- Cross-chain arbitrage (liquidity fragmentation across chains)
- Governance / DAO-controlled executor upgrades
- On-chain MEV strategy registry for community-submitted strategies
