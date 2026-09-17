// Typed fetch client for the mev-scout API (baseURL = "/api", proxied to
// 127.0.0.1:7600 by Vite in dev, same-origin in production).

const BASE = "/api";

export class ApiError extends Error {
  readonly status: number;
  readonly detail: string;

  constructor(status: number, detail: string) {
    super(`${status} ${detail}`);
    this.name = "ApiError";
    this.status = status;
    this.detail = detail;
  }
}

async function request<T>(path: string, init?: RequestInit): Promise<T> {
  let res: Response;
  try {
    res = await fetch(`${BASE}${path}`, {
      ...init,
      headers: { "Content-Type": "application/json", ...(init?.headers ?? {}) },
    });
  } catch (err) {
    throw new ApiError(0, `network error: ${String(err)}`);
  }
  if (!res.ok) {
    let detail = res.statusText;
    try {
      const body = (await res.json()) as { error?: string };
      detail = body.error ?? detail;
    } catch {
      /* non-JSON error body */
    }
    throw new ApiError(res.status, detail);
  }
  return (await res.json()) as T;
}

export const api = {
  health: () => request<HealthResponse>("/health"),
  chains: () => request<ChainDto[]>("/chains"),
  sync: () => request<SyncResponse>("/sync"),
  config: () => request<SanitizedConfig>("/config"),
  putConfig: (body: ConfigEdit) =>
    request<ConfigEditResponse>("/config", { method: "PUT", body: JSON.stringify(body) }),

  jobs: () => request<JobInfo[]>("/jobs"),
  createJob: (command: string, args: string[], timeoutSecs?: number) =>
    request<CreateJobResponse>("/jobs", {
      method: "POST",
      body: JSON.stringify({ command, args, timeout_secs: timeoutSecs }),
    }),
  job: (id: string) => request<JobInfo>(`/jobs/${id}`),
  jobLog: (id: string, tail?: number) =>
    request<string[]>(`/jobs/${id}/log?tail=${tail ?? 100}`),
  jobProgress: (id: string) => request<ProgressResponse | null>(`/jobs/${id}/progress`),
  stopJob: (id: string) => request<JobInfo>(`/jobs/${id}/stop`, { method: "POST" }),

  runs: (offset?: number, limit?: number) =>
    request<Paginated<ResultsRow>>(`/runs?offset=${offset ?? 0}&limit=${limit ?? 50}`),
  results: (offset?: number, limit?: number) =>
    request<Paginated<ResultsRow>>(`/results?offset=${offset ?? 0}&limit=${limit ?? 50}`),
  result: (runId: string) => request<RunDetail>(`/results/${encodeURIComponent(runId)}`),
  resultValidation: (runId: string) =>
    request<ValidationResponse>(`/results/${encodeURIComponent(runId)}/validation`),
  resultPnl: (runId: string) =>
    request<PnlResponse>(`/results/${encodeURIComponent(runId)}/pnl`),

  opportunities: (params: OppQuery = {}) => {
    const q = new URLSearchParams();
    if (params.run_id) q.set("run_id", params.run_id);
    if (params.from !== undefined) q.set("from", String(params.from));
    if (params.to !== undefined) q.set("to", String(params.to));
    q.set("offset", String(params.offset ?? 0));
    q.set("limit", String(params.limit ?? 50));
    return request<Paginated<OpportunityRow>>(`/opportunities?${q.toString()}`);
  },
  oppRuns: () => request<OpportunityRunSummary[]>("/opportunities/runs"),

  pools: (params: PoolsQuery = {}) => {
    const q = new URLSearchParams();
    if (params.q) q.set("q", params.q);
    if (params.dex) q.set("dex", params.dex);
    if (params.token) q.set("token", params.token);
    if (params.min_tvl !== undefined) q.set("min_tvl", String(params.min_tvl));
    if (params.sort) q.set("sort", params.sort);
    q.set("order", params.order ?? "desc");
    q.set("offset", String(params.offset ?? 0));
    q.set("limit", String(params.limit ?? 200));
    return request<Paginated<PoolInfo>>(`/pools?${q.toString()}`);
  },

  explorerFeed: (params: { limit?: number; kinds?: string; q?: string; min_profit_usd?: number } = {}) => {
    const q = new URLSearchParams();
    if (params.limit) q.set("limit", String(params.limit));
    if (params.kinds) q.set("kinds", params.kinds);
    if (params.q) q.set("q", params.q);
    if (params.min_profit_usd !== undefined) q.set("min_profit_usd", String(params.min_profit_usd));
    return request<FeedRow[]>(`/explorer/feed?${q.toString()}`);
  },
  explorerStats: (since?: string) =>
    request<StatsResponse>(`/explorer/stats?since=${since ?? "all"}`),
  explorerOverview: (since?: string) =>
    request<OverviewRow>(`/explorer/overview?since=${since ?? "all"}`),
  explorerTop: (by?: string, since?: string, limit?: number) =>
    request<StatsRow[]>(`/explorer/top?by=${by ?? "sender"}&since=${since ?? "all"}&limit=${limit ?? 20}`),
  explorerOps: (params: { from?: number; to?: number; kinds?: string; q?: string; offset?: number; limit?: number } = {}) => {
    const q = new URLSearchParams();
    if (params.from !== undefined) q.set("from", String(params.from));
    if (params.to !== undefined) q.set("to", String(params.to));
    if (params.kinds) q.set("kinds", params.kinds);
    if (params.q) q.set("q", params.q);
    q.set("offset", String(params.offset ?? 0));
    q.set("limit", String(params.limit ?? 50));
    return request<Paginated<MevOpRow>>(`/explorer/ops?${q.toString()}`);
  },
  explorerOp: (txHash: string, trace?: boolean) =>
    request<ExplainResponse>(
      `/explorer/op/${encodeURIComponent(txHash)}${trace ? "?trace=true" : ""}`,
    ),
  explorerDoctor: () => request<DoctorOutcome>("/explorer/doctor"),
  explorerExportDownload: async (params: { format?: string; since?: string; kinds?: string } = {}) => {
    const q = new URLSearchParams();
    if (params.format) q.set("format", params.format);
    if (params.since) q.set("since", params.since);
    if (params.kinds) q.set("kinds", params.kinds);
    const res = await fetch(`${BASE}/explorer/export?${q.toString()}`);
    if (!res.ok) {
      let detail = res.statusText;
      try {
        const body = (await res.json()) as { error?: string };
        detail = body.error ?? detail;
      } catch {
        /* ignore */
      }
      throw new ApiError(res.status, detail);
    }
    const blob = await res.blob();
    const cd = res.headers.get("Content-Disposition") ?? "";
    const match = /filename="?([^"]+)"?/.exec(cd);
    const filename = match?.[1] ?? `explorer_export.${params.format === "csv" ? "csv" : "json"}`;
    const url = URL.createObjectURL(blob);
    const a = document.createElement("a");
    a.href = url;
    a.download = filename;
    a.click();
    URL.revokeObjectURL(url);
  },
};

// ─── API types (mirror rust DTOs) ───────────────────────────────────────

export interface Paginated<T> {
  items: T[];
  total: number;
  offset: number;
  limit: number;
}

export interface HealthResponse {
  version: string;
  chain: string;
  db_status: { cache: string; explorer: string };
  rpc_provider_count: number;
  job_status: { running: string | null; total: number };
  uptime_seconds: number;
}

export interface ChainDto {
  name: string;
  chain_id: number;
  wrapped_native: string | null;
  has_cache_db: boolean;
  has_explorer_db: boolean;
}

export interface SyncResponse {
  explorer_head: number;
  cache_head: number;
  last_indexed: number | null;
}

export interface RpcSummary {
  providers: number;
  hosts: string[];
  /** Raw on-disk rpc_urls (may contain `${ENV}` placeholders). */
  urls: string[];
  /** On-disk rpc_rps aligned with urls when set. */
  rps: number[];
}

export interface GasConfig {
  gas_model: string;
  gas_limit: number;
  priority_fee_gwei: number;
  gas_limits: Record<string, number>;
}

export interface BacktestConfig {
  flash_loan_provider: string;
  strategies: string;
  max_pairs_per_token: number;
  proximity_window: number;
  capture_pending: boolean;
  min_profit_wei: number;
  max_candidates_per_tx: number;
}

export interface OutputConfig {
  output: string;
  db_path: string;
}

export interface ExplorerConfig {
  db_path?: string;
  confirmations: number;
  poll_interval_ms: number;
  checkpoint_every: number;
}

export interface SanitizedConfig {
  chain: string;
  gas: GasConfig;
  backtest: BacktestConfig;
  output: OutputConfig;
  explorer: ExplorerConfig;
  rpc: RpcSummary;
}

export interface ConfigEdit {
  chain?: string;
  gas?: Partial<GasConfig>;
  backtest?: Partial<BacktestConfig>;
  output?: Partial<OutputConfig>;
  explorer?: Partial<ExplorerConfig>;
  rpc_urls?: string[];
  rpc_rps?: number[];
}

export interface ConfigEditResponse {
  ok: boolean;
  restarted_connections: boolean;
}

export type JobStatus = "running" | "finished" | "failed" | "killed";

export interface JobInfo {
  job_id: string;
  command: string;
  args: string[];
  status: JobStatus;
  exit_code: number | null;
  run_id: string | null;
  pid: number | null;
  created_at: string;
  finished_at: string | null;
  log_path: string;
  timeout_secs: number | null;
}

export interface CreateJobResponse {
  job_id: string;
}

export interface ProgressResponse {
  stage: string;
  done?: number;
  total?: number;
  run_id?: string;
  ops?: number;
  elapsed_ms?: number;
  pct?: number;
}

export interface ResultsRow {
  run_id: string;
  chain: string;
  start_block: number;
  end_block: number;
  range_mode: string;
  strategies: string[];
  resolved_at: number;
  total_ops: number;
  total_net_profit_usd: number | null;
}

export interface RunManifest {
  run_id: string;
  chain: string;
  start_block: number;
  end_block: number;
  resolved_at: number;
  range_mode: string;
  strategies: string[];
  flash_loan_provider: string;
}

export interface MevOpportunity {
  strategy: string;
  block_number: number;
  tx_index: number | null;
  pool_a: string;
  pool_b?: string | null;
  token_in: string;
  token_out: string;
  expected_profit: string;
  gas_cost_wei: number;
  mempool_only?: boolean;
  details?: unknown;
}

export interface RunDetail {
  run_id: string;
  chain: string;
  start_block: number;
  end_block: number;
  resolved_at: number;
  range_mode: string;
  strategies: string[];
  flash_loan_provider: string;
  opportunities: MevOpportunity[];
}

export interface CoverageInfo {
  blocks_indexed: number;
  blocks_total: number;
  covered: boolean;
}

export interface ValidationReport {
  total_realized: number;
  total_candidates: number;
  tier1: number;
  tier2: number;
  tier3: number;
  missed: number;
  coverage: number;
}

export interface ValidationResponse {
  total_realized: number;
  total_candidates: number;
  tier1: number;
  tier2: number;
  tier3: number;
  missed: number;
  coverage: number;
  explorer_coverage: CoverageInfo;
}

export interface TokenPnl {
  token: string;
  gross: string;
  gas: string;
  net: string;
  usd: number | null;
}

export interface PnlTotals {
  gross_usd: number | null;
  gas_usd: number | null;
  net_usd: number | null;
}

export interface PnlResponse {
  simulated: boolean;
  per_token: TokenPnl[];
  totals: PnlTotals;
}

export interface OpportunityRow {
  run_id: string | null;
  block_number: number;
  tx_index: number | null;
  strategy: string;
  pool_a: string | null;
  pool_b: string | null;
  token_in: string | null;
  token_out: string | null;
  expected_profit: string;
  mempool_only: boolean;
  detection_path: string | null;
  canonical_id: string | null;
  tx_hash: string | null;
}

export interface OpportunityRunSummary {
  run_id: string;
  count: number;
  first_ts: number | null;
  last_ts: number | null;
}

export interface PoolInfo {
  address: string;
  token0: string;
  token1: string;
  fee: number;
  name: string | null;
  /** dex type key, e.g. "uniswap_v2" */
  type: string;
  tick_spacing: number | null;
  creation_block: number;
  pool_id: string | null;
  is_fot: boolean | null;
  is_rebase: boolean | null;
  dex_name: string | null;
  token0_symbol: string | null;
  token1_symbol: string | null;
  tvl_usd?: number | null;
  volume_usd_24h?: number | null;
  volume_usd_30d?: number | null;
}

export interface FeedRow {
  ts: number;
  block_number: number;
  kind: string;
  profit_token: string | null;
  profit_amount?: string | null;
  profit_usd: number | null;
  gas_cost_usd: number | null;
  net_profit_usd: number | null;
  eoa: string;
  tx_hash: string;
  route_json: string | null;
  native_price_usd?: number | null;
}

export interface StatsRow {
  label: string;
  ops: number;
  gross_usd: number;
  net_usd: number;
}

export interface StatsResponse {
  by_kind: StatsRow[];
  daily: StatsRow[];
}

export interface OverviewRow {
  ops: number;
  gross_usd: number;
  net_usd: number;
  gas_usd: number;
  highest_single_usd: number;
  searchers: number;
}

export interface MevOpRow {
  id: number;
  block_number: number;
  tx_index: number | null;
  tx_hash: string;
  ts: number;
  kind: string;
  eoa: string;
  contract: string | null;
  confidence: string;
  canonical_id: string | null;
  profit_token: string | null;
  profit_amount: string | null;
  profit_usd: number | null;
  gas_cost_usd: number | null;
  net_profit_usd: number | null;
  route_json: string | null;
  victim_hashes: string | null;
  details_json: string | null;
  detector: string | null;
  created_at: number;
}

export interface RejectedRow {
  block_number: number;
  tx_index: number | null;
  strategy: string;
  pool_a: string | null;
  pool_b: string | null;
  token_in: string | null;
  token_out: string | null;
  expected_profit: string;
  gas_cost_wei: number;
  reject_reason: string;
  detail: string | null;
}

export interface ExplainResponse {
  tx_hash: string;
  ops: MevOpRow[];
  rejected: RejectedRow[];
  trace?: string | null;
}

export interface ProviderProbe {
  url_shown: string;
  latest: string;
  archive: string;
  bulk_receipts: string;
  traces: string;
  rps: number | null;
}

export interface DoctorOutcome {
  chain: string;
  chain_id: number;
  providers: ProviderProbe[];
  gate_ok: boolean;
}

export interface OppQuery {
  run_id?: string;
  from?: number;
  to?: number;
  offset?: number;
  limit?: number;
}

export interface PoolsQuery {
  q?: string;
  dex?: string;
  token?: string;
  min_tvl?: number;
  sort?: string;
  order?: "asc" | "desc";
  offset?: number;
  limit?: number;
}