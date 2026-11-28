# Stage 2 Design — Canonical Master Inventory and safe `v13 → v14` migration

**Status:** design / compatibility contract for approval. No schema, application code, database, or configuration was changed by this document.  
**Date:** 2026-11-28  
**Authority:** `docs/POOL_DISCOVERY_REDESIGN_PLAN.md` §5.2–§5.4 and Stage 2 row (§6).  
**Stage 1 decision:** PublicNode batched `eth_getBlockReceipts` is the chosen 30-day backfill backend; see `docs/benchmarks/avalanche-backfill-pilot.md`. The 30-day run itself is deliberately **not** a prerequisite for this design.

## خلاصه

مرحله ۲ فقط «هویت و ایمنی» را حل میکند: یک `PoolKey` canonical با پشتیبانی از PoolId کامل singletonها، یک Master Inventory که هیچ ردیف ناشناخته را حذف نمیکند، observationهای append-only با provenance، cursor/window جداگانه برای هر source، و مهاجرت امن `v13 → v14` با view-read-only و adapterهای سازگار.

هیچ indexer خارجی، bloom filter، چارچوب enumeration عمومی، adapter رسمی/subgraph، یا سیاست retention پیشرفته اضافه نمیشود. Live در این مرحله فقط reader باقی میماند و هیچ ردیف legacy به طور خودکار وارد Execution-ready نمیشود.

---

## 1. Scope

### In scope

- Canonical `PoolKey` Rust type and stable text encoding, including V4, Balancer V2, Pancake Infinity singleton PoolIds.
- Minimal v14 Master tables: identity, role, observation, status/admission, scan window/cursor, reorg event, registry revision.
- Observation-based merge replacing `INSERT OR REPLACE` / first-wins `merge_from`.
- One authoritative inventory writer; all current writers/adapters keep working.
- Per-source / per-subject scan windows with retryable failed/partial states.
- `pool_info` and `pool_info_legacy_complete` as read-only compatibility views with write-rejection triggers.
- Safe v13 migration using `PRAGMA table_info` preflight and one `BEGIN IMMEDIATE` transaction.
- Targeted bug fixes already named in the Stage 2 plan: V4 `pool_id`, Metric/Fluid/Infinity identifier retention, topic-length guards, `factory_pools.drain()` association bug, destructive persistence caps.

### Out of scope

- Stage 3 backfill/continuous discovery execution and Stage 4 validation/readiness/Live polling.
- Deferred backlog from plan §7.
- Changing external GT/DS adapters except that their output becomes a provisional observation.
- Any new provider, dependency, or network call in the migration path.

---

## 2. Verified current state

This is the baseline the contract must fit; all references are to the current checkout.

- Pool cache schema version is `13` (`core/src/cache/store/mod.rs:72-73`). `schema_version` is declared at `mod.rs:80-82`, but version detection uses `SELECT COALESCE(MAX(version), 0)` at `mod.rs:223-229` and never seeds a row. The update-without-insert path is at `mod.rs:300-303`; the checked-in `cache/avalanche-mev-scout.sqlite` therefore reads as v0 despite having v13 columns.
- `pool_info` is a real table, not a view (`mod.rs:149-160` plus v1–v13 migrations at `mod.rs:239-288`). The legacy reader shape is exactly 22 columns, decoded at `mod.rs:376-459`.
- There is no canonical Rust `PoolKey`. Current identity is address-only; `DiscoveredPool` is at `core/src/pool/discovery/mod.rs:161-215`.
- V4 `Initialize` discards the full 32-byte PoolId and keeps a synthetic address (`pool/discovery/v4.rs:30-74`); Infinity retains PoolId (`infinity.rs:36-77`); Balancer retains PoolId (`balancer.rs:11-67`); Metric discards `poolId` (`metric.rs:18-46`); Fluid discards `dexId` and stores zero tokens (`fluid.rs:19-41`).
- `put_discovered_pool()` uses `INSERT OR REPLACE` (`cache/store/pools.rs:6-51`). All production writers funnel through it: `jobs/discover.rs:191-208`, `pool/discovery/mod.rs:1724-1799`, `jobs/index.rs:127-174`.
- Destructive filters exist before persistence: `min_tvl`, sort and `max_pools` truncation (`jobs/discover.rs:144-162,583-596`) and zero-token skips (`jobs/discover.rs:194-196`, `pool/discovery/mod.rs:1774-1781`).
- `discovery_scan_watermark` is a single chain-level cursor (`cache/store/mod.rs:215-219`, `cache/store/pools.rs:149-181`, `jobs/discover.rs:368-382,384-459,536-557`).
- Live reads every cached pool at session setup (`jobs/live.rs:576-581,607,629-649`); there is no `registry_revision` polling yet.
- `factory_pools.drain()` at `pool/discovery/mod.rs:1392` empties the map before `underlying_tokens` is read at `mod.rs:1434-1436`, so multi-token metadata is lost for rows coming from the metadata-result path.
- Several decoders index `topics[n]` directly without a length guard (for example `pool/discovery/v3.rs:29-35`, `v4.rs:40-57`, `infinity.rs:47-54`, `balancer.rs:45-51`). Malformed logs must return `None`, never panic.

---

## 3. Canonical identity contract

### 3.1 Rust type

Introduce a small, dependency-free enum in `core/src/pool/identity.rs`:

```rust
pub enum PoolKey {
    EvmPool { chain_id: u64, address: Address },
    EvmSingletonPool {
        chain_id: u64,
        singleton: Address, // PoolManager or Vault
        pool_id: [u8; 32],
    },
    EvmSingletonTypedPool {
        chain_id: u64,
        singleton: Address, // Infinity deployment
        pool_type: String,  // normalized slug, or 0x<parameters> while unknown
        pool_id: [u8; 32],
    },
}
```

`Display` and `FromStr` produce/parse the stable text forms below. Parsing is strict: lowercase fixed-width hex, exact segment count, no ambiguity, no silent truncation.

### 3.2 Text encoding

| Family | Canonical `PoolKey` text | Notes |
|---|---|---|
| V2/V3/Algebra/Slipstream/Solidly/Camelot/Curve/TraderJoe LB/Pendle | `chain:evm_pool_address:0x<20-byte-address>` | Pool contract address is authoritative. |
| Uniswap V4 | `chain:evm_singleton_pool:0x<pool-manager>:0x<32-byte-pool-id>` | Synthetic address is a role, never identity. |
| Balancer V2 | `chain:evm_singleton_pool:0x<vault>:0x<32-byte-pool-id>` | Pool contract address is an optional separate role. |
| Pancake Infinity | `chain:evm_singleton_typed_pool:0x<deployment>:<pool_type>:0x<32-byte-pool-id>` | `pool_type` is retained; unknown value is `0x<parameters>` and marked provisional. |
| Metric / Fluid | `chain:evm_pool_address:0x<pool>` provisionally | Keep `poolId` / `dexId` in roles and observation payload until singleton semantics are proven. |
| Balancer V3 | no rule enabled | Do not generalize V2 semantics silently. |

Rules:

- Family and version are classification, not identity.
- Factory, manager, vault, registry, hook, router and execution target are separate roles; they never replace the key.
- Never merge pools because they share a token pair; never split one pool because two sources found it.
- If two protocols ever produce the exact same singleton + PoolId, record a `conflict` observation and require an explicit contract change. Do not silently split or drop either row.

### 3.3 Roles stored with every identity

`pool_role` rows use `(pool_key, role, value, observation_id)` and cover: `pool_address`, `legacy_address`, `factory`, `manager`, `vault`, `registry`, `hook`, `router`, `execution_target`, `pool_id`, `dex_id`, `pool_type`, `underlying_tokens`.

`legacy_address` is stored explicitly on `pool_identity`. For singletons it is the synthetic address needed by old readers; it is **not** the canonical key.

---

## 4. Minimal v14 schema

The design keeps one Master Inventory and derives readiness as read-only state over it. No separate Quoteable/Simulation/Execution tables.

### 4.1 `pool_identity`

```sql
pool_key        TEXT PRIMARY KEY,
chain_id        INTEGER NOT NULL,
namespace       TEXT NOT NULL,
family          TEXT NOT NULL,        -- classification: DexType slug or 'Unknown'
version         TEXT,
legacy_address  BLOB,                  -- synthetic/stable legacy view address
created_block   INTEGER,
first_seen_at   TEXT NOT NULL,
identity_state  TEXT NOT NULL DEFAULT 'provisional', -- provisional | canonical
registry_revision INTEGER NOT NULL DEFAULT 0
```

Rows are never deleted for unknown TVL, unsupported family, zero reserves or zero tokens.

### 4.2 `pool_observation`

```sql
id              INTEGER PRIMARY KEY AUTOINCREMENT,
pool_key        TEXT NOT NULL REFERENCES pool_identity(pool_key),
event_key       TEXT NOT NULL UNIQUE,
chain_id        INTEGER NOT NULL,
source          TEXT NOT NULL,  -- receipts | getlogs | poll | registry | geckoterminal | dexscreener | legacy_v13 | manual
method          TEXT NOT NULL,
authority       TEXT NOT NULL,  -- onchain | external | enrichment | legacy
run_id          TEXT,
endpoint        TEXT,           -- redacted host/provider, never an API key
block_number    INTEGER,
block_hash      TEXT,
tx_hash         TEXT,
log_index       INTEGER,
observed_at     TEXT NOT NULL,
raw_identifier  TEXT,
payload_json    TEXT NOT NULL,
conflict        INTEGER NOT NULL DEFAULT 0,
valid           INTEGER NOT NULL DEFAULT 1,
orphaned_at     TEXT
```

`event_key` is computed by the writer (`chain|source|method|run_id|block|tx|log|raw_identifier`) so SQLite `NULL` semantics cannot defeat deduplication. Appendix-only: corrections are new observations, not rewrites.

### 4.3 `pool_role`

```sql
pool_key       TEXT NOT NULL REFERENCES pool_identity(pool_key),
role           TEXT NOT NULL,
value          TEXT NOT NULL,
observation_id INTEGER REFERENCES pool_observation(id),
PRIMARY KEY (pool_key, role, value)
```

### 4.4 `pool_status`

```sql
pool_key          TEXT PRIMARY KEY REFERENCES pool_identity(pool_key),
validation_state  TEXT NOT NULL DEFAULT 'unverified',
quoteable         INTEGER NOT NULL DEFAULT 0,
simulation_ready  INTEGER NOT NULL DEFAULT 0,
execution_ready   INTEGER NOT NULL DEFAULT 0,
admission_reason  TEXT,
registry_revision INTEGER NOT NULL DEFAULT 0,
updated_at        TEXT NOT NULL
```

Quoteable / Simulation-ready / Execution-ready are read-only projections over these flags plus on-chain validation evidence from Stage 4. Migration always leaves all three flags at `0`.

### 4.5 `scan_window` and `scan_cursor`

```sql
window_key        TEXT PRIMARY KEY,
chain_id          INTEGER NOT NULL,
source            TEXT NOT NULL,
scan_type         TEXT NOT NULL,
subject           TEXT NOT NULL,       -- factory/manager/vault/registry address or '*'
from_block        INTEGER NOT NULL,
to_block          INTEGER NOT NULL,
state             TEXT NOT NULL,       -- pending | running | complete | partial | failed
coverage          TEXT NOT NULL,       -- exhaustive | bounded | provider_limited | unknown
canonical_head_hash TEXT,
cursor_block      INTEGER,
attempts          INTEGER NOT NULL DEFAULT 0,
last_error        TEXT,
created_at        TEXT NOT NULL,
updated_at        TEXT NOT NULL
```

API sketch:

```rust
claim_window(chain_id, source, scan_type, subject, from, to) -> WindowClaim;
mark_window(window_key, state, coverage, cursor_block, head_hash, error);
retry_failed(chain_id, source, scan_type) -> usize;
```

A window is `complete` only when the full requested range succeeded without detected truncation. `partial` and `failed` stay retryable. `coverage = bounded` never implies chain-wide completeness. The legacy `discovery_scan_watermark` remains as a read-only hint; it is never converted into completed windows.

### 4.6 `reorg_event` and `registry_meta`

```sql
reorg_event(id, chain_id, detected_at, old_head_hash, new_head_hash,
            common_ancestor, affected_windows_json, status)

registry_meta(key TEXT PRIMARY KEY, value TEXT NOT NULL)
-- includes 'registry_revision'
```

Reorg procedure:

1. Detect hash mismatch and find the common ancestor with network calls **outside** SQLite write transactions.
2. In one short `BEGIN IMMEDIATE`: orphan affected observations (`valid = 0`, `orphaned_at`), rewind affected cursors/windows to `pending`, recompute affected status flags, bump `registry_revision`.
3. Commit, then rescan affected ranges in separate transactions.

### 4.7 Legacy archive and compatibility snapshot

```sql
pool_info_v13_archive   -- exact renamed legacy table, never written again
pool_legacy_snapshot    -- physical 22-column rows used only by compatibility views
```

No other physical table may be named `pool_info`.

---

## 5. Compatibility contract

### 5.1 Views

Create:

```sql
CREATE VIEW pool_info_legacy_complete AS
SELECT address, token0, token1, fee, dex_type, tick_spacing, creation_block,
       pool_id, factory, is_stable, underlying_tokens, balancer_pool_type,
       hook_address, bin_step, maturity_timestamp, dex_name,
       token0_symbol, token1_symbol, tvl_usd, volume_usd_24h,
       volume_usd_30d, derivation_version
FROM pool_legacy_snapshot;

CREATE VIEW pool_info AS SELECT * FROM pool_info_legacy_complete;
```

Both views get `INSTEAD OF INSERT/UPDATE/DELETE` triggers that call `RAISE(ABORT, 'pool_info is read-only; use InventoryWriter')`. The exact 22-column order matches `row_to_pool_info()` at `cache/store/mod.rs:376-459`.

### 5.2 Adapters

- `SqliteStore::put_discovered_pool(&PoolInfo)` keeps its signature. It converts the input into a `discovery` observation through `InventoryWriter`, then refreshes `pool_legacy_snapshot`. It does **not** write a view.
- `get_discovered_pool()` and `list_discovered_pools()` keep their signatures and read from `pool_info_legacy_complete`.
- Direct readers such as `cli/examples/cacheq.rs:16-37` keep working unchanged.
- `pool_address` is the view key for legacy rows; singleton rows keep their stored `legacy_address`, so old readers see a stable address while canonical identity is the full PoolId.

### 5.3 One writer

`InventoryWriter` is the only component allowed to write Master tables. Discovery jobs, remote candidates, registry reads and enrichment call it through the same `record_observation()` API. Live never writes inventory.

Deterministic conflict rules:

1. On-chain evidence (`receipts`, `getlogs`, `poll`, `registry`) outranks `external` and `enrichment`.
2. A lower-authority observation never overwrites identity or readiness from a higher-authority one; it is stored with `conflict = 1` when it disagrees.
3. Immutable identity fields are first-wins from the highest authority; mutable metadata takes the newest authoritative observation.
4. Zero-token rows, zero `creation_block`, unknown DEX labels and zero/unknown TVL are retained and marked provisional/unknown.
5. Migration never marks a legacy row `canonical` or `execution_ready`.

---

## 6. Migration plan (`v13 → v14`)

### 6.1 Preflight (outside the write transaction is fine)

- Inspect `PRAGMA table_info(pool_info)` and `PRAGMA table_info(schema_version)`; derive actual available columns.
- Accept three fixtures: full v13, sparse v13 (missing optional columns), and empty/unseeded `schema_version`.
- Do **not** trust `MAX(schema_version.version)`; the current code can report v0 for a full v13 database.
- Record legacy row count and a hash of the archived payload for the migration log.

### 6.2 One atomic transaction

`BEGIN IMMEDIATE` wraps all of the following:

1. `INSERT OR IGNORE INTO schema_version(version) VALUES (0)` so stamping cannot no-op.
2. Create all v14 tables, indexes, views and write-rejection triggers.
3. `ALTER TABLE pool_info RENAME TO pool_info_v13_archive`.
4. Create `pool_legacy_snapshot` and copy every legacy row, normalizing missing optional columns to `NULL`/defaults. No row is skipped.
5. For each archived row:
   - derive a provisional `evm_pool_address` PoolKey from `legacy_address`;
   - if the row has `pool_id` and a singleton family, also store `pool_id`, manager/vault candidate and raw row JSON as a provisional singleton observation — **do not** replace the address key yet;
   - insert a `legacy_v13` observation with the complete row as `payload_json`;
   - insert roles for address, factory, pool_id, hook and underlying tokens when present;
   - insert `pool_status` with all readiness flags `0` and `validation_state = 'unverified'`.
6. Seed `registry_meta('registry_revision', '1')` (or existing max + 1 if the key exists).
7. Stamp version 14 with `INSERT OR REPLACE INTO schema_version(version) VALUES (14)`.
8. `COMMIT`.

Failure at any point rolls back to the untouched v13 database. Reopening reruns preflight and is idempotent if the archive and v14 tables already exist.

### 6.3 Explicit non-conversions

- The old chain watermark is **not** converted into completed per-source windows; it may only be copied into `registry_meta` as an `unknown`-coverage hint.
- Legacy rows are archived/linked, not promoted.
- No legacy row is auto-admitted to Live; Stage 4 must verify on-chain first.

---

## 7. Targeted correctness fixes included here

| Fix | Current location | Required behavior |
|---|---|---|
| V4 full PoolId | `pool/discovery/v4.rs:30-74` | Retain `topics[1]` as 32-byte `pool_id`; synthetic address moves to role/`legacy_address`; canonical key uses singleton namespace. |
| Metric identifier | `pool/discovery/metric.rs:18-46` | Retain `bytes32 poolId` from data word 1 in roles and observation payload. |
| Fluid identifier | `pool/discovery/fluid.rs:19-41` | Retain `dexId` from `topics[2]`; zero tokens remain allowed but provisional. |
| Infinity typed identity | `pool/discovery/infinity.rs:36-77` | Retain PoolId and the `parameters` word as `pool_type`/protocol params; unknown tags remain `0x...`. |
| Topic-length guards | decoders listed in §2 | Every decoder checks `topics.len()` and data length before indexing; malformed logs return `None`, never panic. |
| `factory_pools.drain()` | `pool/discovery/mod.rs:1392,1434` | Keep the map alive while building metadata results, or clone needed values before draining; `underlying_tokens` must survive. |
| Destructive caps | `jobs/discover.rs:144-162,583-596,194-196`; `pool/discovery/mod.rs:1774-1781` | `min_tvl` becomes ranking/display only; `max_pools` no longer truncates persistence; zero-token rows enter Master as provisional. |

---

## 8. Test plan

Sanitized temporary SQLite copies only; no production cache is touched.

| Test | Assertion |
|---|---|
| `migration_full_v13` | All rows preserved, archive exists, view has 22 columns, `integrity_check` and `foreign_key_check` OK. |
| `migration_sparse_v13` | Missing optional columns are normalized to NULL; no failure from absent columns. |
| `migration_empty_schema_version` | Preflight seeds version, stamps 14, reopen is idempotent. |
| `migration_crash_rollback` | Injected failure mid-transaction leaves original v13 readable and unmodified. |
| `legacy_unresolved_retention` | Unknown DEX, zero tokens, zero `creation_block`, zero/unknown TVL rows survive as provisional with all readiness flags `0`. |
| `legacy_singleton_link` | Legacy V4/Infinity/Balancer rows with `pool_id` keep address key plus provisional singleton role. |
| `singleton_roundtrip` | V4/Infinity/Balancer canonical text encodes/decodes full PoolId; synthetic address never becomes the key. |
| `identifier_retention` | Metric `poolId`, Fluid `dexId`, Infinity `parameters` survive in observations. |
| `malformed_topic_no_panic` | Short topic/data vectors return `None` from every decoder. |
| `factory_pools_association` | `underlying_tokens` present after `build_discovery_output`. |
| `view_write_rejection` | INSERT/UPDATE/DELETE on `pool_info` and `pool_info_legacy_complete` abort. |
| `adapter_roundtrip` | `put_discovered_pool` / `get_discovered_pool` / `list_discovered_pools` work through the new writer. |
| `failed_window_retry` | `failed`/`partial` windows are never `complete`; `retry_failed` re-claims them. |
| `no_persistence_cap` | Rows beyond legacy `max_pools` still persist; `min_tvl` only affects ranking. |
| `old_reader_smoke` | `cacheq`, discover CLI list and Live initialization read the view without error; no legacy row is auto-admitted. |

---

## 9. Acceptance criteria

- No Master deletion for unknown TVL, unsupported family, zero reserves or zero tokens.
- Every legacy row is archived and linked; none is auto-admitted to Live.
- Failed/partial windows are never marked complete.
- Existing readers and writers keep working through adapters; direct writes to `pool_info` fail safely.
- Full PoolIds survive identity, decoding and round-trip for V4, Balancer V2 and Infinity.
- `integrity_check`, `foreign_key_check`, crash/reopen and old-reader smoke tests pass.

## 10. Decisions required before implementation

1. Approve the exact `PoolKey` text forms and singleton namespaces in §3.2.
2. Approve that both `pool_info` and `pool_info_legacy_complete` are read-only views over `pool_legacy_snapshot`.
3. Approve that singleton-capable legacy rows stay address-keyed and provisional until Stage 4 on-chain verification.
4. Approve that `registry_revision` starts at `1` for a migrated DB and is bumped on reorg/status changes.
5. Approve implementing the fixes in §7 in the same Stage 2 change, rather than deferring them to Stage 4.

Once these are approved, implementation can proceed without further architecture research.
