use mev_scout_core::config::Config;
use mev_scout_core::rpc::consts::ARCHIVE_PROBE_DEPTH_BLOCKS;
use mev_scout_core::rpc::RpcClient;
use mev_scout_core::types::ChainName;

pub struct RpcSetup {
    pub rpc: RpcClient,
    pub provider_configs: Vec<(String, Option<f64>, bool)>,
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
