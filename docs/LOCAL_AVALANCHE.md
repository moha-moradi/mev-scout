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


| Endpoint             | URL                                  |
| -------------------- | ------------------------------------ |
| C-Chain JSON-RPC     | `http://127.0.0.1:9650/ext/bc/C/rpc` |
| C-Chain WebSocket    | `ws://127.0.0.1:9650/ext/bc/C/ws`    |
| AvalancheGo HTTP API | `http://127.0.0.1:9650`              |
| P2P                  | TCP `9651`                           |


HTTP and WebSocket share port `9650`; WS comes up with the node (no separate
flag). With installer `private` RPC, both bind to localhost only.
`mev-scout` today points at HTTP via `rpc_urls` — it has no `ws_url` yet.

`mev-scout` uses the **C-Chain** only (`chain_id = 43114`).

## Hardware


|                  | Spec                                                                                         |
| ---------------- | -------------------------------------------------------------------------------------------- |
| OS               | Ubuntu 22.04 or 24.04 (amd64 / arm64)                                                        |
| CPU / RAM / disk | Avalanche minimum: **8 vCPU / 16 GiB / SSD**; plan **~200–400 GB** with state-sync + pruning |
| Sync             | Hours → ~1 day to tip (I/O-bound)                                                            |


Windows is not a supported AvalancheGo platform — run the node on Ubuntu.

## Disk: minimum SSD footprint

The setup in this guide (state-sync + pruning, both on) is already the
smallest node role that keeps mev-scout fully working: **plan ~200–400 GB and
do not switch to archival** — that turns sync into multi-days and 1 TB+ of
disk, and mev-scout does not need it.


| Workload                                                                  | On this pruned, state-synced node                        |
| ------------------------------------------------------------------------- | -------------------------------------------------------- |
| `live`, `explorer index` (tip + recent blocks)                            | yes                                                      |
| `explorer backfill` (historical range; overlaps up to `block_concurrency` batched fetches; classify stays ordered) | yes for recent windows; deep history uses archive `rpc_rps` |
| `eth_getLogs` within the pruning window (discover lookback = 1000 blocks) | yes                                                      |
| `debug_traceTransaction` on recent blocks (`explorer show --trace`)       | yes                                                      |
| Deep historical `eth_call` / `eth_getProof` / long `eth_getLogs` ranges   | no — served by the public archive fallback in `rpc_urls` |


Keeping the tool healthy while staying minimal:

- **Keep a second (archive-capable) provider in** `rpc_urls` — as in
`mev-scout.local.toml`. mev-scout probes each provider's capability and
routes deep-history reads away from the pruned local node automatically;
all tip work still runs against the local node at full speed.
- Cap journal logs so they cannot eat the disk:
  ```bash
  echo 'SystemMaxUse=500M' | sudo tee /etc/systemd/journald.conf.d/size.conf
  sudo systemctl restart systemd-journald
  ```
- `mev-scout/cache/*.sqlite` (blocks / pools / ticks) grows with indexing
runs — bound it with `--duration` / `--days`. Deleting the cache files is
always safe: they are gitignored and rebuildable.



## OS tuning (optional)

Sync is I/O-bound and RPC is DB-latency-bound — **hardware (SSD IOPS + RAM)
matters more than any OS setting**. These cheap, low-risk tweaks still help;
the *Skip* list below is genuinely not worth doing.

### Worth it


| Tweak                  | Effect                                                | How                                                  |
| ---------------------- | ----------------------------------------------------- | ---------------------------------------------------- |
| `noatime` mount        | Fewer wasted writes during sync / indexing            | add `,noatime` to the data partition in `/etc/fstab` |
| I/O scheduler `none`   | No unnecessary fsync reordering on an SSD             | `echo none                                           |
| `vm.swappiness = 10`   | Keep AvalancheGo in RAM, not swap                     | sysctl (block below)                                 |
| 32 GiB RAM             | Biggest single win — beats every tweak on this page   | upgrade the VM rather than the settings              |
| High `LimitNOFILE`     | Handle many concurrent RPC connections from mev-scout | unit drop-in (block below)                           |
| BBR congestion control | Faster remote state-sync transfers                    | sysctl (block below)                                 |
| TRIM + ~30% free disk  | SSD keeps its IOPS — IOPS collapse when full          | `systemctl status fstrim.timer`; leave headroom      |


```bash
# persistent sysctls (apply live with `sudo sysctl --system`)
sudo tee /etc/sysctl.d/99-node.conf >/dev/null <<'EOF'
vm.swappiness=10
net.ipv4.tcp_congestion_control=bbr
EOF
sudo sysctl --system

# noatime: append ,noatime to the data partition in /etc/fstab, then
# sudo mount -o remount /path/to/data    (e.g. the mount holding ~/.avalanchego)

# more open files for the avalanchego systemd unit
sudo systemctl edit avalanchego
#   [Service]
#   LimitNOFILE=1048576
sudo systemctl daemon-reload && sudo systemctl restart avalanchego

# I/O scheduler (persist with a udev rule if the VM resets it on reboot)
echo none | sudo tee /sys/block/sda/queue/scheduler
```



### Skip


| Setting                                        | Why not                                                      |
| ---------------------------------------------- | ------------------------------------------------------------ |
| `vm.dirty_ratio` / `vm.dirty_background_ratio` | Data-loss risk on power cut, no benefit for a DB that fsyncs |
| Disabling Transparent Huge Pages               | No measurable effect for Go / the C-Chain VM                 |
| CPU governor `performance`                     | Already active on essentially all VPS / cloud                |




## 1. Install AvalancheGo

```bash
wget -nd -m https://raw.githubusercontent.com/ava-labs/avalanche-docs/master/scripts/avalanchego-installer.sh
chmod 755 avalanchego-installer.sh
./avalanchego-installer.sh
```

Answer the prompts:


| Prompt               | Answer                                          |
| -------------------- | ----------------------------------------------- |
| Connection type      | `1` home / dynamic IP, or `2` cloud / static IP |
| RPC public / private | `private` (RPC only on this machine)            |
| State sync           | `on`                                            |


Then:

```bash
sudo systemctl status avalanchego
sudo journalctl -u avalanchego -f
```

Binary: `~/avalanche-node/` · Data / configs: `~/.avalanchego/`

## 2. C-Chain config

Avalanche recommends overriding **only** non-default values. Defaults already
enable pruning and the core eth APIs. This file turns on **state-sync** (fast
tip bootstrap) and adds `debug` **/** `debug-tracer` for
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


| Setting              | Value                               | Why                                 |
| -------------------- | ----------------------------------- | ----------------------------------- |
| `state-sync-enabled` | `true`                              | Fast tip sync (default is `false`)  |
| `pruning-enabled`    | *(default* `true`*)*                | Leave unset — saves disk            |
| `eth-apis`           | defaults + `debug` + `debug-tracer` | tip scan + `debug_traceTransaction` |


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



## 4. Point mev-scout at [localhost](http://localhost)

Two config profiles ship with the repo:


| Profile        | Config                                                        | RPC                                        | Throughput                       |
| -------------- | ------------------------------------------------------------- | ------------------------------------------ | -------------------------------- |
| Free / public  | `mev-scout.toml` (`cp mev-scout.example.toml mev-scout.toml`) | chainlist free endpoints                   | RPS-capped (1–5), works anywhere |
| **Full speed** | `mev-scout.local.toml`                                        | local node first + public archive fallback | unlimited RPS + max concurrency  |


```bash
cargo run -p mev-scout-cli -- --config mev-scout.local.toml config
```



### What makes `mev-scout.local.toml` full speed

```toml
rps_limit = 0           # 0 = unlimited: no rate limiter is created at all
block_concurrency = 32  # auto mode falls back to a conservative 10
batch_rpc = true        # block+txs+receipts in ONE JSON-RPC batch POST
                        # (AvalancheGo batch-request-limit default = 1000)

[chains.avalanche.rpc]
rpc_urls = ["http://127.0.0.1:9650/ext/bc/C/rpc",
            "https://avalanche-c-chain.publicnode.com"]
rpc_rps  = [0.0, 1.0]   # 0.0 = unlimited for the local node

[discover]
rpc_concurrency = 32    # default 8 — on-chain factory scans / multicall

[live]
poll_interval_ms = 500  # default 2000; C-Chain blocks are ~2 s
```


| Knob                                   | Why                                                                                                                    |
| -------------------------------------- | ---------------------------------------------------------------------------------------------------------------------- |
| `rps_limit = 0` / `rpc_rps = [0.0, …]` | A 0-RPS provider gets **no token bucket** — throughput is bounded by concurrency and node hardware, not an RPS counter |
| `block_concurrency = 32`               | Parallel in-flight block fetches for scanner shards **and** `explorer backfill` (classify/persist stays ordered); auto mode falls back to 10 when RPS is unlimited |
| `batch_rpc = true`                     | Cuts HTTP round-trips ~3× for `backtest` / explorer block+receipts fetches (the `live` job does not read this flag)   |
| `[discover] rpc_concurrency = 32`      | Concurrency for pure on-chain factory scans and multicall metadata resolution                                          |
| `[live] poll_interval_ms = 500`        | Polls well inside the ~2 s block time                                                                                  |
| Public archive as `rpc_urls[1]`        | Deep-history reads the pruned local node cannot serve are routed to it automatically; tip work stays on the local node |


Both profiles take every command from §5 — just pass the config you want:

```bash
cargo run -p mev-scout-cli -- --config mev-scout.local.toml live --loop
```



## 5. Run mev-scout

Commands are shown with `mev-scout.toml`; swap in
`--config mev-scout.local.toml` for the full-speed profile.

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


| Path                             | Contents                              |
| -------------------------------- | ------------------------------------- |
| `~/.avalanchego/`                | DB + configs                          |
| `~/avalanche-node/`              | Binaries                              |
| `mev-scout/cache/`               | Scanner / explorer DBs (gitignored)   |
| `mev-scout/mev-scout.toml`       | Local config (do not commit secrets)  |
| `mev-scout/mev-scout.local.toml` | Full-speed local profile (no secrets) |




## 6. Remote access with username/password

AvalancheGo has **no built-in auth** — never bind its RPC to `0.0.0.0`.
Keep the installer's `private` RPC (127.0.0.1:9650) and put a reverse proxy
with Basic Auth + TLS in front of it:

```text
Internet ──HTTPS + user/pass──▶ Caddy :443 ──▶ 127.0.0.1:9650 (localhost only)
```



### Install Caddy

```bash
sudo apt install -y debian-keyring debian-archive-keyring apt-transport-https curl
curl -1sLf 'https://dl.cloudsmith.io/public/caddy/stable/gpg.key' \
  | sudo gpg --dearmor -o /usr/share/keyrings/caddy-stable-archive-keyring.gpg
curl -1sLf 'https://dl.cloudsmith.io/public/caddy/stable/debian.deb.txt' \
  | sudo tee /etc/apt/sources.list.d/caddy-stable.list
sudo apt update && sudo apt install -y caddy
```



### `/etc/caddy/Caddyfile`

```
rpc.example.com {
	encode gzip

	basic_auth {
		scout $2a$14$REPLACE_WITH_HASH
	}

	# HTTP JSON-RPC and the WebSocket share the node's port 9650;
	# Caddy proxies both (WebSocket upgrade is automatic).
	reverse_proxy 127.0.0.1:9650
}
```

Generate the bcrypt hash and reload:

```bash
caddy hash-password --plaintext 'CHOOSE-A-STRONG-PASSWORD'
sudo systemctl reload caddy
```

`rpc.example.com` must be a DNS A/AAAA record pointing at this server so
Caddy can obtain a Let's Encrypt certificate (needs ports 80/443 open). No
domain? Use the Tailscale alternative below instead of self-signed certs.

### Firewall

```bash
sudo ufw allow 22/tcp     # SSH
sudo ufw allow 80/tcp     # ACME challenge / redirect
sudo ufw allow 443/tcp    # RPC proxy
sudo ufw allow 9651/tcp   # node P2P — keep the node reachable for the network
sudo ufw enable
#9650 is intentionally NOT opened: JSON-RPC stays behind Caddy on localhost.
```



### Connect from another machine

reqwest — the HTTP client mev-scout uses — turns `user:pass@` in a URL into
an `Authorization: Basic` header automatically, so no code changes are
needed. Keep the password out of the config file with `${ENV_VAR}` expansion:

```toml
[chains.avalanche.rpc]
rpc_urls = ["https://mev:${RPC_PASS}@rpc.example.com/ext/bc/C/rpc"]
rpc_rps = [0.0]
```

```bash
export RPC_PASS='CHOOSE-A-STRONG-PASSWORD'
```

Verify:

```bash
# with credentials -> "result":"0xa86a"
curl -X POST "https://mev:${RPC_PASS}@rpc.example.com/ext/bc/C/rpc" \
  -H 'content-type: application/json' \
  -d '{"jsonrpc":"2.0","id":1,"method":"eth_chainId","params":[]}'

# without credentials -> 401
curl -X POST https://rpc.example.com/ext/bc/C/rpc \
  -H 'content-type: application/json' \
  -d '{"jsonrpc":"2.0","id":1,"method":"eth_chainId","params":[]}'
```

Any other tool (cast, curl, web3 scripts) authenticates the same way. The
WebSocket (`wss://…/ext/bc/C/ws`) sits behind the same auth; CLI clients must
send the credentials (browsers cannot set the header from `new WebSocket()`).

### Alternative: no public port at all (Tailscale)

```bash
curl -fsSL https://tailscale.com/install.sh | sh && sudo tailscale up
```

Point remote clients at the node's Tailscale IP
(`http://100.x.y.z:9650/ext/bc/C/rpc`) and keep every firewall port closed
except SSH. No username/password — identity is the device key.

## 7. Connect a real MEV bot (or any other tool)

The endpoint is not mev-scout-specific — any EVM tool can use it. Two facts
shape what works out of the box: the RPC is `private` (localhost only), and
the node is pruned + state-synced (§2).


| The bot needs                                                                                | On this node                                                                                        |
| -------------------------------------------------------------------------------------------- | --------------------------------------------------------------------------------------------------- |
| `eth_sendRawTransaction` / `eth_fillTransaction` / `eth_pendingTransactions`                 | yes — `internal-transaction`                                                                        |
| `eth_call` / `eth_estimateGas` / `eth_feeHistory` at tip                                     | yes — `internal-blockchain` / `internal-eth`                                                        |
| `debug_traceCall` / `debug_traceBlockByNumber` (bundle simulation)                           | yes — `debug-tracer`                                                                                |
| `eth_subscribe("newHeads")` / pending-tx filters over WS (`ws://127.0.0.1:9650/ext/bc/C/ws`) | yes — WS shares port 9650, no extra flag                                                            |
| `txpool_content` / `txpool_status` (mempool scanning)                                        | **not enabled — add** `internal-tx-pool` (below)                                                    |
| `trace_*` (`trace_filter`, `trace_call`, …)                                                  | **no — coreth does not implement the trace namespace**; use `debug_trace`* instead                  |
| Deep historical `eth_call` / `eth_getProof` / state older than ~32 blocks                    | no — same archive fallback as §4                                                                    |
| Preferred (not yet accepted) tip                                                             | no — `allow-unfinalized-queries` defaults to `false`; flip only if you accept the finality tradeoff |




### The one config change worth making

Append `"internal-tx-pool"` to `eth-apis` in
`~/.avalanchego/configs/chains/C/config.json` (the file from §2), then:

```bash
sudo systemctl restart avalanchego
```

Same machine → nothing else to change. From another machine, use the Caddy /
Tailscale setup in §6 — never expose `:9650` directly (AvalancheGo has no
auth).

### Rate limits and C-Chain specifics

- Node-side limits are off by default (`api-max-duration = 0`,
`batch-request-limit = 1000`). The `rps_limit` / `block_concurrency` knobs
in §4 are mev-scout config, not node config — a bot gets full speed with
zero extra setup.
- C-Chain blocks are built by validators: your transactions enter through
normal mempool gossip — there is no builder/auction relay to integrate
with (see §8).
- `local-txs-enabled` (default `false`) only grants local treatment to
accounts in the node's keystore; externally signed raw transactions are
gossiped like everyone else's.



## 8. MEV on Avalanche: keeping the bot's opportunities yours

Ethereum-style protection does not exist here: **no Flashbots Protect, no
MEV Blocker, no bloXroute, no** `eth_sendPrivateTransaction`**, no bundle
relay.** Anyone claiming a "MEV-protected RPC" on C-Chain is selling a
faster submission path, not a private mempool contract. Protection is
something you build from three ingredients: **speed, visibility,
atomicity**.

### How C-Chain ordering actually works (the threat model)


| Fact                                                                             | Consequence for a bot                                                                                                                  |
| -------------------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------- |
| No public mempool — a received tx is randomly gossiped to **10 validator peers** | Pending txs are visible only to validators (and their stream partners); "watch the mempool" means running/renting validator visibility |
| Ordering is **first-come-first-served** at the block builder                     | A **latency race**, not a gas auction — `maxPriorityFeePerGas` does **not** buy position                                               |
| Each block has a **single proposer** with full control of intra-block order      | The realistic attacker is the proposer or a searcher with a direct line to it — not a general frontrunning bot                         |
| Fast finality (~1 s), ~2 s blocks                                                | The reaction window is milliseconds; detection→submit latency *is* the moat                                                            |
| No tx replacement/cancel in the gossip model                                     | Get nonce management right the first time; a stuck tx just disappears silently                                                         |


Implication: a tx you broadcast **will** be seen by validators (and by any
mempool stream subscriber) before inclusion. You cannot be invisible — you
can only be **faster to the proposer**, and make copying you unprofitable.

### Protection layers (in order of value)


| Layer                  | What                             | How                                                                                                                                                                                                                                            |
| ---------------------- | -------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| **1. Speed**           | Win the FCFS race                | **Fan-out the same signed tx** (same nonce → same hash, only one lands, so this is safe) to several paths at once: local node (§1) + remote RPCs (§6) + optional paid propagator. Measure submit→inclusion per path and keep a latency heatmap |
| **2. Atomicity**       | Make copying worthless           | Whole strategy in **one tx** with `require(profit > 0)`; a copycat who loses the race executes nothing. Strict slippage bounds block sandwiching of your own swaps                                                                             |
| **3. Visibility**      | See competitors before they land | `internal-tx-pool` (§7) for your node's view; a validator node sees the wider gossip; or subscribe to a mempool stream service                                                                                                                 |
| **4. Colocation**      | Shrink the network leg           | Run the bot box in a region close to major validators; geo-distribute submit paths                                                                                                                                                             |
| **5. Run a validator** | The only structural edge         | 2000 AVAX stake → full mempool gossip view + participation in block proposal. Heavy; only worth it at real P&L                                                                                                                                 |




### Public services (verify before relying on them)

- **Snowsight** (Chainsight Labs) — the best-known C-Chain MEV plumbing:
  - *Mempool Stream*: websocket stream of pending txs aggregated from their
  validator network (signed-key auth) — the "see it before it lands" layer.
  - *Transaction Propagator*: HTTP endpoint that relays your tx through
  their validator network faster than a public RPC — the "land it first"
  layer.
- No bundle/relay standard exists, so there is nothing to "integrate" the
way Ethereum bots integrate Flashbots — submission stays plain
`eth_sendRawTransaction`, just to faster endpoints.



### What mev-scout sees vs. what the bot needs

mev-scout scans **landed** blocks (§4–§5); frontrun decisions happen
**before** landing. The scanner answers "what opportunity existed"; layers
1–3 above answer "did we get it". Same node, same config — the bot in §7
and the scanner in §5 share this box with no extra setup.

## Troubleshooting


| Symptom                                  | Fix                                                                                                         |
| ---------------------------------------- | ----------------------------------------------------------------------------------------------------------- |
| Connection refused on `:9650`            | `sudo systemctl status avalanchego` — installer used `private` RPC                                          |
| `isBootstrapped: false` a long time      | Wait; watch `journalctl` and disk I/O                                                                       |
| `eth_getLogs` range errors               | Shrink lookback / `--blocks` (state-synced history window)                                                  |
| `debug_traceTransaction` missing         | Confirm `debug-tracer` in `C/config.json`, then `sudo systemctl restart avalanchego`                        |
| `txpool_*` returns "method not found"    | Add `internal-tx-pool` to `eth-apis` (§7), then `sudo systemctl restart avalanchego`                        |
| Deep historical `eth_call` / proof fails | Expected with state-sync + pruning — the archive fallback in `rpc_urls` handles it (see **Disk** above)     |
| `401 Unauthorized` from the proxy        | Wrong/missing `user:pass` — regenerate the hash with `caddy hash-password`, check `${RPC_PASS}` is exported |
| RPC is fast but scanning is still slow   | Missing full-speed knobs — see §4 (`block_concurrency`, `batch_rpc`, `discover.rpc_concurrency`)            |




## Upgrade

```bash
./avalanchego-installer.sh
```

The script detects the existing service and upgrades in place. Data in
`~/.avalanchego` is kept.

## Related

- Main quick start: `[README.md](../README.md)`
- CLI surface: `[CLI.md](./CLI.md)`
- Example public RPCs (fallback only): `[mev-scout.example.toml](../mev-scout.example.toml)`
- Full-speed local profile: `[mev-scout.local.toml](../mev-scout.local.toml)`

