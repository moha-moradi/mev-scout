#!/usr/bin/env node
/**
 * BSC liquidation hunt (dwellir RPC):
 * 1. Verify Aave LendingPool 0x6807… identity (V2 vs V3) via eth_call.
 * 2. Hunt LiquidationCall (Aave) on the pool over recent blocks.
 * 3. Hunt Compound-V2 `LiquidateBorrow` (Venus) across all vTokens.
 * Reports emitter density, top searchers (tx sender / liquidator topic),
 * and dense windows for corpus seeding.
 *
 *   node scripts/hunt_bsc_liquidations.mjs --lookback-blocks 400000
 */

const RPC = process.env.BSC_RPC;
if (!RPC) {
  console.error("set BSC_RPC=https://api-bsc-mainnet-full.n.dwellir.com/...");
  process.exit(1);
}

const AAVE_POOL = "0x6807dc923806fE8Fd134338EABCA509979a7e0cB";
const AAVE_LIQ_TOPIC =
  "0xe413a321e8681d831f4dbccbca790d2952b56f977908e45be37335533e005286"; // LiquidationCall
const LIQATE_BORROW_TOPIC =
  "0x298637f684da70674f26509b10f07ec2fbc77a335ab1e7d6215a4b2484d8bb52"; // LiquidateBorrow

import https from "node:https";
function call(method, params) {
  const url = new URL(RPC);
  const path = url.pathname || "/";
  const body = JSON.stringify({ jsonrpc: "2.0", id: 1, method, params });
  return new Promise((resolve, reject) => {
    const req = https.request(
      {
        host: url.host,
        port: url.port ? parseInt(url.port, 10) : 443,
        path: path + (url.search ? url.search : ""),
        method: "POST",
        headers: {
          "Content-Type": "application/json",
          "User-Agent": "mev-scout-bsc-hunt",
          "Host": url.host,
        },
      },
      (res) => {
        let data = "";
        res.on("data", (c) => (data += c));
        res.on("end", () => {
          try {
            const j = JSON.parse(data);
            if (j.error) throw new Error(j.error.message ?? JSON.stringify(j.error));
            resolve(j.result);
          } catch (e) {
            reject(new Error("bad jsonrpc response: " + e.message + " | " + data.slice(0, 400)));
          }
        });
      },
      (e) => reject(e),
    );
    req.setTimeout(60000, () => req.destroy(new Error("request timeout")));
    req.on("error", reject);
    req.write(body);
    req.end();
  });
}
const hexInt = (x) => parseInt(x, 16);

async function chunkLogs(address, topics, tip, lookback, chunk, maxHits) {
  const hits = [];
  let end = tip;
  let c = chunk;
  const start = Math.max(0, tip - lookback);
  while (end >= start && hits.length < maxHits) {
    const fr = Math.max(start, end - c + 1);
    try {
      const batch = await call("eth_getLogs", [
        {
          fromBlock: "0x" + fr.toString(16),
          toBlock: "0x" + end.toString(16),
          ...(address ? { address } : { address: null }),
          topics,
        },
      ]);
      hits.push(...batch);
      console.error(`[scan] ${fr}..${end} +${batch.length} total=${hits.length}`);
      end = fr - 1;
    } catch (e) {
      if (c <= 400) throw e;
      c = Math.max(400, Math.floor(c / 2));
      console.error(`[scan] failed ${fr}..${end}: ${e.message} — shrink chunk to ${c}`);
    }
    await new Promise((r) => setTimeout(r, 150));
  }
  return hits.slice(0, maxHits);
}

async function histogram(hits, bucket) {
  const h = {};
  for (const lg of hits) {
    const b = Math.floor(hexInt(lg.blockNumber) / bucket) * bucket;
    h[b] = (h[b] || 0) + 1;
  }
  return Object.entries(h)
    .map(([k, v]) => [parseInt(k), v])
    .sort((a, b) => a[0] - b[0]);
}

(async () => {
  const tip = hexInt(await call("eth_blockNumber", []));
  console.log("BSC tip:", tip);

  // pool identity
  const code = await call("eth_getCode", [AAVE_POOL]);
  console.log("LendingPool code len:", (code.length - 2) / 2);
  const version = await call("eth_call", [{ from: AAVE_POOL, data: "0xd4f44c97" }]);
  console.log("getVersion():", version);
  try {
    await call("eth_call", [{ from: AAVE_POOL, data: "0x3b3e5e34" }]);
    console.log("V2 liquidate(address,address,uint256,bool) selector CALLS OK => Aave V2 pool");
  } catch (e) {
    console.log("V2 liquidate selector call failed => NOT V2: " + e.message);
  }

  // Aave hunt
  console.log("\n=== Aave LiquidationCall hunt ===");
  const aaveHits = await chunkLogs(AAVE_POOL, [AAVE_LIQ_TOPIC], tip, 400000, 10000, 600);
  console.log("Aave hits (400k blocks):", aaveHits.length);
  if (aaveHits.length) {
    console.log("Aave histogram (5k buckets):");
    for (const [b, n] of histogram(aaveHits, 5000))
      console.log(`  ${b}..${b + 5000 - 1}: ${n}`);
    const top = aaveHits.slice(-8);
    for (const lg of top) {
      let from;
      try {
        const tx = await call("eth_getTransactionByHash", [lg.transactionHash]);
        from = tx.from;
      } catch (e) {
        from = "n/a";
      }
      console.log(
        "  block=", hexInt(lg.blockNumber),
        " tx=", lg.transactionHash,
        "searcher=", from,
        "collateral=", lg.topics[1],
        "debt=", lg.topics[2],
        "user=", lg.topics[3],
        "data=" + lg.data.slice(0, 34),
      );
    }
  }

  // Venus hunt
  console.log("\n=== Venus LiquidateBorrow hunt ===");
  const liqHits = await chunkLogs(null, [LIQATE_BORROW_TOPIC], tip, 400000, 10000, 800);
  console.log("LiquidateBorrow hits (400k blocks, all vTokens):", liqHits.length);
  if (liqHits.length) {
    const byAddr = {};
    for (const lg of liqHits) {
      byAddr[lg.address.toLowerCase()] = (byAddr[lg.address.toLowerCase()] || 0) + 1;
    }
    console.log("by vToken (top 20):");
    Object.entries(byAddr)
      .sort((a, b) => b[1] - a[1])
      .slice(0, 20)
      .forEach(([a, n]) => console.log(`  ${a}: ${n}`));
    console.log("LiquidateBorrow histogram (5k buckets):");
    for (const [b, n] of histogram(liqHits, 5000))
      console.log(`  ${b}..${b + 5000 - 1}: ${n}`);
    const top = liqHits.slice(-8);
    for (const lg of top) {
      let from;
      try {
        const tx = await call("eth_getTransactionByHash", [lg.transactionHash]);
        from = tx.from;
      } catch (e) {
        from = "n/a";
      }
      const liquidator = lg.topics[1];
      const borrower = lg.topics[2];
      const repayAmount = lg.data.length >= 116 ? lg.data.slice(98, 130) : "?";
      const seizeTokens = lg.data.length >= 146 ? lg.data.slice(126, 158) : "?";
      console.log(
        "  block=", hexInt(lg.blockNumber),
        " vToken=", lg.address,
        "liquidator=", liquidator,
        "borrower=", borrower,
        "tx.from=", from,
        "repay=", hexInt(repayAmount),
        "seize=", parseInt(seizeTokens, 16),
      );
    }
  }

  console.log("\ndone");
})();
