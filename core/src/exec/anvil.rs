//! Local Anvil fork: spawn or attach, deploy the executor, send the same calldata.
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use alloy::primitives::{Address, Bytes, U256};
use serde_json::{json, Value};

use crate::paper::types::{ExecAttempt, FillSkipReason};

use super::artifacts::creation_bytecode;

pub struct AnvilNode {
    pub url: String,
    child: Option<Child>,
}

impl Drop for AnvilNode {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

impl AnvilNode {
    pub fn attach(url: String) -> anyhow::Result<Self> {
        assert_loopback(&url)?;
        Ok(Self { url, child: None })
    }

    pub fn spawn(bin: &str, fork_url: &str, block: u64) -> anyhow::Result<Self> {
        let port = pick_port();
        let args = anvil_args(bin, Some(fork_url), Some(block), port);
        let mut cmd = Command::new(&args[0]);
        cmd.args(&args[1..])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let child = cmd.spawn().map_err(|e| {
            anyhow::anyhow!(
                "failed to spawn anvil ({bin}): {e}. Install foundry and put `anvil` on PATH, \
                 or set [paper].anvil_url to a loopback fork."
            )
        })?;
        let url = format!("http://127.0.0.1:{port}");
        Ok(Self {
            url,
            child: Some(child),
        })
    }

    pub async fn wait_ready(&self) -> anyhow::Result<()> {
        let deadline = Instant::now() + Duration::from_secs(20);
        let mut last = String::from("not started");
        while Instant::now() < deadline {
            match rpc(&self.url, "eth_chainId", json!([])).await {
                Ok(_) => return Ok(()),
                Err(e) => last = e.to_string(),
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        anyhow::bail!("anvil at {} did not answer eth_chainId: {last}", self.url)
    }
}

pub fn anvil_args(bin: &str, fork_url: Option<&str>, block: Option<u64>, port: u16) -> Vec<String> {
    let mut args = vec![
        bin.to_string(),
        "--host".into(),
        "127.0.0.1".into(),
        "--port".into(),
        port.to_string(),
    ];
    if let Some(url) = fork_url {
        args.push("--fork-url".into());
        args.push(url.to_string());
    }
    if let Some(block) = block {
        args.push("--fork-block-number".into());
        args.push(block.to_string());
    }
    args
}

pub async fn reset_fork(url: &str, fork_url: &str, block: u64) -> anyhow::Result<()> {
    rpc(
        url,
        "anvil_reset",
        json!([{ "forking": { "jsonRpcUrl": fork_url, "blockNumber": block } }]),
    )
    .await?;
    Ok(())
}

pub async fn impersonate_and_fund(url: &str, who: Address) -> anyhow::Result<()> {
    let addr = format!("{who:#x}");
    rpc(url, "anvil_impersonateAccount", json!([addr])).await?;
    // 1000 native. Anvil accepts a hex quantity.
    rpc(
        url,
        "anvil_setBalance",
        json!([addr, format!("0x{:x}", 1_000u128 * 10u128.pow(18))]),
    )
    .await?;
    Ok(())
}

pub async fn deploy_executor(url: &str, from: Address) -> anyhow::Result<Address> {
    let code = creation_bytecode()?;
    let hash = send_tx(url, from, None, code, 8_000_000).await?;
    let receipt = wait_receipt(url, &hash).await?;
    let addr = receipt
        .get("contractAddress")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("anvil deploy receipt missing contractAddress"))?;
    addr.parse()
        .map_err(|e| anyhow::anyhow!("anvil deploy address: {e}"))
}

pub async fn send_exec(
    url: &str,
    from: Address,
    to: Address,
    data: Bytes,
) -> anyhow::Result<ExecAttempt> {
    let before = balance_of(url, from).await?;
    let hash = match send_tx(url, from, Some(to), data, 2_500_000).await {
        Ok(h) => h,
        Err(e) => {
            tracing::warn!("anvil send failed: {e}");
            return Ok(ExecAttempt::failed(FillSkipReason::ExecBuildFailed));
        }
    };
    let receipt = match wait_receipt(url, &hash).await {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!("anvil receipt failed: {e}");
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
    let after = balance_of(url, from).await.unwrap_or(before);
    let net = signed_delta(before, after);
    Ok(ExecAttempt::ran(
        status,
        net,
        gas_used,
        gas_used.saturating_mul(gas_price),
        Some(hash),
    ))
}

async fn send_tx(
    url: &str,
    from: Address,
    to: Option<Address>,
    data: Bytes,
    gas: u64,
) -> anyhow::Result<String> {
    let mut tx = serde_json::Map::new();
    tx.insert("from".into(), json!(format!("{from:#x}")));
    if let Some(to) = to {
        tx.insert("to".into(), json!(format!("{to:#x}")));
    }
    tx.insert("data".into(), json!(format!("0x{}", hex::encode(data))));
    tx.insert("gas".into(), json!(format!("0x{gas:x}")));
    let v = rpc(url, "eth_sendTransaction", json!([Value::Object(tx)])).await?;
    v.as_str()
        .map(|s| s.to_string())
        .ok_or_else(|| anyhow::anyhow!("eth_sendTransaction returned {v}"))
}

async fn wait_receipt(url: &str, hash: &str) -> anyhow::Result<Value> {
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline {
        let v = rpc(url, "eth_getTransactionReceipt", json!([hash])).await?;
        if !v.is_null() {
            return Ok(v);
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    anyhow::bail!("timed out waiting for receipt {hash}")
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

pub async fn rpc(url: &str, method: &str, params: Value) -> anyhow::Result<Value> {
    let body = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": method,
        "params": params,
    });
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()?;
    let resp = client.post(url).json(&body).send().await?;
    let value: Value = resp.json().await?;
    if let Some(err) = value.get("error") {
        anyhow::bail!("{method} error: {err}");
    }
    Ok(value.get("result").cloned().unwrap_or(Value::Null))
}

fn assert_loopback(url: &str) -> anyhow::Result<()> {
    let parsed = url::Url::parse(url).map_err(|e| anyhow::anyhow!("anvil url: {e}"))?;
    let host = parsed.host_str().unwrap_or("");
    if host == "127.0.0.1" || host == "localhost" || host == "::1" {
        Ok(())
    } else {
        anyhow::bail!("anvil url must be loopback, got {url}")
    }
}

fn pick_port() -> u16 {
    18000 + (std::process::id() % 20000) as u16
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
    fn fork_args_include_block() {
        let args = anvil_args("anvil", Some("http://rpc"), Some(100), 8545);
        assert!(args.iter().any(|a| a == "--fork-url"));
        assert!(args.iter().any(|a| a == "--fork-block-number"));
        assert!(args.iter().any(|a| a == "100"));
        assert!(args.iter().any(|a| a == "127.0.0.1"));
    }

    #[test]
    fn remote_anvil_url_is_refused() {
        let err = AnvilNode::attach("http://1.2.3.4:8545".into());
        assert!(err.is_err());
    }
}
