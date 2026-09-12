use mev_scout_core::config::{Config, ProviderConfig};
use mev_scout_core::rpc::RpcClient;
use mev_scout_core::types::ChainName;

pub struct RpcSetup {
    pub rpc: RpcClient,
    pub provider_configs: Vec<ProviderConfig>,
}

pub async fn init_rpc(
    config: &Config,
    chain_name: ChainName,
    check_connection: bool,
) -> anyhow::Result<RpcSetup> {
    let provider_configs = config.effective_provider_configs(chain_name)?;
    let chain_id = chain_name.chain_id();
    let rpc_refs: Vec<&str> = provider_configs
        .iter()
        .map(|p| p.url.as_str())
        .collect();
    let rpc = RpcClient::from_urls(&rpc_refs, chain_id)?;
    rpc.with_provider_rps(
        &provider_configs
            .iter()
            .map(|p| p.rps.unwrap_or(config.rpc.rps_limit))
            .collect::<Vec<_>>(),
    )
    .await;
    rpc.with_provider_archive(
        &provider_configs
            .iter()
            .map(|p| p.archive)
            .collect::<Vec<_>>(),
    )
    .await;
    if check_connection {
        rpc.check_connection().await?;
    }
    Ok(RpcSetup {
        rpc,
        provider_configs,
    })
}
