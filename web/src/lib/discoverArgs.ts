/** Mirrors `mev-scout discover` CLI flags (cli/src/cli.rs DiscoverArgs). */

export type DiscoverySource = "onchain" | "remote" | "hybrid";

/** Exactly one range mode, matching BlockRangeArgs mutual exclusivity. */
export type RangeMode = "auto" | "blocks" | "days" | "range" | "block";

export interface DiscoverFormState {
  source: DiscoverySource;
  rangeMode: RangeMode;
  blocks: string;
  days: string;
  fromBlock: string;
  toBlock: string;
  block: string;
  enrich: boolean;
  minTvl: string;
  maxPools: string;
  incremental: boolean;
  healthCheck: boolean;
  batchSize: string;
  rpcConcurrency: string;
  solidlyFeeBps: string;
  resolveRemoteMetadata: boolean;
  json: boolean;
}

export const DEFAULT_DISCOVER_STATE: DiscoverFormState = {
  source: "remote",
  rangeMode: "blocks",
  blocks: "1000",
  days: "1",
  fromBlock: "",
  toBlock: "",
  block: "",
  enrich: true,
  minTvl: "",
  maxPools: "1000",
  incremental: false,
  healthCheck: true,
  batchSize: "500",
  rpcConcurrency: "8",
  solidlyFeeBps: "",
  resolveRemoteMetadata: false,
  json: true,
};

export function needsBlockRange(source: DiscoverySource): boolean {
  return source === "onchain" || source === "hybrid";
}

export function validateDiscoverState(s: DiscoverFormState): string | null {
  if (!needsBlockRange(s.source) || s.rangeMode === "auto") return null;
  if (s.rangeMode === "blocks") {
    const n = Number(s.blocks);
    if (!Number.isFinite(n) || n < 1) return "Enter a blocks count ≥ 1.";
  } else if (s.rangeMode === "days") {
    const n = Number(s.days);
    if (!Number.isFinite(n) || n < 1 || n > 365) return "Enter days between 1 and 365.";
  } else if (s.rangeMode === "range") {
    if (!s.fromBlock || !s.toBlock) return "Enter both from-block and to-block.";
    const from = Number(s.fromBlock);
    const to = Number(s.toBlock);
    if (!Number.isFinite(from) || !Number.isFinite(to) || from < 0 || to < from) {
      return "Invalid from/to block range.";
    }
  } else if (s.rangeMode === "block") {
    const n = Number(s.block);
    if (!Number.isFinite(n) || n < 1) return "Enter a block number ≥ 1.";
  }
  return null;
}

/** Build argv for `api.createJob("discover", …)`. */
export function buildDiscoverArgs(s: DiscoverFormState): string[] {
  const args: string[] = ["--source", s.source];

  if (needsBlockRange(s.source)) {
    if (s.rangeMode === "blocks") args.push("--blocks", String(Number(s.blocks) || 1000));
    else if (s.rangeMode === "days") args.push("--days", String(Number(s.days) || 1));
    else if (s.rangeMode === "range") {
      args.push("--from-block", String(Number(s.fromBlock)), "--to-block", String(Number(s.toBlock)));
    } else if (s.rangeMode === "block") {
      args.push("--block", String(Number(s.block)));
    }
    // "auto" → no range flags; backend uses lookback / start_block from config
  }

  if (s.enrich) args.push("--enrich");
  if (s.minTvl.trim() && Number(s.minTvl) > 0) args.push("--min-tvl", String(Number(s.minTvl)));
  if (s.maxPools.trim() && Number(s.maxPools) !== 1000) {
    args.push("--max-pools", String(Number(s.maxPools) || 1000));
  }
  if (s.incremental) args.push("--incremental");
  if (!s.healthCheck) args.push("--health-check", "false");
  if (s.batchSize.trim() && Number(s.batchSize) !== 500) {
    args.push("--batch-size", String(Number(s.batchSize) || 500));
  }
  if (s.rpcConcurrency.trim() && Number(s.rpcConcurrency) !== 8) {
    args.push("--rpc-concurrency", String(Number(s.rpcConcurrency) || 8));
  }
  if (s.solidlyFeeBps.trim()) args.push("--solidly-fee-bps", String(Number(s.solidlyFeeBps)));
  if (s.resolveRemoteMetadata) args.push("--resolve-remote-metadata");
  if (s.json) args.push("--json");

  return args;
}

/** Best-effort parse of a freeform argv string back into form state. */
export function parseDiscoverArgs(argv: string[]): DiscoverFormState {
  const s: DiscoverFormState = { ...DEFAULT_DISCOVER_STATE };
  const get = (flag: string): string | undefined => {
    const i = argv.indexOf(flag);
    return i >= 0 && i + 1 < argv.length ? argv[i + 1] : undefined;
  };
  const has = (flag: string) => argv.includes(flag);

  const source = get("--source");
  if (source === "onchain" || source === "remote" || source === "hybrid") s.source = source;

  if (get("--blocks")) {
    s.rangeMode = "blocks";
    s.blocks = get("--blocks")!;
  } else if (get("--days")) {
    s.rangeMode = "days";
    s.days = get("--days")!;
  } else if (get("--from-block") || get("--to-block")) {
    s.rangeMode = "range";
    s.fromBlock = get("--from-block") ?? "";
    s.toBlock = get("--to-block") ?? "";
  } else if (get("--block")) {
    s.rangeMode = "block";
    s.block = get("--block")!;
  } else {
    s.rangeMode = "auto";
  }

  s.enrich = has("--enrich");
  s.minTvl = get("--min-tvl") ?? "";
  s.maxPools = get("--max-pools") ?? "1000";
  s.incremental = has("--incremental");
  const hc = get("--health-check");
  s.healthCheck = hc === undefined || hc !== "false";
  s.batchSize = get("--batch-size") ?? "500";
  s.rpcConcurrency = get("--rpc-concurrency") ?? "8";
  s.solidlyFeeBps = get("--solidly-fee-bps") ?? "";
  s.resolveRemoteMetadata = has("--resolve-remote-metadata");
  s.json = has("--json");

  return s;
}
