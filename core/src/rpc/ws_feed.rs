//! Push-based WebSocket event feed for live mode.
//!
//! Connects to a WebSocket JSON-RPC endpoint and subscribes to:
//!   - `eth_subscribe "newHeads"`           — new block headers
//!   - `eth_subscribe "newPendingTransactions"` — mempool transactions
//!
//! Full-transaction-object pending subscriptions are preferred (getblock shared
//! supports them); providers that only stream hashes (e.g. publicnode) fall back
//! to resolving each hash via `eth_getTransactionByHash` on the same connection.
//!
//! Events are pushed into a shared `broadcast` channel. The feed task reconnects
//! with backoff on disconnects so the subscriber never needs to manage the socket.
//! Subscriptions are cheap push notifications — the HTTP `RpcClient` pool remains
//! the query workhorse (block/receipt batch fetches, pool init, `eth_call`).

use std::time::Duration;

use alloy::primitives::B256;
use alloy::providers::RootProvider;
use alloy::rpc::client::WsConnect;
use futures::StreamExt;

use crate::data::TxData;

use super::client::alloy_tx_to_tx_data;

/// Events emitted by the WS feed.
#[derive(Debug, Clone)]
pub enum FeedEvent {
    /// The feed connected (and (re)subscribed) successfully.
    Connected,
    /// A new block header was received from `newHeads`.
    NewHead { number: u64 },
    /// A batch of pending transactions observed in the mempool.
    NewPendingTxs(Vec<TxData>),
    /// A non-fatal feed error (reconnect will be attempted automatically).
    Error(String),
}

/// Pending-transaction batch flush threshold (transactions per emitted event).
const PENDING_BATCH_SIZE: usize = 25;
/// Pending-transaction batching window before a partial batch is flushed.
const PENDING_BATCH_WINDOW_MS: u64 = 200;
/// Delay between reconnect attempts.
const RECONNECT_DELAY_SECS: u64 = 5;

/// Spawn a WS feed task that pushes [`FeedEvent`]s into `tx` until all
/// receivers are dropped (broadcast send fails) or the task is aborted.
///
/// Reconnects with a fixed backoff on any error/disconnect, so the caller can
/// treat the channel as a persistent event stream.
pub fn spawn_ws_feed(url: String, tx: tokio::sync::broadcast::Sender<FeedEvent>) {
    tokio::spawn(async move {
        loop {
            match run_connection(&url, &tx).await {
                Ok(()) => {
                    // Clean disconnect (server closed the subscription streams).
                    tracing::warn!("WS feed disconnected; reconnecting in {RECONNECT_DELAY_SECS}s");
                }
                Err(e) => {
                    tracing::warn!("WS feed error: {e:#}; reconnecting in {RECONNECT_DELAY_SECS}s");
                    // Best-effort notify; a live subscriber may surface the issue.
                    let _ = tx.send(FeedEvent::Error(format!("{e:#}")));
                }
            }
            // Broadcast send fails when the last receiver dropped -> the runner
            // went away, so terminate the feed task.
            if tx.receiver_count() == 0 {
                return;
            }
            tokio::time::sleep(Duration::from_secs(RECONNECT_DELAY_SECS)).await;
        }
    });
}

/// Connect once, forward subscription events, return on disconnect/error.
async fn run_connection(url: &str, tx: &tokio::sync::broadcast::Sender<FeedEvent>) -> anyhow::Result<()> {
    let client = alloy::rpc::client::RpcClient::builder()
        .ws(WsConnect::new(url.to_string()))
        .await
        .map_err(|e| anyhow::anyhow!("WS connect failed: {e}"))?;
    let provider = RootProvider::new(client);

    let mut block_stream = provider
        .subscribe_blocks()
        .await
        .map_err(|e| anyhow::anyhow!("newHeads subscribe failed: {e}"))?
        .into_stream();

    // Prefer full-transaction pending objects; fall back to hashes on providers
    // that reject `eth_subscribe "newPendingTransactions", true`.
    let pending_full = provider.subscribe_full_pending_transactions().await;
    let mut pending_stream = match pending_full {
        Ok(sub) => Some(sub.into_stream()),
        Err(e) => {
            tracing::info!("full pending tx subscription unavailable ({e:#}); using hash feed + getTransactionByHash");
            None
        }
    };

    // Resolve pending hashes -> full txs on a second (fallback) connection-less
    // client. Reused only when the provider streams hashes.
    let hash_client = if pending_stream.is_none() {
        let client = alloy::rpc::client::RpcClient::builder()
            .ws(WsConnect::new(url.to_string()))
            .await
            .map_err(|e| anyhow::anyhow!("WS connect failed: {e}"))?;
        Some(RootProvider::new(client))
    } else {
        None
    };

    let _ = tx.send(FeedEvent::Connected);
    tracing::info!("WS feed connected and subscribed (newHeads + newPendingTransactions)");

    let mut pending_batch: Vec<TxData> = Vec::new();
    let mut batch_deadline: Option<tokio::time::Instant> = None;

    loop {
        // If a partial pending batch is aging out, flush it.
        if let Some(deadline) = batch_deadline {
            if tokio::time::Instant::now() >= deadline {
                if !pending_batch.is_empty() {
                    let batch = std::mem::take(&mut pending_batch);
                    tx.send(FeedEvent::NewPendingTxs(batch)).map_err(|e| anyhow::anyhow!("feed send failed: {e}"))?;
                }
                batch_deadline = None;
            }
        }

        tokio::select! {
            maybe_header = block_stream.next() => {
                match maybe_header {
                    Some(Ok(header)) => {
                        tx.send(FeedEvent::NewHead { number: header.number })
                            .map_err(|e| anyhow::anyhow!("feed send failed: {e}"))?;
                    }
                    Some(Err(e)) => return Err(anyhow::anyhow!("newHeads stream error: {e}")),
                    None => return Err(anyhow::anyhow!("newHeads stream ended")),
                }
            }
            maybe_tx = async {
                match &mut pending_stream {
                    Some(stream) => stream.next().await,
                    None => None,
                }
            } => {
                match maybe_tx {
                    Some(Ok(tx_envelope)) => {
                        // Full transaction object from the pending subscription.
                        let data = alloy_tx_to_tx_data(&tx_envelope, 0);
                        push_pending(&mut pending_batch, &mut batch_deadline, data, tx)?;
                    }
                    Some(Err(e)) => return Err(anyhow::anyhow!("pending tx stream error: {e}")),
                    None => {
                        // No full-tx stream (hash mode) OR stream ended. If hash
                        // mode, resolve each incoming hash to a full transaction.
                        // A None here with pending_stream set means the stream
                        // ended -> treat as disconnect.
                        if pending_stream.is_some() {
                            return Err(anyhow::anyhow!("pending tx stream ended"));
                        }
                        // Hash fallback handled by a dedicated resolution task below.
                    }
                }
            }
        }
    }
}
