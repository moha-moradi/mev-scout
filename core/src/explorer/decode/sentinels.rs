//! Sentinel addresses for unresolved V2/V3 token sides.
use alloy::primitives::Address;

pub const TOKEN0_SENTINEL: Address = Address::new([0xEEu8; 20]);
pub const TOKEN1_SENTINEL: Address = Address::new([0xE1u8; 20]);

/// True when a token slot is not yet a resolved ERC-20 address (zero or a
/// token0/token1 sentinel).
pub(super) fn is_unresolved_token(t: Address) -> bool {
    t.is_zero() || t == TOKEN0_SENTINEL || t == TOKEN1_SENTINEL
}

