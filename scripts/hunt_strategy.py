#!/usr/bin/env python3
"""Hunt candidate blocks for explorer strategy tags via eth_getLogs.

Usage:
  python scripts/hunt_strategy.py --label flash_loan_liq --chain avalanche
  python scripts/hunt_strategy.py --list
  python scripts/hunt_strategy.py --label keeper --rpc-url https://... --lookback-blocks 200000

On Windows hosts where python.exe is policy-blocked, use the Node twin:
  node scripts/hunt_strategy.mjs --label flash_loan_liq --chain avalanche
"""
from __future__ import annotations

import argparse
import json
import sys
import time
import urllib.error
import urllib.request
from typing import Any

# keccak256 topic0 pins (match core/src/chain/events.rs signatures).
TOPICS = {
    "aave_v3_flash": "0xefefaba5e921573100900a3ad9cf29f222d995fb3b6045797eaea7521bd8d6f0",
    "aave_v3_liq": "0xe413a321e8681d831f4dbccbca790d2952b56f977908e45be37335533e005286",
    "balancer_flash": "0xef073525e2a5ca16f7554219d9966620e60c96a429911c910fcac813af510d90",
    "morpho_flash": "0xc76f1b4fe4396ac07a9fa55a415d4ca430e72651d37d3401f3bed7cb13fc4f12",
    "gelato_exec": "0xa458375b1282695a972870cbfbc4891a9d856b79d563d17667d171d87e0c527a",
    "user_op": "0x49628fd1471006c1482da88028e9ce4dbb080b815c9b0344d39e5a8e6ec1419f",
    "v3_mint": "0x7a53080ba414158be7ec69b987b5fb7d07dee101fe85488f0853ae16239d0bde",
    "v3_burn": "0x0c396cd989a39f4459b5fa1aed6a9a8dcdbc45908acfd67e028cd568da98982c",
    "transfer": "0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef",
    "comp_v2_liq": "0x298637f684da70674f26509b10f07ec2fbc77a335ab1e7d6215a4b2484d8bb52",
    "morpho_liq": "0xa4946ede45d0c6f06a0f5ce92c9ad3b4751452d2fe0e25010783bcab57a67e41",
    "buy_collateral": "0xf891b2a411b0e66a5f0a6ff1368670fefa287a13f541eb633a386a1a9cc7046b",
    "notify_reward": "0xf70d5c697de7ea828df48e5c4573cb2194c659f1901f70110c52b066dcf50826",
    "answer_updated": "0x0559884fd3a460db3073b7fc896cc77986f16e378210ded43186175bf646fc5f",
    "reserve_data": "0x804c9b842b2748a22bb64b345453a3de7ca54a6ca45ce00d415894979e22897a",
    "rate_cache": "0xb77a83204ca282e08dc3a65b0a1ca32ea4e6875c38ef0bf5bf75e52a67354fac",
    "lb_deposit": "0x87f1f9dcf5e8089a3e00811b6a008d8f30293a3da878cb1fe8c90ca376402f8a",
    "lb_withdraw": "0xa32e146844d6144a22e94c586715a1317d58a8aa3581ec33d040113ddcb24350",
    "upkeep": "0xad8cc9579b21dfe2c2f6ea35ba15b656e46b4f5b0cb424f52739b8ce5cac9c5b",
    "absorb": "0xa308ab8d07a41d55945beacfb51054867b6f082af2c1fae104eba505a0dae644",
    "curve_ex": "0x8b3e96f2b889fa771c53c981b40daf005f63f637f1869f707052d15a3dd97140",
}

RPC_DEFAULTS = {
    "avalanche": "https://avalanche-c-chain-rpc.publicnode.com",
    "polygon": "https://polygon-bor-rpc.publicnode.com",
    "ethereum": "https://ethereum-rpc.publicnode.com",
}

AAVE_POOLS = {
    "avalanche": [
        "0x69FA688f1Dc47d4B5d8029D5a35FB7a548E0B9b0",  # V3
        "0x4F01aEd16D97E3aB5ab2B501154DC9bb0F1A5A2C",  # V2
    ],
    "polygon": ["0x794a61358D6845594F94dc1DB02A252b5b4814aD"],
    "ethereum": ["0x87870Bca3F3fD6335C3F4ce8392D69350B4fA4E2"],
}

ENTRY_POINT = "0x5FF137D4b0FDCD49DcA30c7CF57E578a026d2789"
GMX_EMITTER = "0xDb17B233827785b5EA1aD91C9bD7D4dC478B9389"
MORPHO_BLUE = "0xBBBBBbbBBb9cC5e90e3b3Af64bdAF62C37EEFFCb"
SAVAX = "0x2b2C81e08f1Af8835a78Bb2A90AE924ACE0eA4bE"
WAVAX = "0xB31f66AA3C1e785363F0875A1B74E27b85FD66c7"
BALANCER_VAULT = "0xBA12222222228d8Ba445958a75a0704d566BF2C8"

# First Benqi qiToken (qiAVAX) — scan LiquidateBorrow from aliases.
BENQI_QIAVAX = "0x5c0401e81bc07ca70fad469b451682c0d747ef1c"

# Compound V3 Comet markets (ETH) for buyCollateral.
ETH_COMETS = [
    "0xc3d688B66703497DAA19211EEdff47f25384cdc3",  # USDC
    "0xA17581A9E3356d9A858b789D68B4d866e593aE94",  # WETH
]


def rpc(url: str, method: str, params: list[Any]) -> Any:
    body = json.dumps({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}).encode()
    req = urllib.request.Request(
        url,
        data=body,
        headers={"Content-Type": "application/json", "User-Agent": "mev-scout-hunt/0.1"},
    )
    for attempt in range(5):
        try:
            with urllib.request.urlopen(req, timeout=60) as resp:
                payload = json.load(resp)
            if "error" in payload:
                raise RuntimeError(payload["error"])
            return payload["result"]
        except (urllib.error.URLError, TimeoutError, RuntimeError) as e:
            if attempt == 4:
                raise
            time.sleep(1.5 * (attempt + 1))
            print(f"retry {attempt+1}: {e}", file=sys.stderr)
    raise RuntimeError("unreachable")


def hex_int(x: str | int) -> int:
    if isinstance(x, int):
        return x
    return int(x, 16)


def get_logs(
    url: str,
    *,
    address: str | list[str] | None,
    topics: list[Any],
    from_block: int,
    to_block: int,
) -> list[dict[str, Any]]:
    filt: dict[str, Any] = {
        "fromBlock": hex(from_block),
        "toBlock": hex(to_block),
        "topics": topics,
    }
    if address is not None:
        filt["address"] = address
    return rpc(url, "eth_getLogs", [filt]) or []


def chunked_logs(
    url: str,
    *,
    address: str | list[str] | None,
    topics: list[Any],
    tip: int,
    lookback: int,
    chunk: int,
    max_hits: int,
) -> list[dict[str, Any]]:
    start = max(0, tip - lookback)
    hits: list[dict[str, Any]] = []
    end = tip
    while end >= start and len(hits) < max_hits:
        fr = max(start, end - chunk + 1)
        try:
            batch = get_logs(url, address=address, topics=topics, from_block=fr, to_block=end)
        except Exception as e:  # noqa: BLE001 — shrink chunk on provider limits
            if chunk <= 500:
                print(f"getLogs failed {fr}..{end}: {e}", file=sys.stderr)
                break
            chunk = max(500, chunk // 2)
            print(f"shrinking chunk to {chunk}", file=sys.stderr)
            continue
        hits.extend(batch)
        print(f"scanned {fr}..{end} hits=+{len(batch)} total={len(hits)}", file=sys.stderr)
        end = fr - 1
        time.sleep(0.15)
    return hits[:max_hits]


def same_tx(a: dict[str, Any], b: dict[str, Any]) -> bool:
    return a.get("transactionHash") == b.get("transactionHash")


def hunt_flash_loan_liq(url: str, chain: str, tip: int, lookback: int, chunk: int) -> list[dict]:
    pools = AAVE_POOLS[chain]
    liqs = chunked_logs(
        url,
        address=pools if len(pools) > 1 else pools[0],
        topics=[TOPICS["aave_v3_liq"]],
        tip=tip,
        lookback=lookback,
        chunk=chunk,
        max_hits=40,
    )
    out = []
    flash_topics = [
        TOPICS["aave_v3_flash"],
        TOPICS["balancer_flash"],
        TOPICS["morpho_flash"],
    ]
    for lg in liqs:
        bn = hex_int(lg["blockNumber"])
        flashes = get_logs(
            url,
            address=None,
            topics=[flash_topics],
            from_block=bn,
            to_block=bn,
        )
        if any(same_tx(lg, f) for f in flashes):
            out.append(
                {
                    "label": "flash_loan_liq",
                    "chain": chain,
                    "block": bn,
                    "tx": lg["transactionHash"],
                    "reason": "LiquidationCall + flash loan same tx",
                }
            )
            if len(out) >= 5:
                break
    return out


def hunt_flash_arb(url: str, chain: str, tip: int, lookback: int, chunk: int) -> list[dict]:
    pools = AAVE_POOLS.get(chain) or []
    addrs = [
        *pools,
        BALANCER_VAULT,
        *([MORPHO_BLUE] if chain != "avalanche" else []),
    ]
    flashes = chunked_logs(
        url,
        address=addrs if len(addrs) > 1 else addrs[0],
        topics=[[TOPICS["aave_v3_flash"], TOPICS["balancer_flash"], TOPICS["morpho_flash"]]],
        tip=tip,
        lookback=lookback,
        chunk=chunk,
        max_hits=30,
    )
    return [
        {
            "label": "flash_arb",
            "chain": chain,
            "block": hex_int(lg["blockNumber"]),
            "tx": lg["transactionHash"],
            "reason": "flash-loan log — index block and filter arb_atomic+flash_arb",
        }
        for lg in flashes[:10]
    ]


def hunt_keeper(url: str, chain: str, tip: int, lookback: int, chunk: int) -> list[dict]:
    hits = chunked_logs(
        url,
        address=None,
        topics=[[TOPICS["gelato_exec"], TOPICS["upkeep"]]],
        tip=tip,
        lookback=min(lookback, 50_000),
        chunk=min(chunk, 2_000),
        max_hits=15,
    )
    return [
        {
            "label": "keeper",
            "chain": chain,
            "block": hex_int(lg["blockNumber"]),
            "tx": lg["transactionHash"],
            "reason": "Gelato ExecSuccess or Chainlink UpkeepPerformed",
        }
        for lg in hits[:8]
    ]


def hunt_bundler(url: str, chain: str, tip: int, lookback: int, chunk: int) -> list[dict]:
    hits = chunked_logs(
        url,
        address=ENTRY_POINT,
        topics=[TOPICS["user_op"]],
        tip=tip,
        lookback=lookback,
        chunk=chunk,
        max_hits=10,
    )
    return [
        {
            "label": "bundler",
            "chain": chain,
            "block": hex_int(lg["blockNumber"]),
            "tx": lg["transactionHash"],
            "reason": "EntryPoint UserOperationEvent",
        }
        for lg in hits[:5]
    ]


def hunt_benqi(url: str, chain: str, tip: int, lookback: int, chunk: int) -> list[dict]:
    if chain != "avalanche":
        return []
    hits = chunked_logs(
        url,
        address=BENQI_QIAVAX,
        topics=[TOPICS["comp_v2_liq"]],
        tip=tip,
        lookback=lookback,
        chunk=chunk,
        max_hits=10,
    )
    return [
        {
            "label": "benqi",
            "chain": chain,
            "block": hex_int(lg["blockNumber"]),
            "tx": lg["transactionHash"],
            "reason": "Benqi qiAVAX LiquidateBorrow",
        }
        for lg in hits[:5]
    ]


def hunt_jit(url: str, chain: str, tip: int, lookback: int, chunk: int) -> list[dict]:
    mints = chunked_logs(
        url,
        address=None,
        topics=[TOPICS["v3_mint"]],
        tip=tip,
        lookback=min(lookback, 30_000),
        chunk=min(chunk, 1_500),
        max_hits=40,
    )
    out = []
    for m in mints:
        bn = hex_int(m["blockNumber"])
        burns = get_logs(
            url, address=m["address"], topics=[TOPICS["v3_burn"]], from_block=bn, to_block=bn
        )
        if burns:
            out.append(
                {
                    "label": "jit",
                    "chain": chain,
                    "block": bn,
                    "tx": m["transactionHash"],
                    "reason": "V3 Mint+Burn same pool/block — verify jit kind",
                }
            )
        if len(out) >= 5:
            break
    return out


def hunt_lb_bin_jit(url: str, chain: str, tip: int, lookback: int, chunk: int) -> list[dict]:
    deps = chunked_logs(
        url,
        address=None,
        topics=[TOPICS["lb_deposit"]],
        tip=tip,
        lookback=min(lookback, 80_000),
        chunk=min(chunk, 2_000),
        max_hits=30,
    )
    out = []
    for d in deps:
        bn = hex_int(d["blockNumber"])
        wds = get_logs(
            url,
            address=d["address"],
            topics=[TOPICS["lb_withdraw"]],
            from_block=bn,
            to_block=bn,
        )
        if any(same_tx(d, w) for w in wds):
            out.append(
                {
                    "label": "lb_bin_jit",
                    "chain": chain,
                    "block": bn,
                    "tx": d["transactionHash"],
                    "reason": "LB DepositedToBins+WithdrawnFromBins same tx",
                }
            )
        if len(out) >= 5:
            break
    return out


def hunt_gmx_adl(url: str, chain: str, tip: int, lookback: int, chunk: int) -> list[dict]:
    if chain != "avalanche":
        return []
    # Prefer known candidate, then recent emitter activity.
    candidates = [81075048]
    out = []
    for bn in candidates:
        logs = get_logs(url, address=GMX_EMITTER, topics=[], from_block=bn, to_block=bn)
        if logs:
            out.append(
                {
                    "label": "gmx_adl_arb",
                    "chain": chain,
                    "block": bn,
                    "tx": logs[0]["transactionHash"],
                    "reason": "GMX EventEmitter activity on candidate block",
                }
            )
    hits = chunked_logs(
        url,
        address=GMX_EMITTER,
        topics=[],
        tip=tip,
        lookback=min(lookback, 100_000),
        chunk=min(chunk, 2_000),
        max_hits=20,
    )
    for lg in hits[:5]:
        out.append(
            {
                "label": "gmx_adl_arb",
                "chain": chain,
                "block": hex_int(lg["blockNumber"]),
                "tx": lg["transactionHash"],
                "reason": "GMX EventEmitter log — index and check gmx_adl_arb tag",
            }
        )
    return out


def hunt_epoch(url: str, chain: str, tip: int, lookback: int, chunk: int) -> list[dict]:
    hits = chunked_logs(
        url,
        address=None,
        topics=[TOPICS["notify_reward"]],
        tip=tip,
        lookback=min(lookback, 200_000),
        chunk=min(chunk, 3_000),
        max_hits=15,
    )
    return [
        {
            "label": "epoch_transition",
            "chain": chain,
            "block": hex_int(lg["blockNumber"]),
            "tx": lg["transactionHash"],
            "reason": "NotifyReward — index Solidly/LB arb co-occurrence",
        }
        for lg in hits[:8]
    ]


def hunt_oracle_poke(url: str, chain: str, tip: int, lookback: int, chunk: int) -> list[dict]:
    pools = AAVE_POOLS.get(chain) or []
    if not pools:
        return []
    liqs = chunked_logs(
        url,
        address=pools if len(pools) > 1 else pools[0],
        topics=[TOPICS["aave_v3_liq"]],
        tip=tip,
        lookback=lookback,
        chunk=chunk,
        max_hits=25,
    )
    out = []
    for lg in liqs:
        bn = hex_int(lg["blockNumber"])
        answers = get_logs(
            url, address=None, topics=[TOPICS["answer_updated"]], from_block=bn, to_block=bn
        )
        if answers:
            out.append(
                {
                    "label": "oracle_poke_block",
                    "chain": chain,
                    "block": bn,
                    "tx": lg["transactionHash"],
                    "reason": "LiquidationCall co-block with AnswerUpdated",
                }
            )
        if len(out) >= 5:
            break
    return out


def hunt_buycollateral(url: str, chain: str, tip: int, lookback: int, chunk: int) -> list[dict]:
    if chain != "ethereum":
        return []
    hits = chunked_logs(
        url,
        address=ETH_COMETS,
        topics=[TOPICS["buy_collateral"]],
        tip=tip,
        lookback=lookback,
        chunk=chunk,
        max_hits=15,
    )
    return [
        {
            "label": "buycollateral",
            "chain": chain,
            "block": hex_int(lg["blockNumber"]),
            "tx": lg["transactionHash"],
            "reason": "Compound V3 BuyCollateral",
        }
        for lg in hits[:8]
    ]


def hunt_bad_debt(url: str, chain: str, tip: int, lookback: int, chunk: int) -> list[dict]:
    if chain == "avalanche":
        return []
    hits = chunked_logs(
        url,
        address=MORPHO_BLUE,
        topics=[TOPICS["morpho_liq"]],
        tip=tip,
        lookback=lookback,
        chunk=chunk,
        max_hits=20,
    )
    return [
        {
            "label": "bad_debt_liq",
            "chain": chain,
            "block": hex_int(lg["blockNumber"]),
            "tx": lg["transactionHash"],
            "reason": "Morpho Liquidate — verify badDebtAssets>0 after index",
        }
        for lg in hits[:10]
    ]


def hunt_savax(url: str, chain: str, tip: int, lookback: int, chunk: int) -> list[dict]:
    if chain != "avalanche":
        return []
    # Transfers of sAVAX as a cheap co-presence hunt; explorer must confirm arb tag.
    hits = chunked_logs(
        url,
        address=SAVAX,
        topics=[TOPICS["transfer"]],
        tip=tip,
        lookback=min(lookback, 40_000),
        chunk=min(chunk, 1_500),
        max_hits=40,
    )
    # Prefer blocks with multiple sAVAX transfers (swap-like).
    by_block: dict[int, list] = {}
    for lg in hits:
        bn = hex_int(lg["blockNumber"])
        by_block.setdefault(bn, []).append(lg)
    out = []
    for bn, logs in sorted(by_block.items(), key=lambda kv: -len(kv[1])):
        if len(logs) < 2:
            continue
        out.append(
            {
                "label": "savax_rate_arb",
                "chain": chain,
                "block": bn,
                "tx": logs[0]["transactionHash"],
                "reason": f"{len(logs)} sAVAX transfers in block — index for savax_rate_arb",
            }
        )
        if len(out) >= 8:
            break
    return out


HUNTERS = {
    "flash_loan_liq": hunt_flash_loan_liq,
    "flash_arb": hunt_flash_arb,
    "keeper": hunt_keeper,
    "bundler": hunt_bundler,
    "benqi": hunt_benqi,
    "jit": hunt_jit,
    "lb_bin_jit": hunt_lb_bin_jit,
    "gmx_adl_arb": hunt_gmx_adl,
    "epoch_transition": hunt_epoch,
    "oracle_poke_block": hunt_oracle_poke,
    "buycollateral": hunt_buycollateral,
    "bad_debt_liq": hunt_bad_debt,
    "savax_rate_arb": hunt_savax,
}


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--list", action="store_true")
    ap.add_argument("--label", help="strategy label to hunt")
    ap.add_argument("--chain", default="avalanche", choices=sorted(RPC_DEFAULTS))
    ap.add_argument("--rpc-url")
    ap.add_argument("--lookback-blocks", type=int, default=150_000)
    ap.add_argument("--chunk", type=int, default=4_000)
    ap.add_argument("--from-block", type=int)
    ap.add_argument("--to-block", type=int)
    args = ap.parse_args()

    if args.list:
        for k in sorted(HUNTERS):
            print(k)
        return 0
    if not args.label:
        ap.error("--label required (or --list)")
    if args.label not in HUNTERS:
        print(f"unknown label {args.label}; use --list", file=sys.stderr)
        return 2

    url = args.rpc_url or RPC_DEFAULTS[args.chain]
    tip = hex_int(rpc(url, "eth_blockNumber", []))
    if args.to_block:
        tip = args.to_block
    lookback = args.lookback_blocks
    if args.from_block is not None:
        lookback = tip - args.from_block

    print(f"chain={args.chain} tip={tip} lookback={lookback} rpc={url}", file=sys.stderr)
    hits = HUNTERS[args.label](url, args.chain, tip, lookback, args.chunk)
    print(json.dumps({"label": args.label, "chain": args.chain, "tip": tip, "hits": hits}, indent=2))
    return 0 if hits else 1


if __name__ == "__main__":
    raise SystemExit(main())
