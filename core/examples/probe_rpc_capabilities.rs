//! Phase 0 of `docs/pool-discovery-benchmark-plan.md`.
//!
//! Probes each configured JSON-RPC peer for the capabilities the receipts /
//! factory-state discovery paths depend on (P1-P5) and writes a capability
//! matrix to `bench/capability-matrix.json`.
//!
//! Usage:
//!   cargo run -p mev-scout-core --example probe_rpc_capabilities -- \
//!       [--publicnode URL] [--dwellir URL] [--out PATH]
//!
//! Dwellir's URL defaults to the archive endpoint with
//! `$DWELLIR_AVALANCHE_KEY` substituted; it is skipped when the key is absent.
//! This probe is strictly read-only: every call is an `eth_*` read.

use std::time::Instant;

use serde::Serialize;

/// Canonical Uniswap V3 factory on Avalanche C-Chain (has `allPoolsLength()`).
const AVAX_V3_FACTORY: &str = "0x740b1c1de25031C31FF4fC9A62f554A55cdC1baD";
/// keccak256("allPoolsLength()")[..4] (verified against 4byte.directory).
const ALL_POOLS_LENGTH_SELECTOR: &str = "0xefde4e64";
/// keccak256("getAllPoolsLength()")[..4] — alternate factory ABI.
const GET_ALL_POOLS_LENGTH_SELECTOR: &str = "0x69ec9aad";
/// Avalanche C-Chain: 43,200 blocks/day → 30 days.
const BLOCKS_30D: u64 = 1_296_000;

#[derive(Debug, Serialize)]
struct ProbeResult {
    test: &'static str,
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    status: Option<u16>,
    latency_ms: u128,
    bytes: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    detail: Option<String>,
}

#[derive(Debug, Serialize)]
struct PeerResult {
    peer: String,
    url: String,
    tip: Option<u64>,
    probes: Vec<ProbeResult>,
}

fn parse_args() -> (Vec<(String, String)>, String) {
    let mut peers = Vec::new();
    let mut out = "bench/capability-matrix.json".to_string();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--publicnode" | "--dwellir" => {
                let label = if arg == "--publicnode" {
                    "publicnode"
                } else {
                    "dwellir"
                };
                if let Some(url) = args.next() {
                    peers.push((label.to_string(), url));
                }
            }
            "--out" => {
                if let Some(path) = args.next() {
                    out = path;
                }
            }
            _ => {}
        }
    }
    if peers.is_empty() {
        peers.push((
            "publicnode".to_string(),
            "https://avalanche-c-chain.publicnode.com".to_string(),
        ));
        if let Ok(key) = std::env::var("DWELLIR_AVALANCHE_KEY") {
            if !key.is_empty() {
                peers.push((
                    "dwellir".to_string(),
                    format!(
                        "https://api-avalanche-mainnet-archive.n.dwellir.com/{key}/ext/bc/C/rpc"
                    ),
                ));
            }
        }
    }
    (peers, out)
}

async fn post(url: &str, body: serde_json::Value) -> ProbeResult {
    let test = body
        .get("method")
        .and_then(|v| v.as_str())
        .unwrap_or("batch");
    let test = match test {
        "eth_blockNumber" => "P1a",
        "eth_getBlockReceipts" => {
            // The probe label is supplied by the caller via the id field below.
            body.get("id")
                .and_then(|v| v.as_str())
                .unwrap_or("P1b")
        }
        "eth_call" => "P4",
        "eth_getLogs" => "P5",
        _ => "P3",
    };
    let start = Instant::now();
    match reqwest::Client::new()
        .post(url)
        .json(&body)
        .send()
        .await
    {
        Ok(resp) => {
            let status = resp.status().as_u16();
            let bytes = resp.content_length().unwrap_or(0) as usize;
            let text = resp.text().await.unwrap_or_default();
            let detail = summarise(&text, 220);
            ProbeResult {
                test: leak_label(test),
                ok: status < 400 && !detail.contains("\"error\""),
                status: Some(status),
                latency_ms: start.elapsed().as_millis(),
                bytes: bytes.max(text.len()),
                detail: Some(detail),
            }
        }
        Err(e) => ProbeResult {
            test: leak_label(test),
            ok: false,
            status: None,
            latency_ms: start.elapsed().as_millis(),
            bytes: 0,
            detail: Some(format!("transport error: {e:#}")),
        },
    }
}

/// Probe labels are static strings; Leaking here keeps `ProbeResult.test` a
/// `&'static str` without threading a lifetime through every helper.
fn leak_label(s: &str) -> &'static str {
    match s {
        "P1a" => "P1a",
        "P1b" => "P1b",
        "P2" => "P2",
        "P3" => "P3",
        "P4" => "P4",
        "P4-latest" | "P4-latest-get" => "P4-latest",
        "P4-old" | "P4-old-get" => "P4-old",
        "P5-tip" => "P5-tip",
        "P5-10" => "P5-10",
        _ => "P?",
    }
}

fn summarise(text: &str, max: usize) -> String {
    let compact: String = text.chars().take(max).collect();
    compact.replace('\n', " ")
}

/// Keep provider credentials out of the generated capability matrix.
fn redact_url(url: &str) -> String {
    url.split('/')
        .map(|part| {
            let compact = part.replace('-', "");
            if compact.len() >= 32 && compact.chars().all(|c| c.is_ascii_hexdigit()) {
                "<redacted>".to_string()
            } else {
                part.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("/")
}

fn rpc(method: &str, params: serde_json::Value, id: u64) -> serde_json::Value {
    serde_json::json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params})
}

fn hex_block(n: u64) -> String {
    format!("0x{n:x}")
}

async fn probe_peer(label: &str, url: &str) -> PeerResult {
    let bn = rpc("eth_blockNumber", serde_json::json!([]), 1);
    let start = Instant::now();
    let tip = match reqwest::Client::new().post(url).json(&bn).send().await {
        Ok(resp) => match resp.json::<serde_json::Value>().await {
            Ok(v) => v
                .get("result")
                .and_then(|s| s.as_str())
                .and_then(|s| u64::from_str_radix(s.trim_start_matches("0x"), 16).ok()),
            Err(_) => None,
        },
        Err(_) => None,
    };
    let bn_ms = start.elapsed().as_millis();
    let mut probes = vec![ProbeResult {
        test: "P1a",
        ok: tip.is_some(),
        status: Some(200),
        latency_ms: bn_ms,
        bytes: 0,
        detail: tip.map(|t| format!("tip={t}")),
    }];

    let Some(tip) = tip else {
        return PeerResult {
            peer: label.to_string(),
            url: redact_url(url),
            tip: None,
            probes,
        };
    };

    // P1b — receipts at tip.
    let r = post(
        url,
        serde_json::json!({"jsonrpc": "2.0", "id": "P1b", "method": "eth_getBlockReceipts",
                           "params": [hex_block(tip)]}),
    )
    .await;
    probes.push(r);

    // P2 — 30-day retention.
    let old = tip.saturating_sub(BLOCKS_30D);
    let r = post(
        url,
        serde_json::json!({"jsonrpc": "2.0", "id": "P2", "method": "eth_getBlockReceipts",
                           "params": [hex_block(old)]}),
    )
    .await;
    probes.push(r);

    // P3 — four getBlockReceipts in one JSON-RPC batch POST.
    let batch: Vec<_> = (0..4)
        .map(|i| {
            serde_json::json!({"jsonrpc": "2.0", "id": i, "method": "eth_getBlockReceipts",
                               "params": [hex_block(tip - i)]})
        })
        .collect();
    let start = Instant::now();
    let r = match reqwest::Client::new().post(url).json(&batch).send().await {
        Ok(resp) => {
            let status = resp.status().as_u16();
            let text = resp.text().await.unwrap_or_default();
            let count = serde_json::from_str::<serde_json::Value>(&text)
                .ok()
                .and_then(|v| v.as_array().map(|a| a.len()))
                .unwrap_or(0);
            ProbeResult {
                test: "P3",
                ok: status < 400 && count == 4,
                status: Some(status),
                latency_ms: start.elapsed().as_millis(),
                bytes: text.len(),
                detail: Some(format!("batch items={count}")),
            }
        }
        Err(e) => ProbeResult {
            test: "P3",
            ok: false,
            status: None,
            latency_ms: start.elapsed().as_millis(),
            bytes: 0,
            detail: Some(format!("transport error: {e:#}")),
        },
    };
    probes.push(r);

    // P4 — historic eth_call at latest and tip-30d. Probe both common
    // enumeration selectors so a wrong selector cannot masquerade as a
    // missing capability.
    for (selector, suffix) in [
        (ALL_POOLS_LENGTH_SELECTOR, ""),
        (GET_ALL_POOLS_LENGTH_SELECTOR, "-get"),
    ] {
        for (label, block) in [
            (format!("P4-latest{suffix}"), "latest".to_string()),
            (format!("P4-old{suffix}"), hex_block(old)),
        ] {
            let r = post(
                url,
                serde_json::json!({"jsonrpc": "2.0", "id": label,
                    "method": "eth_call",
                    "params": [{"to": AVAX_V3_FACTORY, "data": selector}, block]}),
            )
            .await;
            probes.push(r);
        }
    }

    // P5 — address-less eth_getLogs (single block and 10-block range).
    for (label, from, to) in [
        ("P5-tip", hex_block(tip), hex_block(tip)),
        ("P5-10", hex_block(tip - 10), hex_block(tip)),
    ] {
        let r = post(
            url,
            serde_json::json!({"jsonrpc": "2.0", "id": label, "method": "eth_getLogs",
                "params": [{"fromBlock": from, "toBlock": to}]}),
        )
        .await;
        probes.push(r);
    }

    PeerResult {
        peer: label.to_string(),
        url: redact_url(url),
        tip: Some(tip),
        probes,
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let (peers, out) = parse_args();
    let mut results = Vec::new();
    for (label, url) in &peers {
        results.push(probe_peer(label, url).await);
    }

    if let Some(parent) = std::path::Path::new(&out).parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    std::fs::write(&out, serde_json::to_string_pretty(&results)?)?;

    println!("capability matrix written to {out}\n");
    println!("| peer | test | ok | status | ms | detail |");
    println!("|---|---|---|---:|---:|---|");
    for peer in &results {
        for p in &peer.probes {
            println!(
                "| {} | {} | {} | {} | {} | {} |",
                peer.peer,
                p.test,
                if p.ok { "yes" } else { "NO" },
                p.status.map(|s| s.to_string()).unwrap_or_default(),
                p.latency_ms,
                p.detail.as_deref().unwrap_or("").replace('|', "\\|")
            );
        }
    }
    Ok(())
}
