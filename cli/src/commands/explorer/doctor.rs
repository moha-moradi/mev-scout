//! ``explorer doctor`` - provider capability probe (latest block, archive reads, bulk receipts, traces).

use super::*;

// ── doctor (Phase 0 capability probe) ───────────────────────────────────

pub async fn cmd_doctor(config: &Config) -> anyhow::Result<()> {
    let v = validation::validate_live(config).map_err(|e| anyhow::anyhow!("{e}"))?;
    let chain = v.chain_name;
    println!("Explorer doctor — {chain} (chain id {})", chain.chain_id());

    let setup = init_rpc(config, chain, false).await?;
    let providers = &setup.provider_configs;

    let mut table = Table::new();
    table.set_header(vec![
        "provider",
        "latest",
        "archive",
        "bulk-receipts",
        "traces",
        "rps",
    ]);
    for p in providers.iter() {
        let url = &p.url;
        let rps = p.rps;
        let archive_flag = p.archive;
        let shown = if url.len() > 40 {
            format!("{}..", &url[..38])
        } else {
            url.clone()
        };
        let urls = [url.as_str()];

        let latest = match mev_scout_core::rpc::RpcClient::from_urls(&urls, chain.chain_id()) {
            Ok(c) => c
                .get_block_number()
                .await
                .map(|n| n.to_string())
                .unwrap_or_else(|_| "x".into()),
            Err(_) => "x".into(),
        };
        let archive = if archive_flag { "config" } else { "n/a" };
        let bulk = match mev_scout_core::rpc::RpcClient::from_urls(&urls, chain.chain_id()) {
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
        let traces = match mev_scout_core::rpc::RpcClient::from_urls(&urls, chain.chain_id()) {
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
        table.add_row(vec![
            shown,
            latest,
            archive.to_string(),
            bulk,
            traces,
            rps.map(|r| format!("{r}")).unwrap_or_else(|| "-".into()),
        ]);
    }
    println!("{table}");
    println!("\nGate: Phase 0 requires at least one provider with latest + bulk-receipts support.");
    Ok(())
}
