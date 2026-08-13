use mev_scout_core::config::Config;
use mev_scout_core::dune::client::DuneClient;

fn api_key_from_config() -> Option<String> {
    for path in ["mev-scout.toml", "../mev-scout.toml"] {
        if std::path::Path::new(path).exists() {
            return Config::load_or_default(path).dune.dune_api_key;
        }
    }
    None
}

/// Test Dune API connectivity with a simple raw SQL query.
/// Run with: DUNE_API_KEY=<key> cargo test --test dune_api -- test_dune_raw_sql --nocapture
/// Or place key in mev-scout.toml and run without env var.
#[tokio::test]
async fn test_dune_raw_sql() {
    let api_key = std::env::var("DUNE_API_KEY")
        .ok()
        .or_else(api_key_from_config)
        .unwrap_or_else(|| {
            eprintln!("SKIP: set DUNE_API_KEY or dune_api_key in mev-scout.toml");
            return String::new();
        });
    if api_key.is_empty() {
        return;
    }
    let client = DuneClient::new(&api_key);
    let result = client.execute_raw_sql("SELECT 1 AS n, 'hello' AS msg").await;
    match &result {
        Ok(r) => {
            println!("raw SQL OK  state={}  query_id={:?}", r.state, r.query_id);
            if let Some(ref res) = r.result {
                println!("columns: {:?}", res.metadata.column_names);
                println!("types: {:?}", res.metadata.column_types);
                for row in &res.rows {
                    println!("row: {:?}", row);
                }
            }
        }
        Err(e) => println!("FAILED: {:#}", e),
    }
    if result.is_ok() {
        println!("Dune raw SQL WORKS!");
    }
}
