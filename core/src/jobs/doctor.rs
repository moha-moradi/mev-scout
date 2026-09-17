//! Provider capability probe (explorer doctor).

use serde::Serialize;

use crate::config::validation;
use crate::config::Config;
use crate::progress::JobProgress;
use crate::rpc::RpcClient;

#[derive(Debug, Clone, Serialize)]
pub struct ProviderProbe {
    pub url_shown: String,
    pub latest: String,
    pub archive: String,
    pub bulk_receipts: String,
    pub traces: String,
    pub rps: Option<f64>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DoctorOutcome {
    pub chain: String,
    pub chain_id: u64,
    pub providers: Vec<ProviderProbe>,
    pub gate_ok: bool,
}

fn short_err(e: &anyhow::Error) -> String {
    format!("{e:#}").chars().take(40).collect()
}

fn show_url(url: &str) -> String {
    if url.len() > 40 {
        format!("{}..", &url[..38])
    } else {
        url.to_string()
    }
}

pub async fn job_doctor(
    config: &Config,
    progress: &dyn JobProgress,
) -> anyhow::Result<DoctorOutcome> {
    let v = validation::validate_live(config).map_err(|e| anyhow::anyhow!("{e}"))?;
    let chain = v.chain_name;
    progress.log(&format!(
        "Explorer doctor — {chain} (chain id {})",
        chain.chain_id()
    ));

    let setup = super::rpc::init_rpc(config, chain, false).await?;
    let mut providers = Vec::new();
    let mut gate_ok = false;

    for p in setup.provider_configs.iter() {
        let url = &p.url;
        let shown = show_url(url);
        let urls = [url.as_str()];

        let latest = match RpcClient::from_urls(&urls, chain.chain_id()) {
            Ok(c) => c
                .get_block_number()
                .await
                .map(|n| n.to_string())
                .unwrap_or_else(|_| "x".into()),
            Err(_) => "x".into(),
        };
        let archive = if p.archive { "config" } else { "n/a" };
        let bulk = match RpcClient::from_urls(&urls, chain.chain_id()) {
            Ok(c) => {
                let tip = c.get_block_number().await.unwrap_or(0);
                if tip > 1 {
                    match c.get_receipts(tip - 1).await {
                        Ok(_) => "ok".into(),
                        Err(e) => format!("x {}", short_err(&e)),
                    }
                } else {
                    "x".into()
                }
            }
            Err(_) => "x".into(),
        };
        let traces = match RpcClient::from_urls(&urls, chain.chain_id()) {
            Ok(c) => {
                let tip = c.get_block_number().await.unwrap_or(0);
                let tx_hash = if tip > 2 {
                    c.get_block(tip - 2)
                        .await
                        .ok()
                        .and_then(|(_, txs)| txs.first().map(|t| t.hash))
                } else {
                    None
                };
                match tx_hash {
                    Some(h) => match c.debug_trace_transaction_prestatediff(h).await {
                        Ok(_) => "ok".into(),
                        Err(e) => format!("x {}", short_err(&e)),
                    },
                    None => "n/a".into(),
                }
            }
            Err(_) => "x".into(),
        };

        if latest != "x" && bulk == "ok" {
            gate_ok = true;
        }

        progress.log(&format!(
            "  {shown}  latest={latest} archive={archive} bulk={bulk} traces={traces} rps={:?}",
            p.rps
        ));

        providers.push(ProviderProbe {
            url_shown: shown,
            latest,
            archive: archive.to_string(),
            bulk_receipts: bulk,
            traces,
            rps: p.rps,
        });
    }

    progress.log(
        "Gate: Phase 0 requires at least one provider with latest + bulk-receipts support.",
    );
    if gate_ok {
        progress.log("Gate: PASS");
    } else {
        progress.log("Gate: FAIL");
    }

    Ok(DoctorOutcome {
        chain: chain.to_string(),
        chain_id: chain.chain_id(),
        providers,
        gate_ok,
    })
}
