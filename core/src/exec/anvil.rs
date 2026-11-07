//! Local Anvil fork: spawn or attach, signed EIP-1559 deploy/send of the executor.
use std::io::Read;
use std::process::{Child, Command, Stdio};
use std::str::FromStr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use alloy::consensus::{SignableTransaction, TxEip1559};
use alloy::eips::eip2718::Encodable2718;
use alloy::primitives::{Address, Bytes, TxKind, U256};
use alloy::signers::local::PrivateKeySigner;
use alloy::signers::SignerSync;
use serde_json::{json, Value};

use crate::paper::types::{ExecAttempt, FillSkipReason};

use super::artifacts::{append_address_args, creation_bytecode};

/// Dev (no fork) is instant; forked tip against a public RPC often needs a minute+.
const READY_TIMEOUT_DEV: Duration = Duration::from_secs(20);
const READY_TIMEOUT_FORK: Duration = Duration::from_secs(120);

pub struct AnvilNode {
    pub url: String,
    child: Option<Child>,
    forked: bool,
    /// Tail of anvil stderr, filled by a reader thread (non-blocking for wait_ready).
    stderr: Arc<Mutex<String>>,
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
        Ok(Self {
            url,
            child: None,
            forked: false,
            stderr: Arc::new(Mutex::new(String::new())),
        })
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
        let forked = fork_url.is_some();
        let args = anvil_args(bin, fork_url, block, port);
        let mut cmd = Command::new(&args[0]);
        // Keep stderr so a failed fork (bad RPC / block) surfaces in wait_ready.
        cmd.args(&args[1..])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        let mut child = cmd.spawn().map_err(|e| {
            anyhow::anyhow!(
                "failed to spawn anvil ({bin}): {e}. Install foundry and put `anvil` on PATH, \
                 or set [paper].anvil_url to a loopback fork."
            )
        })?;
        let stderr = Arc::new(Mutex::new(String::new()));
        if let Some(pipe) = child.stderr.take() {
            spawn_stderr_reader(pipe, Arc::clone(&stderr));
        }
        let url = format!("http://127.0.0.1:{port}");
        Ok(Self {
            url,
            child: Some(child),
            forked,
            stderr,
        })
    }

    pub async fn wait_ready(&mut self) -> anyhow::Result<()> {
        let timeout = if self.forked {
            READY_TIMEOUT_FORK
        } else {
            READY_TIMEOUT_DEV
        };
        let deadline = Instant::now() + timeout;
        let mut last = String::from("not started");
        while Instant::now() < deadline {
            if let Some(child) = self.child.as_mut() {
                if let Ok(Some(status)) = child.try_wait() {
                    anyhow::bail!(
                        "anvil exited early ({status}) before eth_chainId answered. stderr:\n{}",
                        self.stderr_tail()
                    );
                }
            }
            match rpc(&self.url, "eth_chainId", json!([])).await {
                Ok(_) => return Ok(()),
                Err(e) => last = e.to_string(),
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        anyhow::bail!(
            "anvil at {} did not answer eth_chainId within {}s: {last}. stderr:\n{}",
            self.url,
            timeout.as_secs(),
            self.stderr_tail()
        )
    }

    fn stderr_tail(&self) -> String {
        self.stderr
            .lock()
            .map(|s| {
                if s.len() > 4_000 {
                    s[s.len() - 4_000..].to_string()
                } else {
                    s.clone()
                }
            })
            .unwrap_or_default()
    }
}

/// Signed EIP-1559 sender for a local Anvil node.
pub struct AnvilSender {
    url: String,
    chain_id: u64,
    signer: PrivateKeySigner,
    pub from: Address,
    nonce: u64,
}

impl AnvilSender {
    pub async fn connect(url: &str, key: &str) -> anyhow::Result<Self> {
        assert_loopback(url)?;
        let chain_id = remote_chain_id(url).await?;
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

    pub async fn fund(&mut self, wei: u128) -> anyhow::Result<()> {
        assert_loopback(&self.url)?;
        rpc(
            &self.url,
            "anvil_setBalance",
            json!([format!("{:#x}", self.from), format!("0x{wei:x}")]),
        )
        .await?;
        Ok(())
    }

    /// Deploy FlashArbExecutor with constructor args `[operator, withdrawer, weth]`.
    /// Returns `(address, setup_gas_wei)`.
    pub async fn deploy_executor(&mut self, args: &[Address]) -> anyhow::Result<(Address, u128)> {
        let code = append_address_args(&creation_bytecode()?, args);
        let hash = self.send(None, code, 8_000_000).await?;
        let receipt = wait_receipt(&self.url, &hash).await?;
        let addr = receipt
            .get("contractAddress")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("anvil deploy receipt missing contractAddress"))?;
        let addr: Address = addr
            .parse()
            .map_err(|e| anyhow::anyhow!("anvil deploy address: {e}"))?;
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
        Ok((addr, (gas_used as u128).saturating_mul(gas_price)))
    }

    pub async fn send_exec(&mut self, to: Address, data: Bytes) -> anyhow::Result<ExecAttempt> {
        let before = balance_of(&self.url, self.from).await?;
        let hash = match self.send(Some(to), data, 2_500_000).await {
            Ok(h) => h,
            Err(e) => {
                tracing::warn!("anvil send failed: {e}");
                return Ok(ExecAttempt::failed(FillSkipReason::ExecBuildFailed));
            }
        };
        let receipt = match wait_receipt(&self.url, &hash).await {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!("anvil receipt failed: {e}");
                return Ok(ExecAttempt::reverted(0, 0, Some(hash)));
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
        let after = balance_of(&self.url, self.from).await.unwrap_or(before);
        let net = signed_delta(before, after);
        Ok(ExecAttempt::ran(
            status,
            net,
            gas_used,
            (gas_used as u128).saturating_mul(gas_price),
            Some(hash),
        ))
    }

    async fn send(&mut self, to: Option<Address>, data: Bytes, gas: u64) -> anyhow::Result<String> {
        assert_loopback(&self.url)?;
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
        let v = rpc(
            &self.url,
            "eth_sendRawTransaction",
            json!([format!("0x{}", hex::encode(raw))]),
        )
        .await?;
        self.nonce = self.nonce.saturating_add(1);
        v.as_str()
            .map(|s| s.to_string())
            .ok_or_else(|| anyhow::anyhow!("eth_sendRawTransaction returned {v}"))
    }
}

fn spawn_stderr_reader(mut pipe: impl Read + Send + 'static, sink: Arc<Mutex<String>>) {
    std::thread::spawn(move || {
        let mut buf = [0u8; 1024];
        loop {
            match pipe.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    if let Ok(mut s) = sink.lock() {
                        s.push_str(&String::from_utf8_lossy(&buf[..n]));
                        // Cap growth so a noisy fork does not balloon RAM.
                        if s.len() > 16_000 {
                            *s = s[s.len() - 8_000..].to_string();
                        }
                    }
                }
                Err(_) => break,
            }
        }
    });
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
    assert_loopback(url)?;
    rpc(
        url,
        "anvil_reset",
        json!([{ "forking": { "jsonRpcUrl": fork_url, "blockNumber": block } }]),
    )
    .await?;
    Ok(())
}

pub async fn wait_receipt(url: &str, hash: &str) -> anyhow::Result<Value> {
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

async fn remote_chain_id(url: &str) -> anyhow::Result<u64> {
    let v = rpc(url, "eth_chainId", json!([])).await?;
    let s = v
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("eth_chainId returned {v}"))?;
    u64::from_str_radix(s.trim_start_matches("0x"), 16)
        .map_err(|e| anyhow::anyhow!("eth_chainId: {e}"))
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
    use crate::exec::ANVIL_DEFAULT_KEY;

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

    /// Spawns a local Anvil, deploys the executor via signed CREATE.
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
            let mut node = AnvilNode::spawn_dev(&bin).expect("spawn");
            node.wait_ready().await.expect("ready");
            let mut sender = AnvilSender::connect(&node.url, ANVIL_DEFAULT_KEY)
                .await
                .expect("connect");
            sender.fund(10u128.pow(18)).await.expect("fund");
            let weth = Address::repeat_byte(0x11);
            let (deployed, _) = sender
                .deploy_executor(&[sender.from, sender.from, weth])
                .await
                .expect("deploy");
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

    /// Non-operator cannot call execute (on-chain NotOperator).
    #[test]
    fn non_operator_execute_reverts() {
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
            let mut node = AnvilNode::spawn_dev(&bin).expect("spawn");
            node.wait_ready().await.expect("ready");
            let mut operator = AnvilSender::connect(&node.url, ANVIL_DEFAULT_KEY)
                .await
                .expect("op");
            operator.fund(10u128.pow(18)).await.expect("fund");
            let weth = Address::repeat_byte(0x22);
            let (executor, _) = operator
                .deploy_executor(&[operator.from, operator.from, weth])
                .await
                .expect("deploy");

            // Anvil account #1 — not the operator (Foundry default, 32-byte key).
            const OTHER: &str =
                "0x59c6995e998f97a5a0044966f0945389dc9e86dae88c7a8412f4603b6b786466";
            let mut stranger = AnvilSender::connect(&node.url, OTHER)
                .await
                .expect("stranger");
            stranger.fund(10u128.pow(18)).await.expect("fund stranger");
            let data = crate::exec::encode_execute(&sample_plan(
                Address::repeat_byte(0x33),
                weth,
                Address::repeat_byte(0x44),
                Address::repeat_byte(0x55),
                Address::repeat_byte(0x66),
                U256::from(1u64),
            ));
            let attempt = stranger.send_exec(executor, data).await.expect("send");
            assert!(!attempt.status, "stranger execute must revert: {attempt:?}");
        });
    }

    /// Fixed local chain, two flash-arbs through the same V2 pools.
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
        let forge = foundry_bin("forge");
        if std::process::Command::new(&forge)
            .arg("--version")
            .output()
            .is_err()
        {
            return;
        }
        let weth_code = forge_creation(&forge, "MockERC20");
        let pair_code = forge_creation(&forge, "MockV2Pair");
        let vault_code = forge_creation(&forge, "MockVault");

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        rt.block_on(async {
            let mut node = AnvilNode::spawn_dev(&bin).expect("spawn");
            node.wait_ready().await.expect("ready");
            let mut sender = AnvilSender::connect(&node.url, ANVIL_DEFAULT_KEY)
                .await
                .expect("connect");
            sender.fund(1_000u128 * 10u128.pow(18)).await.expect("fund");
            let from = sender.from;

            let weth = deploy_raw(&mut sender, Bytes::from(weth_code.clone())).await;
            let usdc = deploy_raw(&mut sender, Bytes::from(weth_code)).await;
            let cheap = deploy_raw(&mut sender, with_addrs(pair_code.clone(), &[weth, usdc])).await;
            let rich = deploy_raw(&mut sender, with_addrs(pair_code, &[weth, usdc])).await;
            let vault = deploy_raw(&mut sender, Bytes::from(vault_code)).await;
            let (executor, _) = sender
                .deploy_executor(&[from, from, weth])
                .await
                .expect("exec deploy");

            call_raw(&mut sender, weth, selector_call("setWithdrawer()")).await;
            let ten = eth(10);
            let hundred = eth(100);
            call_raw(&mut sender, weth, mint_data(cheap, ten)).await;
            call_raw(&mut sender, usdc, mint_data(cheap, eth(20_000))).await;
            call_raw(&mut sender, cheap, selector_call("sync()")).await;
            call_raw(&mut sender, weth, mint_data(rich, ten)).await;
            call_raw(&mut sender, usdc, mint_data(rich, eth(30_000))).await;
            call_raw(&mut sender, rich, selector_call("sync()")).await;
            call_raw(&mut sender, weth, mint_data(vault, hundred)).await;
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

            let plan = sample_plan(vault, weth, usdc, rich, cheap, eth(1));
            let data = crate::exec::encode_execute(&plan);
            let first = sender
                .send_exec(executor, data.clone())
                .await
                .expect("first");
            let second = sender.send_exec(executor, data).await.expect("second");

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

    fn foundry_bin(name: &str) -> String {
        let home = std::env::var("USERPROFILE")
            .or_else(|_| std::env::var("HOME"))
            .unwrap_or_default();
        let candidate = std::path::PathBuf::from(home)
            .join(".foundry")
            .join("bin")
            .join(if cfg!(windows) {
                format!("{name}.exe")
            } else {
                name.to_string()
            });
        if candidate.exists() {
            candidate.to_string_lossy().into_owned()
        } else {
            name.to_string()
        }
    }

    fn forge_creation(forge: &str, contract: &str) -> Vec<u8> {
        let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../contracts");
        let path = find_artifact(&dir.join("out"), contract).unwrap_or_else(|| {
            let out = std::process::Command::new(forge)
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
        amount: U256,
    ) -> crate::exec::plan::ExecPlan {
        use crate::exec::plan::{ExecLeg, ExecPlan};
        use crate::types::FlashLoanProvider;
        ExecPlan {
            vault,
            borrow_token: weth,
            amount,
            min_profit: U256::from(1u64),
            deadline: u64::MAX / 2,
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
            provider: FlashLoanProvider::Balancer,
        }
    }

    async fn deploy_raw(sender: &mut AnvilSender, code: Bytes) -> Address {
        let hash = sender
            .send(None, code, 8_000_000)
            .await
            .expect("deploy send");
        let receipt = wait_receipt(&sender.url, &hash)
            .await
            .expect("deploy receipt");
        assert_status(&receipt, "deploy");
        receipt
            .get("contractAddress")
            .and_then(|v| v.as_str())
            .expect("contractAddress")
            .parse()
            .expect("address")
    }

    async fn call_raw(sender: &mut AnvilSender, to: Address, data: Bytes) {
        let hash = sender.send(Some(to), data, 2_000_000).await.expect("call");
        let receipt = wait_receipt(&sender.url, &hash)
            .await
            .expect("call receipt");
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
