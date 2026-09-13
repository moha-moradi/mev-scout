//! ``explorer validate`` - cross-check indexed ops against live pool state.

use super::*;

// ── validate ────────────────────────────────────────────────────────────

pub async fn cmd_validate(config: &Config, args: &ValidateArgs) -> anyhow::Result<()> {
    let ValidateArgs {
        since,
        match_window,
        run,
        threshold_sweep,
        emit_missing_pools,
        json,
    } = args;
    let run_ids = if run.is_empty() {
        None
    } else {
        Some(run.clone())
    };
    let v = validation::validate_live(config).map_err(|e| anyhow::anyhow!("{e}"))?;
    let chain = v.chain_name;
    let store = explorer_store(config, chain)?;

    let ts = since_ts(since.as_deref());
    let (from_block, to_block) = block_window(&store, ts)?;

    let report = validate::compute_validation(
        &store,
        chain,
        from_block,
        to_block,
        *match_window,
        run_ids.as_deref(),
        *threshold_sweep,
    )?;

    if *json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        println!("{}", validate::render_validation_report(&report));
    }

    if *emit_missing_pools && !report.missing_pools.is_empty() {
        let path = "results/missing_pools.txt";
        std::fs::create_dir_all("results").ok();
        std::fs::write(path, report.missing_pools.join("\n"))?;
        println!("\nMissing pools written to {path}");
    }
    Ok(())
}

/// Block window covering `since_ts`: [min, max] block_number in mev_ops.
fn block_window(store: &ExplorerStore, since_ts: u64) -> anyhow::Result<(u64, u64)> {
    let ops = store.ops_since(since_ts)?;
    let mut blocks: Vec<u64> = ops.iter().map(|o| o.block_number).collect();
    if blocks.is_empty() {
        anyhow::bail!("no indexed ops in the requested window — run `explorer index` first");
    }
    blocks.sort_unstable();
    Ok((blocks[0], blocks[blocks.len() - 1]))
}
