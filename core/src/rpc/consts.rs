pub const HTTP_TIMEOUT_SECS: u64 = 30;
pub const DEAD_PROVIDER_COOLDOWN_SECS: u64 = 300;
pub const MAX_BACKOFF_SECS: u64 = 300;
/// How far behind the chain tip the archive-support probe runs.
/// Probes historical state (e.g. `eth_getProof` at tip − this depth) instead of
/// the tip, so recent-only / pruned / load-balanced shared endpoints that cannot
/// serve replay state are correctly flagged as non-archive.
pub const ARCHIVE_PROBE_DEPTH_BLOCKS: u64 = 10_000;
