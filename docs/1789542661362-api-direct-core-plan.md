# Plan: Make API independent — jobs talk directly to core (drop CLI subprocess)

## Context

- Current architecture: `web → api → mev-scout CLI (subprocess) → core`.
- The API's `JobManager` (`api/src/jobs.rs`, 396 lines) spawns the `mev-scout` binary as a
  subprocess for long-running jobs (`run`, `live`, `discover`, `tokens`, `scan`, `report`,
  `explorer index`), captures stdout/stderr into log files, and parses `Run ID: …` /
  NDJSON `{"stage":…}` `--progress json` events out of the child's stdout.
- Pain points with the subprocess model:
  - Fragile — job state is driven by parsing the child's stdout strings.
  - Overhead — each job is a separate OS process (spawn + IPC).
  - Error handling — child exit code + stderr must be converted into a structured error.
  - Cancellation — relies on killing process trees (`taskkill /T /F` / SIGTERM).
  - Constrained — the job allowlist is limited to CLI subcommands.
- The CLI itself is a thin orchestrator over `core` (config → RPC init → fetch → detect →
  persist → render). The engine logic (fetch, replay, detection, discovery, explorer store)
  all lives in `mev-scout-core` and is directly callable from the API.

## Decision

- **Keep the CLI** as a developer tool / alternative entry point. It stays a thin
  orchestrator over `core` (unchanged behavior).
- **Make the API independent**: jobs run **in-process**, calling `core` functions directly,
  instead of spawning the CLI binary.
- Both `CLI` and `API` become first-class, layered directly on `core`:
  `CLI → core` and `web → API → core`.
- The subprocess job manager and NDJSON stdout-parsing contract are removed.

## Goals

1. Kill the CLI-binary dependency for API jobs.
2. Replace process-based progress reporting with typed channels.
3. Replace process-kill cancellation with cooperative `CancellationToken`.
4. Reduce job-management code and runtime overhead.
5. Keep CLI behavior intact (11 subcommands, tests green).

## Non-goals

- Removing the CLI binary or its commands.
- Signing/sending real transactions (out of scope, simulated PnL only).
- Multi-job concurrency (stays single-job-at-a-time for MVP).

## Architecture after the change

```
             ┌────────────────────┐
 CLI ───────►│                    │
             │   mev-scout-core   │
 API ───────►│   (engine + stores)│
             └────────────────────┘
```

- API keeps serving the built web UI and read endpoints against core's SQLite stores
  (`SqliteStore` / `ExplorerStore`), unchanged.
- API job execution runs `core` pipelines inside the API process (tokio tasks).

## Implementation steps

### 1. Rewrite the job runner (`api/src/jobs.rs`)

Replace `JobManager`'s subprocess logic with an in-process task runner:

- **`Command::run`** → `core::pipeline::BacktestRunner::run_range` /
  `run_range_hybrid` + `core::fetch::Fetcher` + `core::resolver::RangeResolver`,
  matching `cli/src/commands/run.rs` orchestration.
- **`Command::live`** → `core::pipeline` `run_once` / `run_loop` loop, matching
  `cli/src/commands/live.rs`.
- **`Command::discover`** → `core::pool::discovery::discover_pools`,
  matching `cli/src/commands/discover.rs`.
- **`Command::scan` / `tokens` / `report` / `explorer index`** → the corresponding
  `core` calls, matching `cli/src/commands/{scan,tokens,report}.rs` and
  `cli/src/commands/explorer/index.rs`.

New runner primitives:

- `tokio::sync::broadcast` channel for typed progress events → exposed to the UI via
  SSE or WebSocket (replaces NDJSON `--progress json` parsing).
- `tokio_util::sync::CancellationToken` for cooperative cancellation (checked in fetch
  loop, replay loop, discovery legs). Stop = cancel token; shutdown rejects new jobs
  and cancels the running one.
- Single-worker semaphore/mutex to keep one job at a time.
- Structured log capture via `tracing` target per job (still persisted to a log file if
  desired) instead of child stdout bytes.

Keep the existing DTO shapes (`JobInfo`, `JobStatus`, `ProgressEvent`) and the HTTP
surface (`POST /api/jobs`, `GET /api/jobs/:id`, `/log`, `/progress`, `stop`) stable so
the web UI and API tests keep working.

### 2. Remove subprocess infrastructure

- Drop the `--binary` CLI-arg handling for jobs, the binary-path resolution, and the
  job allowlist that references CLI commands.
- Remove `stub_main()` / `MEV_SCOUT_STUB=1` integration-test shim.
- Remove the `--progress json` parsing path in the API.

### 3. Keep CLI thin and intact

- CLI stays a direct orchestrator over `core`; no changes to its commands or output.
- Optionally remove the `--progress json` flag emission if no other consumer exists
  (only the API consumed it).

### 4. Preserve tests

- Update `api/tests/jobs.rs` to drive the new in-process runner (drop the stub binary,
  assert typed progress events + cooperative stop).
- Keep CLI tests untouched (should stay green — CLI behavior unchanged).
- Core tests untouched.

## Risks / mitigations

| Risk | Mitigation |
|---|---|
| Cancellation didn't break mid-state for long runs | `CancellationToken` checked at tight boundaries in fetch/replay/discovery loops; test with a long `run` job |
| `?Send` / `async_trait` constraints in `core::pipeline` | Run blockers (`run_range`, discovery) in `spawn_blocking` if a pipeline API is sync; keep the tokio task wrapper thin |
| Race around single-job guard during cancel/stop | Release the semaphore guard on drop, stop endpoint awaits task join with timeout |
| Progress event shape drift for the UI | Keep `ProgressEvent` DTOs; emit from typed events at the same stage boundaries the current NDJSON emits |
| Config re-resolve after chain switch mid-job | Job pins the chain/config snapshot at start (same as today); 409 on config PUT while a job runs (unchanged) |

## Out of scope (follow-ups)

- Real executor / signed transactions.
- Multi-job concurrency.
- Pushing job execution to a separate worker process for isolation (revisit if in-process blocking becomes a problem).