# CLI test harness notes

Integration tests for the `mev-scout` CLI. Offline tests (validation, config,
tokens, report, args) run on every `cargo test`; the network-facing tests are
gated behind `MEV_SCOUT_E2E=1` plus an RPC reachability probe and SKIP silently
otherwise.

## Live smoke contract (tip / RPC)

Always-on CI must not multiply tip smokes. The agreed surfaces are:

| Layer | Binary / test | Role |
|---|---|---|
| Core pipeline smoke | `core/tests/e2e.rs` (gated) | One end-to-end core path against RPC |
| CLI smoke | `cli_e2e` (gated) | One binary-level tip path |

Other gated CLI binaries (`cli_live_*`, `cli_data_foundation`, `cli_network_coverage`)
are specialized or demoted — do not add another tip smoke without replacing one
of the two above. Corpus floors (`explorer_corpus`, `mev_corpus`, `paper_corpus`)
stay opt-in and are not tip smokes.

## RPC rate-limit serialization (RPC_MUTEX)

`common::RPC_MUTEX` serializes the RPC-heavy tests **within one test binary
only**. It is a `static` per binary, and `cargo test` runs all test binaries in
parallel processes, so:

- `cli_live_report`, `cli_live_mode`, `cli_data_foundation`, and
  `cli_network_coverage` each serialize internally via `common::rpc_lock()`
  (poison-recovering) followed by `common::ensure_gate_and_rpc` — the RPC
  probe runs *inside* the lock.
- Tests across **different binaries can still hit public RPCs concurrently**.
- `cli_e2e` does not participate in the mutex at all.

To serialize everything, run the gated binaries one at a time:

```powershell
$env:MEV_SCOUT_E2E = "1"
cargo test -p mev-scout-cli --test cli_e2e -- --test-threads=1
cargo test -p mev-scout-cli --test cli_live_report -- --test-threads=1
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
- `extract_json_array(s)` — ANSI-tolerant JSON array extraction (tracing INFO
  lines share the process stdout, so JSON output is rarely "pure").

## RPC URLs in tests (API-key hygiene)

Tests never need live API keys. The gated suites resolve their endpoint via
`common::first_rpc_url()`:

1. `RPC_URL` env var (wins) — a fresh clone without `mev-scout.toml` runs with
   `RPC_URL=https://...` alone.
2. First `https://` URL in `mev-scout.toml` (the historical default).

`mev-scout.toml` itself supports `${ENV_VAR}` placeholders inside `rpc_url`
and `rpc_urls` (expanded at config load; unset vars stay
verbatim so failures are loud). See `mev-scout.example.toml` for a keyless
template — prefer it over committing live keys.
