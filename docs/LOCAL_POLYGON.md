# Local Polygon PoS for mev-scout

One Ubuntu box runs Heimdall-v2 + Bor and `mev-scout`. You use it from the
laptop over SSH. Bor is a **pruned** tip RPC on localhost. Heimdall has to
be healthy or Bor stops following consensus; `mev-scout` never talks to
Heimdall.

`mev-scout` needs `live` (the default scan; the old `run` command is this)
and `explorer index` for chain id `137`. There is no web UI. Historical
`eth_call` stays on the public archive URL in
`mev-scout.local-polygon.toml`.

Official prerequisite tables say 8–16 cores, 32–64 GB RAM, and 4–8 TB.
Those numbers are the **full-history** sentry (Bor ~7 TB extracted). The
pruned snapshot this project uses is ~100 GB. Buying the official box
stores chain history `mev-scout` does not read.

**Target:** Heimdall-v2 (`heimdalld` ≥ v0.2) + Bor ≥ v2.0, mainnet.

One chain per box. Avalanche and BSC are separate machines — see
[`LOCAL_AVALANCHE.md`](./LOCAL_AVALANCHE.md) and [`LOCAL_BSC.md`](./LOCAL_BSC.md).

- [Full node binaries](https://docs.polygon.technology/pos/how-to/full-node/full-node-binaries)
- [Snapshots](https://docs.polygon.technology/pos/how-to/snapshots)
- Community index: [All4nodes](https://all4nodes.io/Polygon) (PublicNode pruned snaps are listed there)

## What you get

| Endpoint           | URL / port                          |
| ------------------ | ----------------------------------- |
| Bor JSON-RPC       | `http://127.0.0.1:8545`             |
| Bor WebSocket      | `ws://127.0.0.1:8546`               |
| Heimdall (ops)     | `http://127.0.0.1:26657`            |
| Bor P2P            | TCP+UDP `30303`                     |
| Heimdall P2P       | TCP `26656`                         |

## Hardware

|        | This box                                              |
| ------ | ----------------------------------------------------- |
| OS     | Ubuntu 22.04 or 24.04 (amd64)                        |
| CPU    | **4 vCPU**                                            |
| RAM    | **16 GB**                                             |
| Disk   | **500 GB** SSD                                        |

Why it is not smaller, and why it is not 1 TB+ / 32 GB:

- Oct 2026 PublicNode pruned snapshots: Bor ~109 GB on disk, Heimdall
  pruned ~10 GB. 500 GB covers extract temp and growth. If the datadir
  passes ~300 GB, stop and restore a fresh **pruned** snapshot instead of
  expanding the disk. Full-history Bor (~7 TB) is a different machine.
- 16 GB is Heimdall + Bor with a small cache + one `mev-scout` process.
  32 GB is the official full-node minimum, not this role. 8 GB is tight
  once both clients are catching up; don’t go there.
- 4 vCPU holds tip for a single local client. Catch-up after a long
  outage is the case that wants 8 cores — still not 16, and still not
  64 GB.

Build `mev-scout` before the clients are syncing:

```bash
cargo build -j 2 --release -p mev-scout-cli
```

## What this node will and will not answer

| Workload                                   | Local node                               |
| ------------------------------------------ | ---------------------------------------- |
| `live` (last 64 blocks) and `live --loop` | yes, once Bor `eth_syncing` is false    |
| `explorer index` of retained blocks       | yes                                      |
| `discover` over a long lookback           | only inside retained logs                |
| Historical `eth_call` / proofs            | no — public URL in `rpc_urls`            |

First `discover` on a fresh pruned snap: `discover --blocks 2000`.

## 1. Install

Pin the version args to current GitHub release tags. Do not reuse tags
from old blog posts.

```bash
sudo apt-get update && sudo apt-get install -y build-essential curl lz4 aria2 jq

curl -L https://raw.githubusercontent.com/0xPolygon/install/heimdall-v2/heimdall-v2.sh \
  | bash -s -- <heimdall_version> mainnet sentry

curl -L https://raw.githubusercontent.com/0xPolygon/install/main/bor.sh \
  | bash -s -- <bor_version> mainnet sentry

heimdalld version
bor version
```

Seeds: `seeds` / `persistent_peers` in `/var/lib/heimdall/config/config.toml`,
`bootnodes` in `/var/lib/bor/config.toml`, from the current Polygon seed
list. If genesis is missing:

```bash
cd /var/lib/heimdall/config
sudo curl -fsSL \
  https://storage.googleapis.com/mainnet-heimdallv2-genesis/migrated_dump-genesis.json \
  -o genesis.json
```

## 2. Pruned snapshots

Do not sync from genesis and do not download the full Bor snapshot.

1. From All4nodes / PublicNode, take **pruned Heimdall** and **pruned Bor**.
   Check the size before buying disk. If the “pruned” Bor archive is
   terabytes, it is mislabeled; skip it.
2. `sudo service heimdalld stop; sudo service bor stop`
3. Extract into the datadirs (or symlink, as in the
   [snapshot guide](https://docs.polygon.technology/pos/how-to/snapshots)).
4. `chown` to the `heimdall` and `bor` users.

```bash
sudo rm -rf /var/lib/heimdall/data /var/lib/bor/data/bor/chaindata
sudo ln -sfn /data/heimdall /var/lib/heimdall/data
sudo ln -sfn /data/bor/chaindata /var/lib/bor/data/bor/chaindata
sudo chown -R heimdall:nogroup /var/lib/heimdall
sudo chown -R bor:nogroup /var/lib/bor
```

Paths follow the provider’s README when they differ.

## 3. Bor RPC on localhost, small cache

In `/var/lib/bor/config.toml` (or the `[jsonrpc]` / `[jsonrpc.http]`
sections, depending on the file you got):

- HTTP `127.0.0.1:8545`, WS `127.0.0.1:8546`
- APIs: `eth`, `net`, `web3`, `txpool`, `debug`
- Cache around **2048** MB, not the 8–12 GB validator cache. On a 16 GB
  box Heimdall needs the rest.

Never bind `0.0.0.0`.

## 4. Heimdall, then Bor

```bash
sudo service heimdalld start
curl -s localhost:26657/status | jq '.result.sync_info.catching_up'
# wait until false

sudo service bor start
```

Starting Bor while Heimdall is still catching up is the usual failure mode.

## 5. Wait for Bor

```bash
curl -s -X POST http://127.0.0.1:8545 -H 'content-type: application/json' \
  -d '{"jsonrpc":"2.0","id":1,"method":"eth_chainId","params":[]}'
# "0x89" (137)

curl -s -X POST http://127.0.0.1:8545 -H 'content-type: application/json' \
  -d '{"jsonrpc":"2.0","id":1,"method":"eth_syncing","params":[]}'
```

Do not point `mev-scout` at Bor until `eth_syncing` is `false`.

```bash
echo 'SystemMaxUse=200M' | sudo tee /etc/systemd/journald.conf.d/size.conf
sudo systemctl restart systemd-journald
```

## 6. mev-scout

```bash
./target/release/mev-scout --config mev-scout.local-polygon.toml live
./target/release/mev-scout --config mev-scout.local-polygon.toml live --loop
./target/release/mev-scout --config mev-scout.local-polygon.toml explorer index --days 1
./target/release/mev-scout --config mev-scout.local-polygon.toml discover --blocks 2000
```

Concurrency in the local profile is 4. The toml’s `lookback_days = 30`
only works for logs this pruned node still has; otherwise `getLogs` falls
through to the public URL and the scan gets slow. Prefer `--blocks` until
you know the window. Raise concurrency only when `free -h` shows idle RAM
during a scan.

## 7. From the laptop

```bash
ssh user@server
cd ~/mev-scout && ./target/release/mev-scout --config mev-scout.local-polygon.toml live
```

CLI on the laptop:

```bash
ssh -N -L 8545:127.0.0.1:8545 user@server
```

Firewall: `22`, Bor P2P `30303` (TCP+UDP), Heimdall P2P `26656`. Not
`8545` / `8546`.

## Troubleshooting

| Symptom                        | Fix                                                              |
| ------------------------------ | ---------------------------------------------------------------- |
| Disk heading toward 1 TB      | Full snapshot, or a pruned node that was left to grow. Re-seed pruned |
| `:8545` refused                | `service bor status`. HTTP bind must be `127.0.0.1`              |
| Bor restarts / reorgs          | Heimdall `catching_up` still true. Stop Bor and wait             |
| `eth_getLogs` range errors     | Shrink `--blocks` / `--days`                                     |
| Swapping                       | Bor cache above ~2 GB, or `cargo build` while both clients are up |
| Historical `eth_call` fails    | Expected. Public URL in `rpc_urls`                               |

## Upgrade

Check [Heimdall-v2](https://github.com/0xPolygon/heimdall-v2/releases) and
[Bor](https://github.com/0xPolygon/bor/releases). Stop Bor, then Heimdall.
Re-run the install scripts with the new tags. Start Heimdall, wait until
`catching_up` is false, start Bor. If disk is the problem, restore a new
pruned snapshot; don’t convert this box into a full-history node.

## Related

- Avalanche: [`LOCAL_AVALANCHE.md`](./LOCAL_AVALANCHE.md)
- BSC: [`LOCAL_BSC.md`](./LOCAL_BSC.md)
- CLI: [`CLI.md`](./CLI.md)
- Profile: [`mev-scout.local-polygon.toml`](../mev-scout.local-polygon.toml)
