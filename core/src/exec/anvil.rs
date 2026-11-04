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

    /// Dev chain, no fork. Used by the deploy smoke test.
    pub fn spawn_dev(bin: &str) -> anyhow::Result<Self> {
        Self::start(bin, None, None)
    }

    pub fn spawn(bin: &str, fork_url: &str, block: u64) -> anyhow::Result<Self> {
        Self::start(bin, Some(fork_url), Some(block))
    }

    fn start(bin: &str, fork_url: Option<&str>, block: Option<u64>) -> anyhow::Result<Self> {
        let port = pick_port();
        let args = anvil_args(bin, fork_url, block, port);
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
    let gas_used = hex_u64(
        receipt
            .get("gasUsed")
            .and_then(|v| v.as_str())
            .unwrap_or("0x0"),
    );
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
        (gas_used as u128).saturating_mul(gas_price),
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
    use std::sync::atomic::{AtomicU32, Ordering};
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let mixed = (std::process::id()).wrapping_mul(17).wrapping_add(n);
    20_000 + (mixed % 40_000) as u16
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
#[allow(clippy::unwrap_used, clippy::expect_used)]
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

    /// Spawns a local Anvil, deploys the executor, and checks code is present.
    /// Returns immediately when `anvil` is not on PATH (or `ANVIL_BIN`).
    #[test]
    fn deploys_executor_when_anvil_is_installed() {
        let bin = std::env::var("ANVIL_BIN").unwrap_or_else(|_| "anvil".to_string());
        if std::process::Command::new(&bin)
            .arg("--version")
            .output()
            .is_err()
        {
            return;
        }
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        rt.block_on(async {
            let node = AnvilNode::spawn_dev(&bin).expect("spawn");
            node.wait_ready().await.expect("ready");
            let from = super::super::ANVIL_DEFAULT_ADDRESS;
            impersonate_and_fund(&node.url, from).await.expect("fund");
            let deployed = deploy_executor(&node.url, from).await.expect("deploy");
            let code = rpc(
                &node.url,
                "eth_getCode",
                json!([format!("{deployed:#x}"), "latest"]),
            )
            .await
            .expect("code");
            let text = code.as_str().unwrap_or("0x");
            assert!(text.len() > 4, "executor has no code: {text}");
        });
    }

    /// Fixed local chain, two flash-arbs through the same V2 pools. The second
    /// receipt is a real Anvil tx and earns less (or reverts) because the first
    /// take moved the reserves. Skips when `anvil` or `forge` is not installed.
    #[test]
    fn same_pool_second_candidate_earns_less() {
        let bin = std::env::var("ANVIL_BIN").unwrap_or_else(|_| "anvil".to_string());
        if std::process::Command::new(&bin)
            .arg("--version")
            .output()
            .is_err()
        {
            return;
        }
        if std::process::Command::new("forge")
            .arg("--version")
            .output()
            .is_err()
        {
            return;
        }
        let weth_code = forge_creation("MockERC20");
        let pair_code = forge_creation("MockV2Pair");
        let vault_code = forge_creation("MockVault");
        let exec_code = forge_creation("FlashArbExecutor");

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        rt.block_on(async {
            let node = AnvilNode::spawn_dev(&bin).expect("spawn");
            node.wait_ready().await.expect("ready");
            let from = super::super::ANVIL_DEFAULT_ADDRESS;
            impersonate_and_fund(&node.url, from).await.expect("fund");

            let weth = deploy(&node.url, from, Bytes::from(weth_code.clone())).await;
            let usdc = deploy(&node.url, from, Bytes::from(weth_code)).await;
            let cheap = deploy(
                &node.url,
                from,
                with_addrs(pair_code.clone(), &[weth, usdc]),
            )
            .await;
            let rich = deploy(&node.url, from, with_addrs(pair_code, &[weth, usdc])).await;
            let vault = deploy(&node.url, from, Bytes::from(vault_code)).await;
            let executor = deploy(&node.url, from, Bytes::from(exec_code)).await;

            call(&node.url, from, weth, selector_call("setWithdrawer()")).await;
            let ten = eth(10);
            let hundred = eth(100);
            call(&node.url, from, weth, mint_data(cheap, ten)).await;
            call(&node.url, from, usdc, mint_data(cheap, eth(20_000))).await;
            call(&node.url, from, cheap, selector_call("sync()")).await;
            call(&node.url, from, weth, mint_data(rich, ten)).await;
            call(&node.url, from, usdc, mint_data(rich, eth(30_000))).await;
            call(&node.url, from, rich, selector_call("sync()")).await;
            call(&node.url, from, weth, mint_data(vault, hundred)).await;
            rpc(
                &node.url,
                "anvil_setBalance",
                json!([
                    format!("{weth:#x}"),
                    format!("0x{:x}", 100u128 * 10u128.pow(18))
                ]),
            )
            .await
            .expect("fund weth");

            let plan = sample_plan(vault, weth, usdc, rich, cheap, from, eth(1));
            let data = crate::exec::encode_execute(&plan);
            let first = send_exec(&node.url, from, executor, data.clone())
                .await
                .expect("first");
            let second = send_exec(&node.url, from, executor, data)
                .await
                .expect("second");

            assert!(first.status, "first candidate should land: {first:?}");
            assert!(first.tx_hash.is_some() && second.tx_hash.is_some());
            assert_ne!(first.tx_hash, second.tx_hash);
            if second.status {
                let p1 = first.net_wei.saturating_add(first.gas_wei as i128);
                let p2 = second.net_wei.saturating_add(second.gas_wei as i128);
                assert!(
                    p2 < p1,
                    "second take on the same pools should earn less ({p2} vs {p1})"
                );
            }
        });
    }

    fn forge_creation(contract: &str) -> Vec<u8> {
        let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../contracts");
        let path = find_artifact(&dir.join("out"), contract).unwrap_or_else(|| {
            let out = std::process::Command::new("forge")
                .arg("build")
                .current_dir(&dir)
                .output()
                .expect("forge build");
            assert!(
                out.status.success(),
                "forge build failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            find_artifact(&dir.join("out"), contract)
                .unwrap_or_else(|| panic!("forge artifact for {contract} missing"))
        });
        let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path:?}: {e}"));
        let v: Value = serde_json::from_str(&text).unwrap_or_else(|e| panic!("{path:?}: {e}"));
        let hex = v
            .pointer("/bytecode/object")
            .and_then(|s| s.as_str())
            .unwrap_or_else(|| panic!("{path:?} has no bytecode.object"));
        decode_hex_bytes(hex)
    }

    fn find_artifact(dir: &std::path::Path, contract: &str) -> Option<std::path::PathBuf> {
        let target = format!("{contract}.json");
        let mut stack = vec![dir.to_path_buf()];
        while let Some(cur) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&cur) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                } else if path.file_name().and_then(|n| n.to_str()) == Some(target.as_str()) {
                    return Some(path);
                }
            }
        }
        None
    }

    fn decode_hex_bytes(raw: &str) -> Vec<u8> {
        let compact = raw.trim().trim_start_matches("0x");
        hex::decode(compact).unwrap_or_else(|e| panic!("artifact hex: {e}"))
    }

    fn with_addrs(code: Vec<u8>, addrs: &[Address]) -> Bytes {
        let mut out = code;
        for addr in addrs {
            let mut word = [0u8; 32];
            word[12..].copy_from_slice(addr.as_slice());
            out.extend_from_slice(&word);
        }
        Bytes::from(out)
    }

    fn eth(n: u128) -> U256 {
        U256::from(n) * U256::from(10u128.pow(18))
    }

    fn selector_call(sig: &str) -> Bytes {
        let hash = alloy::primitives::keccak256(sig.as_bytes());
        Bytes::from(hash[..4].to_vec())
    }

    fn mint_data(to: Address, amount: U256) -> Bytes {
        let mut data = selector_call("mint(address,uint256)").to_vec();
        let mut word = [0u8; 32];
        word[12..].copy_from_slice(to.as_slice());
        data.extend_from_slice(&word);
        let amount_word: [u8; 32] = amount.to_be_bytes();
        data.extend_from_slice(&amount_word);
        Bytes::from(data)
    }

    fn sample_plan(
        vault: Address,
        weth: Address,
        usdc: Address,
        rich: Address,
        cheap: Address,
        profit_to: Address,
        amount: U256,
    ) -> crate::exec::plan::ExecPlan {
        use crate::exec::plan::{ExecLeg, ExecPlan};
        use crate::types::FlashLoanProvider;
        ExecPlan {
            vault,
            borrow_token: weth,
            amount,
            legs: vec![
                ExecLeg {
                    pool: rich,
                    token_in: weth,
                    token_out: usdc,
                    kind: 0,
                    fee: 30,
                },
                ExecLeg {
                    pool: cheap,
                    token_in: usdc,
                    token_out: weth,
                    kind: 0,
                    fee: 30,
                },
            ],
            profit_to,
            weth,
            provider: FlashLoanProvider::Balancer,
        }
    }

    async fn deploy(url: &str, from: Address, code: Bytes) -> Address {
        let hash = send_tx(url, from, None, code, 8_000_000)
            .await
            .expect("deploy send");
        let receipt = wait_receipt(url, &hash).await.expect("deploy receipt");
        assert_status(&receipt, "deploy");
        receipt
            .get("contractAddress")
            .and_then(|v| v.as_str())
            .expect("contractAddress")
            .parse()
            .expect("address")
    }

    async fn call(url: &str, from: Address, to: Address, data: Bytes) {
        let hash = send_tx(url, from, Some(to), data, 2_000_000)
            .await
            .expect("call");
        let receipt = wait_receipt(url, &hash).await.expect("call receipt");
        assert_status(&receipt, "call");
    }

    fn assert_status(receipt: &Value, what: &str) {
        let status = receipt.get("status").and_then(|v| v.as_str()).unwrap_or("");
        assert!(
            status == "0x1" || status == "1",
            "{what} reverted: {receipt}"
        );
    }
}
