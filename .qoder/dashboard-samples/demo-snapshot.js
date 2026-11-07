/* Deterministic demo snapshot shaped like core::explorer::dashboard::DashboardSnapshot.
 * Used only by the design samples in this folder. Real artifacts embed the JSON
 * produced by `mev-scout explorer dashboard --format html`. */
(function () {
  var seed = 0x9e3779b9;
  function R() {
    seed |= 0; seed = (seed + 0x6d2b79f5) | 0;
    var t = Math.imul(seed ^ (seed >>> 15), 1 | seed);
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  }
  function randn() {
    var u = 1 - R(), v = R();
    return Math.sqrt(-2 * Math.log(u)) * Math.cos(2 * Math.PI * v);
  }
  function pick(a) { return a[Math.floor(R() * a.length)]; }
  function hex(n) {
    var s = "";
    for (var i = 0; i < n; i++) s += "0123456789abcdef"[Math.floor(R() * 16)];
    return s;
  }
  function addr() { return "0x" + hex(40); }

  var HOUR = 3600;
  var now = Math.floor(Date.now() / 1000);
  var end = Math.floor(now / HOUR) * HOUR;
  var start = end - 167 * HOUR;
  var HEAD = 49283410;

  var ACTORS = [
    { label: "Nova", eoa: "0x4f3a1c9b2d7e5a8c0b6d2f4e1a9c7b3d5e8f0a12", contract: "0x51ab7c3e9d2f4a6b8c0e1d3f5a7b9c2e4d6f8a01", w: 17 },
    { label: "Cobalt", eoa: "0x7d2e9b4c1a8f6e3d5c0b7a2f9e4d8c1b6a3f5e70", contract: "0x2c8b5e1f9a3d7c4b0e6f2a8d5c1b9e3f7a4d0c62", w: 14 },
    { label: "Meridian", eoa: "0x9a4c2e6b8d1f3a5c7e0b2d4f6a8c1e3b5d7f9a24", contract: null, w: 12 },
    { label: "Rook", eoa: "0x1e5b8d3f6a9c2e4b7d0f1a3c5e8b2d4f6a9c1e35", contract: "0x8f3a6c1e4b9d2f7a0c5e8b1d3f6a9c4e2b7d0f58", w: 11 },
    { label: "Ghostline", eoa: "0x6c1f4a8d2e5b9c3f7a0d4e8b1c6f9a3d5e2b7c40", contract: null, w: 9 },
    { label: "Palisade", eoa: "0x3b7e0c4f8a1d5e9b2c6f0a3d7e1b4c8f5a9d2e63", contract: "0x0a3d6f9c2e5b8d1f4a7c0e3b6d9f2a5c8e1b4d70", w: 8 },
    { label: "Vector", eoa: "0x8d2a5c9f1e4b7d0a3c6f9e2b5d8a1c4f7e0b3d56", contract: "0x5f2c8b1e4a7d0f3c6e9b2d5a8f1c4e7b0d3a6f92", w: 8 },
    { label: "Kiln", eoa: "0xc4f7a2d5e8b3c6f1a4d7e0b3c6f9a2d5e8b1c4f07", contract: null, w: 7 },
    { label: "Atlas", eoa: "0x2a7d0f3c6b9e1d4a8c0f3b6e9d2a5c8f1b4e7d05", contract: "0xe1b4d7a0c3f6e9b2d5a8c1f4e7b0d3a6c9f2e51c", w: 6 },
    { label: "Quarry", eoa: "0xf6c3a0d7e4b1c8f5a2d9e6b3c0f7a4d1e8b5c2f39", contract: null, w: 4 }
  ];

  var KINDS = [
    { k: "arb_atomic", w: 38, subs: ["v2_v3", "triangular", "stable", "v3_v3"] },
    { k: "backrun", w: 15, subs: ["v2", "v3"] },
    { k: "sandwich", w: 13, subs: ["v2", "v3"] },
    { k: "jit", w: 7, subs: ["uniswap_v3"] },
    { k: "jit_arb", w: 5, subs: ["uniswap_v3"] },
    { k: "liquidation", w: 5, subs: ["benqi", "aave_v3"] },
    { k: "frontrun", w: 5, subs: ["v2", "v3"] },
    { k: "skim", w: 2, subs: ["v2"] },
    { k: "utility", w: 10, subs: null }
  ];

  var POOLS = [
    { pool: "0x7f9a2c4e6b8d1f3a5c7e0b2d4f6a8c1e3b5d7f90", amm: "v3", label: "WAVAX / USDC.e" },
    { pool: "0x1b4d7f9a2c5e8b0d3f6a9c1e4b7d0f2a5c8e1b3d", amm: "v2", label: "WAVAX / USDT.e" },
    { pool: "0x8e2b5d8f1a4c7e0b3d6f9a2c5e8b0d3f6a9c1e47", amm: "v3", label: "WETH.e / USDC.e" },
    { pool: "0x3c6e9b2d5f8a1c4e7b0d3f6a9c1e4b7d0f2a5c8e", amm: "curve", label: "USDC.e / USDT.e" },
    { pool: "0x9d1f4b7e0a3c6f9b2d5e8a1c4f7b0d3e6a9c2f58", amm: "v2", label: "JOE / WAVAX" },
    { pool: "0x5a8c1e4b7d0f3a6c9e2b5d8a1c4f7e0b3d6f9a2c", amm: "v3", label: "sAVAX / WAVAX" },
    { pool: "0x2f6a9c1e4b7d0f3a6c9e2b5d8a1c4f7e0b3d6f9a", amm: "algebra", label: "BTC.b / USDC.e" },
    { pool: "0x7b0d3f6a9c2e5b8d1f4a7c0e3b6d9f2a5c8e1b4d", amm: "v3", label: "WAVAX / USDC.e 0.05%" },
    { pool: "0x4e7a0c3f6b9d2e5a8c1f4b7d0a3e6f9c2b5d8a1e", amm: "v2", label: "USDC.e / WETH.e" },
    { pool: "0xd0f3b6e9c2a5d8f1b4e7a0c3f6b9d2e5a8c1f4b7", amm: "v3", label: "JOE / USDC.e" },
    { pool: "0x6b9e2d5a8c1f4b7d0a3e6f9c2b5d8a1e4f7b0d3a", amm: "curve", label: "USDC.e / FRAX" },
    { pool: "0x1f4b7e0a3d6f9c2b5e8a1d4f7b0c3e6a9d2f5b8c", amm: "algebra", label: "WAVAX / BTC.b" },
    { pool: "0xa3d6f9b2e5c8a1d4f7b0e3c6a9d2f5b8e1c4a7d0", amm: "v2", label: "KILN / WAVAX" },
    { pool: "0x8c1e4b7d0a3f6b9d2e5a8c1f4e7b0d3a6c9e2b5d", amm: "v3", label: "USDT.e / WETH.e" }
  ];

  var TOKENS = [
    "0xb31f66aa3c1e785363f0875a1b74e27b85fd66c7",
    "0xa7d7079b0fead91f3e65f86e8915cb59c1a4c664",
    "0xc7198437980c041c805a1edcba50c1ce5db95118",
    "0x49d5c2bdffac6ce2bfdb6640f4f80f226bc10bab",
    "0x2b2c81e08f1af8835a78bb2a90ae924ace0ea4be",
    "0x6e84a6216ea6dacc71ee8e6b0a5b7322eebc0fdd",
    "0x152b9d0fdc40c096757f570a51e494bd4b943e50"
  ];

  var KIND_MULT = {
    arb_atomic: 1, backrun: 0.45, sandwich: 1.9, jit: 0.5, jit_arb: 0.8,
    liquidation: 3.5, frontrun: 1.2, skim: 0.35, utility: 0.08
  };

  var events = [];
  var bucketCounts = [];
  var series = [];

  function kindWeighted() {
    var r = R() * 100, acc = 0, i;
    for (i = 0; i < KINDS.length; i++) { acc += KINDS[i].w; if (r < acc) return KINDS[i]; }
    return KINDS[0];
  }
  function actorWeighted() {
    var labelled = R() > 0.04;
    if (!labelled) {
      return { label: null, eoa: addr(), contract: null, w: 0 };
    }
    var total = 0, i;
    for (i = 0; i < ACTORS.length; i++) total += ACTORS[i].w;
    var r = R() * total, acc = 0;
    for (i = 0; i < ACTORS.length; i++) { acc += ACTORS[i].w; if (r < acc) return ACTORS[i]; }
    return ACTORS[0];
  }

  for (var b = 0; b < 168; b++) {
    var bucketTs = start + b * HOUR;
    var hod = new Date(bucketTs * 1000).getUTCHours();
    var wave = 7 + 5.5 * Math.sin(((hod - 9) / 24) * Math.PI * 2);
    var count = Math.max(1, Math.round(wave + randn() * 2.2));
    if (R() < 0.028) count += 18 + Math.floor(R() * 34);
    bucketCounts.push(count);
    for (var j = 0; j < count; j++) {
      var kd = kindWeighted();
      var actor = actorWeighted();
      var gross = Math.exp(randn() * 0.95 + 3.0) * KIND_MULT[kd.k];
      if (R() < 0.045) gross = -gross * (0.35 + R() * 0.9);
      var priced = R() > 0.03;
      var volume = gross > 0 ? gross * (8 + R() * 120) : gross * 4;
      var gas = 0.015 + Math.pow(R(), 3) * 2.4;
      var flashKind = kd.k === "arb_atomic" || kd.k === "liquidation";
      var flash = flashKind && R() < 0.45 ? volume * 0.0009 : 0;
      var net = gross - gas - flash;
      var excluded = R() < 0.012;
      var ts = bucketTs + Math.floor(R() * HOUR);
      var legs = 2 + (R() < 0.45 ? 1 : 0);
      var route = [], used = {};
      for (var l = 0; l < legs; l++) {
        var p = POOLS[Math.floor(R() * POOLS.length)];
        while (used[p.pool] && l < POOLS.length) p = POOLS[Math.floor(R() * POOLS.length)];
        used[p.pool] = true;
        route.push({
          pool: p.pool,
          amm: p.amm,
          token_in: pick(TOKENS),
          token_out: pick(TOKENS),
          token_source: R() < 0.9 ? "registry" : "synthesized",
          amount_in: String(Math.floor(1e15 + R() * 9e18)),
          amount_out: String(Math.floor(1e15 + R() * 9e18))
        });
      }
      var tags = [kd.k === "arb_atomic" ? "atomic" : kd.k];
      if (flash > 0) tags.push("flashloan");
      if (legs > 2) tags.push("multi_hop");
      tags.push(route[0].amm);
      var reasons = [];
      if (gross < 0 && R() < 0.4) reasons.push("gas_dominates");
      if (R() < 0.05) reasons.push("below_min_profit");
      events.push({
        id: events.length + 1,
        block_number: HEAD - 9 - Math.floor((end + HOUR - ts) / 1.0),
        tx_index: Math.floor(R() * 130),
        tx_hash: "0x" + hex(64),
        ts: ts,
        kind: kd.k,
        subkind: kd.subs ? pick(kd.subs) : null,
        confidence: R() < 0.58 ? "exact" : "inferred",
        eoa: actor.eoa,
        contract: actor.contract,
        actor_label: actor.label,
        profit_usd: priced ? gross : null,
        net_profit_usd: priced ? net : null,
        gas_cost_usd: gas,
        volume_usd: priced ? volume : null,
        flashloan_fee_usd: flash,
        detector: "det." + kd.k.replace("_", "-"),
        excluded_from_net: excluded,
        pnl_basis: "searcher_net",
        route: route,
        tags: tags,
        reasons: reasons,
        victim_hashes: kd.k === "sandwich" || kd.k === "frontrun" ? ["0x" + hex(64)] : [],
        details: { tags: tags, reasons: reasons, pnl_basis: "searcher_net" }
      });
    }
  }

  events.sort(function (a, b) { return a.ts - b.ts || a.block_number - b.block_number; });

  var useBuckets = {};
  bucketCounts.forEach(function (n, i) {
    useBuckets[start + i * HOUR] = { ops: 0, net: 0, blocks: 3590 + Math.floor(R() * 10) };
  });

  var byActor = {}, byKind = {}, byPool = {}, byToken = {}, byTag = {}, byReason = {};
  var kpi = {
    ops: events.length, volume_usd: 0, volume_priced: 0, gross_usd: 0, gross_priced: 0,
    gas_usd: 0, gas_priced: 0, flash_fee_usd: 0, flash_fee_priced: 0, net_usd: 0,
    net_priced: 0, net_gating_available: true, highest_single_usd: 0, searchers: 0, competitors: 0
  };
  var quality = {
    ops_priced: 0, ops_unpriced: 0, ops_exact: 0, ops_inferred: 0, net_usd_exact: 0,
    net_usd_inferred: 0, ops_approx: 0, ops_native_unpriced: 0, ops_multi_asset: 0,
    ops_pricing_clamped: 0, ops_flash_fee_unpriced: 0, ops_excluded_from_net: 0,
    pricing_coverage_pct: 0, pricing_flags_available: true
  };
  var eoas = {}, comps = {};

  events.forEach(function (e) {
    var bucket = Math.floor(e.ts / HOUR) * HOUR;
    if (useBuckets[bucket]) {
      useBuckets[bucket].ops += 1;
      if (!e.excluded_from_net && e.net_profit_usd != null) useBuckets[bucket].net += e.net_profit_usd;
    }
    eoas[e.eoa] = 1;
    var comp = e.contract || e.eoa;
    comps[comp] = 1;

    if (e.volume_usd != null) { kpi.volume_usd += e.volume_usd; kpi.volume_priced += 1; }
    if (e.profit_usd != null) {
      kpi.gross_usd += e.profit_usd; kpi.gross_priced += 1; quality.ops_priced += 1;
      if (e.profit_usd > kpi.highest_single_usd) kpi.highest_single_usd = e.profit_usd;
    } else quality.ops_unpriced += 1;
    kpi.gas_usd += e.gas_cost_usd; kpi.gas_priced += 1;
    kpi.flash_fee_usd += e.flashloan_fee_usd; kpi.flash_fee_priced += 1;
    if (e.excluded_from_net) quality.ops_excluded_from_net += 1;
    else if (e.net_profit_usd != null) {
      kpi.net_usd += e.net_profit_usd; kpi.net_priced += 1;
      if (e.confidence === "exact") quality.net_usd_exact += e.net_profit_usd;
      else quality.net_usd_inferred += e.net_profit_usd;
    }
    if (e.confidence === "exact") quality.ops_exact += 1; else quality.ops_inferred += 1;
    if (e.confidence === "inferred") quality.ops_approx += 1;

    var ak = comp, a = byActor[ak] || (byActor[ak] = {
      competitor: ak, eoa: e.eoa, contract: e.contract, label: e.actor_label,
      ops: 0, gross_usd: 0, net_usd: 0, excluded_ops: 0
    });
    a.ops += 1;
    if (e.profit_usd != null) a.gross_usd += e.profit_usd;
    if (e.excluded_from_net) a.excluded_ops += 1;
    else if (e.net_profit_usd != null) a.net_usd += e.net_profit_usd;

    var kk = e.kind + "|" + (e.subkind || "") + "|" + e.confidence;
    var k = byKind[kk] || (byKind[kk] = {
      kind: e.kind, subkind: e.subkind, confidence: e.confidence, ops: 0, gross_usd: 0, net_usd: 0
    });
    k.ops += 1;
    if (e.profit_usd != null) k.gross_usd += e.profit_usd;
    if (!e.excluded_from_net && e.net_profit_usd != null) k.net_usd += e.net_profit_usd;

    var seen = {};
    e.route.forEach(function (leg) {
      if (seen[leg.pool]) return;
      seen[leg.pool] = 1;
      var poolMeta = null;
      for (var p = 0; p < POOLS.length; p++) if (POOLS[p].pool === leg.pool) poolMeta = POOLS[p];
      var pr = byPool[leg.pool] || (byPool[leg.pool] = {
        pool: leg.pool, label: poolMeta ? poolMeta.label : null, ops: 0, gross_usd: 0,
        net_usd: 0, gas_usd: 0, priced_ops: 0, amm_mix: {}, comps: {}, kinds: {}
      });
      pr.ops += 1;
      if (e.profit_usd != null) { pr.gross_usd += e.profit_usd; pr.priced_ops += 1; }
      if (!e.excluded_from_net && e.net_profit_usd != null) pr.net_usd += e.net_profit_usd;
      pr.gas_usd += e.gas_cost_usd;
      pr.amm_mix[leg.amm] = (pr.amm_mix[leg.amm] || 0) + 1;
      pr.comps[comp] = 1;
      pr.kinds[e.kind] = (pr.kinds[e.kind] || 0) + 1;
    });

    if (e.contract) {
      var tok = pick(TOKENS);
      var tr = byToken[tok] || (byToken[tok] = { token: tok, ops: 0, gross_usd: 0, net_usd: 0, priced_ops: 0 });
      tr.ops += 1;
      if (e.profit_usd != null) { tr.gross_usd += e.profit_usd; tr.priced_ops += 1; }
      if (!e.excluded_from_net && e.net_profit_usd != null) tr.net_usd += e.net_profit_usd;
    }

    e.tags.forEach(function (t) {
      var f = byTag[t] || (byTag[t] = { value: t, ops: 0, gross_usd: 0, net_usd: 0, out_of_catalogue: false });
      f.ops += 1;
      if (e.profit_usd != null) f.gross_usd += e.profit_usd;
      if (!e.excluded_from_net && e.net_profit_usd != null) f.net_usd += e.net_profit_usd;
    });
    e.reasons.forEach(function (t) {
      var f = byReason[t] || (byReason[t] = { value: t, ops: 0, gross_usd: 0, net_usd: 0, out_of_catalogue: false });
      f.ops += 1;
      if (e.profit_usd != null) f.gross_usd += e.profit_usd;
      if (!e.excluded_from_net && e.net_profit_usd != null) f.net_usd += e.net_profit_usd;
    });
  });

  quality.pricing_coverage_pct = kpi.ops ? (quality.ops_priced / kpi.ops) * 100 : 100;
  quality.ops_multi_asset = Math.round(kpi.ops * 0.031);
  quality.ops_pricing_clamped = Math.round(kpi.ops * 0.008);
  quality.ops_flash_fee_unpriced = Math.round(kpi.ops * 0.014);
  kpi.searchers = Object.keys(eoas).length;
  kpi.competitors = Object.keys(comps).length;
  kpi.quality = quality;

  Object.keys(useBuckets).sort(function (a, b) { return a - b; }).forEach(function (t) {
    series.push({ bucket_start: +t, ops: useBuckets[t].ops, net_usd: useBuckets[t].net, blocks_indexed: useBuckets[t].blocks });
  });

  var actors = Object.keys(byActor).map(function (k) { return byActor[k]; }).sort(function (a, b) { return b.net_usd - a.net_usd; });
  var kindMatrix = Object.keys(byKind).map(function (k) { return byKind[k]; }).sort(function (a, b) { return b.net_usd - a.net_usd; });
  var pools = Object.keys(byPool).map(function (k) {
    var p = byPool[k];
    p.unique_competitors = Object.keys(p.comps).length;
    var best = null, bestN = -1;
    Object.keys(p.kinds).forEach(function (kk) { if (p.kinds[kk] > bestN) { bestN = p.kinds[kk]; best = kk; } });
    p.top_kind = best;
    delete p.comps; delete p.kinds;
    return p;
  }).sort(function (a, b) { return b.net_usd - a.net_usd; });
  var tokens = Object.keys(byToken).map(function (k) { return byToken[k]; }).sort(function (a, b) { return b.net_usd - a.net_usd; });
  var tags = Object.keys(byTag).map(function (k) { return byTag[k]; }).sort(function (a, b) { return b.ops - a.ops; });
  var reasons = Object.keys(byReason).map(function (k) { return byReason[k]; }).sort(function (a, b) { return b.ops - a.ops; });
  if (tags.length) tags[tags.length - 1].out_of_catalogue = true;

  var publicEvents = events.slice(-650).reverse().map(function (e) {
    return {
      id: e.id, block_number: e.block_number, tx_index: e.tx_index, tx_hash: e.tx_hash,
      ts: e.ts, kind: e.kind, subkind: e.subkind, confidence: e.confidence, eoa: e.eoa,
      contract: e.contract, profit_usd: e.profit_usd, net_profit_usd: e.net_profit_usd,
      gas_cost_usd: e.gas_cost_usd, volume_usd: e.volume_usd, detector: e.detector,
      excluded_from_net: e.excluded_from_net, pnl_basis: e.pnl_basis
    };
  });
  var withEvidence = events.slice(-220).reverse();
  var evidence = withEvidence.map(function (e) {
    return {
      tx_hash: e.tx_hash, block_number: e.block_number, kind: e.kind, subkind: e.subkind,
      confidence: e.confidence, detector: e.detector, eoa: e.eoa, contract: e.contract,
      profit_usd: e.profit_usd, net_profit_usd: e.net_profit_usd, gas_cost_usd: e.gas_cost_usd,
      volume_usd: e.volume_usd, flashloan_fee_usd: e.flashloan_fee_usd,
      profit_token: e.contract ? pick(TOKENS) : null,
      profit_amount: String(Math.floor(1e15 + R() * 9e18)),
      profit_amount_human: (0.4 + R() * 80).toFixed(4),
      excluded_from_net: e.excluded_from_net, route: e.route,
      victim_hashes: e.victim_hashes, tags: e.tags, reasons: e.reasons,
      pnl_basis: e.pnl_basis, arb_meta: null, details: e.details
    };
  });

  var REJECTS = [
    ["state_stale", 4210], ["no_path", 3860], ["below_min_profit", 3020], ["gas_dominates", 2440],
    ["quote_nonpositive", 1180], ["max_candidates", 940], ["unresolved_cycle", 720],
    ["mixed_flow_owner", 510], ["material_deficit", 430], ["open_inventory", 320],
    ["builder_tip", 260], ["cycle_not_closed", 210], ["no_counterparty_leg", 180],
    ["victim_set_empty", 150], ["causal_window_too_wide", 120], ["skim_looks_like_swap", 90],
    ["jit_pairing_unmatched", 80], ["liquidation_not_reconciled", 60],
    ["causal_price_move_below_floor", 40], ["skim_pool_not_v2like", 30], ["other", 24],
    ["sandwich_disabled", 0], ["frontrun_disabled", 0]
  ];

  function tableCounts(pairs) {
    return pairs.map(function (p) { return { table: p[0], rows: p[1] }; });
  }

  window.DEMO_SNAPSHOT = {
    api_version: 1,
    generator: { version: "0.1.0-demo", generated_at: now - 7 },
    db: {
      path: "cache/explorer-avalanche.sqlite", chain_label: "avalanche", chain_id: 43114,
      size_bytes: 1284112384, user_version: 13, schema_head: 13, status: "at_head"
    },
    caps: {
      user_version: 13, schema_head: 13, has_subkind: true, has_excluded_from_net: true,
      has_volume_usd: true, has_flashloan_fee_usd: true, has_pricing_flags: true,
      has_ops_ts_index: true, has_ops_natural_key: true,
      tables: ["blocks", "blocks_classified", "jit_open_positions", "labels", "mev_ops",
        "opportunities", "oracle_answers", "paper_fills", "paper_sessions", "prices",
        "rejected_candidates", "reserve_rates", "swaps", "sync_state", "transfers", "txs"]
    },
    window: { label: "7d", since_ts: start, until_ts: end },
    semantics: {
      null_usd_means: "not priceable — never zero",
      usd_columns_are: "f64 approximations; prefix ≈; never balances",
      net_excludes: "rows with excluded_from_net=1 (when column present)",
      amounts_are: "raw U256 decimal strings; humanize only with known decimals"
    },
    coverage: {
      head: HEAD, indexed_to: HEAD - 9, last_indexed_at: now - 12,
      blocks_in_window: 604800, ops_in_window: events.length,
      min_block_ts: start, max_block_ts: end + HOUR - 1, gap_count: 2,
      first_gap: HEAD - 640, last_gap: HEAD - 631
    },
    gaps: {
      enabled: false, scanned_from: null, scanned_to: null, gap_count: 0,
      first_gap: null, last_gap: null, ranges: [], sample: [],
      note: "Pass --with-gaps to sample unclassified blocks near tip (expensive CTE)."
    },
    panels: {
      kpi: kpi,
      series: series,
      bucket_secs: 3600,
      kind_matrix: kindMatrix,
      unknown_rewritten: 118,
      actors: actors,
      pools: pools,
      tokens: tokens,
      tag_facets: tags,
      reason_facets: reasons,
      events: publicEvents,
      evidence: evidence,
      truncation: {
        max_ops: 5000, ops_total: events.length, ops_emitted: publicEvents.length,
        max_evidence: 500, evidence_emitted: evidence.length, hit_budget: false
      }
    },
    rejects: {
      recording_empty: false, total: REJECTS.reduce(function (n, p) { return n + p[1]; }, 0),
      ladder: REJECTS.map(function (p) { return { reason: p[0], count: p[1] }; }),
      note: "Counts from rejected_candidates in window."
    },
    jit_open: [
      { pool: POOLS[0].pool, owner: ACTORS[0].contract, tick_lower: -887272, tick_upper: 887272, opened_block: HEAD - 412, liquidity: "1824000000000000" },
      { pool: POOLS[3].pool, owner: ACTORS[3].contract, tick_lower: -120, tick_upper: 80, opened_block: HEAD - 1880, liquidity: "940000000000000" },
      { pool: POOLS[6].pool, owner: ACTORS[1].contract, tick_lower: -60, tick_upper: 60, opened_block: HEAD - 3320, liquidity: "410000000000000" }
    ],
    oracle_feeds: [
      { feed: "0x" + hex(40), answers: 604800, last_block: HEAD - 11 },
      { feed: "0x" + hex(40), answers: 604790, last_block: HEAD - 10 },
      { feed: "0x" + hex(40), answers: 151200, last_block: HEAD - 14 },
      { feed: "0x" + hex(40), answers: 86400, last_block: HEAD - 19 }
    ],
    prices: TOKENS.slice(0, 5).map(function (t, i) {
      return { token: t, hour: Math.floor(now / 3600) - i, price_usd: 10 + i * 8.4, source: "chainlink" };
    }),
    storage: tableCounts([
      ["transfers", 21884120], ["swaps", 8421003], ["txs", 3990120], ["mev_ops", 1284912],
      ["blocks", 604800], ["oracle_answers", 120000], ["prices", 44160],
      ["rejected_candidates", 18234], ["labels", 812], ["jit_open_positions", 3], ["sync_state", 1]
    ]),
    truncation: {
      max_ops: 5000, ops_total: events.length, ops_emitted: publicEvents.length,
      max_evidence: 500, evidence_emitted: evidence.length, hit_budget: false
    }
  };
})();
