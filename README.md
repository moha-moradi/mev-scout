# mev-scout

MEV scanner and backtester for 7 EVM chains (polygon, avalanche, bsc,
arbitrum, base, ethereum, optimism), with an explorer/forensics layer
grounded in SQLite and a local web UI.

## Layout

| Path | What it is |
|---|---|
| `core/` | Library: pool state, quoting, detectors, cache & explorer stores, config |
| `cli/` | `mev-scout` binary — `run`, `live`, `discover`, `tokens`, `scan`, `report`, `explorer`, … |
| `api/` | `mev-scout-api` binary — local-only HTTP API + serves the web UI |
| `web/` | React 19 + Vite + TypeScript + Tailwind 4 frontend |
| `cache/` | SQLite DBs (per-chain scanner cache + explorer stores), gitignored |

## Quick start (CLI)

```powershell
copy mev-scout.example.toml mev-scout.toml   # fill in RPC endpoints
cargo run -p mev-scout-cli -- --config mev-scout.toml run --blocks 100
```

## Web UI

The API serves the built frontend and exposes read endpoints over the
SQLite stores plus job control (spawn/stop the `mev-scout` CLI for
`run`, `live`, `discover`, `tokens`, `scan`, `report`,
`explorer index`). Local-only by design: binds `127.0.0.1`, no auth.

### One-time setup

```powershell
cargo build -p mev-scout-cli -p mev-scout-api   # API spawns target\debug\mev-scout.exe
npm --prefix web install                        # node 20+
npm --prefix web run build                      # -> web/dist (served by the API)
```

### Run (production, single origin)

```powershell
cargo run -p mev-scout-api -- --config mev-scout.toml
# open http://127.0.0.1:7600
```

### Run (dev, Vite HMR)

```powershell
powershell -ExecutionPolicy Bypass -File scripts\dev.ps1
# UI  -> http://127.0.0.1:5173  (Vite dev server, proxies /api)
# API -> http://127.0.0.1:7600
```

`scripts\dev.ps1` flags: `-buildFirst` (build both first), `-noApi` /
`-noWeb`, `-port`, `-webPort`, `-apiBinary`, `-webDir`.

### API surface (summary)

`GET /api/health`, `/api/chains`, `/api/config` (sanitized — RPC keys
are never returned), `PUT /api/config` (non-secret fields; chain switch
swaps the active DBs), `/api/explorer/{feed,stats,overview,top,ops,op/:tx}`,
`/api/results[/:run_id[/validation|/pnl]]`, `/api/runs`,
`/api/opportunities[/runs]`, `/api/pools`, `/api/sync`,
`POST /api/jobs` + `/api/jobs/:id{,/log,/progress,/stop}` (allowlisted
commands only, one running job at a time).

## Configuration & secrets

Copy `mev-scout.example.toml` to `mev-scout.toml` and reference API keys
via `${ENV_VAR}` placeholders — never commit live keys. The API writes
edits only to non-secret fields and round-trips the raw TOML so
placeholders on disk stay verbatim.

## Validation

```powershell
cargo test --workspace          # unit + integration
scripts\clippy.ps1              # clippy guardrail
```
