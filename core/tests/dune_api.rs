use mev_scout_core::dune::client::DuneClient;

/// Test Dune API connectivity with a simple raw SQL query.
/// Run with: DUNE_API_KEY=<key> cargo test --test dune_api -- test_dune_raw_sql --nocapture
#[tokio::test]
async fn test_dune_raw_sql() {
    let api_key = match std::env::var("DUNE_API_KEY") {
        Ok(k) => k,
        Err(_) => {
            eprintln!("SKIP: set DUNE_API_KEY env var to run this test");
            return;
        }
    };
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
