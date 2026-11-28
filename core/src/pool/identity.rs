//! Canonical pool identity.
//!
//! A `PoolKey` is stable, chain-scoped, and independent of source metadata.
//! Singleton protocols use the full bytes32 PoolId; a synthetic EVM address is
//! only a compatibility role, never the identity.

use std::fmt;
use std::str::FromStr;

use alloy::primitives::Address;

use crate::dex_type::DexType;
use crate::pool::state::PoolInfo;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum PoolKey {
    /// `chain:evm_pool_address:0x<20-byte-address>`
    EvmPool { chain_id: u64, address: Address },
    /// `chain:evm_singleton_pool:0x<singleton>:0x<32-byte-pool-id>`
    EvmSingletonPool {
        chain_id: u64,
        singleton: Address,
        pool_id: [u8; 32],
    },
    /// `chain:evm_singleton_typed_pool:0x<deployment>:<pool_type>:0x<32-byte-pool-id>`
    EvmSingletonTypedPool {
        chain_id: u64,
        singleton: Address,
        pool_type: String,
        pool_id: [u8; 32],
    },
}

impl PoolKey {
    pub fn chain_id(&self) -> u64 {
        match self {
            Self::EvmPool { chain_id, .. }
            | Self::EvmSingletonPool { chain_id, .. }
            | Self::EvmSingletonTypedPool { chain_id, .. } => *chain_id,
        }
    }

    pub fn legacy_address(&self) -> Address {
        match self {
            Self::EvmPool { address, .. } => *address,
            Self::EvmSingletonPool { pool_id, .. }
            | Self::EvmSingletonTypedPool { pool_id, .. } => {
                if pool_id.len() < 20 {
                    return Address::ZERO;
                }
                Address::from_slice(&pool_id[pool_id.len() - 20..])
            }
        }
    }

    /// Derive the best available key from the legacy 22-column pool shape.
    ///
    /// This is intentionally conservative: when a singleton PoolId is present
    /// but its exact tag is not carried by `PoolInfo`, the typed namespace uses
    /// `unknown` and stays provisional until on-chain verification fills it.
    pub fn from_pool_info(chain_id: u64, pool: &PoolInfo) -> Self {
        if let Some(pool_id) = pool.pool_id {
            let singleton = pool.factory.unwrap_or(pool.address);
            match pool.dex_type {
                DexType::UniswapV4 | DexType::Balancer => Self::EvmSingletonPool {
                    chain_id,
                    singleton,
                    pool_id,
                },
                DexType::PancakeInfinity => Self::EvmSingletonTypedPool {
                    chain_id,
                    singleton,
                    pool_type: "unknown".to_string(),
                    pool_id,
                },
                _ => Self::EvmPool {
                    chain_id,
                    address: pool.address,
                },
            }
        } else {
            Self::EvmPool {
                chain_id,
                address: pool.address,
            }
        }
    }
}

impl fmt::Display for PoolKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EvmPool { chain_id, address } => {
                write!(f, "{chain_id}:evm_pool_address:{}", hex_addr(address))
            }
            Self::EvmSingletonPool {
                chain_id,
                singleton,
                pool_id,
            } => write!(
                f,
                "{chain_id}:evm_singleton_pool:{}:{}",
                hex_addr(singleton),
                hex_bytes(pool_id)
            ),
            Self::EvmSingletonTypedPool {
                chain_id,
                singleton,
                pool_type,
                pool_id,
            } => write!(
                f,
                "{chain_id}:evm_singleton_typed_pool:{}:{}:{}",
                hex_addr(singleton),
                pool_type,
                hex_bytes(pool_id)
            ),
        }
    }
}

impl FromStr for PoolKey {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let parts: Vec<&str> = s.split(':').collect();
        anyhow::ensure!(parts.len() >= 3, "pool key needs at least 3 segments");
        let chain_id = parts[0]
            .parse::<u64>()
            .map_err(|e| anyhow::anyhow!("invalid chain id '{}': {e}", parts[0]))?;
        match parts[1] {
            "evm_pool_address" => {
                anyhow::ensure!(parts.len() == 3, "evm_pool_address needs 3 segments");
                Ok(Self::EvmPool {
                    chain_id,
                    address: parse_address(parts[2])?,
                })
            }
            "evm_singleton_pool" => {
                anyhow::ensure!(parts.len() == 4, "evm_singleton_pool needs 4 segments");
                Ok(Self::EvmSingletonPool {
                    chain_id,
                    singleton: parse_address(parts[2])?,
                    pool_id: parse_pool_id(parts[3])?,
                })
            }
            "evm_singleton_typed_pool" => {
                anyhow::ensure!(
                    parts.len() == 5,
                    "evm_singleton_typed_pool needs 5 segments"
                );
                anyhow::ensure!(!parts[3].is_empty(), "pool_type must not be empty");
                Ok(Self::EvmSingletonTypedPool {
                    chain_id,
                    singleton: parse_address(parts[2])?,
                    pool_type: parts[3].to_string(),
                    pool_id: parse_pool_id(parts[4])?,
                })
            }
            other => anyhow::bail!("unknown pool key namespace '{other}'"),
        }
    }
}

fn hex_addr(address: &Address) -> String {
    format!("0x{}", hex::encode(address.as_slice()))
}

fn hex_bytes(bytes: &[u8]) -> String {
    format!("0x{}", hex::encode(bytes))
}

fn parse_hex(value: &str, expected: usize, what: &str) -> anyhow::Result<Vec<u8>> {
    let stripped = value.strip_prefix("0x").unwrap_or(value);
    anyhow::ensure!(
        stripped.len() == expected * 2,
        "{what} must be {expected} bytes, got {} hex chars",
        stripped.len()
    );
    let bytes = hex::decode(stripped).map_err(|e| anyhow::anyhow!("invalid {what}: {e}"))?;
    anyhow::ensure!(bytes.len() == expected, "invalid {what} length");
    Ok(bytes)
}

fn parse_address(value: &str) -> anyhow::Result<Address> {
    Ok(Address::from_slice(&parse_hex(value, 20, "address")?))
}

fn parse_pool_id(value: &str) -> anyhow::Result<[u8; 32]> {
    let bytes = parse_hex(value, 32, "pool id")?;
    let mut out = [0u8; 32];
    out.copy_from_slice(&bytes);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn address_key_roundtrip_is_lowercase() {
        let key = PoolKey::EvmPool {
            chain_id: 43114,
            address: Address::repeat_byte(0xab),
        };
        let text = key.to_string();
        assert_eq!(
            text,
            "43114:evm_pool_address:0xabababababababababababababababababababab"
        );
        assert_eq!(text.parse::<PoolKey>().unwrap(), key);
    }

    #[test]
    fn singleton_pool_id_roundtrip_keeps_all_32_bytes() {
        let pool_id = [0xcd; 32];
        let key = PoolKey::EvmSingletonPool {
            chain_id: 43114,
            singleton: Address::repeat_byte(0x12),
            pool_id,
        };
        let text = key.to_string();
        assert!(text.ends_with(&format!("0x{}", hex::encode(pool_id))));
        assert_eq!(text.parse::<PoolKey>().unwrap(), key);
        assert_eq!(key.legacy_address(), Address::repeat_byte(0xcd));
    }

    #[test]
    fn typed_singleton_roundtrip_keeps_pool_type() {
        let key = PoolKey::EvmSingletonTypedPool {
            chain_id: 43114,
            singleton: Address::repeat_byte(0x34),
            pool_type: "0x0100".to_string(),
            pool_id: [0xef; 32],
        };
        let text = key.to_string();
        assert_eq!(text.parse::<PoolKey>().unwrap(), key);
    }

    #[test]
    fn singleton_roundtrip_from_legacy_rows_keeps_full_pool_id() {
        let pool_id = [0x99; 32];
        let singleton = Address::repeat_byte(0x12);
        let synthetic = Address::from_slice(&pool_id[12..]);

        for dex_type in [DexType::UniswapV4, DexType::Balancer] {
            let key = PoolKey::from_pool_info(
                43114,
                &PoolInfo {
                    address: synthetic,
                    pool_id: Some(pool_id),
                    factory: Some(singleton),
                    dex_type,
                    ..Default::default()
                },
            );
            let expected = PoolKey::EvmSingletonPool {
                chain_id: 43114,
                singleton,
                pool_id,
            };
            assert_eq!(key, expected);
            assert_eq!(key.to_string().parse::<PoolKey>().unwrap(), expected);
            assert_eq!(key.legacy_address(), synthetic);
        }

        let typed = PoolKey::from_pool_info(
            43114,
            &PoolInfo {
                address: synthetic,
                pool_id: Some(pool_id),
                factory: Some(singleton),
                dex_type: DexType::PancakeInfinity,
                ..Default::default()
            },
        );
        assert!(matches!(typed, PoolKey::EvmSingletonTypedPool { .. }));
        assert_eq!(typed.to_string().parse::<PoolKey>().unwrap(), typed);
    }

    #[test]
    fn malformed_keys_are_rejected() {
        assert!("43114:evm_pool_address:0x1234".parse::<PoolKey>().is_err());
        assert!("43114:unknown:0x00".parse::<PoolKey>().is_err());
        assert!("43114:evm_singleton_pool:0x11".parse::<PoolKey>().is_err());
    }
}
