use mev_scout_core::dune::client::DuneClient;

#[tokio::test]
async fn test_dune_raw_sql() {
    let client = DuneClient::new("OKfH0tGoc9OVYWOXIaE7yr2jpbEOsEy0");
    let result = client.execute_raw_sql("SELECT 1 AS n").await;
    match &result {
        Ok(r) => println!("Dune raw SQL OK: state={}, query_id={:?}", r.state, r.query_id),
        Err(e) => println!("Dune raw SQL FAILED: {:#}", e),
    }
    assert!(result.is_ok(), "Dune raw SQL should succeed");
}
