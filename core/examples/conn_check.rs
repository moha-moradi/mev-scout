//! Scratch: reproduce the corpus's RPC transport against a given endpoint.
#![allow(dead_code)]
use std::net::{IpAddr, Ipv4Addr};
#[tokio::main]
async fn main() {
    let url = std::env::args().nth(1).unwrap_or_default();
    let batch = std::env::args()
        .nth(2)
        .map(|s| s == "batch")
        .unwrap_or(false);
    let mut builder = reqwest::Client::builder().gzip(true);
    if std::env::var("MEV_SCOUT_FORCE_IPV4").as_deref() == Ok("1") {
        builder = builder.local_address(IpAddr::V4(Ipv4Addr::UNSPECIFIED));
    }
    let client = builder.build().expect("build");
    let single =
        r#"{"jsonrpc":"2.0","id":1,"method":"eth_getBlockByNumber","params":["0x5b3fcba",false]}"#;
    let receipt =
        r#"{"jsonrpc":"2.0","id":2,"method":"eth_getBlockReceipts","params":["0x5b3fcba"]}"#;
    let payload = if batch {
        format!("[{single},{receipt}]")
    } else {
        single.to_string()
    };
    match client
        .post(&url)
        .header("content-type", "application/json")
        .body(payload)
        .send()
        .await
    {
        Ok(r) => {
            let status = r.status();
            match r.text().await {
                Ok(t) => println!(
                    "OK {status} len={} head={}",
                    t.len(),
                    t.chars().take(120).collect::<String>()
                ),
                Err(e) => println!("read err: {e}"),
            }
        }
        Err(e) => println!("send failed: {e:#?}"),
    }
}
