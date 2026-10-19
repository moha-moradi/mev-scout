# Local Avalanche C-Chain for mev-scout

One Ubuntu box runs AvalancheGo and `mev-scout`. You use it from the laptop
over SSH. The node is a pruned, state-synced C-Chain tip RPC on localhost.
It is not a validator, not a public RPC, and not a bot host.

`mev-scout` only needs the C-Chain (`chain_id = 43114`): `live` (the default
scan; the old `run` command is this) and `explorer index`. There is no web
UI. Deep historical `eth_call` stays on the public archive URL in
`mev-scout.local.toml`.

**Target:** AvalancheGo **≥ v1.15** (post-Helicon). Do not set `eth-apis`.

One chain per box. Polygon and BSC are separate machines — see
[`LOCAL_POLYGON.md`](./LOCAL_POLYGON.md) and [`LOCAL_BSC.md`](./LOCAL_BSC.md).

Official docs (install path, not the hardware target of this guide):

- [Install script](https://build.avax.network/docs/nodes/run-a-node/using-install-script/installing-avalanche-go)
- [System requirements](https://build.avax.network/docs/nodes/system-requirements)
- [C-Chain configs](https://build.avax.network/docs/nodes/chain-configs/primary-network/c-chain)
- [State-sync snapshot deletion](https://build.avax.network/docs/nodes/node-storage/state-sync-snapshot-deletion)

## What you get

| Endpoint             | URL                                  |
| -------------------- | ------------------------------------ |
| C-Chain JSON-RPC     | `http://127.0.0.1:9650/ext/bc/C/rpc` |
| C-Chain WebSocket    | `ws://127.0.0.1:9650/ext/bc/C/ws`    |
| P2P                  | TCP `9651`                           |

HTTP and WebSocket share port `9650`. Installer `private` RPC binds
localhost only. `mev-scout` uses HTTP (`rpc_urls`); it has no `ws_url`.

## Hardware

This is the small box that stays at tip and answers one local client.
Official validator pages say 8 vCPU / 16 GB and prefer 32 GB because a
staked node does more work than this. Do not buy that.

|        | This box                                                         |
| ------ | ---------------------------------------------------------------- |
| OS     | Ubuntu 22.04 or 24.04 (amd64 or arm64)                          |
| CPU    | **4 vCPU**                                                       |
| RAM    | **16 GB**                                                        |
| Disk   | **1 TB** local NVMe                                              |
| Sync   | Hours, I/O-bound                                                 |

Why it is not smaller:

- C-Chain **active state is ~500 GB** after state sync (Avalanche’s own
  figure). `mev-scout`’s SQLite cache is noise next to that. 8 GB RAM OOMs
  during state sync; the default trie caches alone are hundreds of MB, and
  the process sits well above that.
- The other 500 GB is headroom for state-sync snapshots, which grow ~150 GB
  / month if you leave them. Delete them (link above) or resync when the
  disk fills. That is how this stays a 1 TB disk instead of “1 TB and
  growing.”
- Archive mode is ~12 TB. Do not turn pruning or state sync off.

Windows is not an AvalancheGo platform. Build `mev-scout` with
`cargo build -j 2 --release` **before** the node is syncing — a parallel
compile plus state sync will swap a 16 GB box to death.

## What this node will and will not answer

| Workload                                              | Local node                                      |
| ----------------------------------------------------- | ----------------------------------------------- |
| `live` / bare `mev-scout` (default: last 64 blocks)  | yes, once `isBootstrapped`                      |
| `live --loop`                                         | yes                                             |
| `explorer index` of blocks since this node synced    | yes                                             |
| `discover` over a long lookback                       | only for logs the node has stored since sync    |
| Historical `eth_call` / proofs                        | no — public URL in `rpc_urls`                   |

First `discover` on a fresh node: `discover --blocks 2000`. A 30-day
lookback is not on disk yet.

## 1. Install

```bash
wget -nd -m https://raw.githubusercontent.com/ava-labs/avalanche-docs/master/scripts/avalanchego-installer.sh
chmod 755 avalanchego-installer.sh
./avalanchego-installer.sh
```

| Prompt               | Answer                               |
| -------------------- | ------------------------------------ |
| Connection type      | `1` home / dynamic, or `2` static IP |
| RPC public / private | `private`                            |
| State sync           | `on`                                 |

```bash
~/avalanche-node/avalanchego --version   # >= v1.15
sudo systemctl status avalanchego
```

Binary: `~/avalanche-node/`. Data: `~/.avalanchego/`.

Cap journald so logs cannot eat the disk:

```bash
echo 'SystemMaxUse=200M' | sudo tee /etc/systemd/journald.conf.d/size.conf
sudo systemctl restart systemd-journald
```

No other OS tuning. `noatime`, BBR, and scheduler tweaks do not change the
500 GB state.

## 2. C-Chain config

Post-Helicon defaults are already the minimal mev-scout node:
`state-sync-enabled` and `pruning-enabled` on, and `eth` / `txpool` /
`debug_trace*` enabled. Write an empty config so a leftover `eth-apis`
list cannot disable those:

```bash
mkdir -p ~/.avalanchego/configs/chains/C
echo '{}' > ~/.avalanchego/configs/chains/C/config.json
sudo systemctl restart avalanchego
```

A non-empty `apis` list **replaces** defaults. Leave it empty.

## 3. Wait until bootstrapped

```bash
curl -s -X POST http://127.0.0.1:9650/ext/info \
  -H 'content-type: application/json' \
  -d '{"jsonrpc":"2.0","id":1,"method":"info.isBootstrapped","params":{"chain":"C"}}'
```

Wait for `"isBootstrapped": true`, then:

```bash
curl -s -X POST http://127.0.0.1:9650/ext/bc/C/rpc \
  -H 'content-type: application/json' \
  -d '{"jsonrpc":"2.0","id":1,"method":"eth_chainId","params":[]}'
# "0xa86a" (43114)
```

## 4. mev-scout

`mev-scout.local.toml` points at localhost first and a public archive
second. On this box the file’s concurrency is deliberately small (4).
Raising `block_concurrency` / `rpc_concurrency` toward 32 is what makes
people buy 32 GB; do it only when `free -h` still shows idle RAM during a
scan.

```bash
cargo build -j 2 --release -p mev-scout-cli
./target/release/mev-scout --config mev-scout.local.toml live
./target/release/mev-scout --config mev-scout.local.toml live --loop
./target/release/mev-scout --config mev-scout.local.toml explorer index --days 1
./target/release/mev-scout --config mev-scout.local.toml discover --blocks 2000
```

`live` with no flags scans the last 64 blocks and exits. `explorer index`
without `--days` / `--loop` is the ingest; bare `explorer` is the revenue
report and does not need the node. Bound SQLite with `--days` /
`--duration`. Deleting `cache/*.sqlite` is safe.

## 5. From the laptop

SSH in and run the binary. RPC stays on `127.0.0.1`. Do not put Caddy,
TLS, or Basic Auth in front of it — nothing off-box should call this node.

```bash
ssh user@server
cd ~/mev-scout && ./target/release/mev-scout --config mev-scout.local.toml live
```

To run the CLI on the laptop instead, forward the port and keep the
firewall closed:

```bash
ssh -N -L 9650:127.0.0.1:9650 user@server
# laptop config: http://127.0.0.1:9650/ext/bc/C/rpc
```

Firewall on the server: SSH and P2P only.

```bash
sudo ufw allow 22/tcp
sudo ufw allow 9651/tcp
sudo ufw enable
```

## Troubleshooting

| Symptom                               | Fix                                                                 |
| ------------------------------------- | ------------------------------------------------------------------- |
| Connection refused on `:9650`         | `systemctl status avalanchego` — RPC is private (localhost)        |
| `isBootstrapped` stays false          | Wait. Watch `journalctl -u avalanchego -f` and disk I/O            |
| Disk climbs past ~700 GB              | Delete state-sync snapshots (official link above) or resync        |
| `eth_getLogs` range errors            | Shorter `--blocks` / `--days`. Fresh nodes have no pre-sync history |
| `txpool_*` / `debug_trace*` missing   | `config.json` is not `{}`. Reset it and restart                    |
| Box is swapping                       | Compile is still running, or concurrency was raised. `cargo` first, then the node, and keep concurrency at 4 |
| Historical `eth_call` fails           | Expected. The public URL in `rpc_urls` is the archive              |

## Upgrade

```bash
./avalanchego-installer.sh
~/avalanche-node/avalanchego --version
```

Data in `~/.avalanchego` is kept. Confirm `C/config.json` is still `{}`.

## Related

- Polygon: [`LOCAL_POLYGON.md`](./LOCAL_POLYGON.md)
- BSC: [`LOCAL_BSC.md`](./LOCAL_BSC.md)
- CLI: [`CLI.md`](./CLI.md)
- Profile: [`mev-scout.local.toml`](../mev-scout.local.toml)
