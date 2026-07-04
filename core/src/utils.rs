//! Small utility functions shared across the crate.

/// Decode a uint128 from the last 16 bytes of a byte slice.
/// If the slice is shorter than 16 bytes, leading bytes are treated as zero.
pub fn u128_from_be_bytes(bytes: &[u8]) -> u128 {
    let mut buf = [0u8; 16];
    let len = bytes.len().min(16);
    buf[16 - len..].copy_from_slice(&bytes[bytes.len().saturating_sub(len)..]);
    u128::from_be_bytes(buf)
}

