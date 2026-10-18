#!/usr/bin/env node
/**
 * Node twin of hunt_strategy.py (Windows hosts may block python.exe).
 *
 *   node scripts/hunt_strategy.mjs --list
 *   node scripts/hunt_strategy.mjs --label flash_loan_liq --chain avalanche
 *   node scripts/hunt_strategy.mjs --label keeper --lookback-blocks 80000
 */
import { keccak256 } from "js-sha3";

const topic = (sig) => "0x" + keccak256(sig);

const TOPICS = {
  aave_v3_flash: topic(
    "FlashLoan(address,address,address,uint256,uint8,uint256,uint16)",
  ),
  aave_v2_flash: topic(
    "FlashLoan(address,address,address,uint256,uint256,uint16)",
  ),
  aave_liq: topic(
    "LiquidationCall(address,address,address,uint256,uint256,address,bool)",
  ),
  balancer_flash: topic("FlashLoan(address,address,address,uint256,bytes)"),
  morpho_flash: topic("FlashLoan(address,address,uint256)"),
  gelato_exec: topic(
    "ExecSuccess(uint256,address,address,bytes,bytes32,bool)",
  ),
  user_op: topic(
    "UserOperationEvent(bytes32,address,address,uint256,bool,uint256,uint256)",
  ),
  v3_mint: topic("Mint(address,address,int24,int24,uint128,uint256,uint256)"),
  v3_burn: topic("Burn(address,int24,int24,uint128,uint256,uint256)"),
  transfer: topic("Transfer(address,address,uint256)"),
  comp_v2_liq: topic(
    "LiquidateBorrow(address,address,uint256,address,uint256)",
  ),
  morpho_liq: topic(
    "Liquidate(bytes32,address,address,uint256,uint256,uint256,uint256,uint256)",
  ),
  buy_collateral: topic("BuyCollateral(address,address,uint256,uint256)"),
  notify_reward: topic("NotifyReward(address,address,uint256)"),
  answer_updated: topic("AnswerUpdated(int256,uint256,uint256)"),
  lb_deposit: topic("DepositedToBins(address,address,uint256[],bytes32[])"),
  lb_withdraw: topic("WithdrawnFromBins(address,address,uint256[],bytes32[])"),
  upkeep: topic(
    "UpkeepPerformed(uint256,bool,uint96,uint256,uint256,bytes)",
  ),
  curve_ex: topic("TokenExchange(address,int128,uint256,int128,uint256)"),
  balancer_swap: topic("Swap(bytes32,address,address,uint256,uint256)"),
};

// publicnode is reliable for address-bounded getLogs; set AVAX_RPC for archive.
const RPC_DEFAULTS = {
  avalanche:
    process.env.AVAX_RPC || "https://avalanche-c-chain-rpc.publicnode.com",
  polygon: process.env.POLYGON_RPC || "https://polygon-bor-rpc.publicnode.com",
  ethereum: process.env.ETH_RPC || "https://ethereum-rpc.publicnode.com",
};

// Known keeper registries (address required — public nodes reject topic-only).
const GELATO_AUTOMATE = {
  avalanche: "0x340759c8346A1E1Ed47082f1f1F353f8efc34D9b",
  polygon: "0xB3f5867E977688E877C1A56050AE5597A08EDA0",
  ethereum: "0x2A6C106ae13B558BB9E2Ec64Bd2f1f7BEFF3A5E0",
};
const CHAINLINK_REGISTRY = {
  avalanche: "0x02777053d6764996e594c3E88AF1D58D5363a2e6",
  polygon: "0x02777053d6764996e594c3E88AF1D58D5363a2e6",
  ethereum: "0x02777053d6764996e594c3E88AF1D58D5363a2e6",
};

const AAVE_POOLS = {
  avalanche: [
    "0x69FA688f1Dc47d4B5d8029D5a35FB7a548E0B9b0", // V3
    "0x4F01aEd16D97E3aB5ab2B501154DC9bb0F1A5A2C", // V2
  ],
  polygon: ["0x794a61358D6845594F94dc1DB02A252b5b4814aD"],
  ethereum: ["0x87870Bca3F3fD6335C3F4ce8392D69350B4fA4E2"],
};

const ENTRY_POINT = "0x5FF137D4b0FDCD49DcA30c7CF57E578a026d2789";
const GMX_EMITTER = "0xDb17B233827785b5EA1aD91C9bD7D4dC478B9389";
const MORPHO_BLUE = "0xBBBBBbbBBb9cC5e90e3b3Af64bdAF62C37EEFFCb";
const SAVAX = "0x2b2C81e08f1Af8835a78Bb2A90AE924ACE0eA4bE";
const BALANCER_VAULT = "0xBA12222222228d8Ba445958a75a0704d566BF2C8";
const BENQI_QIAVAX = "0x5c0401e81bc07ca70fad469b451682c0d747ef1c";
const ETH_COMETS = [
  "0xc3d688B66703497DAA19211EEdff47f25384cdc3",
  "0xA17581A9E3356d9A858b789D68B4d866e593aE94",
];

function parseArgs(argv) {
  const out = {
    list: false,
    label: null,
    chain: "avalanche",
    rpcUrl: null,
    lookback: 120000,
    chunk: 3000,
    fromBlock: null,
    toBlock: null,
  };
  for (let i = 2; i < argv.length; i++) {
    const a = argv[i];
    const next = () => argv[++i];
    if (a === "--list") out.list = true;
    else if (a === "--label") out.label = next();
    else if (a === "--chain") out.chain = next();
    else if (a === "--rpc-url") out.rpcUrl = next();
    else if (a === "--lookback-blocks") out.lookback = Number(next());
    else if (a === "--chunk") out.chunk = Number(next());
    else if (a === "--from-block") out.fromBlock = Number(next());
    else if (a === "--to-block") out.toBlock = Number(next());
    else throw new Error(`unknown arg ${a}`);
  }
  return out;
}

async function rpc(url, method, params) {
  for (let attempt = 0; attempt < 5; attempt++) {
    try {
      const r = await fetch(url, {
        method: "POST",
        headers: {
          "Content-Type": "application/json",
          "User-Agent": "mev-scout-hunt/0.1",
        },
        body: JSON.stringify({ jsonrpc: "2.0", id: 1, method, params }),
      });
      const j = await r.json();
      if (j.error) throw new Error(JSON.stringify(j.error));
      return j.result;
    } catch (e) {
      if (attempt === 4) throw e;
      await new Promise((r) => setTimeout(r, 1200 * (attempt + 1)));
      console.error(`retry ${attempt + 1}: ${e}`);
    }
  }
}

const hexInt = (x) => (typeof x === "number" ? x : parseInt(x, 16));

async function getLogs(url, { address, topics, fromBlock, toBlock }) {
  const filt = {
    fromBlock: "0x" + fromBlock.toString(16),
    toBlock: "0x" + toBlock.toString(16),
    topics,
  };
  if (address != null) filt.address = address;
  return (await rpc(url, "eth_getLogs", [filt])) || [];
}

async function chunkedLogs(
  url,
  { address, topics, tip, lookback, chunk, maxHits },
) {
  const start = Math.max(0, tip - lookback);
  const hits = [];
  let end = tip;
  let curChunk = chunk;
  while (end >= start && hits.length < maxHits) {
    const fr = Math.max(start, end - curChunk + 1);
    try {
      const batch = await getLogs(url, {
        address,
        topics,
        fromBlock: fr,
        toBlock: end,
      });
      hits.push(...batch);
      console.error(
        `scanned ${fr}..${end} hits=+${batch.length} total=${hits.length}`,
      );
      end = fr - 1;
    } catch (e) {
      if (curChunk <= 400) {
        console.error(`getLogs failed ${fr}..${end}: ${e}`);
        break;
      }
      curChunk = Math.max(400, Math.floor(curChunk / 2));
      console.error(`shrinking chunk to ${curChunk}`);
      continue;
    }
    await new Promise((r) => setTimeout(r, 120));
  }
  return hits.slice(0, maxHits);
}

const sameTx = (a, b) => a.transactionHash === b.transactionHash;

async function huntFlashLoanLiq(url, chain, tip, lookback, chunk) {
  const pools = AAVE_POOLS[chain] || [];
  const liqs = await chunkedLogs(url, {
    address: pools.length === 1 ? pools[0] : pools,
    topics: [TOPICS.aave_liq],
    tip,
    lookback,
    chunk,
    maxHits: 40,
  });
  const out = [];
  const flashTopics = [
    TOPICS.aave_v3_flash,
    TOPICS.aave_v2_flash,
    TOPICS.balancer_flash,
    TOPICS.morpho_flash,
  ];
  for (const lg of liqs) {
    const bn = hexInt(lg.blockNumber);
    const flashes = await getLogs(url, {
      address: null,
      topics: [flashTopics],
      fromBlock: bn,
      toBlock: bn,
    });
    if (flashes.some((f) => sameTx(lg, f))) {
      out.push({
        label: "flash_loan_liq",
        chain,
        block: bn,
        tx: lg.transactionHash,
        reason: "LiquidationCall + flash loan same tx",
      });
    }
    if (out.length >= 5) break;
  }
  return out;
}

async function huntFlashArb(url, chain, tip, lookback, chunk) {
  const addrs = [
    ...(AAVE_POOLS[chain] || []),
    BALANCER_VAULT,
    ...(chain === "avalanche" ? [] : [MORPHO_BLUE]),
  ];
  const flashes = await chunkedLogs(url, {
    address: addrs,
    topics: [[TOPICS.aave_v3_flash, TOPICS.aave_v2_flash, TOPICS.balancer_flash]],
    tip,
    lookback,
    chunk,
    maxHits: 25,
  });
  return flashes.slice(0, 10).map((lg) => ({
    label: "flash_arb",
    chain,
    block: hexInt(lg.blockNumber),
    tx: lg.transactionHash,
    reason: "flash-loan log — index and filter arb_atomic+flash_arb",
  }));
}

async function huntKeeper(url, chain, tip, lookback, chunk) {
  const addrs = [GELATO_AUTOMATE[chain], CHAINLINK_REGISTRY[chain]].filter(
    Boolean,
  );
  const hits = await chunkedLogs(url, {
    address: addrs.length === 1 ? addrs[0] : addrs,
    topics: [[TOPICS.gelato_exec, TOPICS.upkeep]],
    tip,
    lookback: Math.min(lookback, 200000),
    chunk: Math.min(chunk, 4000),
    maxHits: 15,
  });
  return hits.slice(0, 8).map((lg) => ({
    label: "keeper",
    chain,
    block: hexInt(lg.blockNumber),
    tx: lg.transactionHash,
    reason: "Gelato ExecSuccess or Chainlink UpkeepPerformed",
  }));
}

async function huntBundler(url, chain, tip, lookback, chunk) {
  const hits = await chunkedLogs(url, {
    address: ENTRY_POINT,
    topics: [TOPICS.user_op],
    tip,
    lookback,
    chunk,
    maxHits: 8,
  });
  return hits.slice(0, 5).map((lg) => ({
    label: "bundler",
    chain,
    block: hexInt(lg.blockNumber),
    tx: lg.transactionHash,
    reason: "EntryPoint UserOperationEvent",
  }));
}

async function huntBenqi(url, chain, tip, lookback, chunk) {
  if (chain !== "avalanche") return [];
  const hits = await chunkedLogs(url, {
    address: BENQI_QIAVAX,
    topics: [TOPICS.comp_v2_liq],
    tip,
    lookback,
    chunk,
    maxHits: 10,
  });
  return hits.slice(0, 5).map((lg) => ({
    label: "benqi",
    chain,
    block: hexInt(lg.blockNumber),
    tx: lg.transactionHash,
    reason: "Benqi qiAVAX LiquidateBorrow",
  }));
}

async function huntJit(url, chain, tip, lookback, chunk) {
  const mints = await chunkedLogs(url, {
    address: null,
    topics: [TOPICS.v3_mint],
    tip,
    lookback: Math.min(lookback, 25000),
    chunk: Math.min(chunk, 1200),
    maxHits: 40,
  });
  const out = [];
  for (const m of mints) {
    const bn = hexInt(m.blockNumber);
    const burns = await getLogs(url, {
      address: m.address,
      topics: [TOPICS.v3_burn],
      fromBlock: bn,
      toBlock: bn,
    });
    if (burns.length) {
      out.push({
        label: "jit",
        chain,
        block: bn,
        tx: m.transactionHash,
        reason: "V3 Mint+Burn same pool/block",
      });
    }
    if (out.length >= 5) break;
  }
  return out;
}

async function huntLbBinJit(url, chain, tip, lookback, chunk) {
  const deps = await chunkedLogs(url, {
    address: null,
    topics: [TOPICS.lb_deposit],
    tip,
    lookback: Math.min(lookback, 80000),
    chunk: Math.min(chunk, 2000),
    maxHits: 30,
  });
  const out = [];
  for (const d of deps) {
    const bn = hexInt(d.blockNumber);
    const wds = await getLogs(url, {
      address: d.address,
      topics: [TOPICS.lb_withdraw],
      fromBlock: bn,
      toBlock: bn,
    });
    if (wds.some((w) => sameTx(d, w))) {
      out.push({
        label: "lb_bin_jit",
        chain,
        block: bn,
        tx: d.transactionHash,
        reason: "LB deposit+withdraw same tx",
      });
    }
    if (out.length >= 5) break;
  }
  return out;
}

async function huntGmx(url, chain, tip, lookback, chunk) {
  if (chain !== "avalanche") return [];
  const out = [];
  const cand = 81075048;
  try {
    const logs = await getLogs(url, {
      address: GMX_EMITTER,
      topics: [],
      fromBlock: cand,
      toBlock: cand,
    });
    if (logs.length) {
      out.push({
        label: "gmx_adl_arb",
        chain,
        block: cand,
        tx: logs[0].transactionHash,
        reason: "GMX EventEmitter on candidate block",
      });
    }
  } catch (e) {
    console.error(`gmx candidate ${cand} skipped: ${e}`);
  }
  const hits = await chunkedLogs(url, {
    address: GMX_EMITTER,
    topics: [],
    tip,
    lookback: Math.min(lookback, 80000),
    chunk: Math.min(chunk, 2000),
    maxHits: 15,
  });
  for (const lg of hits.slice(0, 5)) {
    out.push({
      label: "gmx_adl_arb",
      chain,
      block: hexInt(lg.blockNumber),
      tx: lg.transactionHash,
      reason: "GMX EventEmitter log",
    });
  }
  return out;
}

async function huntEpoch(url, chain, tip, lookback, chunk) {
  const hits = await chunkedLogs(url, {
    address: null,
    topics: [TOPICS.notify_reward],
    tip,
    lookback: Math.min(lookback, 150000),
    chunk: Math.min(chunk, 2500),
    maxHits: 15,
  });
  return hits.slice(0, 8).map((lg) => ({
    label: "epoch_transition",
    chain,
    block: hexInt(lg.blockNumber),
    tx: lg.transactionHash,
    reason: "NotifyReward",
  }));
}

async function huntOraclePoke(url, chain, tip, lookback, chunk) {
  const pools = AAVE_POOLS[chain] || [];
  if (!pools.length) return [];
  const liqs = await chunkedLogs(url, {
    address: pools.length === 1 ? pools[0] : pools,
    topics: [TOPICS.aave_liq],
    tip,
    lookback,
    chunk,
    maxHits: 25,
  });
  const out = [];
  for (const lg of liqs) {
    const bn = hexInt(lg.blockNumber);
    const answers = await getLogs(url, {
      address: null,
      topics: [TOPICS.answer_updated],
      fromBlock: bn,
      toBlock: bn,
    });
    if (answers.length) {
      out.push({
        label: "oracle_poke_block",
        chain,
        block: bn,
        tx: lg.transactionHash,
        reason: "LiquidationCall co-block AnswerUpdated",
      });
    }
    if (out.length >= 5) break;
  }
  return out;
}

async function huntBuyCollateral(url, chain, tip, lookback, chunk) {
  if (chain !== "ethereum") return [];
  const hits = await chunkedLogs(url, {
    address: ETH_COMETS,
    topics: [TOPICS.buy_collateral],
    tip,
    lookback,
    chunk,
    maxHits: 15,
  });
  return hits.slice(0, 8).map((lg) => ({
    label: "buycollateral",
    chain,
    block: hexInt(lg.blockNumber),
    tx: lg.transactionHash,
    reason: "Compound V3 BuyCollateral",
  }));
}

async function huntBadDebt(url, chain, tip, lookback, chunk) {
  if (chain === "avalanche") return [];
  const hits = await chunkedLogs(url, {
    address: MORPHO_BLUE,
    topics: [TOPICS.morpho_liq],
    tip,
    lookback,
    chunk,
    maxHits: 20,
  });
  return hits.slice(0, 10).map((lg) => ({
    label: "bad_debt_liq",
    chain,
    block: hexInt(lg.blockNumber),
    tx: lg.transactionHash,
    reason: "Morpho Liquidate — check badDebtAssets after index",
  }));
}

async function huntSavax(url, chain, tip, lookback, chunk) {
  if (chain !== "avalanche") return [];
  const hits = await chunkedLogs(url, {
    address: SAVAX,
    topics: [TOPICS.transfer],
    tip,
    lookback: Math.min(lookback, 40000),
    chunk: Math.min(chunk, 1500),
    maxHits: 50,
  });
  const byBlock = new Map();
  for (const lg of hits) {
    const bn = hexInt(lg.blockNumber);
    if (!byBlock.has(bn)) byBlock.set(bn, []);
    byBlock.get(bn).push(lg);
  }
  return [...byBlock.entries()]
    .filter(([, logs]) => logs.length >= 2)
    .sort((a, b) => b[1].length - a[1].length)
    .slice(0, 8)
    .map(([bn, logs]) => ({
      label: "savax_rate_arb",
      chain,
      block: bn,
      tx: logs[0].transactionHash,
      reason: `${logs.length} sAVAX transfers in block`,
    }));
}

async function huntCurve(url, chain, tip, lookback, chunk) {
  const hits = await chunkedLogs(url, {
    address: null,
    topics: [TOPICS.curve_ex],
    tip,
    lookback: Math.min(lookback, 100000),
    chunk: Math.min(chunk, 2000),
    maxHits: 20,
  });
  return hits.slice(0, 8).map((lg) => ({
    label: "curve_imbalance",
    chain,
    block: hexInt(lg.blockNumber),
    tx: lg.transactionHash,
    reason: "Curve TokenExchange — index for cross-venue arb tag",
  }));
}

async function huntBalancerStale(url, chain, tip, lookback, chunk) {
  const hits = await chunkedLogs(url, {
    address: BALANCER_VAULT,
    topics: [TOPICS.balancer_swap],
    tip,
    lookback: Math.min(lookback, 80000),
    chunk: Math.min(chunk, 2000),
    maxHits: 20,
  });
  return hits.slice(0, 8).map((lg) => ({
    label: "rate_provider_staleness",
    chain,
    block: hexInt(lg.blockNumber),
    tx: lg.transactionHash,
    reason: "Balancer swap — index for cross-venue arb without rate cache",
  }));
}

const HUNTERS = {
  flash_loan_liq: huntFlashLoanLiq,
  flash_arb: huntFlashArb,
  keeper: huntKeeper,
  bundler: huntBundler,
  benqi: huntBenqi,
  jit: huntJit,
  lb_bin_jit: huntLbBinJit,
  gmx_adl_arb: huntGmx,
  epoch_transition: huntEpoch,
  oracle_poke_block: huntOraclePoke,
  buycollateral: huntBuyCollateral,
  bad_debt_liq: huntBadDebt,
  savax_rate_arb: huntSavax,
  curve_imbalance: huntCurve,
  rate_provider_staleness: huntBalancerStale,
};

const args = parseArgs(process.argv);
if (args.list) {
  for (const k of Object.keys(HUNTERS).sort()) console.log(k);
  process.exit(0);
}
if (!args.label) {
  console.error("--label required (or --list)");
  process.exit(2);
}
if (!HUNTERS[args.label]) {
  console.error(`unknown label ${args.label}`);
  process.exit(2);
}

const url = args.rpcUrl || RPC_DEFAULTS[args.chain];
let tip = hexInt(await rpc(url, "eth_blockNumber", []));
if (args.toBlock != null) tip = args.toBlock;
let lookback = args.lookback;
if (args.fromBlock != null) lookback = tip - args.fromBlock;

console.error(`chain=${args.chain} tip=${tip} lookback=${lookback} rpc=${url}`);
const hits = await HUNTERS[args.label](
  url,
  args.chain,
  tip,
  lookback,
  args.chunk,
);
console.log(
  JSON.stringify(
    { label: args.label, chain: args.chain, tip, hits },
    null,
    2,
  ),
);
process.exit(hits.length ? 0 : 1);
