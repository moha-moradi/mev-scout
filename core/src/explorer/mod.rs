//! Realized-MEV explorer: forensic reconstruction of MEV actually extracted
//! on-chain, from raw block/receipt data via RPC (logs-only, no traces in the
//! backfill path).
//!
//! Conceptual split vs the rest of the crate: `mev::` detectors answer "what
//! could be made" (opportunity simulation); `explorer::` answers "what was
//! made" (pattern classification over realized transactions + profit
//! attribution from token balance deltas).
//!
//! Pipeline: `ingest` (block+receipt fetch, reorg-aware) → `decode` (swaps,
//! transfers, liquidation facts) → `classify` (per-block pattern passes) →
//! `profit` (balance-delta accounting, gas, USD) → `store` (SQLite facts) →
//! `query` (live/stats/top/show/export CLI surface).

pub mod canonical;
pub mod classify;
pub mod decode;
pub mod ingest;
pub mod pricing;
pub mod profit;
pub mod reject;
pub mod store;
pub mod types;
pub mod validate;

pub use canonical::explorer_canonical_id;
pub use classify::classify_block;
pub use reject::{RejectReason, RejectedCandidate};
pub use types::{Confidence, MevBundle, MevEvent, MevKind};
