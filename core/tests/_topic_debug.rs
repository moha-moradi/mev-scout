use alloy::primitives::keccak256;

#[test]
fn print_topics_for_bsc_hunt() {
    let aake = keccak256(b"LiquidationCall(address,address,address,uint256,uint256,address,bool)");
    let liqate = keccak256(b"LiquidateBorrow(address,address,uint256,address,uint256)");
    println!("AAVE_LIQ_TOPIC=0x{:x}", aake);
    println!("LIQATE_BORROW_TOPIC=0x{:x}", liqate);
    let v = keccak256(b"getVersion()");
    println!("SEL_GET_VERSION=0x{:x}", v[0] as u32);
    let v2 = keccak256(b"liquidate(address,address,uint256,bool)");
    println!("SEL_LIQATE_V2=0x{:x}", v2[0] as u32);
}
