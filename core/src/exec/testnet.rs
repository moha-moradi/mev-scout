//! Testnet submit path. Refuses every chain id that is not on the allowlist,
//! so a mainnet RPC can never receive `eth_sendRawTransaction` from `--exec testnet`.
use std::str::FromStr;
use std::time::Duration;

use alloy::consensus::{SignableTransaction, TxEip1559};
use alloy::eips::eip2718::Encodable2718;
use alloy::primitives::{Address, Bytes, TxKind, U256};
use alloy::signers::local::PrivateKeySigner;
use alloy::signers::SignerSync;
use serde_json::{json, Value};

use crate::paper::types::{ExecAttempt, FillSkipReason};

use super::anvil::rpc;
use super::artifacts::creation_bytecode;

/// Public testnets this mode is willing to sign for. Anything else — including
/// every mainnet id — is refused before a transaction is built.
const TESTNET_CHAIN_IDS: &[u64] = &[
    5,          // goerli
    97,         // bsc testnet
    300,        // zksync sepolia
    17000,      // holesky
    80001,      // polygon mumbai
    80002,      // polygon amoy
    84532,      // base sepolia
    421614,     // arbitrum sepolia
    43113,      // avalanche fuji
    59141,      // linea sepolia
    11155111,   // sepolia
    11155420,   // optimism sepolia
    534351,     // scroll sepolia
    560048,     // hoodi
    168587773,  // blast sepolia
];

pub fn assert_testnet_chain(chain_id: u64) -> anyhow::Result<()> {
    if TESTNET_CHAIN_IDS.contains(&chain_id) {
        Ok(())
    } else {
        anyhow::bail!(
            "--exec testnet refuses chain id {chain_id}. \
             This mode only submits to a known testnet (Sepolia, Amoy, Fuji, …) \
             and never to mainnet."
        )
    }
}

pub async fn remote_chain_id(url: &str) -> anyhow::Result<u64> {
    let v = rpc(url, "eth_chainId", json!([])).await?;
    let s = v
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("eth_chainId returned {v}"))?;
    u64::from_str_radix(s.trim_start_matches("0x"), 16)
        .map_err(|e| anyhow::anyhow!("eth_chainId: {e}"))
}

pub struct TestnetSender {
    url: String,
    chain_id: u64,
    signer: PrivateKeySigner,
    pub from: Address,
    nonce: u64,
}

impl TestnetSender {
    pub async fn connect(url: &str, key: &str) -> anyhow::Result<Self> {
        let chain_id = remote_chain_id(url).await?;
        assert_testnet_chain(chain_id)?;
        let signer = PrivateKeySigner::from_str(key.trim())
            .map_err(|e| anyhow::anyhow!("invalid exec key: {e}"))?;
        let from = signer.address();
        let nonce = pending_nonce(url, from).await?;
        Ok(Self {
            url: url.to_string(),
            chain_id,
            signer,
            from,
            nonce,
        })
    }

    pub async fn deploy_executor(&mut self) -> anyhow::Result<Address> {
        assert_testnet_chain(self.chain_id)?;
        let code = creation_bytecode()?;
        let hash = self.send(None, code, 8_000_000).await?;
        let receipt = wait_receipt(&self.url, &hash).await?;
        let addr = receipt
            .get("contractAddress")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("testnet deploy receipt missing contractAddress"))?;
        addr.parse()
            .map_err(|e| anyhow::anyhow!("testnet deploy address: {e}"))
    }

    pub async fn send_exec(&mut self, to: Address, data: Bytes) -> anyhow::Result<ExecAttempt> {
        assert_testnet_chain(self.chain_id)?;
        let before = balance_of(&self.url, self.from).await?;
        let hash = match self.send(Some(to), data, 2_500_000).await {
            Ok(h) => h,
            Err(e) => {
                tracing::warn!("testnet send failed: {e}");
                return Ok(ExecAttempt::failed(FillSkipReason::ExecBuildFailed));
            }
        };
        let receipt = match wait_receipt(&self.url, &hash).await {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!("testnet receipt failed: {e}");
                return Ok(ExecAttempt::failed(FillSkipReason::ExecRevert));
            }
        };
        let status = receipt
            .get("status")
            .and_then(|v| v.as_str())
            .map(|s| s == "0x1" || s == "1")
            .unwrap_or(false);
        let gas_used = hex_u64(receipt.get("gasUsed").and_then(|v| v.as_str()).unwrap_or("0x0"));
        let gas_price = hex_u128(
            receipt
                .get("effectiveGasPrice")
                .and_then(|v| v.as_str())
                .unwrap_or("0x0"),
        );
        let after = balance_of(&self.url, self.from).await.unwrap_or(before);
        Ok(ExecAttempt::ran(
            status,
            signed_delta(before, after),
            gas_used,
            (gas_used as u128).saturating_mul(gas_price),
            Some(hash),
        ))
    }

    async fn send(&mut self, to: Option<Address>, data: Bytes, gas: u64) -> anyhow::Result<String> {
        assert_testnet_chain(self.chain_id)?;
        let gas_price = gas_price(&self.url).await?;
        let tx = TxEip1559 {
            chain_id: self.chain_id,
            nonce: self.nonce,
            gas_limit: gas,
            max_fee_per_gas: gas_price.saturating_mul(2),
            max_priority_fee_per_gas: 1_000_000_000,
            to: match to {
                Some(addr) => TxKind::Call(addr),
                None => TxKind::Create,
            },
            value: U256::ZERO,
            input: data,
            access_list: Default::default(),
        };
        let sig = self
            .signer
            .sign_hash_sync(&tx.signature_hash())
            .map_err(|e| anyhow::anyhow!("sign: {e}"))?;
        let raw = tx.into_signed(sig).encoded_2718();
        self.nonce = self.nonce.saturating_add(1);
        let v = rpc(
            &self.url,
            "eth_sendRawTransaction",
            json!([format!("0x{}", hex::encode(raw))]),
        )
        .await?;
        v.as_str()
            .map(|s| s.to_string())
            .ok_or_else(|| anyhow::anyhow!("eth_sendRawTransaction returned {v}"))
    }
}

async fn pending_nonce(url: &str, who: Address) -> anyhow::Result<u64> {
    let v = rpc(
        url,
        "eth_getTransactionCount",
        json!([format!("{who:#x}"), "pending"]),
    )
    .await?;
    let s = v
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("nonce returned {v}"))?;
    u64::from_str_radix(s.trim_start_matches("0x"), 16).map_err(|e| anyhow::anyhow!("nonce: {e}"))
}

async fn gas_price(url: &str) -> anyhow::Result<u128> {
    let v = rpc(url, "eth_gasPrice", json!([])).await?;
    let s = v
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("eth_gasPrice returned {v}"))?;
    u128::from_str_radix(s.trim_start_matches("0x"), 16)
        .map_err(|e| anyhow::anyhow!("eth_gasPrice: {e}"))
}

async fn balance_of(url: &str, who: Address) -> anyhow::Result<U256> {
    let v = rpc(
        url,
        "eth_getBalance",
        json!([format!("{who:#x}"), "latest"]),
    )
    .await?;
    let s = v
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("eth_getBalance returned {v}"))?;
    Ok(U256::from_str_radix(s.trim_start_matches("0x"), 16).unwrap_or(U256::ZERO))
}

async fn wait_receipt(url: &str, hash: &str) -> anyhow::Result<Value> {
    let deadline = std::time::Instant::now() + Duration::from_secs(120);
    while std::time::Instant::now() < deadline {
        let v = rpc(url, "eth_getTransactionReceipt", json!([hash])).await?;
        if !v.is_null() {
            return Ok(v);
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    anyhow::bail!("timed out waiting for testnet receipt {hash}")
}

fn hex_u64(s: &str) -> u64 {
    u64::from_str_radix(s.trim_start_matches("0x"), 16).unwrap_or(0)
}

fn hex_u128(s: &str) -> u128 {
    u128::from_str_radix(s.trim_start_matches("0x"), 16).unwrap_or(0)
}

fn signed_delta(before: U256, after: U256) -> i128 {
    if after >= before {
        let d = after - before;
        if d > U256::from(i128::MAX as u128) {
            i128::MAX
        } else {
            d.to::<i128>()
        }
    } else {
        let d = before - after;
        if d > U256::from(i128::MAX as u128) {
            i128::MIN
        } else {
            -(d.to::<i128>())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sepolia_and_amoy_pass_mainnet_does_not() {
        assert!(assert_testnet_chain(11_155_111).is_ok());
        assert!(assert_testnet_chain(80_002).is_ok());
        assert!(assert_testnet_chain(1).is_err());
        assert!(assert_testnet_chain(137).is_err());
        assert!(assert_testnet_chain(43_114).is_err());
    }
}
