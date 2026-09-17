# mev-scout

MEV scanner and backtester for 7 EVM chains (polygon, avalanche, bsc,
arbitrum, base, ethereum, optimism), with an explorer/forensics layer
grounded in SQLite and a local web UI.

The local HTTP API (`mev-scout-api`) serves the web UI and talks to
SQLite plus in-process jobs (no CLI subprocess). It binds `127.0.0.1`
only — no auth by design.

## Prerequisites

| Tool | Version | Check |
|---|---|---|
| [Rust](https://rustup.rs/) (`rustc` + `cargo`) | stable | `cargo --version` |
| [Node.js](https://nodejs.org/) + npm | **20+** | `node --version` |
| PowerShell | Windows (repo scripts) | — |

All commands below assume you are in the **repo root**.

## Run the API + UI (first time)

### 1. Create config

```powershell
copy mev-scout.example.toml mev-scout.toml
```

Edit `mev-scout.toml` and set your RPC endpoints. Prefer `${ENV_VAR}`
placeholders (see the example file), then export keys in the same shell
before starting anything:

```powershell
$env:ALCHEMY_API_KEY = "..."
$env:DRPC_KEY        = "..."
$env:GETBLOCK_KEY    = "..."
```

Never commit live API keys. Unset placeholders stay literal and fail
loudly at the provider.

### 2. Pick a run mode

**Dev (recommended while working on the UI)** — API + Vite with hot
reload. One script builds (first time) and starts both:

```powershell
powershell -ExecutionPolicy Bypass -File scripts\dev.ps1 -buildFirst
```

| Service | URL |
|---|---|
| UI (Vite, proxies `/api`) | http://127.0.0.1:5173 |
| API | http://127.0.0.1:7600 |

Press **Ctrl+C** to stop both. Later runs can omit `-buildFirst` if the
API binary and `web/node_modules` already exist:

```powershell
powershell -ExecutionPolicy Bypass -File scripts\dev.ps1
```

Optional `dev.ps1` flags: `-noApi` / `-noWeb`, `-port`, `-webPort`,
`-apiBinary`, `-webDir`, `-config`.

`dev.ps1` frees ports `7600` / `5173` before starting (kills whatever
already owns them). Prefer that over leaving orphaned API/Vite processes
around.

### Stop / kill leftover sessions

| Situation | What to do |
|---|---|
| Started with `dev.ps1` | **Ctrl+C** in that terminal — stops API and Vite |
| Started with `cargo run` / `npm run dev` | **Ctrl+C** in each terminal |
| Ports still busy / process orphaned | Free them manually (below) |

```powershell
# Kill whatever is listening on the default API / Vite ports
foreach ($p in 7600, 5173) {
  Get-NetTCPConnection -LocalPort $p -ErrorAction SilentlyContinue |
    ForEach-Object { Stop-Process -Id $_.OwningProcess -Force -ErrorAction SilentlyContinue }
}

# Or stop the API binary by name
Get-Process -Name "mev-scout-api" -ErrorAction SilentlyContinue |
  Stop-Process -Force
```

Logs from `dev.ps1` (if you need to debug a crash):
`$env:TEMP\mev-api-out.log`, `mev-api-err.log`, `mev-vite-out.log`,
`mev-vite-err.log`.

**Local optimized (single origin)** — release API binary serves `web/dist`.
One URL, no Vite. Still **localhost-only** (see [Production scope](#production-scope)
below):

```powershell
cargo build -p mev-scout-api --release
npm --prefix web install
npm --prefix web run build          # -> web/dist

.\target\release\mev-scout-api.exe --config mev-scout.toml
# open http://127.0.0.1:7600
```

Sanity check either mode: http://127.0.0.1:7600/api/health

### 3. Manual two-terminal setup (optional)

If you prefer not to use `dev.ps1`:

```powershell
# Terminal 1 — API (allow Vite origin)
cargo run -p mev-scout-api -- --config mev-scout.toml --cors-origin http://127.0.0.1:5173

# Terminal 2 — UI
npm --prefix web install
npm --prefix web run dev
# open http://127.0.0.1:5173
```

### API CLI flags

| Flag | Default | Meaning |
|---|---|---|
| `--port` | `7600` | Listen port (`127.0.0.1`) |
| `--config` | `mev-scout.toml` | Config path |
| `--web-dir` | `web/dist` | Static frontend directory |
| `--data-dir` | `api_data` | Job log directory |
| `--cors-origin` | `http://localhost:5173` | Allowed Vite origin |

## Production scope

**What this repo supports today:** a local tool on one machine. The API
binds `127.0.0.1`, has **no authentication**, and anyone who can reach
it can start jobs and edit config (including raw `rpc_urls`). That is intentional for
workstation / private-VM use — not a hosted multi-user service.

| Mode | Meaning | Safe to expose on a network? |
|---|---|---|
| Dev (`scripts\dev.ps1`) | Debug API + Vite HMR | No |
| Local optimized (`--release` + `web/dist`) | Fast single-origin localhost stack | No — still no auth |
| Hosted / internet-facing | Not provided | Do not deploy as-is |

**Local optimized** (above) is the right “production” path for daily
use on your own box: release binary, built UI, secrets via `${ENV_VAR}`,
keep the process on loopback only.

**Hosted production** would need work this project does not ship:
authentication on `/api/*` (especially jobs and config), TLS (usually
via a reverse proxy), deliberate bind/firewall policy, stricter CORS,
process supervision (systemd / Windows Service / container), log
rotation, SQLite backup (or a stronger store if you need concurrent
writers), and ops/metrics. Until those exist, do not put
`mev-scout-api` behind a public URL.

## Layout

| Path | What it is |
|---|---|
| `core/` | Library: pool state, quoting, detectors, cache & explorer stores, config |
| `cli/` | `mev-scout` binary — `run`, `live`, `discover`, `tokens`, `scan`, `report`, `explorer`, … |

Pool discovery remotes are **GeckoTerminal + DexScreener** (plus on-chain
factory scans). DefiLlama yields is intentionally **not** a pool source
(UUID ids, no AMM pool contract addresses). Token metadata can be enriched
with `mev-scout tokens --enrich` (DefiLlama coins for symbol/decimals,
CoinGecko for name/icon URL).
| `api/` | `mev-scout-api` binary — local-only HTTP API + serves the web UI |
| `web/` | React 19 + Vite + TypeScript + Tailwind 4 frontend |
| `cache/` | SQLite DBs (per-chain scanner cache + explorer stores), gitignored |

## Quick start (CLI only)

Scanner/backtester without the web stack:

```powershell
copy mev-scout.example.toml mev-scout.toml   # if you have not already
cargo run -p mev-scout-cli -- --config mev-scout.toml run --blocks 100
```

## Configuration & secrets

Copy `mev-scout.example.toml` to `mev-scout.toml` and reference API keys
via `${ENV_VAR}` placeholders — never commit live keys. The Config UI can
edit `rpc_urls` / `rpc_rps` (same shape as CLI `--rpc-urls`); `GET /api/config`
returns the **raw** on-disk strings (placeholders visible, env-expanded keys
never returned). `PUT /api/config` writes the TOML and hot-reloads AppState.
Switching chain swaps the active SQLite DBs. Changing an env var’s *value*
still requires restarting the API process.

## API surface (summary)

`GET /api/health`, `/api/chains`, `/api/config` (raw disk `rpc_urls` /
placeholders; never env-expanded keys), `PUT /api/config` (including
`rpc_urls` / `rpc_rps`),
`/api/explorer/{feed,stats,overview,top,ops,op/:tx}`,
`/api/results[/:run_id[/validation|/pnl]]`, `/api/runs`,
`/api/opportunities[/runs]`, `/api/pools`, `/api/sync`,
`POST /api/jobs` + `/api/jobs/:id{,/log,/progress,/stop}` (allowlisted
commands only, one running job at a time). Jobs call `mev-scout-core`
in-process for `run`, `live`, `discover`, `tokens`, `scan`, `report`,
and `explorer index`.

## Validation

```powershell
cargo test --workspace          # unit + integration
cargo clippy --workspace --all-targets -- -D warnings
```
