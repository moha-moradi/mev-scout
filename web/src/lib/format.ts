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

/** Deterministic pastel from a hex string (for avatars / token chips). */
export function colorFromHex(hex: string): string {
  let h = 0;
  const s = hex.toLowerCase();
  for (let i = 0; i < s.length; i++) h = (h * 31 + s.charCodeAt(i)) >>> 0;
  const hue = h % 360;
  return `hsl(${hue} 70% 55%)`;
}
