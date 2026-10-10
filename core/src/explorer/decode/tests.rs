//! Decode unit tests (moved from monolithic decode.rs).
use super::*;
use alloy::primitives::{address, b256, Address, B256, U256};

use crate::chain::events::{
    AAVE_V2_FLASH_LOAN_TOPIC, AAVE_V3_FLASH_LOAN_TOPIC, AAVE_V3_LIQUIDATION_CALL_TOPIC,
    COMPOUND_V2_LIQUIDATE_BORROW_TOPIC, TRANSFER_TOPIC, V2_SWAP_TOPIC, V3_SWAP_TOPIC,
};
use crate::data::LogData;
use crate::explorer::types::{Amm, LegSource, SwapFact, TransferFact};
use crate::pool::decoders::{LB_DEPOSITED_TO_BINS_TOPIC, SOLIDLY_SWAP_TOPIC, V3_BURN_TOPIC, V3_MINT_TOPIC};


fn log(address: Address, topics: Vec<B256>, data: Vec<u8>) -> LogData {
    LogData {
        address,
        topics,
        data: alloy::primitives::Bytes::from(data),
    }
}

fn word(v: u128) -> Vec<u8> {
    let mut b = [0u8; 32];
    b[16..].copy_from_slice(&v.to_be_bytes());
    b.to_vec()
}

fn hexdata(s: &str) -> Vec<u8> {
    alloy::primitives::hex::decode(s).expect("valid hex")
}

#[test]
fn v3_mint_decodes_canonical_indexed_ticks() {
    // Real mainnet log: UniswapV3 USDC/WETH 0.05% Mint in block 26051637.
    // topics: [sig, owner, tickLower, tickUpper]
    // data: [sender, amount, amount0, amount1]
    let l = log(
        address!("88e6a0c2ddd26feeb64f039a2c41296fcb3f5640"),
        vec![
            V3_MINT_TOPIC,
            b256!("000000000000000000000000c36442b4a4522e871399cd717abdd847ab11fe88"),
            b256!("0000000000000000000000000000000000000000000000000000000000030246"),
            b256!("00000000000000000000000000000000000000000000000000000000000303f4"),
        ],
        hexdata(concat!(
            "000000000000000000000000c36442b4a4522e871399cd717abdd847ab11fe88",
            "0000000000000000000000000000000000000000000000000004ca38c9eecc93",
            "000000000000000000000000000000000000000000000000000000002d5b38bf",
            "00000000000000000000000000000000000000000000000003c9c2b775e90ef4",
        )),
    );
    let f = decode_v3_mint_burn(&l).unwrap();
    assert!(f.is_mint);
    assert_eq!(
        f.owner,
        address!("c36442b4a4522e871399cd717abdd847ab11fe88")
    );
    assert_eq!(f.tick_lower, 197190);
    assert_eq!(f.tick_upper, 197620);
    assert_eq!(f.liquidity, 0x4ca38c9eecc93);
    assert_eq!(f.amount0, U256::from(0x2d5b38bfu64));
    assert_eq!(f.amount1, U256::from(0x3c9c2b775e90ef4u64));
}

#[test]
fn v3_burn_decodes_canonical_indexed_ticks() {
    // Real mainnet log: UniswapV3 USDC/WETH 0.05% Burn in block 26051637.
    // topics: [sig, owner, tickLower, tickUpper]; data: [amount, amount0, amount1]
    let l = log(
        address!("88e6a0c2ddd26feeb64f039a2c41296fcb3f5640"),
        vec![
            V3_BURN_TOPIC,
            b256!("000000000000000000000000c36442b4a4522e871399cd717abdd847ab11fe88"),
            b256!("000000000000000000000000000000000000000000000000000000000003011a"),
            b256!("00000000000000000000000000000000000000000000000000000000000302b4"),
        ],
        hexdata(concat!(
            "000000000000000000000000000000000000000000000000000503364091d046",
            "0000000000000000000000000000000000000000000000000000000000000000",
            "00000000000000000000000000000000000000000000000007a45af71b4f9a78",
        )),
    );
    let f = decode_v3_mint_burn(&l).unwrap();
    assert!(!f.is_mint);
    assert_eq!(
        f.owner,
        address!("c36442b4a4522e871399cd717abdd847ab11fe88")
    );
    assert_eq!(f.tick_lower, 196890);
    assert_eq!(f.tick_upper, 197300);
    assert_eq!(f.liquidity, 0x0503364091d046);
    assert_eq!(f.amount0, U256::ZERO);
    assert_eq!(f.amount1, U256::from(0x7a45af71b4f9a78u64));
}

#[test]
fn v3_mint_and_burn_ticks_come_from_topics_not_data() {
    // Both real logs above are in block 26051637 for the same owner but carry
    // different tick ranges. Reading ticks out of the data payload (the old
    // behaviour) produced identical garbage for both, which would have paired
    // them as a false JIT candidate.
    let mint = log(
        address!("88e6a0c2ddd26feeb64f039a2c41296fcb3f5640"),
        vec![
            V3_MINT_TOPIC,
            b256!("000000000000000000000000c36442b4a4522e871399cd717abdd847ab11fe88"),
            b256!("0000000000000000000000000000000000000000000000000000000000030246"),
            b256!("00000000000000000000000000000000000000000000000000000000000303f4"),
        ],
        hexdata(concat!(
            "000000000000000000000000c36442b4a4522e871399cd717abdd847ab11fe88",
            "0000000000000000000000000000000000000000000000000004ca38c9eecc93",
            "000000000000000000000000000000000000000000000000000000002d5b38bf",
            "00000000000000000000000000000000000000000000000003c9c2b775e90ef4",
        )),
    );
    let burn = log(
        address!("88e6a0c2ddd26feeb64f039a2c41296fcb3f5640"),
        vec![
            V3_BURN_TOPIC,
            b256!("000000000000000000000000c36442b4a4522e871399cd717abdd847ab11fe88"),
            b256!("000000000000000000000000000000000000000000000000000000000003011a"),
            b256!("00000000000000000000000000000000000000000000000000000000000302b4"),
        ],
        hexdata(concat!(
            "000000000000000000000000000000000000000000000000000503364091d046",
            "0000000000000000000000000000000000000000000000000000000000000000",
            "00000000000000000000000000000000000000000000000007a45af71b4f9a78",
        )),
    );
    let m = decode_v3_mint_burn(&mint).unwrap();
    let b = decode_v3_mint_burn(&burn).unwrap();
    assert_eq!(m.owner, b.owner);
    assert_ne!((m.tick_lower, m.tick_upper), (b.tick_lower, b.tick_upper));
}

#[test]
fn v3_mint_sign_extends_negative_ticks_from_topics() {
    let l = log(
        address!("88e6a0c2ddd26feeb64f039a2c41296fcb3f5640"),
        vec![
            V3_MINT_TOPIC,
            b256!("0000000000000000000000000000000000000000000000000000000000000001"),
            b256!("0000000000000000000000000000000000000000000000000000000000ffffff"),
            b256!("0000000000000000000000000000000000000000000000000000000000f27618"),
        ],
        hexdata(concat!(
            "0000000000000000000000000000000000000000000000000000000000000001",
            "0000000000000000000000000000000000000000000000000000000000000001",
            "0000000000000000000000000000000000000000000000000000000000000001",
            "0000000000000000000000000000000000000000000000000000000000000001",
        )),
    );
    let f = decode_v3_mint_burn(&l).unwrap();
    assert_eq!(f.tick_lower, -1);
    assert_eq!(f.tick_upper, -887272);
}

#[test]
fn v3_mint_rejects_non_canonical_topic_layout() {
    // `IUniswapV3PoolEvents` indexes owner, tickLower and tickUpper, so a
    // 2-topic log is not a V3 Mint. It must be rejected rather than decoded
    // from the data payload, which would produce invented tick values.
    let l = log(
        address!("88e6a0c2ddd26feeb64f039a2c41296fcb3f5640"),
        vec![
            V3_MINT_TOPIC,
            b256!("0000000000000000000000000000000000000000000000000000000000000001"),
        ],
        hexdata(concat!(
            "0000000000000000000000000000000000000000000000000000000000000009",
            "0000000000000000000000000000000000000000000000000000000000030246",
            "00000000000000000000000000000000000000000000000000000000000303f4",
            "0000000000000000000000000000000000000000000000000004ca38c9eecc93",
            "000000000000000000000000000000000000000000000000000000002d5b38bf",
            "00000000000000000000000000000000000000000000000003c9c2b775e90ef4",
        )),
    );
    assert!(decode_v3_mint_burn(&l).is_none());
}

#[test]
fn v3_mint_and_burn_reject_truncated_data() {
    let pool = address!("88e6a0c2ddd26feeb64f039a2c41296fcb3f5640");
    let owner = b256!("0000000000000000000000000000000000000000000000000000000000000001");
    // Mint needs 128 bytes; Burn needs 96. Shorter payloads must be rejected.
    let cases: &[(&str, B256, B256, B256, usize)] = &[
        (
            "mint",
            V3_MINT_TOPIC,
            b256!("0000000000000000000000000000000000000000000000000000000000030246"),
            b256!("00000000000000000000000000000000000000000000000000000000000303f4"),
            3,
        ),
        (
            "burn",
            V3_BURN_TOPIC,
            b256!("000000000000000000000000000000000000000000000000000000000003011a"),
            b256!("00000000000000000000000000000000000000000000000000000000000302b4"),
            2,
        ),
    ];
    for (name, topic, tick_lo, tick_hi, words) in cases {
        let data = std::iter::repeat_n(word(0), *words)
            .flatten()
            .collect::<Vec<_>>();
        let l = log(pool, vec![*topic, owner, *tick_lo, *tick_hi], data);
        assert!(decode_v3_mint_burn(&l).is_none(), "{name}");
    }
}

#[test]
fn transfer_decodes() {
    let l = log(
        address!("a0b86991c6218b36c1d19d4a2e9eb0ce3606eb48"),
        vec![
            TRANSFER_TOPIC,
            b256!("0000000000000000000000000000000000000000000000000000000000000001"),
            b256!("0000000000000000000000000000000000000000000000000000000000000002"),
        ],
        {
            let mut b = vec![0u8; 32];
            b[31] = 42;
            b
        },
    );
    let t = decode_transfer(&l).unwrap();
    assert_eq!(
        t.token,
        address!("a0b86991c6218b36c1d19d4a2e9eb0ce3606eb48")
    );
    assert_eq!(t.amount, U256::from(42));
}

#[test]
fn v3_swap_decodes_direction() {
    let pool = address!("1111111111111111111111111111111111111111");
    // amount0 = -100 (pool received), amount1 = +90 (pool paid out)
    let mut data = vec![0u8; 160];
    for b in data[0..24].iter_mut() {
        *b = 0xff;
    }
    data[24..32].copy_from_slice(&(-100i64).to_be_bytes());
    data[56..64].copy_from_slice(&90u64.to_be_bytes());
    let l = log(pool, vec![V3_SWAP_TOPIC, B256::ZERO, B256::ZERO], data);
    let (amm, s) = decode_swap(&l).unwrap();
    assert_eq!(amm, Amm::V3);
    assert_eq!(s.pool, pool);
    assert_eq!(s.amount_in, U256::from(100));
    assert_eq!(s.amount_out, U256::from(90));
    assert_eq!(s.token_in, TOKEN0_SENTINEL);
}

#[test]
fn aave_flash_loans_decode() {
    let target = b256!("0000000000000000000000001111111111111111111111111111111111111111");
    let asset = b256!("0000000000000000000000002222222222222222222222222222222222222222");
    let initiator = address!("3333333333333333333333333333333333333333");
    let mut data = vec![0u8; 160];
    data[12..32].copy_from_slice(initiator.as_slice());
    data[56..64].copy_from_slice(&1_000u64.to_be_bytes());
    data[64 + 31] = 1; // interestRateMode
    data[96 + 24..96 + 32].copy_from_slice(&5u64.to_be_bytes()); // premium
    let l = log(
        address!("4444444444444444444444444444444444444444"),
        vec![*AAVE_V3_FLASH_LOAN_TOPIC, target, asset],
        data,
    );
    let f = decode_flash_loan(&l).unwrap();
    assert_eq!(f.protocol, "aave_v3");
    assert_eq!(
        f.token,
        address!("2222222222222222222222222222222222222222")
    );
    assert_eq!(f.amount, U256::from(1_000));
    assert_eq!(f.fee, Some(U256::from(5)));
    assert_eq!(f.initiator, initiator);

    let initiator_topic =
        b256!("0000000000000000000000003333333333333333333333333333333333333333");
    let asset = b256!("0000000000000000000000002222222222222222222222222222222222222222");
    let mut data = vec![0u8; 96];
    data[24..32].copy_from_slice(&3_000u64.to_be_bytes());
    data[56..64].copy_from_slice(&7u64.to_be_bytes());
    let l = log(
        address!("4444444444444444444444444444444444444444"),
        vec![*AAVE_V2_FLASH_LOAN_TOPIC, target, initiator_topic, asset],
        data,
    );
    let f = decode_flash_loan(&l).unwrap();
    assert_eq!(f.protocol, "aave_v2");
    assert_eq!(f.amount, U256::from(3_000));
    assert_eq!(f.fee, Some(U256::from(7)));
}

#[test]
fn liquidation_aave_v3_decodes() {
    let collateral = b256!("000000000000000000000000bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
    let debt = b256!("000000000000000000000000cccccccccccccccccccccccccccccccccccccccc");
    let user = b256!("000000000000000000000000dddddddddddddddddddddddddddddddddddddddd");
    let mut data = vec![0u8; 64];
    data[24..32].copy_from_slice(&99u64.to_be_bytes()); // debtToCover
    data[56..64].copy_from_slice(&500u64.to_be_bytes()); // liquidatedCollateralAmount
    let l = log(
        address!("794a61358d6845594f94dc1db02a252b5b4814ad"),
        vec![*AAVE_V3_LIQUIDATION_CALL_TOPIC, collateral, debt, user],
        data,
    );
    let liq = decode_liquidation(&l).unwrap();
    assert_eq!(liq.protocol, "aave_v3");
    assert_eq!(
        liq.user,
        address!("dddddddddddddddddddddddddddddddddddddddd")
    );
    // liquidator is not in the event â€” resolved to tx.from at classify time
    assert_eq!(
        liq.liquidator,
        address!("0000000000000000000000000000000000000000")
    );
    assert_eq!(liq.debt_to_cover, U256::from(99));
    assert_eq!(liq.collateral_amount, U256::from(500));
}

#[test]
fn liquidation_compound_v2_decodes() {
    let liquidator = b256!("000000000000000000000000aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
    let borrower = b256!("000000000000000000000000bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
    let mut data = vec![0u8; 96];
    data[24..32].copy_from_slice(&99u64.to_be_bytes()); // repayAmount
    data[44..64]
        .copy_from_slice(address!("cccccccccccccccccccccccccccccccccccccccc").as_slice()); // cTokenCollateral
    data[88..96].copy_from_slice(&500u64.to_be_bytes()); // seizeTokens
    let l = log(
        address!("dddddddddddddddddddddddddddddddddddddddd"),
        vec![*COMPOUND_V2_LIQUIDATE_BORROW_TOPIC, liquidator, borrower],
        data,
    );
    let liq = decode_liquidation(&l).unwrap();
    assert_eq!(liq.protocol, "compound_v2");
    assert_eq!(
        liq.liquidator,
        address!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
    );
    assert_eq!(
        liq.user,
        address!("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb")
    );
    assert_eq!(
        liq.debt_asset,
        address!("dddddddddddddddddddddddddddddddddddddddd")
    );
    assert_eq!(
        liq.collateral_asset,
        address!("cccccccccccccccccccccccccccccccccccccccc")
    );
    assert_eq!(liq.debt_to_cover, U256::from(99));
    assert_eq!(liq.collateral_amount, U256::from(500));
}

/// P0.2 positive (آ§17.8.4 mode A / آ§24): Compound V2 `LiquidateBorrow`
/// emitted by a Benqi qiToken market shares topic0 with Compound, so only
/// the emitter address identifies the protocol â€” the alias registry
/// relabels it to `benqi`. Amounts (the `O` P&L inputs) are untouched.
#[test]
fn liquidation_benqi_market_relabels_to_benqi() {
    let liquidator = b256!("000000000000000000000000aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
    let borrower = b256!("000000000000000000000000bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
    let mut data = vec![0u8; 96];
    data[24..32].copy_from_slice(&99u64.to_be_bytes()); // repayAmount
    data[44..64]
        .copy_from_slice(address!("cccccccccccccccccccccccccccccccccccccccc").as_slice()); // cTokenCollateral
    data[88..96].copy_from_slice(&500u64.to_be_bytes()); // seizeTokens
                                                         // qiAVAX â€” Benqi core market on Avalanche (43114).
    let l = log(
        address!("5c0401e81bc07ca70fad469b451682c0d747ef1c"),
        vec![*COMPOUND_V2_LIQUIDATE_BORROW_TOPIC, liquidator, borrower],
        data,
    );
    let mut liq = decode_liquidation(&l).unwrap();
    assert_eq!(liq.protocol, "compound_v2"); // shared topic0 pre-relabel
    let aliases = std::collections::HashMap::from([(l.address, "benqi")]);
    remap_liquidation_protocol(&mut liq, &aliases);
    assert_eq!(liq.protocol, "benqi");
    assert_eq!(liq.debt_to_cover, U256::from(99));
    assert_eq!(liq.collateral_amount, U256::from(500));
}

/// P0.2 negative: the same event from an emitter that is not in the
/// registry keeps the generic `compound_v2` label.
#[test]
fn liquidation_unaliased_emitter_keeps_compound_v2() {
    let liquidator = b256!("000000000000000000000000aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
    let borrower = b256!("000000000000000000000000bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
    let mut data = vec![0u8; 96];
    data[24..32].copy_from_slice(&99u64.to_be_bytes());
    data[44..64]
        .copy_from_slice(address!("cccccccccccccccccccccccccccccccccccccccc").as_slice());
    data[88..96].copy_from_slice(&500u64.to_be_bytes());
    let l = log(
        address!("dddddddddddddddddddddddddddddddddddddddd"),
        vec![*COMPOUND_V2_LIQUIDATE_BORROW_TOPIC, liquidator, borrower],
        data,
    );
    let mut liq = decode_liquidation(&l).unwrap();
    let aliases = std::collections::HashMap::from([(
        address!("5c0401e81bc07ca70fad469b451682c0d747ef1c"),
        "benqi",
    )]);
    remap_liquidation_protocol(&mut liq, &aliases);
    assert_eq!(liq.protocol, "compound_v2");
}

/// P1.4: Chainlink AnswerUpdated decodes to the feed emitter.
#[test]
fn decode_chainlink_answer_updated() {
    use crate::chain::events::CHAINLINK_ANSWER_UPDATED_TOPIC;
    let feed = address!("0a77230d17318075983913bc2145db16c7366156");
    let l = log(
        feed,
        vec![
            *CHAINLINK_ANSWER_UPDATED_TOPIC,
            b256!("0000000000000000000000000000000000000000000000000000000000000064"),
            b256!("0000000000000000000000000000000000000000000000000000000000000001"),
        ],
        vec![0u8; 32],
    );
    let o = decode_oracle_update(&l).unwrap();
    assert_eq!(o.feed, feed);
}

/// P1.5: Gelato ExecSuccess carries fee + feeToken (basis F inputs).
#[test]
fn decode_gelato_exec_success_fee() {
    use crate::chain::events::GELATO_EXEC_SUCCESS_TOPIC;
    let mut data = vec![0u8; 192];
    data[24..32].copy_from_slice(&42u64.to_be_bytes()); // txFee
    data[44..64]
        .copy_from_slice(address!("4000000000000000000000000000000000000005").as_slice());
    let l = log(
        address!("cccccccccccccccccccccccccccccccccccccccc"),
        vec![*GELATO_EXEC_SUCCESS_TOPIC],
        data,
    );
    let k = decode_keeper(&l).unwrap();
    assert_eq!(k.protocol, "gelato");
    assert_eq!(k.fee, Some(U256::from(42)));
    assert_eq!(
        k.fee_token,
        Some(address!("4000000000000000000000000000000000000005"))
    );
}

#[test]
fn attach_tokens_pairs_transfers() {
    let pool = address!("1000000000000000000000000000000000000000");
    let tin = address!("2000000000000000000000000000000000000000");
    let tout = address!("3000000000000000000000000000000000000000");
    let mk = |li: u64, token: Address, from: Address, to: Address| TransferFact {
        tx_index: 0,
        log_index: li,
        token,
        from,
        to,
        amount: U256::from(1),
    };
    let transfers = vec![
        mk(
            0,
            tin,
            address!("4000000000000000000000000000000000000000"),
            pool,
        ),
        mk(
            2,
            tout,
            pool,
            address!("4000000000000000000000000000000000000000"),
        ),
    ];
    let (_, mut s) = decode_swap(&log(pool, vec![V2_SWAP_TOPIC, B256::ZERO, B256::ZERO], {
        let mut d = vec![0u8; 128];
        d[31] = 5; // amount0In
        d[64 + 31] = 4; // amount0Out
        d
    }))
    .unwrap();
    s.log_index = 1;
    attach_swap_tokens(
        std::slice::from_mut(&mut s),
        &transfers,
        &std::collections::HashMap::new(),
    );
    assert_eq!(s.token_in, tin);
    assert_eq!(s.token_out, tout);
    assert_eq!(s.token_source, LegSource::Transfer);
    // Flow ownership (آ§7.1): the funder of the input leg is the `from` of
    // the nearest inbound transfer to the pool before the swap log.
    assert_eq!(
        s.owner,
        Some(address!("4000000000000000000000000000000000000000"))
    );
}

#[test]
fn registry_resolves_sentinel_direction_without_transfers() {
    // V2 swap where amount0In > 0 => token0 in. Registry resolves without
    // any transfer-pairing hints.
    let pool = address!("1000000000000000000000000000000000000000");
    let t0 = address!("2000000000000000000000000000000000000000");
    let t1 = address!("3000000000000000000000000000000000000000");
    let (_, mut s) = decode_swap(&log(pool, vec![V2_SWAP_TOPIC, B256::ZERO, B256::ZERO], {
        let mut d = vec![0u8; 128];
        d[31] = 5; // amount0In
        d[64 + 31] = 4; // amount0Out
        d
    }))
    .unwrap();
    s.log_index = 1;
    let mut pools = std::collections::HashMap::new();
    pools.insert(pool, (t0, t1));
    attach_swap_tokens(std::slice::from_mut(&mut s), &[], &pools);
    assert_eq!(s.token_in, t0);
    assert_eq!(s.token_out, t1);
    assert_eq!(s.token_source, LegSource::Registry);

    // V3 token1-in sentinel: registry resolves token_in = t1, token_out = t0.
    let mut data = vec![0u8; 160];
    for b in data[0..24].iter_mut() {
        *b = 0xff; // amount0 negative
    }
    data[24..32].copy_from_slice(&(-100i64).to_be_bytes());
    data[56..64].copy_from_slice(&90u64.to_be_bytes());
    let (_, mut s3) = decode_swap(&log(
        pool,
        vec![V3_SWAP_TOPIC, B256::ZERO, B256::ZERO],
        data,
    ))
    .unwrap();
    s3.log_index = 0;
    attach_swap_tokens(std::slice::from_mut(&mut s3), &[], &pools);
    assert_eq!(s3.token_in, t0); // amount0 < 0 => token0 in
    assert_eq!(s3.token_out, t1);
    assert_eq!(s3.token_source, LegSource::Registry);
}

#[test]
fn proximity_window_marks_leg_untrusted() {
    let pool = address!("1000000000000000000000000000000000000000");
    let tin = address!("2000000000000000000000000000000000000000");
    let tout = address!("3000000000000000000000000000000000000000");
    let who = address!("4000000000000000000000000000000000000000");
    let mk = |li: u64, token: Address, from: Address, to: Address| TransferFact {
        tx_index: 0,
        log_index: li,
        token,
        from,
        to,
        amount: U256::from(1),
    };
    // Outflow is before the swap, so the strict after-leg misses it and the
    // آ±24-log window binds token_out. That taints the whole leg.
    let transfers = vec![mk(8, tout, pool, who), mk(9, tin, who, pool)];
    let (_, mut s) = decode_swap(&log(pool, vec![V2_SWAP_TOPIC, B256::ZERO, B256::ZERO], {
        let mut d = vec![0u8; 128];
        d[31] = 5; // amount0In
        d[96 + 31] = 4; // amount1Out, opposite side
        d
    }))
    .unwrap();
    s.log_index = 10;
    attach_swap_tokens(
        std::slice::from_mut(&mut s),
        &transfers,
        &std::collections::HashMap::new(),
    );
    assert_eq!(s.token_in, tin);
    assert_eq!(s.token_out, tout);
    assert_eq!(s.token_source, LegSource::Proximity);
}

#[test]
fn v2_and_solidly_derive_both_sides_from_one_direction() {
    let pool = address!("1000000000000000000000000000000000000000");
    // Both inputs non-zero. token0 in (10 >= 4) so the output is amount1Out,
    // not max(amount0Out, amount1Out) = 100.
    let mut data = vec![0u8; 128];
    data[31] = 10; // amount0In
    data[63] = 4; // amount1In
    data[95] = 100; // amount0Out
    data[127] = 7; // amount1Out
    for topic in [V2_SWAP_TOPIC, *SOLIDLY_SWAP_TOPIC] {
        let (_, s) = decode_swap(&log(
            pool,
            vec![topic, B256::ZERO, B256::ZERO],
            data.clone(),
        ))
        .unwrap();
        assert_eq!(s.amount_in, U256::from(10));
        assert_eq!(s.amount_out, U256::from(7));
        assert_eq!(s.token_in, TOKEN0_SENTINEL);
        assert_eq!(s.token_out, TOKEN1_SENTINEL);
    }
}

fn topic_addr(a: Address) -> B256 {
    let mut b = [0u8; 32];
    b[12..].copy_from_slice(a.as_slice());
    B256::from(b)
}

#[test]
fn oneinch_swapped_decodes() {
    let router = address!("1111111254fb6c44bac0bed2854e76f90643097d");
    let src = address!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
    let dst = address!("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
    let mut data = vec![0u8; 192];
    data[32 + 12..32 + 32].copy_from_slice(src.as_slice()); // srcToken
    data[64 + 12..64 + 32].copy_from_slice(dst.as_slice()); // dstToken
    data[128 + 24..160].copy_from_slice(&1000u64.to_be_bytes()); // spentAmount
    data[160 + 24..192].copy_from_slice(&990u64.to_be_bytes()); // returnAmount
    let (amm, s) = decode_swap(&log(router, vec![ONEINCH_SWAPPED_TOPIC], data)).unwrap();
    assert_eq!(amm, Amm::Aggregator);
    assert_eq!(s.pool, router);
    assert_eq!(s.token_in, src);
    assert_eq!(s.token_out, dst);
    assert_eq!(s.amount_in, U256::from(1000));
    assert_eq!(s.amount_out, U256::from(990));
}

#[test]
fn paraswap_swapped_variants_decode() {
    let router = address!("def171fe48cf0115b1d80b88dc8eab59176fee57");
    let src = address!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
    let dst = address!("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
    let beneficiary = address!("cccccccccccccccccccccccccccccccccccccccc");
    // (layout, topic, data words, srcAmount word, receivedAmount word, in, out)
    let cases = [
        (
            PARASWAP_SWAPPED_TOPIC,
            160usize,
            32usize,
            64usize,
            5000u64,
            4900u64,
        ),
        (PARASWAP_SWAPPED_V3_TOPIC, 224, 128, 160, 7000, 6900),
    ];
    for (topic, len, src_at, dst_at, amount_in, amount_out) in cases {
        let mut data = vec![0u8; len];
        data[src_at + 24..src_at + 32].copy_from_slice(&amount_in.to_be_bytes());
        data[dst_at + 24..dst_at + 32].copy_from_slice(&amount_out.to_be_bytes());
        let (amm, s) = decode_swap(&log(
            router,
            vec![
                topic,
                topic_addr(beneficiary),
                topic_addr(src),
                topic_addr(dst),
            ],
            data,
        ))
        .unwrap();
        assert_eq!(amm, Amm::Aggregator);
        assert_eq!(s.token_in, src);
        assert_eq!(s.token_out, dst);
        assert_eq!(s.amount_in, U256::from(amount_in));
        assert_eq!(s.amount_out, U256::from(amount_out));
    }
}

#[test]
fn zrx_fill_decodes_amounts_tokens_unresolved() {
    let exchange = address!("4f833a24e1f95d70f837921e96e4e6def2ad0bee");
    let mut data = vec![0u8; 352];
    data[192 + 24..224].copy_from_slice(&999u64.to_be_bytes()); // makerAssetFilled
    data[224 + 24..256].copy_from_slice(&100_000u64.to_be_bytes()); // takerAssetFilled
    let (amm, s) = decode_swap(&log(
        exchange,
        vec![
            ZRX_FILL_TOPIC,
            topic_addr(address!("1250a4395798a18a48c6118a7f8dff8e8479c29a")),
            topic_addr(address!("eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee")),
            alloy::primitives::b256!(
                "1111111111111111111111111111111111111111111111111111111111111111"
            ),
        ],
        data,
    ))
    .unwrap();
    assert_eq!(amm, Amm::Aggregator);
    assert_eq!(s.pool, exchange);
    // Tokens are inside dynamic asset blobs; direction is resolved later.
    assert!(s.token_in.is_zero());
    assert!(s.token_out.is_zero());
    assert_eq!(s.amount_in, U256::from(100_000)); // taker-side input
    assert_eq!(s.amount_out, U256::from(999)); // maker-side output
}

#[test]
fn aggregator_dup_edge_removed_when_dex_pair_covers() {
    let src = address!("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
    let dst = address!("bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
    let router = address!("1111111254fb6c44bac0bed2854e76f90643097d");
    let pool = address!("3000000000000000000000000000000000000003");
    let pair =
        |amm: Amm, pool: Address, tin: Address, tout: Address, ain: u64, aout: u64| SwapFact {
            tx_index: 0,
            log_index: 0,
            pool,
            amm,
            token_in: tin,
            token_out: tout,
            token_source: LegSource::Registry,
            amount_in: U256::from(ain),
            amount_out: U256::from(aout),
            tick: None,
            owner: None,
        };
    // Same Aâ†’B flow on a single pool: the DEX edge wins.
    let mut edges = vec![
        pair(Amm::Aggregator, router, src, dst, 1000, 990),
        pair(Amm::V2, pool, src, dst, 1000, 990),
    ];
    dedup_aggregator_facts(&mut edges);
    assert_eq!(edges.len(), 1);
    assert_eq!(edges[0].amm, Amm::V2);

    // Multi-hop Aâ†’Xâ†’B covers an Aâ†’B aggregator edge (2-hop chain).
    let x = address!("cccccccccccccccccccccccccccccccccccccccc");
    let mut edges = vec![
        pair(Amm::Aggregator, router, src, dst, 1000, 980),
        pair(Amm::V2, pool, src, x, 1000, 990),
        pair(Amm::V3, pool, x, dst, 990, 980),
    ];
    dedup_aggregator_facts(&mut edges);
    assert_eq!(edges.len(), 2);
    assert!(!edges.iter().any(|s| s.amm == Amm::Aggregator));

    // Unrelated aggregator flow (no DEX chain) is kept.
    let mut edges = vec![pair(Amm::Aggregator, router, src, dst, 1000, 990)];
    dedup_aggregator_facts(&mut edges);
    assert_eq!(edges.len(), 1);
}

#[test]
fn lb_deposited_to_bins_decodes() {
    let pool = address!("1111111111111111111111111111111111111111");
    let to = address!("2222222222222222222222222222222222222222");
    // ABI: offset ids=0x40, offset amounts=0x80, len=1, id=100, len=1, packed xy
    let mut data = vec![0u8; 192];
    data[31] = 0x40; // ids offset
    data[63] = 0x80; // amounts offset
    data[95] = 1; // ids len
    data[127] = 100; // bin id 100
    data[159] = 1; // amounts len
                   // packed: amountY high 16 bytes = 7, amountX low 16 bytes = 9
    data[175] = 7;
    data[191] = 9;
    let j = decode_lb_bins_liquidity(&log(
        pool,
        vec![
            *LB_DEPOSITED_TO_BINS_TOPIC,
            topic_addr(address!("3333333333333333333333333333333333333333")),
            topic_addr(to),
        ],
        data,
    ))
    .unwrap();
    assert!(j.is_mint);
    assert!(j.bin_amm);
    assert_eq!(j.owner, to);
    assert_eq!(j.tick_lower, 100);
    assert_eq!(j.tick_upper, 100);
    assert_eq!(j.liquidity, 1);
    assert_eq!(j.amount0, U256::from(9));
    assert_eq!(j.amount1, U256::from(7));
}
