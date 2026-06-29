//! Multi-provider RPC client with per-endpoint rate limiting, weighted selection,
//! and block-range sharding for load distribution across public/private RPC endpoints.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Instant;

use alloy::consensus::Transaction;
use alloy::eips::BlockNumberOrTag;
use alloy::network::TransactionBuilder;
use alloy::primitives::{Address, Bytes, B256, U256};
use alloy::providers::{Provider, RootProvider};
use alloy::rpc::types::eth::TransactionRequest;
use alloy::rpc::types::{Block, Filter, Log, Transaction as AlloyTx, TransactionReceipt};
use alloy::rpc::client::{BatchRequest, Waiter};
use serde_json::Value;
use url::Url;
use rand::Rng;

use crate::data::types::{AccessListItem, BlockData, LogData, ReceiptData, TxData};

use super::middleware::{ProviderState, RateLimiter};

/// Multi-provider RPC client with per-endpoint rate limiting, weighted selection,
/// and health tracking.
///
/// Each provider has its own rate limiter. When an RPC call fails, the provider
/// enters a cooldown with exponential backoff. Available providers are selected
/// by weighted random selection (weight = RPS).
#[derive(Debug, Clone)]
pub struct RpcClient {
    providers: Arc<tokio::sync::Mutex<Vec<ProviderState>>>,
    chain_id: u64,
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
    /// Each URL gets its own `ProviderState` with no rate limiter (use
    /// `with_provider_rps` to set per-provider limits, or `with_rate_limit`
    /// for a shared single limiter on the first provider).
    pub fn from_urls(urls: &[&str], chain_id: u64) -> anyhow::Result<Self> {
        if urls.is_empty() {
            anyhow::bail!("At least one RPC URL is required");
        }
        let providers: Vec<ProviderState> = urls
            .iter()
            .enumerate()
            .map(|(i, url)| {
                let u: Url = url.parse().map_err(|e| anyhow::anyhow!("Invalid RPC URL '{url}': {e}"))?;
                let provider = RootProvider::new_http(u);
                Ok(ProviderState::new(provider, None, format!("provider-{i}")))
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        Ok(RpcClient {
            providers: Arc::new(tokio::sync::Mutex::new(providers)),
            chain_id,
            current: Arc::new(AtomicUsize::new(0)),
        })
    }

    /// Create a client from provider config tuples (URL, optional RPS).
    pub fn from_provider_configs(
        configs: &[(String, Option<f64>)],
        chain_id: u64,
    ) -> anyhow::Result<Self> {
        if configs.is_empty() {
            anyhow::bail!("At least one RPC provider is required");
        }
        let providers: Vec<ProviderState> = configs
            .iter()
            .enumerate()
            .map(|(i, (url, rps))| {
                let u: Url = url.parse().map_err(|e| anyhow::anyhow!("Invalid RPC URL '{url}': {e}"))?;
                let provider = RootProvider::new_http(u);
                Ok(ProviderState::new(provider, *rps, format!("provider-{i}")))
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        Ok(RpcClient {
            providers: Arc::new(tokio::sync::Mutex::new(providers)),
            chain_id,
            current: Arc::new(AtomicUsize::new(0)),
        })
    }

    /// Number of configured RPC providers.
    pub async fn providers_count(&self) -> usize {
        self.providers.lock().await.len()
    }

    /// Reset all providers to healthy state.
    pub async fn reset(&self) {
        let mut provs = self.providers.lock().await;
        for p in provs.iter_mut() {
            p.is_alive = true;
            p.cooldown_until = None;
            p.consecutive_failures = 0;
        }
        self.current.store(0, Ordering::Relaxed);
    }

    /// Returns the chain ID this client is configured for.
    pub fn chain_id(&self) -> u64 {
        self.chain_id
    }

    /// Enable a shared token-bucket rate limiter for the first provider only.
    /// This is backward-compat for single-provider usage. For multi-provider,
    /// use `from_provider_configs` with per-provider RPS.
    pub async fn with_rate_limit(self, requests_per_second: f64) -> Self {
        let rps = requests_per_second.max(1.0);
        let rl = Arc::new(RateLimiter::new(rps, rps));
        if let Some(first) = self.providers.lock().await.first_mut() {
            first.rate_limiter = Some(rl);
        }
        self
    }

    /// Set per-provider RPS limits. Index i maps to provider i.
    pub async fn with_provider_rps(&self, rps_list: &[f64]) {
        let mut provs = self.providers.lock().await;
        for (i, &rps) in rps_list.iter().enumerate() {
            if let Some(p) = provs.get_mut(i) {
                if rps > 0.0 {
                    p.rate_limiter = Some(Arc::new(RateLimiter::new(rps, rps)));
                    p.weight = rps;
                }
            }
        }
    }

    /// Pick a provider by weighted random selection from available providers.
    async fn pick_provider(&self) -> Option<(usize, ProviderState)> {
        let provs = self.providers.lock().await;
        let available: Vec<(usize, &ProviderState)> = provs
            .iter()
            .enumerate()
            .filter(|(_, p)| p.is_available())
            .collect();

        if available.is_empty() {
            return None;
        }

        let total_weight: f64 = available.iter().map(|(_, p)| p.weight).sum();
        if total_weight <= 0.0 {
            return available.first().map(|(i, p)| (*i, ProviderState::clone(p)));
        }

        let mut rng = rand::rng();
        let mut pick = rng.random::<f64>() * total_weight;
        for (i, p) in &available {
            pick -= p.weight;
            if pick <= 0.0 {
                return Some((*i, ProviderState::clone(p)));
            }
        }

        available.last().map(|(i, p)| (*i, ProviderState::clone(p)))
    }

    /// Execute an RPC call with per-provider rate limiting, weighted selection,
    /// and health tracking with exponential-backoff cooldown.
    ///
    /// Returns the first success or the last error if all providers fail.
    async fn retry_call<F, Fut, T>(&self, f: F) -> anyhow::Result<T>
    where
        F: Fn(RootProvider) -> Fut,
        Fut: std::future::Future<Output = anyhow::Result<T>>,
    {
        let start = self.current.load(Ordering::Relaxed);
        let mut last_err = None;
        let provs_len = self.providers.lock().await.len();

        for offset in 0..provs_len {
            // Weighted random pick — fall back to round-robin if no weighted pick
            let maybe_pick = if offset == 0 {
                self.pick_provider().await
            } else {
                None
            };

            let (idx, provider) = if let Some(pick) = maybe_pick {
                pick
            } else {
                let idx = (start + offset) % provs_len;
                let prov_state = {
                    let provs = self.providers.lock().await;
                    provs.get(idx).cloned()
                };
                match prov_state {
                    Some(p) if p.is_available() => (idx, p),
                    _ => continue,
                }
            };

            // Acquire per-provider rate limiter token
            provider.acquire_permit().await;

            let t0 = Instant::now();
            match f(provider.provider).await {
                Ok(val) => {
                    let latency = t0.elapsed();
                    let mut provs = self.providers.lock().await;
                    if let Some(p) = provs.get_mut(idx) {
                        p.record_success(latency);
                    }
                    self.current.store(idx, Ordering::Relaxed);
                    return Ok(val);
                }
                Err(e) => {
                    let mut provs = self.providers.lock().await;
                    if let Some(p) = provs.get_mut(idx) {
                        p.record_failure();
                        tracing::warn!(
                            "RPC call failed on {} (failures={}, cooldown={:?}): {e:#}",
                            p.label,
                            p.consecutive_failures,
                            p.cooldown_until,
                        );
                    }
                    last_err = Some(e);
                }
            }
        }

        match last_err {
            Some(e) => anyhow::bail!("All RPC providers failed: {e:#}"),
            None => anyhow::bail!("All RPC providers exhausted or in cooldown"),
        }
    }

    /// Distribute a block range across providers by weight.
    ///
    /// Returns `Vec<(usize, Vec<u64>)>` — (provider_index, block_numbers).
    /// Contiguous shards are allocated proportionally to each provider's weight (RPS).
    pub async fn distribute_blocks(&self, start: u64, end: u64) -> Vec<(usize, Vec<u64>)> {
        let total_blocks = (end - start + 1) as f64;
        let provs = self.providers.lock().await;
        let alive: Vec<(usize, f64)> = provs
            .iter()
            .enumerate()
            .filter(|(_, p)| p.is_available())
            .map(|(i, p)| (i, p.weight.max(0.1)))
            .collect();

        if alive.len() <= 1 {
            let blocks: Vec<u64> = (start..=end).collect();
            return vec![(alive.first().map(|(i, _)| *i).unwrap_or(0), blocks)];
        }

        let total_weight: f64 = alive.iter().map(|(_, w)| w).sum();
        let mut shards: Vec<(usize, Vec<u64>)> = Vec::new();
        let mut current_start = start;

        for (idx, (provider_idx, weight)) in alive.iter().enumerate() {
            let is_last = idx == alive.len() - 1;
            let shard_size = if is_last {
                (end as f64) - current_start as f64 + 1.0
            } else {
                (total_blocks * weight / total_weight).ceil()
            } as u64;

            let shard_end = (current_start + shard_size - 1).min(end);
            let blocks: Vec<u64> = (current_start..=shard_end).collect();
            shards.push((*provider_idx, blocks));
            current_start = shard_end + 1;
        }

        shards
    }

    /// Validate all providers in parallel — chain ID, block number, and archive support.
    pub async fn validate_all(&self, expected_chain_id: u64) -> anyhow::Result<Vec<anyhow::Result<()>>> {
        let provs = self.providers.lock().await;
        let mut results = Vec::new();

        for (i, state) in provs.iter().enumerate() {
            let provider = state.provider.clone();
            let label = state.label.clone();
            let result = Self::check_single_provider(&provider, &label, expected_chain_id).await;
            results.push(result);
            if let Some(Err(ref e)) = results.last() {
                tracing::warn!("Provider {i} ({label}) failed validation: {e}");
            }
        }

        Ok(results)
    }

    async fn check_single_provider(
        provider: &RootProvider,
        label: &str,
        expected_chain_id: u64,
    ) -> anyhow::Result<()> {
        let actual_chain_id = provider
            .get_chain_id()
            .await
            .map_err(|e| anyhow::anyhow!("{label}: eth_chainId failed: {e}"))?;

        if actual_chain_id != expected_chain_id {
            anyhow::bail!(
                "{label}: chain ID mismatch: got {actual_chain_id}, expected {expected_chain_id}"
            );
        }

        let tip = provider
            .get_block_number()
            .await
            .map_err(|e| anyhow::anyhow!("{label}: eth_blockNumber failed: {e}"))?;

        // eth_getProof probe — needed by CachedRpcDb
        provider
            .get_proof(Address::ZERO, vec![])
            .number(tip)
            .await
            .map_err(|e| anyhow::anyhow!("{label}: eth_getProof failed (archive required): {e}"))?;

        tracing::info!("{label}: OK chain_id={actual_chain_id} tip={tip} archive=supported");
        Ok(())
    }

    /// Fetch the latest block number from the chain.
    pub async fn get_block_number(&self) -> anyhow::Result<u64> {
        self.retry_call(|provider| async move {
            provider
                .get_block_number()
                .await
                .map_err(|e| anyhow::anyhow!("{}", e))
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
                .map_err(|e| anyhow::anyhow!("{}", e))?
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
                    .map_err(|e| anyhow::anyhow!("{}", e))
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
                    .map_err(|e| anyhow::anyhow!("{}", e))?;

                if raw.is_null() {
                    anyhow::bail!("Block {} not found", block_number);
                }

                let mut raw = raw;
                Self::clean_block_transactions(&mut raw);

                serde_json::from_value::<Block>(raw).map_err(|e| anyhow::anyhow!("{}", e))
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
                    .map_err(|e| anyhow::anyhow!("{}", e))?
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
                    .map_err(|e| anyhow::anyhow!("{}", e))?
                    .ok_or_else(|| anyhow::anyhow!("Receipts not found for block {}", block_number))
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
                .map_err(|e| anyhow::anyhow!("{}", e))?;

            let receipts_waiter: Waiter<Vec<TransactionReceipt>> = batch
                .add_call(
                    "eth_getBlockReceipts",
                    &(alloy::eips::BlockId::number(block_number),),
                )
                .map_err(|e| anyhow::anyhow!("{}", e))?;

            batch.send().await.map_err(|e| anyhow::anyhow!("{}", e))?;

            let raw: Value = block_waiter.await.map_err(|e| anyhow::anyhow!("{}", e))?;
            if raw.is_null() {
                anyhow::bail!("Block {} not found", block_number);
            }
            let mut raw = raw;
            Self::clean_block_transactions(&mut raw);
            let block: Block = serde_json::from_value(raw).map_err(|e| anyhow::anyhow!("{}", e))?;

            let receipts: Vec<TransactionReceipt> =
                receipts_waiter.await.map_err(|e| anyhow::anyhow!("{}", e))?;

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
                    .map_err(|e| anyhow::anyhow!("{}", e))?;
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
                .map_err(|e| anyhow::anyhow!("{}", e))
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
                    .map_err(|e| anyhow::anyhow!("{}", e))
            }),
            self.retry_call(|provider| async move {
                provider
                    .get_balance(address)
                    .number(block)
                    .await
                    .map_err(|e| anyhow::anyhow!("{}", e))
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
                .map_err(|e| anyhow::anyhow!("{}", e))
        })
        .await
    }

    /// Fetch code at a historical block with no retry.
    /// Uses the first available provider. Still respects per-provider rate limiters.
    pub async fn get_code_no_retry(&self, address: Address, block: u64) -> anyhow::Result<Bytes> {
        let first = {
            let provs = self.providers.lock().await;
            provs.first().cloned()
        };
        match first {
            Some(p) => {
                p.acquire_permit().await;
                p.provider
                    .get_code_at(address)
                    .number(block)
                    .await
                    .map_err(|e| anyhow::anyhow!("{}", e))
            }
            None => anyhow::bail!("No providers available"),
        }
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
                    .map_err(|e| anyhow::anyhow!("{}", e))
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
                .map_err(|e| anyhow::anyhow!("{}", e))
        })
        .await
    }

    /// Pre-flight connection check — validates at least one provider is reachable.
    ///
    /// Checks each provider's chain ID, block number access, and archive support.
    /// Returns success if at least one provider passes all checks.
    pub async fn check_connection(&self, expected_chain_id: u64) -> anyhow::Result<()> {
        let results = self.validate_all(expected_chain_id).await?;
        let failures: Vec<String> = results
            .iter()
            .filter_map(|r| r.as_ref().err().map(|e| e.to_string()))
            .collect();

        if failures.len() == results.len() {
            anyhow::bail!(
                "All RPC providers failed connection check:\n{}",
                failures.join("\n"),
            );
        }

        let success_count = results.len() - failures.len();
        if !failures.is_empty() {
            tracing::warn!(
                "{}/{} providers passed, {} failed:\n{}",
                success_count,
                results.len(),
                failures.len(),
                failures.join("\n"),
            );
        }

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
                    .map_err(|e| anyhow::anyhow!("{}", e))
            }
        })
        .await
    }

    /// Fetch the current gas price from the chain via `eth_gasPrice`.
    ///
    /// Returns the current base fee per gas in wei.
    pub async fn get_gas_price(&self) -> anyhow::Result<u128> {
        self.retry_call(|provider| async move {
            let raw: U256 = provider
                .client()
                .request("eth_gasPrice", ())
                .await
                .map_err(|e| anyhow::anyhow!("{}", e))?;
            Ok(raw.to::<u128>())
        })
        .await
    }

    /// Fetch the current max priority fee per gas via `eth_maxPriorityFeePerGas`.
    ///
    /// Returns the priority fee in wei.
    pub async fn get_max_priority_fee(&self) -> anyhow::Result<u128> {
        self.retry_call(|provider| async move {
            let raw: U256 = provider
                .client()
                .request("eth_maxPriorityFeePerGas", ())
                .await
                .map_err(|e| anyhow::anyhow!("{}", e))?;
            Ok(raw.to::<u128>())
        })
        .await
    }
}

/// Extract the first 4 bytes of transaction calldata as a method selector.
/// Returns `None` if input is shorter than 4 bytes (plain ETH transfer or CREATE).
pub(crate) fn extract_selector(input: &Bytes) -> Option<[u8; 4]> {
    if input.len() >= 4 {
        let mut sel = [0u8; 4];
        sel.copy_from_slice(&input[..4]);
        Some(sel)
    } else {
        None
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
