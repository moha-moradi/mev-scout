# Local Avalanche C-Chain + Caddy (mev-scout)

Fresh-VPS runbook: bring up a **pruned, state-synced C-Chain tip RPC** on
`127.0.0.1:9650`, put Caddy in front for TLS + password, then run `mev-scout`
(`live`, `explorer index`, `discover`) from the server or the laptop.

This is **not a validator**, not a public RPC, and not a bot host. One chain per
box. Target: Ubuntu 22.04/24.04, **4 vCPU / 16 GB**, a **~180 GB system disk**
for the OS, binaries, reports and mev-scout SQLite files, plus a **1 TB
attached disk** for AvalancheGo's DB. AvalancheGo **≥ v1.15.3** (post-Helicon).
C-Chain only (`chain_id = 43114`). State sync gives ~500 GB active state;
snapshots grow, so keep `df -h` below 80% (see [Maintenance](#7-maintenance)).
Archive mode (~12 TB) is not used: deep historical `eth_call` stays on the
Dwellir archive URL in the profile.

## 0. Prerequisites (before the node starts syncing)

Build `mev-scout` **first**: a parallel `cargo build` plus state sync will swap
a 16 GB box. No git is required: there are no git dependencies, so upload the
source as a zip/tar over FTP/SFTP and build it on the VPS. Include `Cargo.toml`,
`Cargo.lock` and all crate sources; exclude `.git/`, `target/`, `cache/` and any
secrets. A Windows `.exe` built locally will **not** run on the VPS — either
build on Linux/WSL or upload a Linux `x86_64-unknown-linux-gnu` binary.

```bash
sudo apt update && sudo apt install -y curl wget jq sqlite3 ufw unzip
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
. "$HOME/.cargo/env"
# after uploading mev-scout-src.zip to $HOME:
unzip -q ~/mev-scout-src.zip -d ~/mev-scout
cd ~/mev-scout
cargo build -j 2 --release -p mev-scout-cli
cargo --version   # stable
```

Plain FTP sends credentials in clear text; prefer SFTP/SSH. If FTP is the only
option, keep it behind the firewall and delete the uploaded archive after the
build.

Run the installer as a normal user with sudo rights (**never** with `sudo`).

## 1. Install AvalancheGo

Mount the extra disk first. **Check `lsblk` carefully — formatting the wrong
device destroys the system.**

```bash
lsblk -f                                  # identify the new empty disk, e.g. /dev/sdb1
sudo mkfs.ext4 /dev/sdb1                  # only if it is new/empty
sudo mkdir -p /mnt/avalanche-db
sudo mount /dev/sdb1 /mnt/avalanche-db
sudo chown -R "$USER":"$USER" /mnt/avalanche-db
UUID=$(sudo blkid -s UUID -o value /dev/sdb1)
echo "UUID=$UUID /mnt/avalanche-db ext4 defaults,noatime 0 2" | sudo tee -a /etc/fstab
sudo mount -a
df -h /mnt/avalanche-db
```

Then install with the DB on that disk:

```bash
wget -nd -m https://raw.githubusercontent.com/ava-labs/avalanche-docs/master/scripts/avalanchego-installer.sh
chmod 755 avalanchego-installer.sh
./avalanchego-installer.sh --ip dynamic --rpc private --state-sync on \
  --db-dir /mnt/avalanche-db
jq -r '."db-dir"' ~/.avalanchego/configs/node.json   # /mnt/avalanche-db
```

For a static-IP VPS use `--ip <public-ip>` (or `--ip static`); for a fresh
reinstall add `--reinstall` (it overwrites node + chain configs, not the DB).
After `--reinstall`, rewrite `C/config.json` again (step 2). `--rpc private`
omits `http-host`, so HTTP/WS bind to localhost only.

```bash
~/avalanche-node/avalanchego --version            # >= v1.15.3
cat ~/.avalanchego/configs/node.json              # no "http-host": ""
systemctl status avalanchego
systemctl is-enabled avalanchego
ss -ltnp | grep 9650                              # 127.0.0.1:9650, not *:9650
echo 'SystemMaxUse=200M' | sudo tee /etc/systemd/journald.conf.d/size.conf
sudo systemctl restart systemd-journald
```

Binary: `~/avalanche-node/`. Data: `/mnt/avalanche-db/` (`db-dir`), not `~/.avalanchego/db`.

## 2. C-Chain config: `{}` = every RPC method mev-scout needs

On AvalancheGo ≥ v1.15 the defaults enable **web3, net, txpool, price, chain,
tx, subscription, filter, logs, avalanche, trace** — that covers every
production call in this repo. `db` and `profile` stay disabled and are not
used. A non-empty `apis` list **replaces** the defaults, so write an empty
config so a leftover list cannot turn methods off:

```bash
mkdir -p ~/.avalanchego/configs/chains/C
echo '{}' > ~/.avalanchego/configs/chains/C/config.json
sudo systemctl restart avalanchego
```

Do **not** set `apis` or the legacy `eth-apis`. Unrecognized keys are ignored
with a warning; an unknown name inside `apis` is a fatal config error.

Inventory of production calls (all POSTed to the configured URL verbatim;
`report` and `config` are offline):

| Used by | Methods |
| --- | --- |
| connectivity, tip, all ranges | `eth_blockNumber` |
| `live`, `explorer index`, replay | `eth_getBlockByNumber`, `eth_getBlockReceipts`, `eth_getBlockByHash` |
| `discover`, pool scan, `explorer --discover-pools` | `eth_getLogs` (address-less; never JSON-RPC batched) |
| replay / live state | `eth_getStorageAt`, `eth_getTransactionCount`, `eth_getBalance`, `eth_getCode` |
| pool metadata, sAVAX, funding, explorer evidence | `eth_call` (`latest` → local node; historical block/hash → archive peer) |
| `explorer audit` (optional) | `debug_traceTransaction`, then `trace_transaction` (AvalancheGo does not serve the latter; the audit records it as unavailable) |
| `live --exec anvil` (opt-in) | `anvil_*` on loopback Anvil only, not on this node |

`eth_getProof` is **not** called by the codebase; deep historical state reads
are `eth_call` at a block number/hash.

Probed against the Dwellir archive endpoint (key in env) at `latest` and at an
old block: `eth_chainId`, `eth_blockNumber`, `net_version`,
`web3_clientVersion`, `eth_gasPrice`, `eth_feeHistory`,
`eth_getBlockByNumber`, `eth_getBlockReceipts`, `eth_getBalance`,
`eth_getCode`, `eth_getStorageAt`, `eth_getTransactionCount`, `eth_call`,
`eth_getProof` — all answered. **Not** answered there: `txpool_status`
(`-32601`), address-less `eth_getLogs` and `debug_trace*` (403), and
`trace_transaction` (400). Those stay on the local node, which has all
default API namespaces enabled by `{}`.

Quick probe of the cheap namespaces on the local node:

```bash
for m in eth_chainId eth_blockNumber net_version web3_clientVersion \
         txpool_status eth_gasPrice eth_feeHistory; do
  curl -s http://127.0.0.1:9650/ext/bc/C/rpc -H 'content-type: application/json' \
    -d "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"$m\",\"params\":[]}"
  echo
done
tip=$(curl -s http://127.0.0.1:9650/ext/bc/C/rpc -H 'content-type: application/json' \
  -d '{"jsonrpc":"2.0","id":1,"method":"eth_blockNumber","params":[]}' | jq -r .result)
curl -s http://127.0.0.1:9650/ext/bc/C/rpc -H 'content-type: application/json' \
  -d "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"eth_getBlockByNumber\",\"params\":[\"$tip\",false]}" | jq -c '.result.number'
curl -s http://127.0.0.1:9650/ext/bc/C/rpc -H 'content-type: application/json' \
  -d "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"eth_getLogs\",\"params\":[{\"fromBlock\":\"$tip\",\"toBlock\":\"$tip\"}]}" | jq -c '.result | length'
```

`eth_getLogs` with **no address** is the key call: public RPCs reject it
(`-32701: please specify an address`), so it must hit this node.

## 3. Wait until the C-Chain is at tip

```bash
until curl -s http://127.0.0.1:9650/ext/info -H 'content-type: application/json' \
  -d '{"jsonrpc":"2.0","id":1,"method":"info.isBootstrapped","params":{"chain":"C"}}' \
  | jq -e '.result.isBootstrapped == true' >/dev/null; do
  echo "state syncing... $(date +%T)"; sleep 60
done
curl -s http://127.0.0.1:9650/ext/info -H 'content-type: application/json' \
  -d '{"jsonrpc":"2.0","id":1,"method":"info.getNetworkID","params":[]}'   # 1 = mainnet
```

Hours, I/O-bound. Follow with `journalctl -u avalanchego -f`.

## 4. mev-scout profile

`mev-scout.local.toml` uses one env var for the first RPC, so the same file
works on the server and on the laptop (the public peer stays as archive
failover):

```bash
# on the server
export AVAX_RPC_URL='http://127.0.0.1:9650/ext/bc/C/rpc'
# on the laptop (URL-encode @ : / # etc. in the password)
export AVAX_RPC_URL='https://mev:p%40ss@rpc.example.com/ext/bc/C/rpc'
# archive peer for historical state reads
export DWELLIR_AVALANCHE_KEY='...'

./target/release/mev-scout --config mev-scout.local.toml config   # offline check
```

The userinfo in the URL becomes an HTTP Basic `Authorization` header (verified
with this codebase's HTTP client), so it works with Caddy's `basic_auth`.
Dwellir is not in the built-in endpoint catalog, so historical calls reach it
through the generic fallback: the pruned local node fails fast on old state,
then Dwellir answers (the code skips state-unavailable providers without
penalty). Swap in `https://avalanche-c-chain.publicnode.com` if you have no key.
Concurrency stays at 4 (`block_concurrency`, `rpc_concurrency`) so node + CLI
fit in 16 GB; raise only when `free -h` shows idle RAM during a scan.
`batch_rpc` affects the library backtest only — `live` leaves batching off and
`explorer index` always chunks by `rpc_batch_blocks` (AvalancheGo's
`batch-request-limit` default is 1000, so 4 is safe).
If IPv6 is broken on the VPS, `export MEV_SCOUT_FORCE_IPV4=1`.

**Database: keep SQLite.** This binary has `rusqlite` only — no PostgreSQL
driver, DSN setting or migrations. Moving to Postgres would mean rewriting the
whole store layer; it is not a config switch. Both stores already use WAL
(`journal_mode=WAL`, `synchronous=NORMAL`, `busy_timeout`) and are fine for a
single writer. Defaults stay on the system disk under `./cache/`
(`./cache/avalanche-mev-scout.sqlite` and
`./cache/explorer-avalanche.sqlite`). If the 180 GB fills, move them explicitly;
`db_path` is a **top-level** key (the `output` table is flattened), while the
explorer DB uses its named section:

```toml
# mev-scout.local.toml — optional, only if needed
db_path = "/mnt/avalanche-db/mev-scout/cache.sqlite"

[explorer]
db_path = "/mnt/avalanche-db/mev-scout/explorer.sqlite"
```

Do not run `discover`, `live --loop` and `explorer index` against the same DB
at the same time: SQLite allows concurrent readers, but there is only one
writer.

## 5. Caddy (TLS + password, only if you need remote RPC)

Prefer SSH + `ssh -L 9650:127.0.0.1:9650` if only you use it. If you want a
named HTTPS endpoint, keep AvalancheGo's own RPC private and let Caddy be the
only public door:

```bash
sudo apt install -y caddy
caddy hash-password            # prompts; paste the hash below (avoid shell history)
sudo tee /etc/caddy/Caddyfile >/dev/null <<'EOF'
{
    email admin@example.com
}

rpc.example.com {
    @mev path /ext/bc/C/rpc /ext/bc/C/ws /ext/info
    # Optional second lock for a fixed laptop IP:
    # @mev remote_ip 203.0.113.7/32
    handle @mev {
        basic_auth {
            mev $2a$14$REPLACE_WITH_CADDY_HASH
        }
        reverse_proxy 127.0.0.1:9650
    }
    handle {
        abort
    }
}
EOF
sudo caddy validate --config /etc/caddy/Caddyfile
sudo systemctl enable --now caddy
sudo systemctl reload caddy
```

Never expose `9650`, `/ext/admin`, `/ext/bc/P/rpc` or the P2P API through
Caddy. Firewall: **22/tcp** (adjust if your SSH port differs), **9651/tcp**
(P2P; also open it in the VPS security group or forward it on a home router),
**80/tcp + 443/tcp** (Caddy). Add SSH *before* `ufw enable`, and verify:

```bash
sudo ufw default deny incoming
sudo ufw allow 22/tcp
sudo ufw allow 9651/tcp
sudo ufw allow 80/tcp
sudo ufw allow 443/tcp
sudo ufw enable
sudo ufw status verbose
curl -u 'mev:p@ss' https://rpc.example.com/ext/bc/C/rpc \
  -H 'content-type: application/json' \
  -d '{"jsonrpc":"2.0","id":1,"method":"eth_blockNumber","params":[]}'
```

## 6. Smoke tests (small ranges, then real ones)

Run these once the node is bootstrapped; `tip` is a recent block number:

```bash
cd ~/mev-scout
tip=$(curl -s http://127.0.0.1:9650/ext/bc/C/rpc -H 'content-type: application/json' \
  -d '{"jsonrpc":"2.0","id":1,"method":"eth_blockNumber","params":[]}' | jq -r .result)

# explorer: 1000 most recent blocks, writes results/explorer-avalanche-<range>.html
R=$((tip-999))
./target/release/mev-scout --config mev-scout.local.toml explorer index \
  --from-block $R --to-block $tip --out results/explorer-smoke.html

# discover: start small, then the real 5000-block (~2.8 h) pass
./target/release/mev-scout --config mev-scout.local.toml discover --blocks 20
./target/release/mev-scout --config mev-scout.local.toml discover --blocks 5000

# live: one-shot 64 blocks (bare command is the same), then a bounded loop
./target/release/mev-scout --config mev-scout.local.toml live --blocks 64
./target/release/mev-scout --config mev-scout.local.toml live --loop --duration 1m
```

`explorer index --window 24h` is ~43,000 C-Chain blocks; use exact bounds for
smoke tests. Deleting `cache/*.sqlite` is safe (it resets caches, not the
node).

### Opening the HTML reports

The file is self-contained (inline JSON + vendored ECharts), so the simplest
access is to download it over SFTP/FTP and open `explorer-smoke.html` in a
browser on the laptop. A headless VPS will not pop a browser window; the
`xdg-open` attempt is harmless.

To view it in a browser on the server's address, add a second Caddy site (DNS
A record for `reports.example.com` → VPS IP, reuse the same hash from step 5):

```bash
sudo mkdir -p /var/www/mev-reports
sudo cp ~/mev-scout/results/explorer-smoke.html /var/www/mev-reports/
sudo chown -R caddy:caddy /var/www/mev-reports
sudo tee -a /etc/caddy/Caddyfile >/dev/null <<'EOF'

reports.example.com {
    basic_auth {
        mev $2a$14$REPLACE_WITH_SAME_CADDY_HASH
    }
    root * /var/www/mev-reports
    file_server
}
EOF
sudo caddy validate --config /etc/caddy/Caddyfile
sudo systemctl reload caddy
```

Open `https://reports.example.com/explorer-smoke.html`. External token icons
or links still need internet; the chart/table itself does not. To regenerate
later without re-fetching blocks, rerun the same `explorer index ... --out`
command (idempotent); bare `explorer --out results/all.html` emits the all-time
dashboard.

**Can a fresh node index the last month of pools? No.** Bare `discover` (there
is no `--days` CLI flag in this build) uses `[discover].lookback_days = 30`
→ `30 × 43,200 = 1,296,000` blocks. Every window issues address-less
`eth_getLogs` for DEX activity plus 12 factory scanners per ~10,000-block step
(~1,700 log calls). A state-synced node stores neither the blocks nor the logs
from before the day it synced, so those calls fail and the code logs
`Skipping range … after 3 retries` and returns empty — a near-empty universe,
not a loud error. Log scans are pinned to the `rpc_rps = 0` local provider;
Dwellir is **not** a substitute: it returns 403 for every `eth_getLogs`, even
with `address` + `topic` (probed). To get a real 30-day pool index: keep this
node online for 30 days and then run bare `discover`, or use a full/archive
node that serves address-less logs. Until then use `discover --blocks N` with
`N` inside the node's own uptime.

A different option is a **current pool list, not a historical index**: set
`[discover].source = "remote"` in the TOML (there is no `--source` CLI flag;
this profile currently has `onchain`). It skips the on-chain factory/activity
scan entirely — no `eth_getLogs` at all — and fetches the TVL-ranked **top**
pools from GeckoTerminal + DexScreener over plain HTTPS (`min_tvl = 25000`,
`max_pools = 1000` by default). New rows get `creation_block = 0`, so it does
not recover creation history or the missing month, and it is not the full
universe. It still needs an RPC for the startup check and, with defaults,
`eth_call`/Multicall3 at `latest` plus pool health probes; set
`resolve_remote_metadata = false` and `health_check = false` for a listing-only
run. Verified with a 5-pool cap: `5 pool(s) in 0 active blocks`, all with
`creation_block = 0`.

**One-off 30-day pool list without 30 days of uptime:** don't use `discover`.
`explorer index` fetches receipts (publicnode serves 30-day-old receipts —
verified) and stores every decodable swap's `pool` in the explorer SQLite,
even pools unknown to the discovery cache. Use a public-only TOML, no
`--discover-pools`, then query:

```bash
# copy mev-scout.local.toml → mev-scout.public.toml and set:
# rpc_urls = ["https://avalanche-c-chain.publicnode.com", "<Dwellir archive URL>"]
A=$((tip-1296000)); B=$tip      # 30 days × 43,200 blocks
./target/release/mev-scout --config mev-scout.public.toml explorer index \
  --from-block $A --to-block $B
sqlite3 cache/explorer-avalanche.sqlite \
  "SELECT DISTINCT pool, amm FROM swaps WHERE block_number BETWEEN $A AND $B;"
```

That is ~1.3M blocks (~325k batched calls with `rpc_batch_blocks = 4`): hours,
not minutes, and it only covers DEXes whose swap topics the decoder knows. It
gives **active pools**, not `creation_block` or `dex_name`.

Notes from actual CLI smoke runs (public peers, not a local node):
`explorer index` over 11 blocks worked (~21 s); `live --blocks 3` finished but
publicnode capped `eth_getLogs` at 10 addresses (403) and lacked state at older
blocks; `discover` failed with `-32701` on address-less `eth_getLogs`. Dwellir
serves historical state but also rejects address-less logs (403) and debug
traces. That is why block/log/trace traffic must hit this node.

**Why `explorer index` still works on public RPCs:** normal indexing fetches
`eth_getBlockByNumber` + `eth_getBlockReceipts` (batched) and decodes the
receipt logs **locally** against the cached pool registry
(`address → token0/token1, dex_name, factory`). It does **not** issue
address-less or address-list `eth_getLogs`; only `explorer index
--discover-pools` does, and that flag belongs on the local node. Cross-DEX
tagging reads `dex_name` from that registry (`is_cross_dex = unique venues > 1`,
basis `protocol`); if names are missing it falls back to AMM family and is less
reliable. Seed the cache once with local `discover --blocks N` or
`[discover].source = "remote"`, then index with public RPCs. Pools absent from
the cache can still be decoded by topic and have tokens paired from Transfer
logs, but venue tags, fee/skim metadata and full route resolution degrade —
so the cache quality decides detection quality.

## 7. Maintenance

- **Disk ≥80% on `/mnt/avalanche-db`:** no offline pruning on post-Helicon
  C-Chain — resync. Since this node is not a validator, in-place is enough:
  ```bash
  sudo systemctl stop avalanchego
  sudo mv /mnt/avalanche-db /mnt/avalanche-db.old-$(date +%F)
  sudo mkdir -p /mnt/avalanche-db && sudo chown "$USER":"$USER" /mnt/avalanche-db
  sudo systemctl start avalanchego
  # wait for isBootstrapped again, then:
  sudo rm -rf /mnt/avalanche-db.old-*
  ```
  Needs free space for old + new DB on the same 1 TB disk during the sync.
- **Upgrade:** rerun `./avalanchego-installer.sh`, check `--version`, confirm
  `~/.avalanchego/configs/node.json` still has the `db-dir`, and confirm
  `~/.avalanchego/configs/chains/C/config.json` is still `{}`.
- **Logs:** journald capped at 200 MB above; no other OS tuning needed.

| Symptom | Fix |
| --- | --- |
| `connection refused` on `:9650` | `systemctl status avalanchego`; RPC is private |
| `isBootstrapped` stays false | Wait; `journalctl -u avalanchego -f`, check disk I/O |
| `eth_getLogs` range error | Shorter range; fresh node has no history before sync |
| Historical `eth_call` at old block fails | Expected on a pruned node; Dwellir URL in `rpc_urls` handles it |
| `txpool_*` / `debug_trace*` missing | `C/config.json` is not `{}`; reset and restart |
| Caddy 401 | Re-check hash and that the URL is `https://` with URL-encoded password |
| Report 404/403 | DNS A record, `root` path, and `chown caddy:caddy /var/www/mev-reports` |
| `SQLITE_BUSY` | Only one CLI process may write a DB at a time; stop the loop/backfill |
| System disk full | Node DB should be on `/mnt/avalanche-db`; optionally move mev-scout `db_path` there too |
| Box swaps | Finish `cargo build` before sync; keep concurrency at 4 |

Official references: [installer](https://build.avax.network/docs/nodes/run-a-node/using-install-script/installing-avalanche-go) · [C-Chain configs](https://build.avax.network/docs/nodes/chain-configs/primary-network/c-chain) · [system requirements](https://build.avax.network/docs/nodes/system-requirements) · [fresh state sync](https://build.avax.network/docs/nodes/node-storage/periodic-state-sync) · [CLI](./CLI.md)
