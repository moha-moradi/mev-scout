use std::time::Duration;

use alloy::consensus::Transaction;
use alloy::network::TransactionBuilder;
use alloy::primitives::{Address, Bytes, B256, U256};
use alloy::providers::{Provider, RootProvider};
use alloy::rpc::types::eth::TransactionRequest;
use alloy::rpc::types::{Block, Filter, Log, Transaction as AlloyTx, TransactionReceipt};
use tokio::time::sleep;
use url::Url;

use crate::data::{AccessListItem, BlockData, LogData, ReceiptData, TxData};

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

#[derive(Debug, Clone)]
pub struct RpcClient {
    provider: RootProvider,
    chain_id: u64,
    retry: RetryConfig,
}

impl RpcClient {
    pub fn new(rpc_url: &str, chain_id: u64) -> anyhow::Result<Self> {
        let url = rpc_url.parse::<Url>()?;
        let provider = RootProvider::new_http(url);
        Ok(RpcClient {
            provider,
            chain_id,
            retry: RetryConfig::default(),
        })
    }

    pub fn with_retry(mut self, retry: RetryConfig) -> Self {
        self.retry = retry;
        self
    }

    pub fn chain_id(&self) -> u64 {
        self.chain_id
    }

    async fn retry_call<F, Fut, T>(&self, f: F) -> anyhow::Result<T>
    where
        F: Fn() -> Fut,
        Fut: std::future::Future<Output = anyhow::Result<T>>,
    {
        let mut last_err = None;
        for attempt in 0..=self.retry.max_retries {
            match f().await {
                Ok(val) => return Ok(val),
                Err(e) => {
                    tracing::warn!(
                        "RPC call failed (attempt {}/{}): {:?}",
                        attempt + 1,
                        self.retry.max_retries + 1,
                        e
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
        Err(anyhow::anyhow!(
            "RPC call failed after {} retries: {:?}",
            self.retry.max_retries,
            last_err.unwrap()
        ))
    }

    pub async fn get_block_number(&self) -> anyhow::Result<u64> {
        self.retry_call(|| async {
            self.provider
                .get_block_number()
                .await
                .map_err(|e| anyhow::anyhow!(e))
        })
        .await
    }

    pub async fn get_block_timestamp(&self, block_number: u64) -> anyhow::Result<u64> {
        self.retry_call(|| {
            let provider = self.provider.clone();
            async move {
                let block = provider
                    .get_block_by_number(block_number.into())
                    .hashes()
                    .await
                    .map_err(|e| anyhow::anyhow!(e))?
                    .ok_or_else(|| anyhow::anyhow!("Block {} not found", block_number))?;
                Ok(block.header.timestamp)
            }
        })
        .await
    }

    pub async fn get_logs(&self, filter: &Filter) -> anyhow::Result<Vec<Log>> {
        self.retry_call(|| {
            let provider = self.provider.clone();
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

    pub async fn get_block(&self, block_number: u64) -> anyhow::Result<(BlockData, Vec<TxData>)> {
        let block: Block = self
            .retry_call(|| {
                let provider = self.provider.clone();
                async move {
                    provider
                        .get_block_by_number(block_number.into())
                        .full()
                        .await
                        .map_err(|e| anyhow::anyhow!(e))?
                        .ok_or_else(|| anyhow::anyhow!("Block {} not found", block_number))
                }
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

    pub async fn get_receipts(&self, block_number: u64) -> anyhow::Result<Vec<ReceiptData>> {
        let receipts = self
            .retry_call(|| {
                let provider = self.provider.clone();
                async move {
                    provider
                        .get_block_receipts(alloy::eips::BlockId::number(block_number))
                        .await
                        .map_err(|e| anyhow::anyhow!(e))?
                        .ok_or_else(|| {
                            anyhow::anyhow!("Receipts not found for block {}", block_number)
                        })
                }
            })
            .await?;

        Ok(receipts
            .iter()
            .map(alloy_receipt_to_receipt_data)
            .collect())
    }

    pub async fn get_proof(
        &self,
        address: Address,
        slots: &[U256],
        block: u64,
    ) -> anyhow::Result<(u64, U256, B256, Vec<(U256, U256)>)> {
        let keys: Vec<B256> = slots.iter().map(|s| {
            B256::from(s.to_be_bytes::<32>())
        }).collect();
        self.retry_call(|| {
            let provider = self.provider.clone();
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

    pub async fn get_storage_at(
        &self,
        address: Address,
        slot: U256,
        block: u64,
    ) -> anyhow::Result<U256> {
        self.retry_call(|| {
            let provider = self.provider.clone();
            async move {
                provider
                    .get_storage_at(address, slot)
                    .number(block)
                    .await
                    .map_err(|e| anyhow::anyhow!(e))
            }
        })
        .await
    }

    pub async fn get_account(
        &self,
        address: Address,
        block: u64,
    ) -> anyhow::Result<(u64, U256, Bytes)> {
        let (nonce, balance, code) = futures::try_join!(
            self.retry_call(|| {
                let provider = self.provider.clone();
                async move {
                    provider
                        .get_transaction_count(address)
                        .number(block)
                        .await
                        .map_err(|e| anyhow::anyhow!(e))
                }
            }),
            self.retry_call(|| {
                let provider = self.provider.clone();
                async move {
                    provider
                        .get_balance(address)
                        .number(block)
                        .await
                        .map_err(|e| anyhow::anyhow!(e))
                }
            }),
            self.get_code(address, block),
        )?;
        Ok((nonce, balance, code))
    }

    pub async fn get_code(&self, address: Address, block: u64) -> anyhow::Result<Bytes> {
        self.retry_call(|| {
            let provider = self.provider.clone();
            async move {
                provider
                    .get_code_at(address)
                    .number(block)
                    .await
                    .map_err(|e| anyhow::anyhow!(e))
            }
        })
        .await
    }

    /// Fetch code at a historical block with no retry.
    /// Useful for non-critical lookups (e.g. precompile detection)
    /// where unavailability should just produce empty code.
    pub async fn get_code_no_retry(&self, address: Address, block: u64) -> anyhow::Result<Bytes> {
        self.provider
            .get_code_at(address)
            .number(block)
            .await
            .map_err(|e| anyhow::anyhow!(e))
    }

    /// Estimate gas for a transaction.
    /// Returns gas units required.
    pub async fn estimate_gas(&self, to: Address, data: Bytes) -> anyhow::Result<u64> {
        self.retry_call(|| {
            let provider = self.provider.clone();
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

    /// Execute an `eth_call` at a historical block.
    pub async fn call(&self, to: Address, data: Bytes, block: u64) -> anyhow::Result<Bytes> {
        self.retry_call(|| {
            let provider = self.provider.clone();
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
