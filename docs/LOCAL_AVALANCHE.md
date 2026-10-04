# Local Avalanche C-Chain next to mev-scout

Run AvalancheGo natively on **Ubuntu 22.04 / 24.04** and point `mev-scout` at
it. This is the supported AvalancheGo OS path (installer script + `systemd`),
tuned for tip scanning: fast state-sync, pruning on (default), private RPC,
and the eth APIs mev-scout needs including `debug_traceTransaction`.

Official source of truth:

- [Install script](https://build.avax.network/docs/nodes/run-a-node/using-install-script/installing-avalanche-go)
- [C-Chain configs](https://build.avax.network/docs/nodes/chain-configs/primary-network/c-chain)
- [Upgrade your node](https://build.avax.network/docs/nodes/maintain/upgrade)
- [AvalancheGo releases](https://github.com/ava-labs/avalanchego/releases)

## What you get

| Endpoint | URL |
|---|---|
| C-Chain JSON-RPC | `http://127.0.0.1:9650/ext/bc/C/rpc` |
| C-Chain WebSocket | `ws://127.0.0.1:9650/ext/bc/C/ws` |
| AvalancheGo HTTP API | `http://127.0.0.1:9650` |
| P2P | TCP `9651` |

HTTP and WebSocket share port `9650`; WS comes up with the node (no separate
flag). With installer **`private`** RPC, both bind to localhost only.
`mev-scout` today points at HTTP via `rpc_urls` — it has no `ws_url` yet.

`mev-scout` uses the **C-Chain** only (`chain_id = 43114`).

## Hardware

| | Spec |
|---|---|
| OS | Ubuntu 22.04 or 24.04 (amd64 / arm64) |
| CPU / RAM / disk | Avalanche minimum: **8 vCPU / 16 GiB / SSD**; plan **~200–400 GB** with state-sync + pruning |
| Sync | Hours → ~1 day to tip (I/O-bound) |

Windows is not a supported AvalancheGo platform — run the node on Ubuntu.

## 1. Install AvalancheGo

```bash
wget -nd -m https://raw.githubusercontent.com/ava-labs/avalanche-docs/master/scripts/avalanchego-installer.sh
chmod 755 avalanchego-installer.sh
./avalanchego-installer.sh
```

Answer the prompts:

| Prompt | Answer |
|---|---|
| Connection type | `1` home / dynamic IP, or `2` cloud / static IP |
| RPC public / private | **`private`** (RPC only on this machine) |
| State sync | **`on`** |

Then:

```bash
sudo systemctl status avalanchego
sudo journalctl -u avalanchego -f
```

Binary: `~/avalanche-node/` · Data / configs: `~/.avalanchego/`

## 2. C-Chain config

Avalanche recommends overriding **only** non-default values. Defaults already
enable pruning and the core eth APIs. This file turns on **state-sync** (fast
tip bootstrap) and adds **`debug` / `debug-tracer`** for
`mev-scout explorer show --trace`.

Write it, then restart so the node picks it up **before** a long sync finishes
with the wrong settings:

```bash
mkdir -p ~/.avalanchego/configs/chains/C
cat > ~/.avalanchego/configs/chains/C/config.json <<'EOF'
{
  "state-sync-enabled": true,
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
sudo systemctl restart avalanchego
```

| Setting | Value | Why |
|---|---|---|
| `state-sync-enabled` | `true` | Fast tip sync (default is `false`) |
| `pruning-enabled` | *(default `true`)* | Leave unset — saves disk |
| `eth-apis` | defaults + `debug` + `debug-tracer` | tip scan + `debug_traceTransaction` |

Do not flip to archival (`state-sync` off / pruning off) on this host unless you
accept a multi-day sync and 1 TB+ disk — that is a different node role.

## 3. Wait until C-Chain is bootstrapped

Do not point mev-scout at the node until `"isBootstrapped": true` for chain `C`:

```bash
curl -X POST http://127.0.0.1:9650/ext/info \
  -H "content-type: application/json" \
  -d '{"jsonrpc":"2.0","id":1,"method":"info.isBootstrapped","params":{"chain":"C"}}'
```

Tip check:

```bash
curl -X POST http://127.0.0.1:9650/ext/bc/C/rpc \
  -H "content-type: application/json" \
  -d '{"jsonrpc":"2.0","id":1,"method":"eth_blockNumber","params":[]}'
```

Confirm `eth_chainId` → `0xa86a` (43114).

```bash
sudo systemctl status avalanchego
sudo journalctl -u avalanchego -f
```

## 4. Point mev-scout at localhost

```bash
cp mev-scout.example.toml mev-scout.toml
```

```toml
chain = "avalanche"
rps_limit = 0

[chains.avalanche]
chain_id = 43114

[chains.avalanche.rpc]
rpc_urls = ["http://127.0.0.1:9650/ext/bc/C/rpc"]
rpc_rps  = [0.0]

[discover]
source = "onchain"
```

```bash
cargo run -p mev-scout-cli -- --config mev-scout.toml config
```

## 5. Run mev-scout

```bash
cargo run -p mev-scout-cli -- --config mev-scout.toml discover
cargo run -p mev-scout-cli -- --config mev-scout.toml
cargo run -p mev-scout-cli -- --config mev-scout.toml live --loop
cargo run -p mev-scout-cli -- --config mev-scout.toml explorer index --loop --duration 15m
```

```text
Terminal A                          Terminal B
─────────────────────────────       ─────────────────────────────
sudo journalctl -u avalanchego -f   cd mev-scout
                                    cargo run -p mev-scout-cli -- \
                                      --config mev-scout.toml live --loop
```

| Path | Contents |
|---|---|
| `~/.avalanchego/` | DB + configs |
| `~/avalanche-node/` | Binaries |
| `mev-scout/cache/` | Scanner / explorer DBs (gitignored) |
| `mev-scout/mev-scout.toml` | Local config (do not commit secrets) |

## Troubleshooting

| Symptom | Fix |
|---|---|
| Connection refused on `:9650` | `sudo systemctl status avalanchego` — installer used `private` RPC |
| `isBootstrapped: false` a long time | Wait; watch `journalctl` and disk I/O |
| `eth_getLogs` range errors | Shrink lookback / `--blocks` (state-synced history window) |
| `debug_traceTransaction` missing | Confirm `debug-tracer` in `C/config.json`, then `sudo systemctl restart avalanchego` |
| Deep historical `eth_call` / proof fails | Expected with state-sync + pruning — use a paid archive RPC for that work |

## Upgrade

```bash
./avalanchego-installer.sh
```

The script detects the existing service and upgrades in place. Data in
`~/.avalanchego` is kept.

## Related

- Main quick start: [`README.md`](../README.md)
- CLI surface: [`ARCHITECTURE.md`](./ARCHITECTURE.md)
- Example public RPCs (fallback only): [`mev-scout.example.toml`](../mev-scout.example.toml)
