use std::fmt;
use std::str::FromStr;

use alloy::primitives::FixedBytes;
use alloy::rpc::types::{TransactionReceipt, TransactionRequest};
use reqwest::Client as HttpClient;

use crate::error;

/// Describes how transactions are submitted to the network.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum BroadcastMode {
    Flashbots,
    MevShare,
    Public,
}

impl fmt::Display for BroadcastMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BroadcastMode::Flashbots => write!(f, "flashbots"),
            BroadcastMode::MevShare => write!(f, "mevshare"),
            BroadcastMode::Public => write!(f, "public"),
        }
    }
}

impl FromStr for BroadcastMode {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "flashbots" => Ok(BroadcastMode::Flashbots),
            "mevshare" | "mev-share" => Ok(BroadcastMode::MevShare),
            "public" => Ok(BroadcastMode::Public),
            _ => Err(format!(
                "unknown broadcast mode '{s}'. Supported: flashbots, mevshare, public"
            )),
        }
    }
}

impl BroadcastMode {
    pub fn for_chain(self, chain_id: u64) -> Self {
        match chain_id {
            // Ethereum
            1 => self,
            _ => {
                if self != BroadcastMode::Public {
                    tracing::warn!(
                        "Flashbots/MEV-Share not available on chain {chain_id}, falling back to Public"
                    );
                }
                BroadcastMode::Public
            }
        }
    }
}

/// Wraps a Flashbots-compatible relay endpoint.
#[derive(Debug, Clone)]
pub struct FlashbotsClient {
    url: String,
    client: HttpClient,
}

impl FlashbotsClient {
    pub fn new(url: &str) -> Self {
        FlashbotsClient {
            url: url.to_string(),
            client: HttpClient::new(),
        }
    }

    pub async fn send_bundle(&self, txs: &[TransactionRequest]) -> Result<String, error::Error> {
        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "eth_sendBundle",
            "params": [txs.iter().map(|tx| {
                serde_json::json!({"tx": format!("{:?}", tx)})
            }).collect::<Vec<_>>()]
        });
        let resp = self
            .client
            .post(&self.url)
            .json(&body)
            .send()
            .await
            .map_err(|e| error::Error::Other(format!("Flashbots relay error: {e}")))?;
        let text = resp
            .text()
            .await
            .map_err(|e| error::Error::Other(format!("Flashbots response error: {e}")))?;
        Ok(text)
    }
}

/// Wraps an MEV-Share compatible relay endpoint.
#[derive(Debug, Clone)]
pub struct MevShareClient {
    url: String,
    client: HttpClient,
}

impl MevShareClient {
    pub fn new(url: &str) -> Self {
        MevShareClient {
            url: url.to_string(),
            client: HttpClient::new(),
        }
    }

    pub async fn send_bundle(&self, txs: &[TransactionRequest]) -> Result<String, error::Error> {
        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "mev_sendBundle",
            "params": [txs.iter().map(|tx| {
                serde_json::json!({"tx": format!("{:?}", tx)})
            }).collect::<Vec<_>>()]
        });
        let resp = self
            .client
            .post(&self.url)
            .json(&body)
            .send()
            .await
            .map_err(|e| error::Error::Other(format!("MEV-Share relay error: {e}")))?;
        let text = resp
            .text()
            .await
            .map_err(|e| error::Error::Other(format!("MEV-Share response error: {e}")))?;
        Ok(text)
    }
}

/// Handles transaction submission via public RPC, Flashbots, or MEV-Share.
#[derive(Debug, Clone)]
pub struct TxBroadcaster {
    pub mode: BroadcastMode,
    pub flashbots_relay: Option<FlashbotsClient>,
    pub mevshare_relay: Option<MevShareClient>,
    http_client: HttpClient,
}

impl TxBroadcaster {
    pub fn new(
        mode: BroadcastMode,
        flashbots_relay_url: Option<String>,
        mevshare_relay_url: Option<String>,
    ) -> Self {
        TxBroadcaster {
            mode,
            flashbots_relay: flashbots_relay_url.map(|u| FlashbotsClient::new(&u)),
            mevshare_relay: mevshare_relay_url.map(|u| MevShareClient::new(&u)),
            http_client: HttpClient::new(),
        }
    }

    pub async fn simulate_tx(&self, _tx: &TransactionRequest) -> Result<(), error::Error> {
        Ok(())
    }

    /// Send a raw signed transaction via eth_sendRawTransaction.
    /// For Public mode, the URL is the user's RPC.
    /// For Flashbots/MEV-Share, the URL is the relay endpoint.
    pub async fn submit_tx(
        &self,
        raw_signed_tx: Vec<u8>,
        url: &str,
    ) -> Result<FixedBytes<32>, error::Error> {
        let hex = format!("0x{}", alloy::primitives::hex::encode(&raw_signed_tx));
        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "eth_sendRawTransaction",
            "params": [hex]
        });
        let resp = self
            .http_client
            .post(url)
            .json(&body)
            .send()
            .await
            .map_err(|e| error::Error::Other(format!("RPC error: {e}")))?;
        let text: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| error::Error::Other(format!("RPC response error: {e}")))?;
        if let Some(hash_str) = text["result"].as_str() {
            let hash = hash_str
                .parse::<FixedBytes<32>>()
                .map_err(|e| error::Error::Other(format!("Invalid tx hash: {e}")))?;
            Ok(hash)
        } else if let Some(err) = text["error"].as_object() {
            let msg = err["message"].as_str().unwrap_or("unknown RPC error");
            Err(error::Error::Other(format!("RPC error: {msg}")))
        } else {
            Err(error::Error::Other(format!("Unexpected RPC response: {text}")))
        }
    }

    pub async fn submit_bundle(
        &self,
        txs: Vec<TransactionRequest>,
    ) -> Result<FixedBytes<32>, error::Error> {
        match self.mode {
            BroadcastMode::Flashbots => {
                if let Some(ref relay) = self.flashbots_relay {
                    relay.send_bundle(&txs).await?;
                    Ok(FixedBytes::ZERO)
                } else {
                    Err(error::Error::Other("Flashbots relay not configured".into()))
                }
            }
            BroadcastMode::MevShare => {
                if let Some(ref relay) = self.mevshare_relay {
                    relay.send_bundle(&txs).await?;
                    Ok(FixedBytes::ZERO)
                } else {
                    Err(error::Error::Other("MEV-Share relay not configured".into()))
                }
            }
            BroadcastMode::Public => {
                Err(error::Error::Other(
                    "Bundle submission not supported in Public mode. Use Flashbots or MEV-Share.".into(),
                ))
            }
        }
    }

    pub async fn wait_for_confirmation(
        &self,
        _tx_hash: FixedBytes<32>,
        _max_blocks: u64,
    ) -> Result<TransactionReceipt, error::Error> {
        Err(error::Error::Other("confirmation polling not yet implemented".into()))
    }
}
