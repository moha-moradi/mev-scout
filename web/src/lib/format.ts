export function shortHex(v: string | null | undefined, head = 6, tail = 4): string {
  if (!v) return "—";
  if (v.length <= head + tail + 2) return v;
  return `${v.slice(0, head + 2)}…${v.slice(-tail)}`;
}

export function relativeTime(tsSec: number, nowMs = Date.now()): string {
  const diff = Math.max(0, Math.floor(nowMs / 1000 - tsSec));
  if (diff < 60) return `${diff}s ago`;
  if (diff < 3600) return `${Math.floor(diff / 60)}m ago`;
  if (diff < 86_400) return `${Math.floor(diff / 3600)}h ago`;
  return `${Math.floor(diff / 86_400)}d ago`;
}

export function formatUsd(v: number | null | undefined, digits = 2): string {
  if (v == null || Number.isNaN(v)) return "—";
  const abs = Math.abs(v);
  if (abs > 0 && abs < 0.01) return `${v < 0 ? "−" : ""}≤ $0.01`;
  if (abs >= 1_000_000_000) return `${v < 0 ? "−" : ""}$${(abs / 1_000_000_000).toFixed(2)}B`;
  if (abs >= 1_000_000) return `${v < 0 ? "−" : ""}$${(abs / 1_000_000).toFixed(2)}M`;
  if (abs >= 1_000) return `${v < 0 ? "−" : ""}$${(abs / 1_000).toFixed(1)}k`;
  const sign = v < 0 ? "−" : "";
  return `${sign}$${abs.toLocaleString(undefined, {
    minimumFractionDigits: digits,
    maximumFractionDigits: digits,
  })}`;
}

export function formatUsdSigned(v: number | null | undefined): string {
  if (v == null || Number.isNaN(v)) return "—";
  const abs = Math.abs(v);
  if (abs > 0 && abs < 0.01) return `${v < 0 ? "−" : ""}≤ $0.01`;
  const prefix = v > 0 ? "+" : v < 0 ? "−" : "";
  return `${prefix}$${abs.toLocaleString(undefined, {
    minimumFractionDigits: 2,
    maximumFractionDigits: 2,
  })}`;
}

export interface RouteHop {
  pool?: string;
  amm?: string;
  token_in?: string;
  token_out?: string;
  amount_in?: string;
  amount_out?: string;
}

export function parseRoute(routeJson: string | null | undefined): RouteHop[] {
  if (!routeJson) return [];
  try {
    const parsed = JSON.parse(routeJson) as unknown;
    if (Array.isArray(parsed)) return parsed as RouteHop[];
    if (parsed && typeof parsed === "object" && Array.isArray((parsed as { route?: unknown }).route)) {
      return (parsed as { route: RouteHop[] }).route;
    }
  } catch {
    // ignore malformed
  }
  return [];
}

/** Flatten hop list into ordered token addresses for a path viz. */
export function routeTokens(hops: RouteHop[]): string[] {
  const tokens: string[] = [];
  for (const h of hops) {
    if (h.token_in && (tokens.length === 0 || tokens[tokens.length - 1] !== h.token_in)) {
      tokens.push(h.token_in);
    }
    if (h.token_out) tokens.push(h.token_out);
  }
  return tokens;
}

const EXPLORERS: Record<string, string> = {
  ethereum: "https://etherscan.io",
  polygon: "https://polygonscan.com",
  avalanche: "https://snowtrace.io",
  bsc: "https://bscscan.com",
  arbitrum: "https://arbiscan.io",
  base: "https://basescan.org",
  optimism: "https://optimistic.etherscan.io",
};

export function txExplorerUrl(chain: string | undefined, txHash: string): string | null {
  if (!chain) return null;
  const base = EXPLORERS[chain.toLowerCase()];
  return base ? `${base}/tx/${txHash}` : null;
}

export function addressExplorerUrl(chain: string | undefined, address: string): string | null {
  if (!chain) return null;
  const base = EXPLORERS[chain.toLowerCase()];
  return base ? `${base}/address/${address}` : null;
}

/** Authoritative block timing — mirrors `core/src/chain/timing.rs`. */
const CHAIN_TIMING: Record<
  string,
  { genesisTs: number; secsPerBlock: number; anchorBlock?: number; anchorTs?: number }
> = {
  ethereum: { genesisTs: 1_438_269_988, secsPerBlock: 12 },
  polygon: {
    genesisTs: 1_591_031_691,
    secsPerBlock: 1.5,
    anchorBlock: 91_370_547,
    anchorTs: 1_785_760_841,
  },
  bsc: { genesisTs: 1_597_734_000, secsPerBlock: 3 },
  avalanche: {
    genesisTs: 1_600_641_600,
    secsPerBlock: 2,
    // Verified against live head on 2026-09-16 (block ≈ 95,407,426).
    anchorBlock: 95_407_426,
    anchorTs: 1_789_542_517,
  },
  arbitrum: { genesisTs: 1_630_812_600, secsPerBlock: 0.26 },
  base: { genesisTs: 1_686_787_200, secsPerBlock: 2 },
  optimism: { genesisTs: 1_631_808_000, secsPerBlock: 2 },
};

/** Estimate a unix timestamp (seconds) from a block number. */
export function blockToUnix(chain: string | undefined, block: number): number | null {
  if (!chain || !block || block <= 0) return null;
  const t = CHAIN_TIMING[chain.toLowerCase()];
  if (!t) return null;
  if (t.anchorBlock && t.anchorTs) {
    return Math.round(t.anchorTs + (block - t.anchorBlock) * t.secsPerBlock);
  }
  return Math.round(t.genesisTs + block * t.secsPerBlock);
}

/** Human creation label: `Sep 14, 2026 · #95,320,476`. */
export function formatBlockCreated(chain: string | undefined, block: number): string {
  if (!block || block <= 0) return "—";
  const blockLabel = `#${block.toLocaleString()}`;
  const ts = blockToUnix(chain, block);
  if (ts == null) return blockLabel;
  const d = new Date(ts * 1000);
  if (Number.isNaN(d.getTime())) return blockLabel;
  const date = d.toLocaleDateString(undefined, {
    year: "numeric",
    month: "short",
    day: "numeric",
  });
  return `${date} · ${blockLabel}`;
}

/** `uniswap_v2` → `Uniswap V2`, `trader_joe_lb` → `Trader Joe LB`. */
export function formatAmmType(typeKey: string | null | undefined): string {
  if (!typeKey) return "—";
  const map: Record<string, string> = {
    uniswap_v2: "Uniswap V2",
    uniswap_v3: "Uniswap V3",
    uniswap_v4: "Uniswap V4",
    trader_joe_lb: "Liquidity Book",
    pancake_infinity: "Pancake Infinity",
    solidly: "Solidly",
    camelot: "Camelot",
    curve: "Curve",
    balancer: "Balancer",
    pendle: "Pendle",
    metric: "Metric",
    fluid: "Fluid",
  };
  if (map[typeKey]) return map[typeKey];
  return typeKey
    .split("_")
    .map((w) => w.charAt(0).toUpperCase() + w.slice(1))
    .join(" ");
}

/** Deterministic pastel from a hex string (for avatars / token chips). */
export function colorFromHex(hex: string): string {
  let h = 0;
  const s = hex.toLowerCase();
  for (let i = 0; i < s.length; i++) h = (h * 31 + s.charCodeAt(i)) >>> 0;
  const hue = h % 360;
  return `hsl(${hue} 70% 55%)`;
}
