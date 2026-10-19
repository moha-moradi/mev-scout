# Local BNB Smart Chain for mev-scout

One Ubuntu box runs bnb-chain/bsc (geth) and `mev-scout`. You use it from
the laptop over SSH. The node is a **fast** tip RPC on localhost
(`--tries-verify-mode=none`): snapshot state, no trie, recent block
history only. It is not a validator and not a public RPC.

`mev-scout` needs `live` (the default scan; the old `run` command is this)
and `explorer index` for chain id `56`. There is no web UI. Historical
`eth_call` stays on the public archive URL in `mev-scout.local-bsc.toml`.

The previous revision of this guide used the official **pruned full**
snapshot (~1.9 TB compressed) plus `--cache 10000` and
`--tries-verify-mode=local`. That is a 64 GB / 3 TB machine. This project
does not verify state roots for anyone else, so that role is the wrong one.

**Target:** BSC geth **≥ v1.7.3** (what the 48Club fast snapshot loads),
mainnet.

One chain per box. Avalanche and Polygon are separate machines — see
[`LOCAL_AVALANCHE.md`](./LOCAL_AVALANCHE.md) and
[`LOCAL_POLYGON.md`](./LOCAL_POLYGON.md).

- [Fast node](https://docs.bnbchain.org/bnb-smart-chain/developers/node_operators/fast_node/)
- [48Club snapshots](https://github.com/48Club/bsc-snapshots) (`data.json` → `geth.none`)
- [bsc releases](https://github.com/bnb-chain/bsc/releases)

## What you get

| Endpoint  | URL                       |
| --------- | ------------------------- |
| JSON-RPC  | `http://127.0.0.1:8545`   |
| WebSocket | `ws://127.0.0.1:8546`     |
| P2P       | TCP+UDP `30311`           |

`mev-scout` uses HTTP only.

## Hardware

|        | This box                                                                 |
| ------ | ------------------------------------------------------------------------ |
| OS     | Ubuntu 22.04 or 24.04 (amd64)                                           |
| CPU    | **8 vCPU**                                                               |
| RAM    | **16 GB**                                                                |
| Disk   | **1.5 TB** local SSD                                                    |
| Network| Enough to pull a ~460 GB snapshot once                                  |

The fast-node page still prints 16 cores / 32 GB / 2 TB. That line assumes
`--cache 10000` (10 GB of cache) and a node serving other people. This box
uses `--cache 4096` and one local client.

Why it is not smaller:

- The small snapshot **is** ~461 GB compressed (48Club `geth.fast.*`,
  Oct 2026, `data.json` key `geth.none`). Extracted chaindata is larger
  than the archive. 1.5 TB holds both during extract; delete the
  `.tar.zst` after. A 1 TB disk usually dies mid-extract.
- `geth.full` is ~1.4 TB compressed. Official `pruneancient` is ~1.9 TB
  compressed. Neither is this setup.
- 8 vCPU is the floor for staying at tip on ~0.45–0.75 s blocks while
  also executing them. 4 vCPU falls behind. 16 GB fits geth at
  `--cache 4096` plus the `mev-scout` process. It does not fit a parallel
  `cargo build` at the same time.

Build first:

```bash
cargo build -j 2 --release -p mev-scout-cli
```

## What this node will and will not answer

Fast mode keeps the last **600k blocks** of bodies, txs, and logs (~3 days
at a 0.45 s block time) and only recent state (~128 blocks) for `eth_call`.
A fast node can return `0` instead of an error when state is missing.

| Workload                                      | Local node                                      |
| --------------------------------------------- | ----------------------------------------------- |
| `live` (last 64 blocks) and `live --loop`    | yes                                             |
| `explorer index` inside the 600k window      | yes                                             |
| `discover` inside that window                | yes (`lookback_days = 2` in the local profile) |
| `eth_call` older than ~128 blocks            | no — public archive URL                         |
| State-root verification                      | no — that is the full node you are not running |

There is no way to turn a fast datadir into a full node later. You would
download a different snapshot.

## 1. Install geth

```bash
sudo apt-get update && sudo apt-get install -y curl unzip aria2 zstd jq

wget -O geth "$(curl -s https://api.github.com/repos/bnb-chain/bsc/releases/latest \
  | grep browser_download_url | grep geth_linux | cut -d\" -f4)"
chmod +x geth && sudo mv geth /usr/local/bin/geth
geth version   # >= v1.7.3

wget -O mainnet.zip "$(curl -s https://api.github.com/repos/bnb-chain/bsc/releases/latest \
  | grep browser_download_url | grep mainnet | cut -d\" -f4)"
unzip -o mainnet.zip -d ~/bsc-config
```

## 2. Fast snapshot

Read `geth.none` in
[data.json](https://github.com/48Club/bsc-snapshots/blob/main/data.json)
and use that URL. Do not guess the filename; it changes. Oct 2026 example:
`geth.fast.128160000.tar.zst` (461 GB). Flags in that object are mandatory:

```text
--history.transactions=600000 --history.blocks=600000 --history.logs=600000 --tries-verify-mode=none
```

```bash
sudo mkdir -p /data/bsc
cd /data
sudo aria2c -s4 -x4 -k1024M -d /data -o bsc-fast.tar.zst 'PASTE_geth.none_URL'
sudo tar --use-compress-program='zstd -d --long=31' -xf /data/bsc-fast.tar.zst -C /data/bsc
sudo rm /data/bsc-fast.tar.zst
```

Point `--datadir` at the extracted geth datadir (the directory that
contains `geth/chaindata`, not `chaindata` itself). Layout follows the
tarball; check with `find /data/bsc -type d -name chaindata`.

## 3. systemd

Bind RPC to localhost. `--cache 4096` is the 16 GB setting. Copy the
history flags from `data.json` if they differ from the block below.

```ini
# /etc/systemd/system/bsc.service
[Unit]
Description=BSC geth fast node
After=network.target

[Service]
User=ubuntu
LimitNOFILE=1048576
ExecStart=/usr/local/bin/geth \
  --config /home/ubuntu/bsc-config/config.toml \
  --datadir /data/bsc \
  --cache 4096 \
  --syncmode full \
  --history.transactions=600000 \
  --history.blocks=600000 \
  --history.logs=600000 \
  --tries-verify-mode=none \
  --http --http.addr 127.0.0.1 --http.port 8545 \
  --http.api eth,net,web3,txpool,debug \
  --ws --ws.addr 127.0.0.1 --ws.port 8546 \
  --ws.api eth,net,web3
Restart=on-failure

[Install]
WantedBy=multi-user.target
```

`debug` is there for the occasional trace. Drop it if you only run `live`
and `explorer index`. Do not open `8545` / `8546`.

```bash
sudo systemctl daemon-reload
sudo systemctl enable --now bsc
sudo journalctl -u bsc -f
```

```bash
echo 'SystemMaxUse=200M' | sudo tee /etc/systemd/journald.conf.d/size.conf
sudo systemctl restart systemd-journald
```

If `config.toml` sets `HTTPHost = "0.0.0.0"`, the CLI flags above override
it. Confirm with `ss -ltnp | grep 8545` — the address must be `127.0.0.1`.

## 4. Wait for tip

```bash
curl -s -X POST http://127.0.0.1:8545 -H 'content-type: application/json' \
  -d '{"jsonrpc":"2.0","id":1,"method":"eth_chainId","params":[]}'
# "0x38" (56)

curl -s -X POST http://127.0.0.1:8545 -H 'content-type: application/json' \
  -d '{"jsonrpc":"2.0","id":1,"method":"eth_syncing","params":[]}'
# false when caught up
```

## 5. mev-scout

```bash
./target/release/mev-scout --config mev-scout.local-bsc.toml live
./target/release/mev-scout --config mev-scout.local-bsc.toml live --loop
./target/release/mev-scout --config mev-scout.local-bsc.toml explorer index --days 1
./target/release/mev-scout --config mev-scout.local-bsc.toml discover
```

Concurrency in the local profile is 4. `lookback_days = 2` matches the
600k log window. Do not set it to 14: those logs are not on this node, and
the public fallback is the slow path. Raise concurrency only when `free -h`
shows idle RAM during a scan.

## 6. From the laptop

```bash
ssh user@server
cd ~/mev-scout && ./target/release/mev-scout --config mev-scout.local-bsc.toml live
```

CLI on the laptop:

```bash
ssh -N -L 8545:127.0.0.1:8545 user@server
```

Firewall: SSH and the P2P port from `config.toml` (default `30311`). Not
`8545` / `8546`.

## Troubleshooting

| Symptom                         | Fix                                                                 |
| ------------------------------- | ------------------------------------------------------------------- |
| Disk > 1 TB right after extract | You took `geth.full` or official pruneancient. Wipe and use `geth.none` |
| Extract dies at 100% disk       | Archive and datadir overlapped. Free the `.tar.zst` or use a 1.5 TB disk |
| Connection refused `:8545`      | `systemctl status bsc`. Bind is `127.0.0.1`                         |
| `eth_getLogs` empty / too large | Outside 600k blocks. Shrink `--days` / `lookback_days`              |
| `eth_call` returns 0            | Fast-node missing state. Older than ~128 blocks → public archive URL |
| Swapping                        | `--cache` above 4096, or `cargo build` while geth is up             |
| Falling behind tip              | 4 vCPU is not enough here. This guide’s floor is 8                  |

## Upgrade

Stop `bsc`, replace `/usr/local/bin/geth` from the matching
[release](https://github.com/bnb-chain/bsc/releases), start, confirm
`eth_chainId` and `eth_syncing`. If the datadir is corrupt, re-seed from a
new `geth.none` snapshot. Do not “fix” it by downloading the full snapshot.

## Related

- Avalanche: [`LOCAL_AVALANCHE.md`](./LOCAL_AVALANCHE.md)
- Polygon: [`LOCAL_POLYGON.md`](./LOCAL_POLYGON.md)
- CLI: [`CLI.md`](./CLI.md)
- Profile: [`mev-scout.local-bsc.toml`](../mev-scout.local-bsc.toml)
