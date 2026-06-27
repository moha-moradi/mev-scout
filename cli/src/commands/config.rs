use mev_scout_core::config::Config;

pub async fn cmd_config(config: &Config) -> anyhow::Result<()> {
    let toml_str = config.to_toml_string()?;
    println!("{}", toml_str);
    Ok(())
}
