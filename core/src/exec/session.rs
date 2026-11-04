//! Run the paper ledger against revm, a local Anvil fork, or a testnet.
use alloy::primitives::Address;

use crate::paper::{ExecAttempt, FillSkipReason, LedgerPolicy, LedgerResult, SimMode};
use crate::pool::state::PoolManager;
use crate::replay::BlockReplayer;
use crate::types::{FlashLoanProvider, MevOpportunity};

use super::anvil::{deploy_executor, impersonate_and_fund, reset_fork, send_exec, AnvilNode};
use super::calldata::encode_execute;
use super::plan::{plan_from_opportunity, PlanError};
use super::revm_backend::{exec_plan_revm, open_revm_block, RevmBlock};
use super::testnet::TestnetSender;
use super::ExecMode;

/// Backend handles owned by the live session. Revm needs none of the options.
pub struct ExecContext {
    pub mode: ExecMode,
    pub chain_id: u64,
    pub vault: Address,
    pub weth: Address,
    pub searcher: Address,
    pub provider: FlashLoanProvider,
    pub fork_url: String,
    pub anvil: Option<AnvilNode>,
    pub anvil_executor: Option<Address>,
    pub anvil_block: Option<u64>,
    pub testnet: Option<TestnetSender>,
    pub testnet_executor: Option<Address>,
}

pub fn execute_ledger(
    policy: &LedgerPolicy,
    opps: &[MevOpportunity],
    sim: SimMode,
    replayer: &BlockReplayer,
    pools: &PoolManager,
    ctx: &mut ExecContext,
) -> anyhow::Result<LedgerResult> {
    match ctx.mode {
        ExecMode::None => Ok(if sim == SimMode::StateSim {
            policy.apply(opps)
        } else {
            policy.apply(opps)
        }),
        ExecMode::Revm => Ok(run_revm(policy, opps, sim, replayer, pools, ctx)),
        ExecMode::Anvil => run_anvil(policy, opps, sim, pools, ctx),
        ExecMode::Testnet => run_testnet(policy, opps, sim, pools, ctx),
    }
}

fn run_revm(
    policy: &LedgerPolicy,
    opps: &[MevOpportunity],
    sim: SimMode,
    replayer: &BlockReplayer,
    pools: &PoolManager,
    ctx: &ExecContext,
) -> LedgerResult {
    let mut prepared: Option<RevmBlock> = None;
    policy.apply_executed(opps, sim, ctx.mode.as_str(), |opp| {
        if prepared.as_ref().map(|p| p.block_num) != Some(opp.block_number) {
            match open_revm_block(replayer, opp.block_number, ctx.searcher) {
                Ok(block) => prepared = Some(block),
                Err(e) => {
                    tracing::warn!("exec revm block {} state: {e}", opp.block_number);
                    return ExecAttempt::failed(FillSkipReason::ExecBuildFailed);
                }
            }
        }
        let plan = match plan_from_opportunity(
            opp,
            pools,
            ctx.vault,
            ctx.searcher,
            ctx.weth,
            ctx.provider,
        ) {
            Ok(plan) => plan,
            Err(PlanError::StrategyUnsupported) => {
                return ExecAttempt::failed(FillSkipReason::ExecStrategyUnsupported);
            }
            Err(e) => {
                tracing::debug!("exec build failed: {e}");
                return ExecAttempt::failed(FillSkipReason::ExecBuildFailed);
            }
        };
        let Some(block) = prepared.as_mut() else {
            return ExecAttempt::failed(FillSkipReason::ExecBuildFailed);
        };
        match exec_plan_revm(replayer, ctx.chain_id, block, ctx.searcher, &plan) {
            Ok(attempt) => attempt,
            Err(e) => {
                tracing::warn!("exec revm tx: {e}");
                ExecAttempt::failed(FillSkipReason::ExecBuildFailed)
            }
        }
    })
}

fn run_anvil(
    policy: &LedgerPolicy,
    opps: &[MevOpportunity],
    sim: SimMode,
    pools: &PoolManager,
    ctx: &mut ExecContext,
) -> anyhow::Result<LedgerResult> {
    let url = ctx
        .anvil
        .as_ref()
        .map(|n| n.url.clone())
        .ok_or_else(|| anyhow::anyhow!("--exec anvil has no node"))?;
    let fork = ctx.fork_url.clone();
    if fork.is_empty() {
        anyhow::bail!("--exec anvil needs an RPC url to fork");
    }
    let searcher = ctx.searcher;
    let vault = ctx.vault;
    let weth = ctx.weth;
    let provider = ctx.provider;
    let mode = ctx.mode.as_str();
    let mut executor = ctx.anvil_executor;
    let mut prepared_block = ctx.anvil_block;
    let ledger = policy.apply_executed(opps, sim, mode, |opp| {
        let plan = match plan_from_opportunity(opp, pools, vault, searcher, weth, provider) {
            Ok(plan) => plan,
            Err(PlanError::StrategyUnsupported) => {
                return ExecAttempt::failed(FillSkipReason::ExecStrategyUnsupported);
            }
            Err(e) => {
                tracing::debug!("exec build failed: {e}");
                return ExecAttempt::failed(FillSkipReason::ExecBuildFailed);
            }
        };
        if prepared_block != Some(opp.block_number) || executor.is_none() {
            let url = url.clone();
            let fork = fork.clone();
            let block = opp.block_number;
            let deployed = wait(async move {
                reset_fork(&url, &fork, block).await?;
                impersonate_and_fund(&url, searcher).await?;
                deploy_executor(&url, searcher).await
            });
            match deployed {
                Ok(addr) => {
                    executor = Some(addr);
                    prepared_block = Some(block);
                }
                Err(e) => {
                    tracing::warn!("anvil fork reset/deploy: {e}");
                    return ExecAttempt::failed(FillSkipReason::ExecBuildFailed);
                }
            }
        }
        let Some(to) = executor else {
            return ExecAttempt::failed(FillSkipReason::ExecBuildFailed);
        };
        let data = encode_execute(&plan);
        let url = url.clone();
        match wait(async move { send_exec(&url, searcher, to, data).await }) {
            Ok(attempt) => attempt,
            Err(e) => {
                tracing::warn!("anvil exec: {e}");
                ExecAttempt::failed(FillSkipReason::ExecBuildFailed)
            }
        }
    });
    ctx.anvil_executor = executor;
    ctx.anvil_block = prepared_block;
    Ok(ledger)
}

fn run_testnet(
    policy: &LedgerPolicy,
    opps: &[MevOpportunity],
    sim: SimMode,
    pools: &PoolManager,
    ctx: &mut ExecContext,
) -> anyhow::Result<LedgerResult> {
    let executor = ctx.testnet_executor.ok_or_else(|| {
        anyhow::anyhow!(
            "--exec testnet needs [paper].executor_address or a successful deploy this session"
        )
    })?;
    if ctx.testnet.is_none() {
        anyhow::bail!("--exec testnet has no signer");
    }
    let searcher = ctx.searcher;
    let vault = ctx.vault;
    let weth = ctx.weth;
    let provider = ctx.provider;
    let mode = ctx.mode.as_str();
    // Split the borrow: sender is taken out for the callback, then put back.
    let mut sender = ctx
        .testnet
        .take()
        .ok_or_else(|| anyhow::anyhow!("--exec testnet has no signer"))?;
    let ledger = policy.apply_executed(opps, sim, mode, |opp| {
        let plan = match plan_from_opportunity(opp, pools, vault, searcher, weth, provider) {
            Ok(plan) => plan,
            Err(PlanError::StrategyUnsupported) => {
                return ExecAttempt::failed(FillSkipReason::ExecStrategyUnsupported);
            }
            Err(e) => {
                tracing::debug!("exec build failed: {e}");
                return ExecAttempt::failed(FillSkipReason::ExecBuildFailed);
            }
        };
        let data = encode_execute(&plan);
        match wait(async { sender.send_exec(executor, data).await }) {
            Ok(attempt) => attempt,
            Err(e) => {
                tracing::warn!("testnet exec: {e}");
                ExecAttempt::failed(FillSkipReason::ExecBuildFailed)
            }
        }
    });
    ctx.testnet = Some(sender);
    Ok(ledger)
}

fn wait<T>(fut: impl std::future::Future<Output = T>) -> T {
    tokio::task::block_in_place(|| tokio::runtime::Handle::current().block_on(fut))
}
