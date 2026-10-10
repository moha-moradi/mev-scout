//! Small utility functions shared across the crate.
use std::time::{SystemTime, UNIX_EPOCH};

use alloy::primitives::{Address, U256};

/// Return the current Unix timestamp in whole seconds since the epoch.
pub fn epoch_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Decode a uint128 from the last 16 bytes of a byte slice.
/// If the slice is shorter than 16 bytes, leading bytes are treated as zero.
pub fn u128_from_be_bytes(bytes: &[u8]) -> u128 {
    let mut buf = [0u8; 16];
    let len = bytes.len().min(16);
    buf[16 - len..].copy_from_slice(&bytes[bytes.len().saturating_sub(len)..]);
    u128::from_be_bytes(buf)
}

/// Decode a u128 from a 32-byte ABI word (right-aligned).
pub fn abi_decode_u128(data: &[u8], offset: usize) -> Option<u128> {
    if offset + 32 > data.len() {
        return None;
    }
    Some(u128_from_be_bytes(&data[offset..offset + 32]))
}

/// Address in ABI word `index` (32-byte words, address right-aligned).
/// Panics if `data` is shorter than that word.
pub fn abi_word_address(data: impl AsRef<[u8]>, index: usize) -> Address {
    let data = data.as_ref();
    let off = index * 32;
    Address::from_slice(&data[off + 12..off + 32])
}

/// Address in the last 20 bytes of a 32-byte topic or ABI word.
pub fn topic_address(word: impl AsRef<[u8]>) -> Address {
    let word = word.as_ref();
    let start = word.len().saturating_sub(20);
    Address::from_slice(&word[start..])
}

/// Decode an Address from a 32-byte ABI word (right-aligned).
pub fn abi_decode_address(data: &[u8], offset: usize) -> Option<Address> {
    if offset + 32 > data.len() {
        return None;
    }
    Some(Address::from_slice(&data[offset + 12..offset + 32]))
}

/// Decode a U256 from a 32-byte ABI word.
pub fn abi_decode_u256(data: &[u8], offset: usize) -> Option<U256> {
    if offset + 32 > data.len() {
        return None;
    }
    Some(U256::from_be_slice(&data[offset..offset + 32]))
}
