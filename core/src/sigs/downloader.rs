use std::io::Read;
use std::path::PathBuf;

/// URL for the pre-built signature database (zstd-compressed SQLite).
const SIG_DB_URL: &str = "https://d39my35jed0oxi.cloudfront.net/mevlog-sigs-v5.db.zst";
/// Local filename for the decompressed signature database.
const SIG_DB_FILENAME: &str = "mevlog-sigs-v5.db";

/// Return the default path for the signature database.
pub fn default_sig_db_path() -> PathBuf {
    let mut path = dirs_data_dir();
    path.push(SIG_DB_FILENAME);
    path
}

/// Return the data directory for mev-scout.
fn dirs_data_dir() -> PathBuf {
    let base = std::env::var("MEV_SCOUT_DATA_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            let home = std::env::var("HOME")
                .or_else(|_| std::env::var("USERPROFILE"))
                .unwrap_or_else(|_| ".".to_string());
            PathBuf::from(home).join(".mev-scout")
        });
    base
}

/// Build a comprehensive signature DB with all known DEX/MEV signatures.
/// Used as fallback when the CDN download fails.
/// Covers Uniswap V2/V3, Balancer V2, Curve, Aave V3, WETH, ERC20/721, MEV ops.
fn build_fallback_db(db_path: &PathBuf) -> anyhow::Result<()> {
    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let conn = rusqlite::Connection::open(db_path)?;
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS methods (
            selector BLOB PRIMARY KEY,
            signature TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS events (
            topic BLOB PRIMARY KEY,
            signature TEXT NOT NULL
        );",
    )?;

    // ── Comprehensive method signatures ──────────────────────────────────
    // ERC20 / ERC721 / common
    let methods: &[(&str, &str)] = &[
        ("a9059cbb", "transfer(address,uint256)"),
        ("23b872dd", "transferFrom(address,address,uint256)"),
        ("095ea7b3", "approve(address,uint256)"),
        ("70a08231", "balanceOf(address)"),
        ("18160ddd", "totalSupply()"),
        ("dd62ed3e", "allowance(address,address)"),
        ("313ce567", "decimals()"),
        ("06fdde03", "name()"),
        ("95d89b41", "symbol()"),
        ("d0e30db0", "deposit()"),
        ("2e1a7d4d", "withdraw(uint256)"),
        ("6352211e", "ownerOf(uint256)"),
        ("c87b56dd", "tokenURI(uint256)"),
        ("42842e0e", "safeTransferFrom(address,address,uint256)"),
        ("b88d4fde", "safeTransferFrom(address,address,uint256,bytes)"),
        ("f242432a", "safeTransferFrom(address,address,uint256,uint256,bytes)"),
        ("01ffc9a7", "supportsInterface(bytes4)"),
        ("17307eab", "setApprovalForAll(address,bool)"),
        ("ac9650d8", "multicall(bytes[])"),

        // ── Uniswap V2 Pair ───────────────────────────────────────────────
        ("022c0d9f", "swap(uint256,uint256,address,bytes)"),
        ("6a627842", "mint(address)"),
        ("89afcb44", "burn(address)"),
        ("bc25cf77", "skim(address)"),
        ("fff6cae9", "sync()"),
        ("0dfe1681", "token0()"),
        ("d21220a7", "token1()"),
        ("0902f1ac", "getReserves()"),
        ("7464fc3d", "price0CumulativeLast()"),
        ("0b6d63b5", "price1CumulativeLast()"),
        ("ba37fdbc", "kLast()"),
        ("0d6f813b", "factory()"),
        ("570d8e1d", "initialize(address,address)"),
        ("d505accf", "permit(address,address,uint256,uint256,uint8,bytes32,bytes32)"),
        ("e3ee160e", "transferWithAuthorization(address,address,uint256,uint256,uint256,bytes32,uint8,bytes32,bytes32)"),

        // ── Uniswap V2 Factory ────────────────────────────────────────────
        ("c9c65396", "createPair(address,address)"),
        ("a2e74af6", "setFeeTo(address)"),
        ("f46901ed", "setFeeToSetter(address)"),
        ("1e3dd18b", "allPairs(uint256)"),
        ("574f2ba3", "allPairsLength()"),
        ("66bd2e3a", "feeTo()"),
        ("c45a0155", "feeToSetter()"),
        ("09218e2b", "getPair(address,address)"),

        // ── Uniswap V3 Pool ───────────────────────────────────────────────
        ("128acb08", "swap(address,bool,int256,uint256,uint256,address,bytes)"),
        ("3ccfd60b", "mint(address,address,int24,int24,uint128,bytes)"),
        ("0dfe1681", "token0()"),
        ("d21220a7", "token1()"),
        ("ddca3f43", "fee()"),
        ("3850c7bd", "slot0()"),
        ("883bdbfd", "observations(uint256)"),
        ("4f76a8fa", "tickSpacing()"),
        ("a34123a7", "mint(address,address,int24,int24,uint128,bytes)"),
        ("514ea4bf", "liquidity()"),
        ("81794f1b", "tickBitmap(int16)"),
        ("9ad05bc6", "tickSpacingToMaxLiquidityPerTick(int24)"),
        ("d5764e8e", "increaseObservationCardinalityNext(uint16)"),
        ("e67446fd", "snapshotCumulativesInside(int24,int24)"),
        ("f3058399", "positions(bytes32)"),
        ("99c6b2f4", "maxLiquidityPerTick()"),
        ("240bc6ad", "burn(int24,int24,uint128)"),
        ("7c1fe0c6", "collect(address,int24,int24,uint128,uint128)"),

        // ── Uniswap V3 Factory ────────────────────────────────────────────
        ("1698ee82", "createPool(address,address,uint24)"),
        ("22e024cf", "enableFeeAmount(uint24,int24)"),
        ("13e75725", "setOwner(address)"),
        ("1e8c5b89", "owner()"),
        ("2e8ce9bc", "feeAmountTickSpacing(uint24)"),
        ("30b46e1c", "parameters()"),
        ("96afc450", "getPool(address,address,uint24)"),

        // ── Uniswap Universal Router / Permit2 ────────────────────────────
        ("3593564c", "execute(bytes,bytes[],uint256)"),
        ("24856bc3", "execute(bytes,bytes[],uint256,address)"),
        ("12210e8a", "transferNativeToken(address,uint256)"),
        ("2b5c727c", "receiveNativeToken(address,uint256)"),
        ("b75b86b2", "executeCommand(uint256,bytes)"),
        ("f2372ab9", "executeCommandWithDeadline(uint256,bytes,uint256)"),
        ("2255c255", "takeAll(address,uint256,address)"),
        ("f7a8b9e0", "take(bytes,address,uint256)"),
        ("a3233f1e", "settle(address,uint256,uint256,address)"),
        ("30c28b55", "sweep(address,address,uint256)"),
        ("0d5f1a08", "wrapETH(address,uint256)"),
        ("6a2d9c20", "unwrapWETH(address,uint256)"),
        ("2e2a4e50", "approveERC20(address,address,uint256)"),
        ("0a19dfb8", "lockdown(bytes32[])"),

        // ── Uniswap V4 ────────────────────────────────────────────────────
        ("b9c27f3a", "swap(address,bytes,(address,bool,int256,uint256,uint256,address,bytes))"),
        ("a6b57bed", "modifyLiquidity(bytes32,address,(int256,int256,uint256,bytes))"),
        ("6872ae88", "donate(address,bytes,uint256,uint256)"),
        ("69de36b6", "initialize(bytes32,uint160,uint256)"),
        ("93fced1c", "lock(bytes,uint256)"),

        // ── Uniswap V2 Router ─────────────────────────────────────────────
        ("38ed1739", "swapExactTokensForTokens(uint256,uint256,address[],address,uint256)"),
        ("7ff36ab5", "swapExactETHForTokens(uint256,address[],address,uint256)"),
        ("8803dbee", "swapTokensForExactTokens(uint256,uint256,address[],address,uint256)"),
        ("fb3bdb41", "swapETHForExactTokens(uint256,address[],address,uint256)"),
        ("18cbafe5", "swapExactTokensForETH(uint256,uint256,address[],address,uint256)"),
        ("5c11d795", "swapExactTokensForTokensSupportingFeeOnTransferTokens(uint256,uint256,address[],address,uint256)"),
        ("db6d1610", "swapTokensForExactETH(uint256,uint256,address[],address,uint256)"),
        ("880b9543", "swapExactETHForTokensSupportingFeeOnTransferTokens(uint256,address[],address,uint256)"),
        ("02751cec", "swapExactTokensForETHSupportingFeeOnTransferTokens(uint256,uint256,address[],address,uint256)"),
        ("ad615dec", "addLiquidity(address,address,uint256,uint256,uint256,uint256,address,uint256)"),
        ("e8e33700", "addLiquidityETH(address,uint256,uint256,uint256,address,uint256)"),
        ("b4f9c3e4", "removeLiquidity(address,address,uint256,uint256,uint256,address,uint256)"),
        ("02751cec", "removeLiquidityETH(address,uint256,uint256,uint256,address,uint256)"),
        ("0dede6c4", "removeLiquidityWithPermit(address,address,uint256,uint256,uint256,address,uint256,bool,uint8,bytes32,bytes32)"),
        ("ac378cff", "removeLiquidityETHWithPermit(address,uint256,uint256,uint256,address,uint256,bool,uint8,bytes32,bytes32)"),

        // ── Uniswap V3 Router ─────────────────────────────────────────────
        ("49404b7c", "exactInputSingle(tuple)"),
        ("414bf389", "exactInput(tuple)"),
        ("db3e2198", "exactOutputSingle(tuple)"),
        ("2eac270a", "exactOutput(tuple)"),
        ("0dc4bdae", "exactInputV2Swap((address,address,uint256,uint256,uint256,uint256,address[],address[],bytes,string),uint256)"),
        ("5c8961fe", "multicall(bytes[])"),
        ("8381f58a", "uniswapV3SwapCallback(int256,int256,bytes)"),

        // ── 0x Protocol ──────────────────────────────────────────────────
        ("6af479b2", "getOrdersInfo(tuple[])"),
        ("95bc731b", "getLimitOrders(tuple)"),
        ("b86a4765", "fillOrder(tuple,uint256,bytes)"),
        ("f78a0cc3", "fillOrKillOrder(tuple,uint256,bytes)"),
        ("92b50462", "batchFillOrders(tuple[],uint256[],bytes[])"),
        ("fc4c0b71", "batchFillOrKillOrders(tuple[],uint256[],bytes[])"),
        ("2d58e7aa", "marketSellOrders(tuple[],uint256,uint256,bytes[])"),
        ("66a8b7c8", "marketBuyOrders(tuple[],uint256,uint256,bytes[])"),
        ("adbf7643", "marketSellOrdersNoThrow(tuple[],uint256,uint256,bytes[])"),
        ("301c68a9", "marketBuyOrdersNoThrow(tuple[],uint256,uint256,bytes[])"),
        ("e38f46fc", "batchCancelOrders(tuple[])"),
        ("3b54736c", "cancelOrder(tuple)"),
        ("0c5ea1f7", "preSign(uint256,bytes,address)"),
        ("3964a0f0", "setSignatureValidatorApproval(address,bool)"),

        // ── Balancer V2 Vault ─────────────────────────────────────────────
        ("52bbbe29", "swap(tuple,address,bytes)"),
        ("7b3c1ef2", "batchSwap(tuple,address[],address,bytes)"),
        ("c4b5e15f", "flashLoan(address,address[],uint256[],uint256[],address,bytes,uint16)"),
        ("b95cac28", "joinPool(bytes32,address,address,(address[],uint256[],bytes,bool))"),
        ("8bdb3913", "exitPool(bytes32,address,address,(address[],uint256[],bytes,bool))"),
        ("dc2f022b", "managePoolBalance(tuple[])"),
        ("bf39c4a7", "getPoolTokens(bytes32)"),
        ("9b3b5f8f", "swap(uint16,address,uint256,uint256,address,bytes)"),
        ("15c0fb60", "getPool(bytes32)"),
        ("f84e2cdf", "getPoolId(address)"),
        ("66c0bd24", "getActionId(bytes4)"),
        ("c072ceac", "getAuthorizer()"),
        ("e33ee663", "getDomainSeparator()"),
        ("2fda3b12", "getNextNonce(address)"),
        ("43f68f68", "setRelayApproval(address,address,bool)"),
        ("5a6bee7b", "setPaused(bool)"),
        ("0263bb64", "setAuthorizer(address)"),
        ("b01a1c14", "changeFlashLoanFeePercentage(uint256)"),
        ("0b6a1f88", "collectProtocolFees()"),
        ("9a28ea83", "recoveryModeExitPool(bytes32,address,address,uint256[],uint256)"),
        ("b7b3a7cf", "registerPool(uint256)"),

        // ── Balancer V2 Pool ──────────────────────────────────────────────
        ("1a148570", "getSwapFeePercentage()"),
        ("f8b49a79", "getProtocolFeesCollector()"),
        ("b7b3a7cf", "registerPool(uint256)"),
        ("6b35adf1", "getBalances()"),
        ("598b4ba6", "getLastInvariant()"),
        ("39b70e4a", "getGradualWeightUpdateParams()"),
        ("3c975d51", "inRecoveryMode()"),

        // ── Curve Pool ────────────────────────────────────────────────────
        ("a6417ed6", "add_liquidity(uint256[2],uint256)"),
        ("4515cef3", "add_liquidity(uint256[3],uint256)"),
        ("029b2f34", "add_liquidity(address,uint256[2],uint256)"),
        ("3df02124", "exchange(int128,int128,uint256,uint256)"),
        ("5b41b908", "exchange_underlying(int128,int128,uint256,uint256)"),
        ("0699bb7c", "exchange(int128,int128,uint256,uint256,address)"),
        ("7c025200", "swap(tuple)"),
        ("21b8c064", "remove_liquidity(uint256,uint256[2])"),
        ("b4d1d795", "remove_liquidity_imbalance(uint256[2],uint256)"),
        ("bfb2676b", "remove_liquidity_one_coin(uint256,int128,uint256)"),
        ("1b6c8bb5", "remove_liquidity_one_coin(uint256,int128,uint256,bool)"),
        ("ecb58678", "calc_withdraw_one_coin(uint256,int128,bool)"),
        ("ced67e0c", "calc_token_amount(uint256[2],bool)"),
        ("3e52d1f1", "calc_withdraw_one_coin(uint256,int128)"),
        ("cf2c6d0e", "get_dy(int128,int128,uint256)"),
        ("f0d07fae", "get_dy_underlying(int128,int128,uint256)"),
        ("08b9c91f", "get_virtual_price()"),
        ("58355032", "price_oracle()"),
        ("4b4ca580", "get_pool()"),
        ("5e2c0ee1", "coins(uint256)"),
        ("329316d1", "balances(uint256)"),
        ("c6610657", "coins(uint256)"),

        // ── Curve Registry ────────────────────────────────────────────────
        ("6b36b0af", "find_pool_for_coins(address,address,uint256)"),
        ("6c8c22bf", "get_coin_indices(address,address,address)"),
        ("1f0e4a0b", "get_pool_name(address)"),
        ("37cd0b68", "get_n_coins(address)"),
        ("8e55d7e4", "get_coins(address)"),
        ("98435af8", "get_underlying_coins(address)"),
        ("2756b99a", "get_decimals(address)"),
        ("611fdfcf", "get_underlying_decimals(address)"),
        ("0bc7353c", "get_rates(address)"),
        ("0d223b63", "get_gauges(address)"),
        ("6f7dc490", "get_metapool_rates(address)"),
        ("b4853633", "get_pool_asset_type(address)"),
        ("25f2b0c2", "get_lp_token(address)"),
        ("55d6c5a8", "get_pool_from_lp_token(address)"),

        // ── Aave V3 Pool ──────────────────────────────────────────────────
        ("617ba037", "supply(address,uint256,address,uint16)"),
        ("69328dec", "withdraw(address,uint256,address)"),
        ("573ade81", "borrow(address,uint256,uint256,uint16,address)"),
        ("ab9c4b5d", "repay(address,uint256,uint256,address)"),
        ("c4b5e15f", "flashLoan(address,address[],uint256[],uint256[],address,bytes,uint16)"),
        ("6b6e46b9", "flashLoanSimple(address,address,uint256,bytes,uint16)"),
        ("a415bcad", "getReserveData(address)"),
        ("35ea6a75", "getReserveData(address)"),
        ("d15e0053", "getReserveNormalizedVariableDebt(address)"),
        ("3864971f", "getConfiguration(address)"),
        ("7c4e560b", "getEModeCategoryData(uint8)"),
        ("73b64ac5", "setUserEMode(uint8)"),
        ("cc0cc2bd", "getUserAccountData(address)"),
        ("a94b93b1", "getUserConfiguration(address)"),
        ("7e865aa4", "initReserve(address,address,uint256,uint256,uint256,address,address)"),
        ("cd0672d5", "dropReserve(address)"),
        ("44b39396", "setReserveInterestRateStrategyAddress(address,address)"),
        ("3da60205", "setConfiguration(address,uint256)"),
        ("247b058b", "liquidationCall(address,address,address,uint256,bool)"),
        ("590f7e3b", "liquidate(address,address,address,uint256,address,uint256,uint16)"),
        ("a37b38bc", "setUserUseReserveAsCollateral(address,bool)"),
        ("f8119d51", "rebalanceStableBorrowRate(address,address)"),
        ("0c4e5e44", "swapBorrowRateMode(address)"),
        ("c58cf0af", "setReserveFactor(address,uint256)"),
        ("b1bf962d", "setPoolPause(bool)"),

        // ── Aave V3 Pool Addresses Provider ───────────────────────────────
        ("6c6f027e", "getPriceOracle()"),
        ("a28e38c7", "getACLAdmin()"),
        ("b9e32e6a", "getPoolConfigurator()"),
        ("a78b1f66", "getPoolDataProvider()"),

        // ── Wrapped Native (WETH/WMATIC/WBNB) ────────────────────────────
        ("d0e30db0", "deposit()"),
        ("2e1a7d4d", "withdraw(uint256)"),

        // ── MEV / Flashbots ──────────────────────────────────────────────
        ("6a761202", "execTransaction(address,uint256,bytes,uint8,uint256,uint256,uint256,address,address,bytes)"),
        ("765e827f", "handleOps((address,uint256,bytes,bytes,bytes32,uint256,bytes32,bytes,bytes)[],address)"),
        ("405cec67", "relayCall(address,address,bytes,uint256,uint256,uint256,uint256,bytes,bytes)"),
        ("a1305b17", "relayCall(address,address,bytes,uint256,bytes,uint256,address,bytes)"),
        ("0a3c4405", "proxy((address,uint256,uint256,(address,uint256,bytes)[])[],bytes[])"),
        ("77835641", "deploy(address[],bytes32[])"),
        ("dbeccb23", "redeemPositions(bytes32,uint256[])"),
        ("5638f1f3", "redeemSilence(address,uint256)"),
        ("46a73fb1", "silence(address,uint256,uint256)"),
        ("3c2b4399", "matchOrders(bytes32,(uint256,address,address,uint256,uint256,uint256,uint8,uint8,uint256,bytes32,bytes32,bytes),(uint256,address,address,uint256,uint256,uint256,uint8,uint8,uint256,bytes32,bytes32,bytes)[],uint256,uint256[],uint256,uint256[])"),
        ("e2bbb158", "deposit(uint256)"),
        ("7c025200", "swap(tuple)"),
        ("6fadcf72", "forward(address,bytes)"),

        // ── Generic / Infrastructure ──────────────────────────────────────
        ("50d25bcd", "latestRoundData()"),
        ("feaf968c", "latestAnswer()"),
        ("8da5cb5b", "owner()"),
        ("715018a6", "renounceOwnership()"),
        ("f2fde38b", "transferOwnership(address)"),
        ("5c975abb", "paused()"),
        ("8456cb59", "pause()"),
        ("3f4ba83a", "unpause()"),
        ("40c10f19", "mint(address,uint256)"),
        ("42966c68", "burn(uint256)"),
        ("d286f3cf", "claimInterest(uint256,uint256)"),
        ("a694fc3a", "stake(uint256)"),
        ("1e83409a", "claim(address)"),
        ("9ebea88c", "unstake(uint256,bool)"),
        ("363f0f0d", "stakeWithPermit(uint256,uint256,uint8,bytes32,bytes32)"),

        // ── LayerZero / Cross-chain ──────────────────────────────────────
        ("353b0d4f", "lzReceive(uint16,bytes,uint64,bytes)"),
        ("967425ba", "lzReceive(uint16,bytes,uint64,bytes,uint256,bytes)"),
        ("c58056ce", "send(uint16,bytes,bytes,address,address,bytes)"),
        ("e5b5019a", "estimateFees(uint16,address,bytes,bool,bytes)"),
        ("2cdf0b95", "setConfig(uint16,uint16,uint256,bytes)"),
        ("46f1d80a", "setSendVersion(uint16)"),
        ("ab8526ad", "setReceiveVersion(uint16)"),
        ("3c73a9e4", "forceResumeReceive(uint16,bytes)"),
        ("7d6bc40e", "setTrustedRemote(uint16,bytes)"),
        ("1d7db736", "setMinDstGas(uint16,uint16,uint256)"),
        ("22015e36", "setPayloadSizeLimit(uint16,uint256)"),

        // ── ERC-2612 Permits ──────────────────────────────────────────────
        ("d505accf", "permit(address,address,uint256,uint256,uint8,bytes32,bytes32)"),
        ("e3ee160e", "transferWithAuthorization(address,address,uint256,uint256,uint256,bytes32,uint8,bytes32,bytes32)"),
        ("2acf3c58", "receiveWithAuthorization(address,address,uint256,uint256,uint256,bytes32,uint8,bytes32,bytes32)"),
        ("891b669b", "cancelAuthorization(address,bytes32,uint8,bytes32,bytes32)"),
        ("79cc6799", "metaTransferAuthorization(address,address,uint256,uint256,uint256,bytes32,uint8,bytes32,bytes32)"),

        // ── ERC-4626 Vault ────────────────────────────────────────────────
        ("b460af94", "deposit(uint256,address)"),
        ("4ce38e37", "mint(uint256,address)"),
        ("b3d7f6b9", "withdraw(uint256,address,address)"),
        ("5b88c2d0", "redeem(uint256,address,address)"),
        ("ce96cb77", "maxDeposit(address)"),
        ("b36a3bd1", "maxMint(address)"),
        ("e8e32529", "maxWithdraw(address)"),
        ("1b871b3b", "maxRedeem(address)"),
        ("07a2d13a", "previewDeposit(uint256)"),
        ("b0c18c2a", "previewMint(uint256)"),
        ("1b3ed722", "previewWithdraw(uint256)"),
        ("188f0d1d", "previewRedeem(uint256)"),
        ("3644e515", "convertToShares(uint256)"),
        ("c6e6f592", "convertToAssets(uint256)"),
        ("0d8e6e2c", "totalAssets()"),

        // ── Curve pool state (used by factory.rs) ──────────────────────────
        ("497b6678", "balances(int128)"),
        ("065a80d8", "balances(int128)"),
        ("c6611f94", "coins(int128)"),
        ("196cac5f", "coins(uint256)"),
        ("23746eb8", "coins(int128)"),
        ("0f0b7c7e", "A()"),
        ("f446c1d0", "A()"),
        ("4d30a47f", "get_A()"),
        ("671d4723", "gamma()"),
        ("5e0d7a5a", "price_scale()"),
        ("aa1e2984", "price_oracle(uint256)"),
        ("9cec6eae", "base_pool()"),

        // ── Uniswap V3 Router (used by mempool detector) ───────────────────
        ("c04b8d59", "exactInput((bytes,address,uint256,uint256,uint256))"),
        ("f28c0498", "exactOutput((bytes,address,uint256,uint256,uint256))"),

        // ── Uniswap V2 Router (correct selectors) ─────────────────────────
        ("8803dbee", "swapTokensForExactTokens(uint256,uint256,address[],address,uint256)"),

        // ── tickSpacing (correct selector) ────────────────────────────────
        ("d0c93a7c", "tickSpacing()"),

        // ── DODO pool selectors (used by discovery.rs) ─────────────────────
        ("e1503108", "_BASE_TOKEN_()"),
        ("0fd8bafe", "_QUOTE_TOKEN_()"),

        // ── PancakeSwap / fork tickSpacing variants (used by discovery.rs) ─
        ("37cfdaca", "tickSpacing()"),
    ];

    // ── Comprehensive event signatures ────────────────────────────────────
    let events: &[(&str, &str)] = &[
        // ERC20 / Generic
        ("ddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef", "Transfer(address,address,uint256)"),
        ("8c5be1e5ebec7d5bd14f71427d1e84f3dd0314c0f7b2291e5b200ac8c7c3b925", "Approval(address,address,uint256)"),
        ("17307eab39ab6107e8899845ad3d59bd9653f200f220920489ca2b5937696c31", "ApprovalForAll(address,address,bool)"),

        // Uniswap V2 Pair
        ("d78ad95fa46c994b6551d0da85fc275fe613ce37657fb8d5e3d130840159d822", "Swap(address,uint256,uint256,uint256,uint256,address)"),
        ("1c411e9a96e071241c2f21f7726b17ae89e3cab4c78be50e062b03a9fffbbad1", "Sync(uint112,uint112)"),
        ("4c209b5fc8ad50758f13e2e1088ba56a560dff690a1c6fef26394f4c03821c4f", "Mint(address,uint256,uint256)"),
        ("dccd412f0b1252819cb1fd330b93224ca42612892bb3f4f789976e6d81936496", "Burn(address,uint256,uint256,address)"),

        // Uniswap V2 Factory
        ("0d3648bd0f6ba80134a33ba9275ac585d9d315f0ad8355cddefde31afa28d0e9", "PairCreated(address,address,address,uint256)"),

        // Uniswap V3 Pool
        ("c42079f94a6350d7e6235f29174924f928cc2ac818eb64fed8004e115fbcca67", "Swap(address,address,int256,int256,uint160,uint128,int24)"),
        ("7a53080ba414158be7ec69b987b5fb7d07dee101fe85488f0853ae16239d0bde", "Mint(address,address,int24,int24,uint128,uint256,uint256)"),
        ("0c396cd989a39f4459b5fa1aed6a9a8dcdbc45908acfd67e028cd568da98982c", "Burn(address,int24,int24,uint128,uint256,uint256)"),
        ("70935338e69775456a85ddef226c395fb668b63fa0115f5f20610b388e6ca9c0", "Collect(address,address,int24,int24,uint128,uint128)"),
        ("98636036cb66a9c19a37435efc1e90142190214e8abeb821bdba3f2990dd4c95", "Initialize(uint160,int24)"),

        // Uniswap V3 Factory
        ("783cca1c0412dd0d695e784568c96da2e9c22ff989357a2e8b1d9b2b4e6b7118", "PoolCreated(address,address,uint24,int24,address)"),

        // Balancer V2 Vault
        ("03f136671577c42a8d927db6cc79022fa6b34165c6b08d1e479327ed183b2fdf", "Swap(bytes32,uint256,uint256,uint256,uint256,address,address,address,uint256,uint256)"),
        ("c58f35ca649f2ebaf8f1189e10cb3c07d7255c417018d4002ad6dceb86d27e74", "FlashLoan(address,address,address,uint256,uint256,uint256)"),
        ("947ff4e0d0464be2d887b57d38dc37b5109e07b91290f501caa5243addc89588", "PoolBalanceChanged(bytes32,address,address,uint256[],uint256[],uint256[],uint256)"),
        ("6edcaf6241105b4c94c2efdbf3a6b12458eb3d07be3a0e81d24b13c44045fe7a", "PoolBalanceManaged(bytes32,address,address,int256,int256)"),

        // Curve Pool
        ("8b3e96f2b889fa771c53c981b40daf005f63f637f1869f707052d15a3dd97140", "TokenExchange(address,int128,uint256,int128,uint256)"),
        ("d013ca23e77a65003c2c659c5442c00c805371b7fc1ebd4c206c41d1536bd90b", "TokenExchangeUnderlying(address,int128,uint256,int128,uint256)"),
        ("26f55a85081d24974e85c6c00045d0f0453991e95873f52bff0d21af4079a768", "AddLiquidity(address,uint256[2],uint256[2],uint256,uint256)"),
        ("7c363854ccf79623411f8995b362bce5eddff18c927edc6f5dbbb5e05819a82c", "RemoveLiquidity(address,uint256[2],uint256[2],uint256)"),
        ("43fb02998f4e03da2e0e6fff53fdbf0c40a9f45f145dc377fc30615d7d7a8a64", "RemoveLiquidityOne(address,uint256,uint256,uint256,uint256)"),
        ("2b5508378d7e19e0d5fa338419034731416c4f5b219a10379956f764317fd47e", "RemoveLiquidityImbalance(address,uint256[2],uint256[2],uint256,uint256)"),

        // Aave V3 Pool
        ("de6857219544bb5b7746f48ed30be6386fefc61b2f864cacf559893bf50fd951", "Deposit(address,address,address,uint256,uint16)"),
        ("3115d1449a7b732c986cba18244e897a450f61e1bb8d589cd2e69e6c8924f9f7", "Withdraw(address,address,address,uint256)"),
        ("39884ffb02602a13fb58b50134a8735509d9c8f846d749abcb003939e159f733", "Borrow(address,address,address,uint256,uint256,uint16)"),
        ("4cdde6e09bb755c9a5589ebaec640bbfedff1362d4b255ebf8339782b9942faa", "Repay(address,address,address,uint256)"),
        ("bc7cd75a20ee27fd9adebab32041f755214dbc6bffa90cc0225b39da2e5c2d3b", "Upgraded(address)"),
        ("7e644d79422f17c01e4894b5f4f588d331ebfa28653d42ae832dc59e38c9798f", "AdminChanged(address,address)"),

        // WETH / Wrapped Native
        ("e1fffcc4923d04b559f4d29a8bfc6cda04eb5b0d3c460751c2402c5c5cc9109c", "Deposit(address,uint256)"),
        ("7fcf532c15f0a6db0bd6d0e038bea71d30d808c7d98cb3bf7268a95bf5081b65", "Withdrawal(address,uint256)"),

        // ERC-4626
        ("dcbc1c05240f31ff3ad067ef1ee35ce4997762752e3a095284754544f4c709d7", "Deposit(address,address,uint256,uint256)"),
        ("fbde797d201c681b91056529119e0b02407c7bb96a4a2c75c01fc9667232c8db", "Withdraw(address,address,address,uint256,uint256)"),

        // ERC-2612 Permit / MetaTx
        ("98de503528ee59b575ef0c0a2576a82497bfc029a5685b209e9ec333479b10a5", "AuthorizationUsed(address,bytes32)"),
        ("1cdd46ff242716cdaa72d159d339a485b3438398348d68f09d7c8c0a59353d81", "AuthorizationCanceled(address,bytes32)"),

        // Flashbots / MEV
        ("7fcf532c15f0a6db0bd6d0e038bea71d30d808c7d98cb3bf7268a95bf5081b65", "Withdrawal(address,uint256)"),

        // LayerZero
        ("7cbf52d2f4e464c2fa3f4efd38a15b0b722d0c038c9b692f54c0e0e0a9dc3a16", "PacketSent(uint16,bytes,bytes,uint64,uint16,bytes,uint256)"),
        ("92cf4c1500f8410f73a50a8f7a94e5f6faa1df68beef7095b789344b8ed29129", "PacketReceived(uint16,bytes,uint64,bytes)"),

        // 0x Protocol
        ("f22d49d86be3f16496ab51f25ce7821e2e17d9539be6c61b6457ed4811cf79cb", "OrderFilled(address,bytes32,address,address,uint256,uint256,uint256,uint256,uint256)"),
        ("26b214029d2b6a3a3bb2ae7cc0a5d4c9329a86381429e16dc45b3633cf83d369", "OrderCancelled(address,bytes32,uint256)"),

        // Generic events
        ("8be0079c531659141344cd1fd0a4f28419497f9722a3daafe3b4186f6b6457e0", "OwnershipTransferred(address,address)"),
        ("62e78cea01bee320cd4e420270b5ea74000d11b0c9f74754ebdbfc544b05a258", "Paused(address)"),
        ("5db9ee0a495bf2e6ff9c91a7834c1ba4fdd244a5e8aa4e537bd38aeae4b073aa", "Unpaused(address)"),
    ];

    let mut m_stmt = conn.prepare("INSERT OR IGNORE INTO methods (selector, signature) VALUES (?1, ?2)")?;
    for (hex, sig) in methods {
        let sel = hex::decode(hex)?;
        m_stmt.execute(rusqlite::params![sel, sig])?;
    }
    drop(m_stmt);

    let mut e_stmt = conn.prepare("INSERT OR IGNORE INTO events (topic, signature) VALUES (?1, ?2)")?;
    for (hex, sig) in events {
        let topic = hex::decode(hex)?;
        e_stmt.execute(rusqlite::params![topic, sig])?;
    }
    drop(e_stmt);

    tracing::info!("Built fallback sig DB: {} methods, {} events", methods.len(), events.len());
    Ok(())
}

/// Ensure the signature database is available, downloading + decompressing if needed.
///
/// If the file already exists at the given path, returns it immediately.
/// Otherwise tries to download the zstd-compressed archive from the CDN.
/// If the CDN download fails, builds a minimal fallback DB with well-known signatures.
pub async fn ensure_signature_db(db_path: Option<PathBuf>) -> anyhow::Result<PathBuf> {
    let db_path = db_path.unwrap_or_else(default_sig_db_path);

    if db_path.exists() {
        return Ok(db_path);
    }

    // Try CDN download first
    match try_download_sig_db(&db_path).await {
        Ok(()) => {
            tracing::info!("Signature database cached at {}", db_path.display());
            return Ok(db_path);
        }
        Err(e) => {
            tracing::warn!("CDN download failed: {e} — building fallback sig DB");
        }
    }

    // Fallback: build minimal DB locally
    build_fallback_db(&db_path)?;
    Ok(db_path)
}

async fn try_download_sig_db(db_path: &PathBuf) -> anyhow::Result<()> {
    tracing::info!("Downloading signature database from {SIG_DB_URL}...");
    let resp = reqwest::get(SIG_DB_URL).await?;
    let compressed_bytes = resp.bytes().await?;
    tracing::info!("Downloaded {} bytes (zstd compressed)", compressed_bytes.len());

    let mut decoder = ruzstd::StreamingDecoder::new(&compressed_bytes[..])?;
    let mut decompressed = Vec::new();
    decoder.read_to_end(&mut decompressed)?;
    tracing::info!("Decompressed to {} bytes", decompressed.len());

    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(db_path, &decompressed)?;
    Ok(())
}
