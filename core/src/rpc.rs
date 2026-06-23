//! RPC client for EVM chain interaction.
//!
//! Provides a thin wrapper around `alloy::providers::RootProvider` with:
//! - Exponential-backoff retry logic for all network calls
//! - Connection pre-flight validation (`eth_chainId`, `eth_blockNumber`, `eth_getProof`)
//! - Type-safe conversion from alloy responses to internal `data` types
//!
//! The `RpcClient` is the single external integration point for the backtest engine.
//! Every blockchain read (blocks, receipts, proofs, storage, code) flows through here.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use alloy::consensus::Transaction;
use alloy::eips::BlockNumberOrTag;
use alloy::network::TransactionBuilder;
use alloy::primitives::{Address, Bytes, B256, U256};
use alloy::providers::{Provider, RootProvider};
use alloy::rpc::types::eth::TransactionRequest;
use alloy::rpc::types::{Block, Filter, Log, Transaction as AlloyTx, TransactionReceipt};
use alloy::rpc::client::{BatchRequest, Waiter};
use serde_json::Value;
use tokio::time::sleep;
use url::Url;

use crate::data::{AccessListItem, BlockData, LogData, ReceiptData, TxData};

/// Retry configuration for RPC calls.
///
/// Uses exponential backoff: `delay = base_delay_ms * 2^attempt`, capped at `max_delay_ms`.
/// Default: 5 retries, 200ms base, 5s cap.
#[derive(Debug, Clone)]
pub struct RetryConfig {
    pub max_retries: u32,
    pub base_delay_ms: u64,
    pub max_delay_ms: u64,
}

impl Default for RetryConfig {
    fn default() -> Self {
        RetryConfig {
            max_retries: 5,
            base_delay_ms: 200,
            max_delay_ms: 5000,
        }
    }
}

/// EVM RPC client with built-in retry, multi-URL rotation, and connection validation.
///
/// Wraps one or more `alloy` `RootProvider` instances and provides:
/// - Automatic retry with exponential backoff for transient RPC failures
/// - Automatic fallback to the next provider when all retries on the current URL are exhausted
/// - Pre-flight connection checks that verify archive-node requirements
/// - Conversion helpers from alloy types to internal `data` types
///
/// All methods return `anyhow::Result` for uniform error handling.
#[derive(Debug, Clone)]
pub struct RpcClient {
    providers: Vec<RootProvider>,
    chain_id: u64,
    retry: RetryConfig,
    current: Arc<AtomicUsize>,
}

impl RpcClient {
    /// Create a new RPC client from a single URL and expected chain ID.
    ///
    /// Backward-compatible convenience wrapper around `from_urls`.
    pub fn new(rpc_url: &str, chain_id: u64) -> anyhow::Result<Self> {
        Self::from_urls(&[rpc_url], chain_id)
    }

    /// Create a new RPC client from one or more URLs.
    ///
    /// The first URL is the primary endpoint. If all retries on the current provider
    /// are exhausted, the client automatically falls back to the next URL in the list.
    pub fn from_urls(urls: &[&str], chain_id: u64) -> anyhow::Result<Self> {
        if urls.is_empty() {
            return Err(anyhow::anyhow!("At least one RPC URL is required"));
        }
        let providers: Vec<RootProvider> = urls
            .iter()
            .map(|url| url.parse::<Url>())
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| anyhow::anyhow!("Invalid RPC URL: {e}"))?
            .into_iter()
            .map(RootProvider::new_http)
            .collect();
        Ok(RpcClient {
            providers,
            chain_id,
            retry: RetryConfig::default(),
            current: Arc::new(AtomicUsize::new(0)),
        })
    }

    /// Number of configured RPC providers.
    pub fn providers_count(&self) -> usize {
        self.providers.len()
    }

    /// Reset the active provider index back to the first URL.
    pub fn reset(&self) {
        self.current.store(0, Ordering::Relaxed);
    }

    /// Override the default retry configuration.
    pub fn with_retry(mut self, retry: RetryConfig) -> Self {
        self.retry = retry;
        self
    }

    /// Returns the chain ID this client is configured for.
    pub fn chain_id(&self) -> u64 {
        self.chain_id
    }

    /// Execute an RPC call with exponential-backoff retry and multi-URL fallback.
    ///
    /// Retries up to `max_retries` times on each provider with delays doubling each attempt.
    /// If all retries on the current provider are exhausted, falls back to the next provider
    /// in the list. Returns immediately for non-retryable errors (e.g. bad request, auth).
    /// Returns the last error if all providers are exhausted.
    async fn retry_call<F, Fut, T>(&self, f: F) -> anyhow::Result<T>
    where
        F: Fn(RootProvider) -> Fut,
        Fut: std::future::Future<Output = anyhow::Result<T>>,
    {
        fn is_retryable(e: &anyhow::Error) -> bool {
            let err_str = format!("{e:#}");
            // Non-retryable: bad request, auth failure, method not found, parse errors
            if err_str.contains("status code 400")
                || err_str.contains("status code 401")
                || err_str.contains("status code 403")
                || err_str.contains("status code 404")
                || err_str.contains("method not found")
                || err_str.contains("parse error")
                || err_str.contains("deserialize")
            {
                return false;
            }
            true
        }

        let start = self.current.load(Ordering::Relaxed);
        let mut last_err = None;

        for offset in 0..self.providers.len() {
            let idx = (start + offset) % self.providers.len();
            let provider = self.providers[idx].clone();

            for attempt in 0..=self.retry.max_retries {
                match f(provider.clone()).await {
                    Ok(val) => {
                        self.current.store(idx, Ordering::Relaxed);
                        return Ok(val);
                    }
                    Err(e) => {
                        if !is_retryable(&e) {
                            return Err(e);
                        }
                        tracing::warn!(
                            "RPC call failed (provider {idx}, attempt {}/{})",
                            attempt + 1,
                            self.retry.max_retries + 1,
                        );
                        last_err = Some(e);
                        if attempt < self.retry.max_retries {
                            let delay = (self.retry.base_delay_ms * 2u64.pow(attempt))
                                .min(self.retry.max_delay_ms);
                            sleep(Duration::from_millis(delay)).await;
                        }
                    }
                }
            }

            tracing::warn!(
                "RPC provider {idx} exhausted, falling back to next provider"
            );
        }

        Err(anyhow::anyhow!(
            "All RPC providers failed after retries: {:?}",
            last_err.unwrap()
        ))
    }

    /// Fetch the latest block number from the chain.
    pub async fn get_block_number(&self) -> anyhow::Result<u64> {
        self.retry_call(|provider| async move {
            provider
                .get_block_number()
                .await
                .map_err(|e| anyhow::anyhow!(e))
        })
        .await
    }

    /// Fetch the timestamp of a specific block.
    ///
    /// Requests the full block header and extracts the timestamp.
    /// Used by `RangeResolver` for `--days` block range resolution.
    pub async fn get_block_timestamp(&self, block_number: u64) -> anyhow::Result<u64> {
        self.retry_call(|provider| async move {
            let block = provider
                .get_block_by_number(block_number.into())
                .hashes()
                .await
                .map_err(|e| anyhow::anyhow!(e))?
                .ok_or_else(|| anyhow::anyhow!("Block {} not found", block_number))?;
            Ok(block.header.timestamp)
        })
        .await
    }

    /// Fetch logs matching an `eth_getLogs` filter.
    ///
    /// Used for pool discovery (scanning `PairCreated` / `PoolCreated` events).
    pub async fn get_logs(&self, filter: &Filter) -> anyhow::Result<Vec<Log>> {
        self.retry_call(|provider| {
            let filter = filter.clone();
            async move {
                provider
                    .get_logs(&filter)
                    .await
                    .map_err(|e| anyhow::anyhow!(e))
            }
        })
        .await
    }

    /// Some chains (e.g. Polygon) include non-standard transaction types (e.g. `"0x7f"`)
    /// that alloy's `TxEnvelope` cannot deserialize. This helper removes them from the raw JSON.
    fn clean_block_transactions(raw: &mut Value) {
        if let Some(transactions) = raw.get_mut("transactions") {
            if let Some(tx_array) = transactions.as_array_mut() {
                tx_array.retain(|tx| {
                    tx.get("type")
                        .and_then(|t| t.as_str())
                        .map(|t| matches!(t, "0x0" | "0x1" | "0x2" | "0x3" | "0x4"))
                        .unwrap_or(true)
                });
            }
        }
    }

    /// Fetch a full block (header + transactions) by block number.
    ///
    /// Returns `BlockData` (header fields) and `Vec<TxData>` (transaction list).
    /// Transactions are converted from alloy types to internal types via `alloy_tx_to_tx_data`.
    pub async fn get_block(&self, block_number: u64) -> anyhow::Result<(BlockData, Vec<TxData>)> {
        let block: Block = self
            .retry_call(|provider| async move {
                let raw: Value = provider
                    .client()
                    .request(
                        "eth_getBlockByNumber",
                        (BlockNumberOrTag::Number(block_number), true),
                    )
                    .await
                    .map_err(|e| anyhow::anyhow!(e))?;

                if raw.is_null() {
                    return Err(anyhow::anyhow!("Block {} not found", block_number));
                }

                let mut raw = raw;
                Self::clean_block_transactions(&mut raw);

                serde_json::from_value::<Block>(raw).map_err(|e| anyhow::anyhow!(e))
            })
            .await?;

        let txs: Vec<TxData> = block
            .transactions
            .as_transactions()
            .map(|txs| {
                txs.iter()
                    .enumerate()
                    .map(|(i, tx)| alloy_tx_to_tx_data(tx, i as u64))
                    .collect()
            })
            .unwrap_or_default();

        let block_data = BlockData {
            number: block.header.number,
            hash: block.header.hash,
            timestamp: block.header.timestamp,
            base_fee_per_gas: block.header.base_fee_per_gas.map(|v| v as u128),
            gas_limit: block.header.gas_limit,
            gas_used: block.header.gas_used,
            coinbase: block.header.beneficiary,
        };

        Ok((block_data, txs))
    }

    /// Fetch the pending block (header + transactions) from the node's mempool.
    ///
    /// Calls `eth_getBlockByNumber("pending", true)` to retrieve all pending
    /// (not-yet-mined) transactions. The pending block number may be `None`
    /// on some nodes — in that case `block_data.number` is set to 0.
    ///
    /// Returns an error if the RPC does not support pending block queries.
    pub async fn get_pending_block(&self) -> anyhow::Result<(BlockData, Vec<TxData>)> {
        let block: Block = self
            .retry_call(|provider| async move {
                provider
                    .get_block_by_number(BlockNumberOrTag::Pending)
                    .full()
                    .await
                    .map_err(|e| anyhow::anyhow!(e))?
                    .ok_or_else(|| anyhow::anyhow!("Pending block not available"))
            })
            .await?;

        let txs: Vec<TxData> = block
            .transactions
            .as_transactions()
            .map(|txs| {
                txs.iter()
                    .enumerate()
                    .map(|(i, tx)| alloy_tx_to_tx_data(tx, i as u64))
                    .collect()
            })
            .unwrap_or_default();

        let block_data = BlockData {
            number: block.header.number,
            hash: block.header.hash,
            timestamp: block.header.timestamp,
            base_fee_per_gas: block.header.base_fee_per_gas.map(|v| v as u128),
            gas_limit: block.header.gas_limit,
            gas_used: block.header.gas_used,
            coinbase: block.header.beneficiary,
        };

        Ok((block_data, txs))
    }

    /// Fetch transaction receipts for a block.
    ///
    /// Uses `eth_getBlockReceipts` (EIP-658 receipt format).
    /// Receipts are converted from alloy types to internal types via `alloy_receipt_to_receipt_data`.
    pub async fn get_receipts(&self, block_number: u64) -> anyhow::Result<Vec<ReceiptData>> {
        let receipts = self
            .retry_call(|provider| async move {
                provider
                    .get_block_receipts(alloy::eips::BlockId::number(block_number))
                    .await
                    .map_err(|e| anyhow::anyhow!(e))?
                    .ok_or_else(|| {
                        anyhow::anyhow!("Receipts not found for block {}", block_number)
                    })
            })
            .await?;

        Ok(receipts
            .iter()
            .map(alloy_receipt_to_receipt_data)
            .collect())
    }

    /// Fetch block + receipts in a single JSON-RPC batch request.
    ///
    /// Sends `eth_getBlockByNumber` and `eth_getBlockReceipts` together in one
    /// HTTP POST, cutting round-trips per block in half.
    pub async fn get_block_and_receipts_batch(
        &self,
        block_number: u64,
    ) -> anyhow::Result<(BlockData, Vec<TxData>, Vec<ReceiptData>)> {
        self.retry_call(|provider| async move {
            let mut batch = BatchRequest::new(provider.client());

            let block_waiter: Waiter<Value> = batch
                .add_call(
                    "eth_getBlockByNumber",
                    &(BlockNumberOrTag::Number(block_number), true),
                )
                .map_err(|e| anyhow::anyhow!(e))?;

            let receipts_waiter: Waiter<Vec<TransactionReceipt>> = batch
                .add_call(
                    "eth_getBlockReceipts",
                    &(alloy::eips::BlockId::number(block_number),),
                )
                .map_err(|e| anyhow::anyhow!(e))?;

            batch.send().await.map_err(|e| anyhow::anyhow!(e))?;

            let raw: Value = block_waiter.await.map_err(|e| anyhow::anyhow!(e))?;
            if raw.is_null() {
                return Err(anyhow::anyhow!("Block {} not found", block_number));
            }
            let mut raw = raw;
            Self::clean_block_transactions(&mut raw);
            let block: Block = serde_json::from_value(raw).map_err(|e| anyhow::anyhow!(e))?;

            let receipts: Vec<TransactionReceipt> =
                receipts_waiter.await.map_err(|e| anyhow::anyhow!(e))?;

            let txs: Vec<TxData> = block
                .transactions
                .as_transactions()
                .map(|txs| {
                    txs.iter()
                        .enumerate()
                        .map(|(i, tx)| alloy_tx_to_tx_data(tx, i as u64))
                        .collect()
                })
                .unwrap_or_default();

            let block_data = BlockData {
                number: block.header.number,
                hash: block.header.hash,
                timestamp: block.header.timestamp,
                base_fee_per_gas: block.header.base_fee_per_gas.map(|v| v as u128),
                gas_limit: block.header.gas_limit,
                gas_used: block.header.gas_used,
                coinbase: block.header.beneficiary,
            };

            Ok((block_data, txs, receipts.iter().map(alloy_receipt_to_receipt_data).collect()))
        })
        .await
    }

    /// Fetch account proof via `eth_getProof`.
    ///
    /// Returns `(nonce, balance, code_hash, storage_proof)` for the given address
    /// at a historical block. This is the primary state-fetching mechanism for
    /// `CachedRpcDb` during EVM replay.
    ///
    /// **Requires an archive node** — unavailable on standard full nodes.
    pub async fn get_proof(
        &self,
        address: Address,
        slots: &[U256],
        block: u64,
    ) -> anyhow::Result<(u64, U256, B256, Vec<(U256, U256)>)> {
        let keys: Vec<B256> = slots.iter().map(|s| {
            B256::from(s.to_be_bytes::<32>())
        }).collect();
        self.retry_call(|provider| {
            let keys = keys.clone();
            async move {
                let proof = provider
                    .get_proof(address, keys)
                    .number(block)
                    .await
                    .map_err(|e| anyhow::anyhow!(e))?;
                let storage: Vec<(U256, U256)> = proof
                    .storage_proof
                    .iter()
                    .map(|sp| {
                        let key_b256 = sp.key.as_b256();
                        (U256::from_be_bytes(key_b256.0), sp.value)
                    })
                    .collect();
                Ok((proof.nonce, proof.balance, proof.code_hash, storage))
            }
        })
        .await
    }

    /// Fetch a single storage slot value at a historical block via `eth_getStorageAt`.
    pub async fn get_storage_at(
        &self,
        address: Address,
        slot: U256,
        block: u64,
    ) -> anyhow::Result<U256> {
        self.retry_call(|provider| async move {
            provider
                .get_storage_at(address, slot)
                .number(block)
                .await
                .map_err(|e| anyhow::anyhow!(e))
        })
        .await
    }

    /// Fetch account state (nonce, balance, bytecode) at a historical block.
    ///
    /// Fires three parallel RPC calls: `eth_getTransactionCount`, `eth_getBalance`,
    /// `eth_getCode`. Returns `(nonce, balance, bytecode)`.
    pub async fn get_account(
        &self,
        address: Address,
        block: u64,
    ) -> anyhow::Result<(u64, U256, Bytes)> {
        let (nonce, balance, code) = futures::try_join!(
            self.retry_call(|provider| async move {
                provider
                    .get_transaction_count(address)
                    .number(block)
                    .await
                    .map_err(|e| anyhow::anyhow!(e))
            }),
            self.retry_call(|provider| async move {
                provider
                    .get_balance(address)
                    .number(block)
                    .await
                    .map_err(|e| anyhow::anyhow!(e))
            }),
            self.get_code(address, block),
        )?;
        Ok((nonce, balance, code))
    }

    /// Fetch contract bytecode at a historical block via `eth_getCode`.
    pub async fn get_code(&self, address: Address, block: u64) -> anyhow::Result<Bytes> {
        self.retry_call(|provider| async move {
            provider
                .get_code_at(address)
                .number(block)
                .await
                .map_err(|e| anyhow::anyhow!(e))
        })
        .await
    }

    /// Fetch code at a historical block with no retry.
    /// Useful for non-critical lookups (e.g. precompile detection)
    /// where unavailability should just produce empty code.
    pub async fn get_code_no_retry(&self, address: Address, block: u64) -> anyhow::Result<Bytes> {
        self.providers[0]
            .get_code_at(address)
            .number(block)
            .await
            .map_err(|e| anyhow::anyhow!(e))
    }

    /// Estimate gas for a transaction.
    /// Returns gas units required.
    pub async fn estimate_gas(&self, to: Address, data: Bytes) -> anyhow::Result<u64> {
        self.retry_call(|provider| {
            let data = data.clone();
            async move {
                let request = TransactionRequest::default()
                    .with_to(to)
                    .with_input(data);
                provider
                    .estimate_gas(request)
                    .await
                    .map_err(|e| anyhow::anyhow!(e))
            }
        })
        .await
    }

    /// Get the chain ID from the RPC endpoint.
    /// This calls `eth_chainId` directly rather than using the cached `self.chain_id`.
    pub async fn get_chain_id(&self) -> anyhow::Result<u64> {
        self.retry_call(|provider| async move {
            provider
                .get_chain_id()
                .await
                .map_err(|e| anyhow::anyhow!(e))
        })
        .await
    }

    /// Pre-flight connection check.
    /// Verifies the RPC endpoint is reachable, on the correct network,
    /// and supports the methods needed for backtesting.
    ///
    /// Checks performed:
    /// 1. `eth_chainId` — confirms the RPC is on the expected network
    /// 2. `eth_blockNumber` — basic block data access
    /// 3. `eth_getProof` — required by the EVM block replayer (CachedRpcDb)
    ///
    /// Returns a descriptive error if any check fails.
    pub async fn check_connection(&self, expected_chain_id: u64) -> anyhow::Result<()> {
        let actual_chain_id = self.get_chain_id().await.map_err(|e| {
            anyhow::anyhow!(
                "RPC connection check failed (eth_chainId): {e}.\n\
                 Verify the RPC URL is correct and the endpoint is reachable."
            )
        })?;

        if actual_chain_id != expected_chain_id {
            return Err(anyhow::anyhow!(
                "Chain ID mismatch: RPC reports chain {actual_chain_id}, \
                 expected chain {expected_chain_id}.\n\
                 Make sure the RPC endpoint is for the correct network."
            ));
        }

        let tip = self.get_block_number().await.map_err(|e| {
            anyhow::anyhow!(
                "RPC connection check failed (eth_blockNumber): {e}.\n\
                 The endpoint is reachable but block queries are failing."
            )
        })?;

        // eth_getProof is required by CachedRpcDb (EVM block replayer).
        // Probe with empty slots at the tip — lightweight call.
        self.get_proof(Address::ZERO, &[], tip).await.map_err(|e| {
            anyhow::anyhow!(
                "RPC check failed — missing required method: eth_getProof.\n\
                 Error: {e}\n\
                 The EVM block replayer needs eth_getProof support.\n\
                 Use an archive or trace-compatible RPC endpoint."
            )
        })?;

        tracing::info!(
            "RPC connection OK: chain_id={actual_chain_id} (expected {expected_chain_id}), \
             tip={tip}, eth_getProof=supported"
        );

        Ok(())
    }

    /// Execute an `eth_call` at a historical block.
    ///
    /// Used for pool state queries (`getReserves()`, `slot0()`, `liquidity()`)
    /// without modifying chain state.
    pub async fn call(&self, to: Address, data: Bytes, block: u64) -> anyhow::Result<Bytes> {
        self.retry_call(|provider| {
            let data = data.clone();
            async move {
                let request = TransactionRequest::default()
                    .with_to(to)
                    .with_input(data);
                provider
                    .call(request)
                    .block(block.into())
                    .await
                    .map_err(|e| anyhow::anyhow!(e))
            }
        })
        .await
    }
}

fn alloy_tx_to_tx_data(tx: &AlloyTx, index: u64) -> TxData {
    TxData {
        hash: *tx.inner.hash(),
        index,
        from: tx.inner.signer(),
        to: tx.inner.to(),
        input: tx.inner.input().clone(),
        value: tx.inner.value(),
        gas_limit: tx.inner.gas_limit(),
        max_fee_per_gas: tx.inner.max_fee_per_gas(),
        max_priority_fee_per_gas: tx.inner.max_priority_fee_per_gas(),
        nonce: tx.inner.nonce(),
        access_list: tx
            .inner
            .access_list()
            .map(|al| {
                al.iter()
                    .map(|item| AccessListItem {
                        address: item.address,
                        slots: item.storage_keys.clone(),
                    })
                    .collect()
            })
            .unwrap_or_default(),
    }
}

fn alloy_receipt_to_receipt_data(receipt: &TransactionReceipt) -> ReceiptData {
    ReceiptData {
        tx_hash: receipt.transaction_hash,
        tx_index: receipt.transaction_index.unwrap_or(0),
        status: receipt.status(),
        gas_used: receipt.gas_used,
        cumulative_gas_used: receipt.inner.cumulative_gas_used(),
        logs: receipt
            .logs()
            .iter()
            .map(|l| LogData {
                address: l.address(),
                topics: l.topics().to_vec(),
                data: l.data().data.clone(),
            })
            .collect(),
        contract_address: receipt.contract_address,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_retry_config_defaults() {
        let cfg = RetryConfig::default();
        assert_eq!(cfg.max_retries, 5);
        assert_eq!(cfg.base_delay_ms, 200);
        assert_eq!(cfg.max_delay_ms, 5000);
    }

    #[test]
    fn test_rpc_client_invalid_url() {
        let result = RpcClient::new("not-a-url", 1);
        assert!(result.is_err());
    }

    #[test]
    fn test_rpc_client_chain_id() {
        let client = RpcClient::new("http://localhost:9999", 137).unwrap();
        assert_eq!(client.chain_id(), 137);
    }

    #[tokio::test]
    async fn test_check_connection_refused() {
        let client = RpcClient::new("http://127.0.0.1:1", 1).unwrap();
        let result = client.check_connection(1).await;
        assert!(result.is_err(), "check_connection should fail on a non-existent RPC");
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("eth_chainId"),
            "Error should mention eth_chainId: {err}"
        );
    }

    #[tokio::test]
    async fn test_check_connection_chain_id_mismatch() {
        let client = RpcClient::new("http://127.0.0.1:1", 137).unwrap();
        let result = client.check_connection(1).await;
        assert!(result.is_err(), "check_connection should fail on a non-existent RPC");
        let err = result.unwrap_err().to_string();
        // The connection fails first, but the error message should be clear
        assert!(
            err.contains("eth_chainId"),
            "Error should mention eth_chainId: {err}"
        );
    }

    #[tokio::test]
    async fn test_get_chain_id_refused() {
        let client = RpcClient::new("http://127.0.0.1:1", 1).unwrap();
        let result = client.get_chain_id().await;
        assert!(result.is_err(), "get_chain_id should fail on a non-existent RPC");
    }
}
