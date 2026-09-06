# CLI test harness notes

Integration tests for the `mev-scout` CLI. Offline tests (validation, config,
tokens, report, args) run on every `cargo test`; the network-facing tests are
gated behind `MEV_SCOUT_E2E=1` plus an RPC reachability probe and SKIP silently
otherwise.

## RPC rate-limit serialization (RPC_MUTEX)

`common::RPC_MUTEX` serializes the RPC-heavy tests **within one test binary
only**. It is a `static` per binary, and `cargo test` runs all test binaries in
parallel processes, so:

- `cli_run_replay`, `cli_live_mode`, `cli_data_foundation`, and
  `cli_network_coverage` each serialize internally via `common::rpc_lock()`
  (poison-recovering) followed by `common::ensure_gate_and_rpc` — the RPC
  probe runs *inside* the lock.
- Tests across **different binaries can still hit public RPCs concurrently**.
- `cli_e2e` does not participate in the mutex at all.

To serialize everything, run the gated binaries one at a time:

```powershell
$env:MEV_SCOUT_E2E = "1"
cargo test -p mev-scout-cli --test cli_e2e -- --test-threads=1
cargo test -p mev-scout-cli --test cli_run_replay -- --test-threads=1
cargo test -p mev-scout-cli --test cli_live_mode -- --test-threads=1
cargo test -p mev-scout-cli --test cli_data_foundation -- --test-threads=1
cargo test -p mev-scout-cli --test cli_network_coverage -- --test-threads=1
```

A cross-process file lock would be needed for full serialization under plain
`cargo test`; for now the convention above is the documented contract.

## Shared config files: last write wins

`common::make_cfg` / `common::temp_config` always write `ws/mev-scout.toml`.
Calling it twice with different extras targets the *same file* — the second
write replaces the first. Recreate the config immediately before each command
run when the extras differ.

## Harness helpers (common/mod.rs)

- `rpc_lock()` — poisoned-mutex-safe `RPC_MUTEX` guard.
- `ensure_gate_and_rpc(tag)` — `MEV_SCOUT_E2E` gate + RPC probe; `None` → SKIP.
- `run_timed(cmd, timeout)` — spawn with timeout + kill; returns captured IO.
- `make_cfg(ws, extras)` / `repo_config_str()` — config file from repo TOML.
- `newest_json_file(dir, prefix)` — newest `<prefix>*.json` by mtime.
- `extract_json_array(s)` — ANSI-tolerant JSON array extraction (tracing INFO
  lines share the process stdout, so JSON output is rarely "pure").
- `parse_receipt_match_pct(line)` — parse the replay match percentage.
