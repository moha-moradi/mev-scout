//! Multicall3 batched `eth_call` helper — resolves pool metadata in bulk.
//!
//! Multicall3 is deployed at the same deterministic address on every supported
//! chain (`0xcA11bde05977b3631167028862bE2a173976CA11`), so no per-chain
//! configuration is needed. One `eth_call` to `aggregate3((address,bool,bytes)[])`
//! resolves `token0()/token1()/fee()/tickSpacing()` for ~25 pools (100 subcalls),
//! turning "N×4 calls" into "⌈N/25⌉×1 calls" for remote-sourced
//! concentrated-liquidity pools whose fee/tickSpacing the aggregators omit.
//!
//! Sub-calls use `allowFailure = true`: a reverted member (e.g. an address that
//! is not actually a CL pool) returns empty data for its slot instead of failing
//! the whole batch.

use std::collections::HashMap;

use alloy::primitives::{address, Address, Bytes};

use crate::rpc::RpcClient;

/// Multicall3 — identical address on all supported chains.
pub const MULTICALL3: Address = address!("cA11bde05977b3631167028862bE2a173976CA11");

const TOKEN0_SELECTOR: [u8; 4] = [0x0d, 0xfe, 0x16, 0x81];
const TOKEN1_SELECTOR: [u8; 4] = [0xd2, 0x12, 0x20, 0xa7];
const FEE_SELECTOR: [u8; 4] = [0xdd, 0xca, 0x3f, 0x43];
const TICK_SPACING_SELECTOR: [u8; 4] = [0x37, 0xcf, 0xda, 0xca];

/// `aggregate3((address,bool,bytes)[])` selector (verified keccak256 prefix).
const AGGREGATE3_SELECTOR: [u8; 4] = [0x82, 0xad, 0x56, 0xcb];

/// Pools per Multicall3 batch: 4 subcalls each keeps batches at ~100 subcalls,
/// comfortably within public-RPC gas and response-size limits.
const POOLS_PER_BATCH: usize = 25;

/// Resolved on-chain metadata for one concentrated-liquidity pool.
#[derive(Debug, Clone, Default)]
pub struct PoolMetadata {
    pub token0: Option<Address>,
    pub token1: Option<Address>,
    pub fee: Option<u32>,
    pub tick_spacing: Option<i32>,
}

struct SubCall {
    target: Address,
    data: Bytes,
}

fn push_word_u64(buf: &mut Vec<u8>, v: u64) {
    buf.extend_from_slice(&[0u8; 24]);
    buf.extend_from_slice(&v.to_be_bytes());
}

/// ABI-encode `aggregate3((address,bool,bytes)[])` with allowFailure=true.
///
/// Layout after the 4-byte selector:
/// ```text
/// word  0      : 0x20                      (array param offset)
/// word  1      : N                         (array length)
/// words 2..2+N : per-element heads
///                { target | allowFailure=1 | tailOffset }
/// tails        : { len | data padded to 32 }
/// ```
/// Tail offsets are relative to the start of the array-data area (right after
/// the length word), per ABI v2 dynamic-in-dynamic encoding.
fn encode_aggregate3(calls: &[SubCall]) -> Bytes {
    let n = calls.len();

    // Pre-compute tail offsets relative to array-data start.
    let mut tail_offsets = Vec::with_capacity(n);
    let mut offset = n * 96; // heads region
    for call in calls {
        tail_offsets.push(offset as u64);
        let padded = ((call.data.len() + 31) / 32) * 32;
        offset += 32 + padded;
    }

    let mut buf = Vec::with_capacity(4 + 64 + offset);
    buf.extend_from_slice(&AGGREGATE3_SELECTOR);
    push_word_u64(&mut buf, 0x20); // array param offset
    push_word_u64(&mut buf, n as u64); // array length

    // Heads.
    for (i, call) in calls.iter().enumerate() {
        buf.extend_from_slice(&[0u8; 12]);
        buf.extend_from_slice(call.target.as_slice());
        buf.extend_from_slice(&[0u8; 31]);
        buf.push(1); // allowFailure = true
        push_word_u64(&mut buf, tail_offsets[i]);
    }

    // Tails.
    for call in calls {
        push_word_u64(&mut buf, call.data.len() as u64);
        let mut d = call.data.to_vec();
        d.resize(((d.len() + 31) / 32) * 32, 0);
        buf.extend_from_slice(&d);
    }

    Bytes::from(buf)
}

/// Read a big-endian 32-byte word as u64, rejecting oversized values.
fn word_to_u64(word: &[u8]) -> Option<u64> {
    if word.len() != 32 || word[..24].iter().any(|&b| b != 0) {
        return None;
    }
    Some(u64::from_be_bytes(word[24..32].try_into().ok()?))
}

/// Decode `aggregate3` return `(bool success, bytes[] returnData)` into
/// per-slot optional payloads.
fn decode_aggregate3(result: &[u8]) -> Option<Vec<Option<Bytes>>> {
    if result.len() < 64 {
        return None;
    }
    // word1: offset of the bytes[] return array (relative to args start).
    let arr_off = word_to_u64(&result[32..64])? as usize;
    if result.len() < arr_off + 32 {
        return None;
    }
    let count = word_to_u64(&result[arr_off..arr_off + 32])? as usize;
    let offsets_start = arr_off + 32;
    if result.len() < offsets_start + count * 32 {
        return None;
    }
    let mut out = Vec::with_capacity(count);
    for i in 0..count {
        let off_pos = offsets_start + i * 32;
        // Element offsets are relative to the array-data area start.
        let rel = word_to_u64(&result[off_pos..off_pos + 32])? as usize;
        let abs = arr_off + rel;
        if result.len() < abs + 32 {
            out.push(None);
            continue;
        }
        let len = word_to_u64(&result[abs..abs + 32])? as usize;
        if len == 0 || abs + 32 + len > result.len() {
            out.push(None);
            continue;
        }
        out.push(Some(Bytes::copy_from_slice(&result[abs + 32..abs + 32 + len])));
    }
    Some(out)
}

/// Run one aggregate3 batch over pre-built subcalls.
async fn run_batch(rpc: &RpcClient, calls: Vec<SubCall>) -> anyhow::Result<Vec<Option<Bytes>>> {
    let calldata = encode_aggregate3(&calls);
    let result = rpc.call_latest(MULTICALL3, calldata).await?;
    decode_aggregate3(&result.0)
        .ok_or_else(|| anyhow::anyhow!("malformed Multicall3 aggregate3 response"))
}

/// Resolve `token0/token1/fee/tickSpacing` for a batch of pool addresses using
/// Multicall3. Pools are chunked into groups of [`POOLS_PER_BATCH`]; each chunk
/// costs exactly one `eth_call`. Failed subcalls yield `None` fields.
///
/// Callers should only pass pools whose metadata is genuinely missing (the
/// results are meant to be persisted once and never re-fetched).
pub async fn resolve_pool_metadata(
    rpc: &RpcClient,
    pools: &[Address],
    concurrency: usize,
) -> anyhow::Result<HashMap<Address, PoolMetadata>> {
    use futures::stream::{self, StreamExt};

    if pools.is_empty() {
        return Ok(HashMap::new());
    }

    let selectors: [[u8; 4]; 4] = [
        TOKEN0_SELECTOR,
        TOKEN1_SELECTOR,
        FEE_SELECTOR,
        TICK_SPACING_SELECTOR,
    ];

    struct Chunk {
        pool: Address,
        calls: Vec<SubCall>,
    }
    let chunks: Vec<Chunk> = pools
        .chunks(POOLS_PER_BATCH)
        .flat_map(|group| {
            group.iter().map(move |&pool| Chunk {
                pool,
                calls: selectors
                    .iter()
                    .map(|sel| SubCall {
                        target: pool,
                        data: Bytes::copy_from_slice(sel),
                    })
                    .collect(),
            })
        })
        .collect();

    tracing::info!(
        "Multicall3: resolving metadata for {} pool(s) via {} eth_call batch(es)",
        pools.len(),
        chunks.len()
    );

    let results: Vec<(Address, Vec<Option<Bytes>>)> = stream::iter(chunks)
        .map(|c| {
            let rpc = rpc.clone();
            async move {
                match run_batch(&rpc, c.calls).await {
                    Ok(ret) => Some((c.pool, ret)),
                    Err(e) => {
                        tracing::warn!("Multicall3 batch failed for {}: {e:#}", c.pool);
                        None
                    }
                }
            }
        })
        .buffer_unordered(concurrency.max(1))
        .filter_map(|r| async move { r })
        .collect()
        .await;

    let mut out = HashMap::with_capacity(pools.len());
    for (pool, slots) in results {
        // Slots are laid out token0, token1, fee, tickSpacing per pool.
        if slots.len() < 4 {
            continue;
        }
        let parse_word = |i: usize| -> Option<[u8; 32]> {
            let b = slots.get(i)?.as_ref()?;
            (b.len() >= 32).then(|| b[..32].try_into().ok())?
        };
        let addr_at = |w: Option<[u8; 32]>| w.map(|b| Address::from_slice(&b[12..]));
        let uint_at = |w: Option<[u8; 32]>| {
            w.filter(|b| b[..28].iter().all(|&x| x == 0))
                .map(|b| u32::from_be_bytes([b[28], b[29], b[30], b[31]]))
                .filter(|v| *v != 0)
        };
        let int_at = |w: Option<[u8; 32]>| {
            w.filter(|b| b[..28].iter().all(|&x| x == 0))
                .map(|b| i32::from_be_bytes([b[28], b[29], b[30], b[31]]))
                .filter(|v| *v != 0)
        };
        let token0 = addr_at(parse_word(0)).filter(|a| !a.is_zero());
        let token1 = addr_at(parse_word(1)).filter(|a| !a.is_zero());
        let fee = uint_at(parse_word(2));
        let tick_spacing = int_at(parse_word(3));
        if token0.is_none() && token1.is_none() && fee.is_none() && tick_spacing.is_none() {
            continue; // whole pool failed — nothing to record
        }
        out.insert(
            pool,
            PoolMetadata { token0, token1, fee, tick_spacing },
        );
    }

    tracing::info!("Multicall3: resolved metadata for {}/{} pool(s)", out.len(), pools.len());
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::hex;

    #[test]
    fn aggregate3_encoding_layout() {
        let calls = vec![
            SubCall { target: Address::LEFT_PAD_ONE, data: Bytes::from_static(&[0xaa, 0xbb]) },
            SubCall { target: Address::ZERO, data: Bytes::from_static(&[0xcc; 32]) },
        ];
        let enc = encode_aggregate3(&calls);

        // selector + args head (array offset 0x20) + array length 2
        assert_eq!(&enc[0..4], &AGGREGATE3_SELECTOR);
        assert_eq!(enc[4..36], hex!("0000000000000000000000000000000000000000000000000000000000000020"));
        assert_eq!(enc[36..68], hex!("0000000000000000000000000000000000000000000000000000000000000002"));

        let data_start = 68usize; // selector(4) + 2 head words
        // Element 0 head: target | allowFailure | tailOffset
        let e0 = &enc[data_start..data_start + 96];
        assert_eq!(&e0[12..32], Address::LEFT_PAD_ONE.as_slice());
        assert_eq!(e0[63], 1);
        assert_eq!(word_to_u64(&e0[64..96]), Some(192)); // two heads × 96

        // Element 1 head
        let e1 = &enc[data_start + 96..data_start + 192];
        assert_eq!(&e1[12..32], Address::ZERO.as_slice());
        assert_eq!(e1[63], 1);
        // Tail 0 size = 32 (len word) + 32 (padded data) → second tail at 256.
        assert_eq!(word_to_u64(&e1[64..96]), Some(192 + 64));

        // Tail 0: len 2, data [aa bb]
        let t0 = data_start + 192;
        assert_eq!(word_to_u64(&enc[t0..t0 + 32]), Some(2));
        assert_eq!(&enc[t0 + 32..t0 + 34], &[0xaa, 0xbb]);

        // Tail 1: len 32, full-word payload
        let t1 = data_start + 192 + 64;
        assert_eq!(word_to_u64(&enc[t1..t1 + 32]), Some(32));
        assert_eq!(&enc[t1 + 32..t1 + 64], &[0xcc; 32]);

        // Total buffer exactly consumes all regions.
        assert_eq!(enc.len(), data_start + 192 + 64 + 64);
    }

    /// Synthetic `(bool, bytes[])` aggregate3 return with one populated and one
    /// empty element — mirrors Solidity's exact dynamic-array layout.
    #[test]
    fn aggregate3_decode_roundtrip() {
        let mut ret: Vec<u8> = Vec::new();
        ret.extend_from_slice(&[0u8; 31]); ret.push(1); // success = true
        ret.extend_from_slice(&[0u8; 31]); ret.push(0x40); // array offset
        ret.extend_from_slice(&[0u8; 31]); ret.push(2); // 2 elements

        let data_area = 96usize; // after success word, offset word, len word
        let e0_tail = data_area + 64; // skip both element-offset words
        let e1_tail = e0_tail + 32 + 32; // elem0 len word + padded data

        ret.extend_from_slice(&[0u8; 24]);
        ret.extend_from_slice(&((e0_tail - data_area) as u64).to_be_bytes());
        ret.extend_from_slice(&[0u8; 24]);
        ret.extend_from_slice(&((e1_tail - data_area) as u64).to_be_bytes());

        // Element 0: len 1, data [0xab]
        ret.extend_from_slice(&[0u8; 24]);
        ret.extend_from_slice(&1u64.to_be_bytes());
        ret.extend_from_slice(&[0xab, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                                 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        // Element 1: len 0 → empty slot
        ret.extend_from_slice(&[0u8; 32]);

        let decoded = decode_aggregate3(&ret).unwrap();
        assert_eq!(decoded.len(), 2);
        assert_eq!(decoded[0].as_ref().unwrap().as_ref(), &[0xab]);
        assert!(decoded[1].is_none());
    }
}
