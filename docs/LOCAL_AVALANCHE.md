# Local Avalanche C-Chain next to mev-scout

Run your own AvalancheGo node and point `mev-scout` at it. This avoids
public / free-tier RPC rate limits, `eth_getLogs` caps, and flaky archive
support.

Official Avalanche docs (source of truth for flags and image tags):

- [Run AvalancheGo with Docker](https://build.avax.network/docs/nodes/run-a-node/using-docker)
- [C-Chain configs](https://build.avax.network/docs/nodes/chain-configs/primary-network/c-chain)
- [AvalancheGo releases](https://github.com/ava-labs/avalanchego/releases)

## What you get

| Endpoint | URL |
|---|---|
| C-Chain JSON-RPC (what mev-scout uses) | `http://127.0.0.1:9650/ext/bc/C/rpc` |
| AvalancheGo HTTP API | `http://127.0.0.1:9650` |
| P2P (staking / peers) | TCP `9651` |

`mev-scout` talks to the **C-Chain** only (`chain_id = 43114`). X/P APIs are
not required for scanning.

## Hardware (practical)

| Profile | Disk | RAM | Sync | Good for |
|---|---|---|---|---|
| **Tip / live** (state-sync) | ~200–400 GB SSD | 16 GB+ | hours → ~1 day | `discover`, `live`, near-tip `explorer index` |
| **Archive-ish** (no prune, no state-sync) | 1 TB+ SSD | 32 GB+ | days | deep history, `eth_getProof`, long backfills |

For day-to-day MEV scanning at tip, **state-sync + pruning** is enough.
Enable `debug-tracer` if you use `explorer show --trace`.

## 1. Install Docker

[Docker Desktop](https://www.docker.com/products/docker-desktop/) (Windows)
or Docker Engine (Linux). Confirm:

```powershell
docker --version
```

## 2. C-Chain config (before first start)

Create the chain config on the host (Docker mounts this into the container).
Pick **one** profile.

### Profile A — tip scanner (recommended start)

Fast bootstrap. Historical state older than the prune window is unavailable.

**PowerShell:**

```powershell
New-Item -ItemType Directory -Force -Path "$env:USERPROFILE\.avalanchego\configs\chains\C" | Out-Null
@'
{
  "state-sync-enabled": true,
  "pruning-enabled": true,
  "eth-apis": [
    "eth",
    "eth-filter",
    "net",
    "web3",
    "internal-eth",
    "internal-blockchain",
    "internal-transaction",
    "debug",
    "debug-tracer"
  ]
}
'@ | Set-Content -Encoding utf8 "$env:USERPROFILE\.avalanchego\configs\chains\C\config.json"
```

**bash:**

```bash
mkdir -p ~/.avalanchego/configs/chains/C
cat > ~/.avalanchego/configs/chains/C/config.json <<'EOF'
{
  "state-sync-enabled": true,
  "pruning-enabled": true,
  "eth-apis": [
    "eth",
    "eth-filter",
    "net",
    "web3",
    "internal-eth",
    "internal-blockchain",
    "internal-transaction",
    "debug",
    "debug-tracer"
  ]
}
EOF
```

`debug` / `debug-tracer` turn on `debug_traceTransaction` for
`mev-scout explorer show --trace`. Omit them if you only run `live` /
`discover`.

### Profile B — more history (slower, larger disk)

```json
{
  "state-sync-enabled": false,
  "pruning-enabled": false,
  "eth-apis": [
    "eth",
    "eth-filter",
    "net",
    "web3",
    "internal-eth",
    "internal-blockchain",
    "internal-transaction",
    "debug",
    "debug-tracer"
  ]
}
```

Changing sync/prune mode after a long sync often means wiping the DB and
re-syncing. Choose before you invest the disk time.

## 3. Launch AvalancheGo

Replace `v1.14.1` with a current tag from the
[AvalancheGo releases](https://github.com/ava-labs/avalanchego/releases) page.

**PowerShell (Docker Desktop):**

```powershell
docker run -d `
  --name avalanchego `
  -p 9650:9650 `
  -p 9651:9651 `
  -v "${env:USERPROFILE}\.avalanchego:/root/.avalanchego" `
  avaplatform/avalanchego:v1.14.1 `
  --http-host=0.0.0.0
```

**bash:**

```bash
docker run -d \
  --name avalanchego \
  -p 9650:9650 \
  -p 9651:9651 \
  -v ~/.avalanchego:/root/.avalanchego \
  avaplatform/avalanchego:v1.14.1 \
  --http-host=0.0.0.0
```

`--http-host=0.0.0.0` makes RPC reachable from the host through the published
port. Keep `9650` firewalled to localhost if the machine is on a public
network.

Useful ops:

```powershell
docker logs -f avalanchego
docker stop avalanchego
docker start avalanchego
```

## 4. Wait until C-Chain is bootstrapped

Do **not** point mev-scout at the node until this returns
`"isBootstrapped": true` for chain `C`:

```powershell
curl -X POST http://127.0.0.1:9650/ext/info `
  -H "content-type: application/json" `
  -d '{"jsonrpc":"2.0","id":1,"method":"info.isBootstrapped","params":{"chain":"C"}}'
```

Sanity-check the EVM tip:

```powershell
curl -X POST http://127.0.0.1:9650/ext/bc/C/rpc `
  -H "content-type: application/json" `
  -d '{"jsonrpc":"2.0","id":1,"method":"eth_blockNumber","params":[]}'
```

Also confirm `eth_chainId` → `0xa86a` (43114).

## 5. Point mev-scout at localhost

From the repo root:

```powershell
copy mev-scout.example.toml mev-scout.toml
```

Edit `mev-scout.toml`:

```toml
chain = "avalanche"

# Optional: raise once the node is local and dedicated to you
rps_limit = 0

[chains.avalanche]
chain_id = 43114

[chains.avalanche.rpc]
rpc_urls = ["http://127.0.0.1:9650/ext/bc/C/rpc"]
rpc_rps  = [0.0]   # 0 = unlimited for this provider
```

Verify the resolved config:

```powershell
cargo run -p mev-scout-cli -- --config mev-scout.toml config
```

## 6. Run mev-scout against the local node

```powershell
# Pool universe (on-chain-only avoids aggregator HTTP)
cargo run -p mev-scout-cli -- --config mev-scout.toml discover --source onchain

# One-shot tip scan (default entrypoint with this config)
cargo run -p mev-scout-cli -- --config mev-scout.toml

# Continuous tip follow
cargo run -p mev-scout-cli -- --config mev-scout.toml live --loop

# Optional forensics (needs tip + debug-tracer for --trace)
cargo run -p mev-scout-cli -- --config mev-scout.toml explorer index --duration 15m
```

On first `live` / bare `mev-scout`, token + pool bootstrap still runs if the
caches are empty. Prefer `discover --source onchain` when you want zero
third-party HTTP.

## Layout next to the tool

Typical two-terminal setup:

```text
Terminal A                          Terminal B
─────────────────────────────       ─────────────────────────────
docker logs -f avalanchego          cd mev-scout
                                    cargo run -p mev-scout-cli -- `
                                      --config mev-scout.toml live --loop
```

Data stays outside the git repo:

| Path | Contents |
|---|---|
| `%USERPROFILE%\.avalanchego\` (or `~/.avalanchego`) | AvalancheGo DB + configs |
| `mev-scout/cache/` | SQLite scanner / explorer DBs (gitignored) |
| `mev-scout/mev-scout.toml` | Local config (do not commit secrets) |

## Troubleshooting

| Symptom | Likely cause | Fix |
|---|---|---|
| Connection refused on `:9650` | Container not up / wrong host bind | `docker ps`; ensure `--http-host=0.0.0.0` and `-p 9650:9650` |
| `isBootstrapped: false` for long time | Still syncing | Wait; check `docker logs -f avalanchego` and disk I/O |
| `eth_getLogs` range errors | Window larger than node history (state-sync) | Shrink `--blocks` / lookback; or use Profile B |
| `debug_traceTransaction` missing | `debug-tracer` not in `eth-apis` | Add it to `C/config.json`, restart container |
| Historical `eth_call` / proof fails | Pruned or state-synced node | Expected on Profile A; use Profile B or a paid archive |
| Windows volume empty after recreate | Mount path typo | Remount `%USERPROFILE%\.avalanchego` explicitly |

## Upgrade AvalancheGo

Data lives on the host volume; recreate the container with a newer image tag:

```powershell
docker stop avalanchego
docker rm avalanchego
# same docker run … with avaplatform/avalanchego:<NEW_VERSION>
```

## Related

- Main quick start: [`README.md`](../README.md)
- CLI surface: [`ARCHITECTURE.md`](./ARCHITECTURE.md)
- Example public RPCs (fallback only): [`mev-scout.example.toml`](../mev-scout.example.toml)
