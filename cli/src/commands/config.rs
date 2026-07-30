use anyhow::Context;
use mev_scout_core::config::Config;

pub async fn cmd_config(config: &Config) -> anyhow::Result<()> {
    let toml_str = config.to_toml_string()
        .context("failed to serialize config to TOML")?;
    println!("{}", toml_str);
    Ok(())
}
