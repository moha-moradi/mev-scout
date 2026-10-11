//! Small utility functions shared across the crate.
use std::time::{SystemTime, UNIX_EPOCH};

use alloy::primitives::{Address, B256, U256};

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

/// Sign-extend a left-padded 24-bit tick (`int24`) from a 32-byte topic.
pub fn i24_from_topic(word: &B256) -> i32 {
    let raw = u32::from_be_bytes([word.0[28], word.0[29], word.0[30], word.0[31]]) & 0x00ff_ffff;
    if raw & 0x0080_0000 != 0 {
        (raw as i32) - 0x0100_0000
    } else {
        raw as i32
    }
}

/// Percent-encode `s` as UTF-8 bytes (RFC 3986 unreserved set left as-is).
pub fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// HTTP client with a shared user-agent. Built per call; no process-wide pool.
pub fn http_client(timeout: std::time::Duration) -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(timeout)
        .user_agent("mev-scout/0.1")
        .build()
        .unwrap_or_else(|_| reqwest::Client::new())
}

/// Decode a U256 from a 32-byte ABI word.
pub fn abi_decode_u256(data: &[u8], offset: usize) -> Option<U256> {
    if offset + 32 > data.len() {
        return None;
    }
    Some(U256::from_be_slice(&data[offset..offset + 32]))
}
