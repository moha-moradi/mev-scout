use std::collections::HashMap;
use std::sync::OnceLock;

use alloy::primitives::{address, Address};

use crate::pool::state::{PoolManager, PoolState};

struct TokenInfo {
    decimals: u8,
    usd_price: f64,
}

fn token_info_map() -> &'static HashMap<Address, TokenInfo> {
    static MAP: OnceLock<HashMap<Address, TokenInfo>> = OnceLock::new();
    MAP.get_or_init(|| {
        let mut m = HashMap::new();

        m.insert(address!("0d500b1d8e8ef31e21c99d1db9a6444d3adf1270"), TokenInfo { decimals: 18, usd_price: 0.50 });  // WMATIC
        m.insert(address!("2791bca1f2de4661ed88a30c99a7a9449aa84174"), TokenInfo { decimals: 6, usd_price: 1.00 });   // USDC
        m.insert(address!("c2132d05d31c914a87c6611c10748aeb04b58e8f"), TokenInfo { decimals: 6, usd_price: 1.00 });   // USDT
        m.insert(address!("7ceb23fd6bc0add59e62ac25578270cff1b9f619"), TokenInfo { decimals: 18, usd_price: 3500.00 }); // WETH
        m.insert(address!("1bfd67037b42cf73acf2047067bd4f2c47d9bfd6"), TokenInfo { decimals: 8, usd_price: 65000.00 }); // WBTC
        m.insert(address!("8f3cf7ad23cd3cadbd9735aff958023239c6a063"), TokenInfo { decimals: 18, usd_price: 1.00 });   // DAI
        m.insert(address!("53e0bca35ec356bd5dddfebbd1fc0fd03fabad39"), TokenInfo { decimals: 18, usd_price: 14.00 });  // LINK
        m.insert(address!("172370d5cd63279efa6d502dab29171933a610af"), TokenInfo { decimals: 18, usd_price: 0.30 });   // CRV
        m.insert(address!("d6df932a45c0f255f85145f286ea0b292b21c90b"), TokenInfo { decimals: 18, usd_price: 90.00 });  // AAVE
        m.insert(address!("45c32fa6df82ead1e2ef74d17b76547eddfaff89"), TokenInfo { decimals: 18, usd_price: 1.00 });   // FRAX
        m.insert(address!("9a71012b13ca4d3d0cdc72a177df3ef03b0e76a3"), TokenInfo { decimals: 18, usd_price: 3.00 });   // BAL
        m.insert(address!("3a58a54c066fdc0f2d55fc9c89f0415c92ebf3c4"), TokenInfo { decimals: 18, usd_price: 0.52 });   // stMATIC
        m.insert(address!("fa68fb4628dff1028cfec22b4162fccd0d45efb6"), TokenInfo { decimals: 18, usd_price: 0.53 });   // MaticX
        m.insert(address!("385eeac5cb85a38a9a07a70c73e0a3271cfb54a7"), TokenInfo { decimals: 18, usd_price: 1.20 });   // GHST
        m.insert(address!("b5c064f955d8e7f38fe0460c556a72987494ee17"), TokenInfo { decimals: 18, usd_price: 45.00 });  // QUICK
        m.insert(address!("0b3f868e0be5597d5db7feb59e1cadbb0fdda50a"), TokenInfo { decimals: 18, usd_price: 1.00 });   // SUSHI
        m.insert(address!("0e1a3d9f5b0e1f1ba2ab5fb742a3c42aef3a9a0b"), TokenInfo { decimals: 18, usd_price: 2.50 });   // CAKE
        m.insert(address!("df7837de1f2fa4631d716cf2502f8b230f1dcc32"), TokenInfo { decimals: 18, usd_price: 0.002 });  // TEL
        m.insert(address!("e0b52e49357fd4daf2c15e02058dce6bc0057db4"), TokenInfo { decimals: 18, usd_price: 1.06 });   // agEUR
        m.insert(address!("e111178a87a3bff0c8d18decba5798827539ae99"), TokenInfo { decimals: 2, usd_price: 1.07 });    // EURS
        m
    })
}

pub fn token_decimals(token: Address) -> Option<u8> {
    token_info_map().get(&token).map(|t| t.decimals)
}

pub fn token_usd_price(token: Address) -> Option<f64> {
    token_info_map().get(&token).map(|t| t.usd_price)
}

/// Compute USD value of a raw token amount.
/// Returns None if token is not in the known list.
pub fn raw_amount_to_usd(token: Address, raw_amount: u128) -> Option<f64> {
    let info = token_info_map().get(&token)?;
    let adjusted = raw_amount as f64 / 10u64.pow(info.decimals as u32) as f64;
    Some(adjusted * info.usd_price)
}

pub fn matic_usd_price() -> f64 {
    0.50
}

// ---- On-chain pricing via PoolManager reserves ----

const WMATIC: Address = address!("0d500b1d8e8ef31e21c99d1db9a6444d3adf1270");
const USDC: Address = address!("2791bca1f2de4661ed88a30c99a7a9449aa84174");
const USDT: Address = address!("c2132d05d31c914a87c6611c10748aeb04b58e8f");
const DAI: Address = address!("8f3cf7ad23cd3cadbd9735aff958023239c6a063");

const STABLECOINS: [Address; 3] = [USDC, USDT, DAI];

fn is_stablecoin(addr: Address) -> bool {
    STABLECOINS.contains(&addr)
}

/// Derive WMATIC/USD price from a WMATIC/stablecoin pool in the manager.
fn onchain_wmatic_usd_price(pm: &PoolManager) -> Option<f64> {
    for &stable in &STABLECOINS {
        let pool_addr = pm.find_pair_pool(&WMATIC, &stable)?;
        let pool = pm.get(&pool_addr)?;
        if let PoolState::UniswapV2(s) = pool {
            if s.reserve0 > 0 && s.reserve1 > 0 {
                return if s.info.token0 == WMATIC {
                    Some(s.reserve1 as f64 / s.reserve0 as f64)
                } else {
                    Some(s.reserve0 as f64 / s.reserve1 as f64)
                };
            }
        }
    }
    None
}

/// Compute USD price of a token using on-chain pool reserves from PoolManager.
/// Falls back to hardcoded price if no WMATIC pair is available.
pub fn onchain_usd_price(token: Address, pm: &PoolManager) -> Option<f64> {
    if is_stablecoin(token) {
        return Some(1.0);
    }

    let wmatic_usd = onchain_wmatic_usd_price(pm)?;

    if token == WMATIC {
        return Some(wmatic_usd);
    }

    // Find a WMATIC pair for the token
    let pool_addr = pm.find_pair_pool(&token, &WMATIC)?;
    let pool = pm.get(&pool_addr)?;
    let (reserve_token, reserve_wmatic) = match pool {
        PoolState::UniswapV2(s) => {
            if s.info.token0 == WMATIC {
                (s.reserve1, s.reserve0)
            } else if s.info.token1 == WMATIC {
                (s.reserve0, s.reserve1)
            } else {
                return None;
            }
        }
        _ => return None,
    };

    if reserve_token == 0 || reserve_wmatic == 0 {
        return None;
    }

    let token_price_in_wmatic = reserve_wmatic as f64 / reserve_token as f64;
    Some(token_price_in_wmatic * wmatic_usd)
}

/// Compute USD value of a raw token amount using on-chain pricing.
/// Falls back to the hardcoded price map if on-chain data is unavailable.
pub fn raw_amount_to_usd_onchain(token: Address, raw_amount: u128, pm: &PoolManager) -> Option<f64> {
    let price = onchain_usd_price(token, pm)
        .or_else(|| token_usd_price(token))?;
    let decimals = token_decimals(token)?;
    let adjusted = raw_amount as f64 / 10u64.pow(decimals as u32) as f64;
    Some(adjusted * price)
}
