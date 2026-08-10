use mev_scout_core::config::Config;
use mev_scout_core::rpc::consts::ARCHIVE_PROBE_DEPTH_BLOCKS;
use mev_scout_core::rpc::RpcClient;
use mev_scout_core::types::ChainName;

pub struct RpcSetup {
    pub rpc: RpcClient,
    pub provider_configs: Vec<(String, Option<f64>, bool)>,
}

/// Resolve `--chain auto` by asking the configured RPC endpoint for its chain ID.
///
/// Silently falls back to the default chain when no RPC URL is configured or the
/// endpoint cannot be probed — `-n auto` never hard-fails here; the subsequent
/// per-command connection check still reports real connectivity problems.
pub async fn resolve_auto_chain(config: &mut Config) -> anyhow::Result<()> {
    if !config.chain.eq_ignore_ascii_case("auto") {
        return Ok(());
    }

    let fallback_chain = Config::default().chain;
    let urls = config.user_rpc_urls().unwrap_or_default();
    if urls.is_empty() {
        tracing::info!(
            "--chain auto with no RPC URL — defaulting to '{}'",
            fallback_chain
        );
        config.chain = fallback_chain;
        return Ok(());
    }

    for url in &urls {
        match probe_chain_id(url).await {
            Ok(Some(name)) => {
                tracing::info!(
                    "--chain auto: endpoint {url} reports chain '{name}' (ID {})",
                    name.chain_id()
                );
                config.chain = name.to_string();
                return Ok(());
            }
            Ok(None) => {
                tracing::warn!(
                    "--chain auto: {url} did not report a known chain ID — falling back to '{fallback_chain}'"
                );
            }
            Err(e) => {
                tracing::warn!("--chain auto: could not probe {url}: {e}");
            }
        }
    }

    config.chain = fallback_chain;
    Ok(())
}

async fn probe_chain_id(url: &str) -> anyhow::Result<Option<ChainName>> {
    let client = RpcClient::from_urls(&[url], 0)?;
    let id = client.get_chain_id().await?;
    Ok(ChainName::from_chain_id(id))
}

pub async fn init_rpc(
    config: &Config,
    chain_name: ChainName,
    check_connection: bool,
) -> anyhow::Result<RpcSetup> {
    let provider_configs = config.effective_provider_configs(chain_name)?;
    let chain_id = chain_name.chain_id();
    let rpc_refs: Vec<&str> = provider_configs.iter().map(|(u, _, _)| u.as_str()).collect();
    let rpc = RpcClient::from_urls(&rpc_refs, chain_id)?;
    rpc.with_provider_rps(
        &provider_configs
            .iter()
            .map(|(_, r, _)| r.unwrap_or(config.rpc.rps_limit))
            .collect::<Vec<_>>(),
    )
    .await;
    rpc.with_provider_archive(&provider_configs.iter().map(|(_, _, a)| *a).collect::<Vec<_>>())
        .await;
    rpc.with_archive_probe_depth(
        config
            .rpc
            .archive_probe_depth_blocks
            .unwrap_or(ARCHIVE_PROBE_DEPTH_BLOCKS),
    );
    if check_connection {
        rpc.check_connection(chain_id).await?;
    }
    Ok(RpcSetup {
        rpc,
        provider_configs,
    })
}
