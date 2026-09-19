import { lazy, Suspense, useEffect, useState } from "react";
import { api, type JobInfo, type ProgressResponse, type RunDetail, type ValidationResponse, type PnlResponse, type SanitizedConfig } from "../api";
import { usePolling } from "../hooks";
import { useToast } from "../components/Toast";
import StageTimeline from "../components/StageTimeline";
import LogViewer from "../components/LogViewer";
import ValidationPanel, { tierBadge } from "../components/ValidationPanel";
import PnlCard from "../components/PnlCard";
import StatCard from "../components/StatCard";
import TerminalPanel from "../components/TerminalPanel";

const RunCharts = lazy(() => import("../components/RunCharts"));

type RangeMode = "blocks" | "days" | "range";

const STRATEGY_COLORS: Record<string, string> = {
  "two_hop_arb": "bg-sky-950/60 border-sky-700/60 text-sky-300",
  "jit": "bg-amber-950/60 border-amber-700/60 text-amber-300",
  "jit_arb": "bg-violet-950/60 border-violet-700/60 text-violet-300",
  "sandwich": "bg-rose-950/60 border-rose-700/60 text-rose-300",
  "liquidation": "bg-emerald-950/60 border-emerald-700/60 text-emerald-300",
};

function shortAddr(a: string | null | undefined): string {
  if (!a) return "—";
  return `${a.slice(0, 6)}…${a.slice(-4)}`;
}

export default function RunBacktest() {
  const { data: cfg } = usePolling<SanitizedConfig>(api.config, 30_000, []);
  const toast = useToast();

  const [mode, setMode] = useState<RangeMode>("blocks");
  const [blocks, setBlocks] = useState("10");
  const [days, setDays] = useState("1");
  const [fromBlock, setFromBlock] = useState("");
  const [toBlock, setToBlock] = useState("");
  const [recordRejections, setRecordRejections] = useState(false);

  const [jobId, setJobId] = useState<string | null>(null);
  const [starting, setStarting] = useState(false);

  const { data: job } = usePolling<JobInfo | null>(
    () => (jobId ? api.job(jobId) : Promise.resolve(null)),
    jobId ? 2000 : 60_000,
    [jobId],
  );
  const { data: progress } = usePolling<ProgressResponse | null>(
    () => (jobId ? api.jobProgress(jobId) : Promise.resolve(null)),
    jobId ? 2000 : 60_000,
    [jobId],
  );

  const [detail, setDetail] = useState<RunDetail | null>(null);
  const [validation, setValidation] = useState<ValidationResponse | null>(null);
  const [pnl, setPnl] = useState<PnlResponse | null>(null);
  const [indexing, setIndexing] = useState(false);
  const [validating, setValidating] = useState(false);

  const jobRunning = job?.status === "running";
  const jobDone = job !== null && !jobRunning && job.status !== "failed" && job.status !== "killed";
  const runId = job?.run_id ?? null;

  useEffect(() => {
    if (jobDone && runId) {
      api.result(runId).then(setDetail).catch(() => undefined);
      api.resultValidation(runId).then(setValidation).catch(() => undefined);
      api.resultPnl(runId).then(setPnl).catch(() => undefined);
    }
  }, [jobDone, runId]);

  function buildArgs(): string[] {
    const args: string[] = [];
    if (mode === "blocks") args.push("--blocks", String(Number(blocks) || 10));
    else if (mode === "days") args.push("--days", String(Number(days) || 1));
    else args.push("--from-block", String(Number(fromBlock)), "--to-block", String(Number(toBlock)));
    if (recordRejections) args.push("--record-rejections");
    args.push("--progress", "json");
    return args;
  }

  async function start() {
    if (mode === "range" && (!fromBlock || !toBlock)) {
      toast("Enter both from and to blocks.", "error");
      return;
    }
    setStarting(true);
    setDetail(null);
    setValidation(null);
    setPnl(null);
    try {
      const res = await api.createJob("run", buildArgs());
      setJobId(res.job_id);
      toast(`Run job ${res.job_id.slice(0, 8)} started.`, "success");
    } catch (e) {
      toast(`Failed to start run: ${e instanceof Error ? e.message : String(e)}`, "error");
    } finally {
      setStarting(false);
    }
  }

  function stop() {
    if (jobId) void api.stopJob(jobId).catch(() => undefined);
  }

  async function indexRange() {
    if (!detail || indexing) return;
    setIndexing(true);
    try {
      await api.createJob(
        "explorer index",
        ["--from", String(detail.start_block), "--to", String(detail.end_block)],
      );
      toast("Indexing queued.", "success");
    } catch (e) {
      toast(`Failed to start indexing: ${e instanceof Error ? e.message : String(e)}`, "error");
    } finally {
      setIndexing(false);
    }
  }

  async function runValidate() {
    if (!runId || validating) return;
    setValidating(true);
    try {
      const res = await api.createJob("explorer validate", ["--run", runId, "--json"]);
      toast(`Validate job ${res.job_id.slice(0, 8)} started.`, "success");
      setTimeout(() => {
        api.resultValidation(runId).then(setValidation).catch(() => undefined);
      }, 4_000);
    } catch (e) {
      toast(`Failed: ${e instanceof Error ? e.message : String(e)}`, "error");
    } finally {
      setValidating(false);
    }
  }

  const strategies = cfg?.backtest.strategies.split(",").map((s) => s.trim()).filter(Boolean) ?? [];

  return (
    <div className="space-y-6">
      <div className="flex items-center justify-between">
        <h1 className="text-xl font-semibold text-zinc-100">Backtest Runner</h1>
        {jobRunning && (
          <button
            onClick={stop}
            className="rounded-md border border-rose-700 bg-rose-950/50 px-3 py-1.5 text-sm text-rose-300 hover:bg-rose-900/50"
          >
            Stop
          </button>
        )}
      </div>

      <div className="rounded-xl border border-zinc-800 bg-zinc-900/60 p-4">
        <div className="grid gap-4 lg:grid-cols-3">
          <div>
            <label className="mb-1.5 block text-xs uppercase tracking-wider text-zinc-500">
              Range mode
            </label>
            <select
              value={mode}
              onChange={(e) => setMode(e.target.value as RangeMode)}
              className="w-full rounded-md border border-zinc-700 bg-zinc-900 px-2 py-2 text-sm text-zinc-100 outline-none focus:border-emerald-400"
            >
              <option value="blocks">Last N blocks</option>
              <option value="days">Last N days</option>
              <option value="range">From – to blocks</option>
            </select>
          </div>
          {mode === "blocks" && (
            <div>
              <label className="mb-1.5 block text-xs uppercase tracking-wider text-zinc-500">Blocks</label>
              <input
                type="number"
                min={1}
                value={blocks}
                onChange={(e) => setBlocks(e.target.value)}
                className="w-full rounded-md border border-zinc-700 bg-zinc-900 px-2 py-2 text-sm text-zinc-100 outline-none focus:border-emerald-400"
              />
            </div>
          )}
          {mode === "days" && (
            <div>
              <label className="mb-1.5 block text-xs uppercase tracking-wider text-zinc-500">Days</label>
              <input
                type="number"
                min={1}
                max={365}
                value={days}
                onChange={(e) => setDays(e.target.value)}
                className="w-full rounded-md border border-zinc-700 bg-zinc-900 px-2 py-2 text-sm text-zinc-100 outline-none focus:border-emerald-400"
              />
            </div>
          )}
          {mode === "range" && (
            <>
              <div>
                <label className="mb-1.5 block text-xs uppercase tracking-wider text-zinc-500">From block</label>
                <input
                  type="number"
                  min={1}
                  value={fromBlock}
                  onChange={(e) => setFromBlock(e.target.value)}
                  className="w-full rounded-md border border-zinc-700 bg-zinc-900 px-2 py-2 text-sm text-zinc-100 outline-none focus:border-emerald-400"
                />
              </div>
              <div>
                <label className="mb-1.5 block text-xs uppercase tracking-wider text-zinc-500">To block</label>
                <input
                  type="number"
                  min={1}
                  value={toBlock}
                  onChange={(e) => setToBlock(e.target.value)}
                  className="w-full rounded-md border border-zinc-700 bg-zinc-900 px-2 py-2 text-sm text-zinc-100 outline-none focus:border-emerald-400"
                />
              </div>
            </>
          )}
        </div>

        <div className="mt-4 flex flex-wrap items-center gap-x-8 gap-y-3">
          <div className="text-sm">
            <span className="mr-2 text-xs uppercase tracking-wider text-zinc-500">Strategies</span>
            {strategies.length === 0 ? (
              <span className="text-zinc-500">(from config)</span>
            ) : (
              strategies.map((s) => (
                <span
                  key={s}
                  className={`mr-1.5 inline-block rounded-full border px-2 py-0.5 text-xs ${
                    STRATEGY_COLORS[s] ?? "border-zinc-700 bg-zinc-800 text-zinc-300"
                  }`}
                >
                  {s}
                </span>
              ))
            )}
          </div>
          <label className="flex items-center gap-2 text-sm text-zinc-300">
            <input
              type="checkbox"
              checked={recordRejections}
              onChange={(e) => setRecordRejections(e.target.checked)}
              className="accent-emerald-400"
            />
            record rejections
          </label>
        </div>

        <div className="mt-4 flex items-center gap-3">
          <button
            onClick={start}
            disabled={starting || jobRunning}
            className="rounded-md bg-emerald-400 px-4 py-2 text-sm font-semibold text-black hover:bg-emerald-300 disabled:cursor-not-allowed disabled:opacity-50"
          >
            {jobRunning ? "Running…" : starting ? "Starting…" : "Run backtest"}
          </button>
          {jobRunning && (
            <span className="flex items-center gap-1.5 text-xs text-zinc-400">
              <span className="h-1.5 w-1.5 animate-pulse rounded-full bg-amber-400" />
              job {jobId?.slice(0, 8)}
              {runId ? ` · run ${runId}` : ""}
            </span>
          )}
        </div>
      </div>

      {jobId && (
        <div className="space-y-4">
          <StageTimeline progress={progress ?? null} running={jobRunning} />
          <LogViewer jobId={jobId} />
        </div>
      )}

      {detail && (
        <div className="space-y-4">
          <div className="rounded-xl border border-zinc-800 bg-zinc-900/60 p-4">
            <div className="mb-3 flex flex-wrap items-center justify-between gap-2">
              <div className="flex items-center gap-2 text-sm text-zinc-300">
                <span className="rounded bg-zinc-800 px-2 py-0.5 font-mono text-xs">{detail.run_id}</span>
                <span>{detail.range_mode}</span>
                <span className="tabular-nums">blocks {detail.start_block}–{detail.end_block}</span>
                <span className="text-zinc-600">·</span>
                <span>{detail.chain}</span>
              </div>
              {validation && !validation.explorer_coverage.covered && (
                <button
                  onClick={indexRange}
                  disabled={indexing}
                  className="rounded-md border border-amber-700 bg-amber-950/40 px-3 py-1.5 text-xs text-amber-300 hover:bg-amber-900/40 disabled:opacity-50"
                >
                  {indexing ? "Indexing…" : "Index this range"}
                </button>
              )}
            </div>
            <StageTimeline
              progress={
                jobDone
                  ? { stage: "complete", run_id: runId!, ops: detail.opportunities.length, elapsed_ms: 0 }
                  : null
              }
              running={false}
            />
          </div>

          <RunDetailSummary detail={detail} />
          <TerminalPanel
            label="run summary"
            lines={[
              `mev-scout run --chain ${detail.chain} --from-block ${detail.start_block} --to-block ${detail.end_block}`,
              `→ strategies: ${detail.strategies.join(", ")}`,
              `→ flash loan provider: ${detail.flash_loan_provider}`,
              `→ net P&L ${detail.opportunities.reduce((s, o) => s + Number(o.expected_profit) / 1e18, 0).toFixed(4)} ETH`,
            ]}
          />
          <Suspense
            fallback={<p className="text-xs text-zinc-500">Loading charts…</p>}
          >
            <RunCharts detail={detail} />
          </Suspense>

          <ValidationPanel
            validation={validation}
            candidates={detail.opportunities}
            runId={runId ?? undefined}
            validating={validating}
            onValidate={() => void runValidate()}
          />
          <PnlCard pnl={pnl} />

          <div className="rounded-xl border border-zinc-800 overflow-x-auto">
            <div className="border-b border-zinc-800 bg-zinc-900/60 px-4 py-2.5 text-sm font-medium text-zinc-200">
              Opportunities ({detail.opportunities.length})
            </div>
            <table className="w-full text-left text-sm">
              <thead className="border-b border-zinc-800 bg-zinc-900/40 text-xs uppercase tracking-wider text-zinc-500">
                <tr>
                  <th className="px-3 py-2">tier</th>
                  <th className="px-3 py-2">strategy</th>
                  <th className="px-3 py-2">block</th>
                  <th className="px-3 py-2">pool A</th>
                  <th className="px-3 py-2">pool B</th>
                  <th className="px-3 py-2">token out</th>
                  <th className="px-3 py-2 text-right">expected profit</th>
                  <th className="px-3 py-2 text-right">gas (wei)</th>
                </tr>
              </thead>
              <tbody className="divide-y divide-zinc-800/70">
                {detail.opportunities.map((o, idx) => (
                  <tr key={idx} className="hover:bg-zinc-900/40">
                    <td className="px-3 py-2">
                      <Badge tier={tierBadge(o)} />
                    </td>
                    <td className="px-3 py-2">
                      <span className={`rounded-full border px-2 py-0.5 text-xs ${STRATEGY_COLORS[o.strategy] ?? "border-zinc-700 bg-zinc-800 text-zinc-300"}`}>
                        {o.strategy}
                      </span>
                    </td>
                    <td className="px-3 py-2 tabular-nums text-zinc-300">{o.block_number}</td>
                    <td className="px-3 py-2 font-mono text-xs text-zinc-400">{shortAddr(o.pool_a)}</td>
                    <td className="px-3 py-2 font-mono text-xs text-zinc-400">{shortAddr(o.pool_b)}</td>
                    <td className="px-3 py-2 font-mono text-xs text-zinc-400">{shortAddr(o.token_out)}</td>
                    <td className="px-3 py-2 text-right tabular-nums text-emerald-400">{o.expected_profit}</td>
                    <td className="px-3 py-2 text-right tabular-nums text-zinc-400">{o.gas_cost_wei}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        </div>
      )}
    </div>
  );
}

function RunDetailSummary({ detail }: { detail: RunDetail }) {
  const ops = detail.opportunities;
  const totalEth = ops.reduce((s, o) => s + Number(o.expected_profit) / 1e18, 0);
  const profitable = ops.filter((o) => Number(o.expected_profit) > 0).length;
  const hitRate = ops.length > 0 ? (profitable / ops.length) * 100 : 0;

  const blocks = [...new Set(ops.map((o) => o.block_number))].sort((a, b) => a - b);
  let acc = 0;
  let peak = 0;
  let maxDd = 0;
  blocks.forEach((b) => {
    const sum = ops.filter((o) => o.block_number === b).reduce(
      (s, o) => s + Number(o.expected_profit) / 1e18,
      0,
    );
    acc += sum;
    peak = Math.max(peak, acc);
    if (peak > 0) maxDd = Math.max(maxDd, ((peak - acc) / peak) * 100);
  });

  return (
    <div className="grid grid-cols-2 gap-4 lg:grid-cols-4">
      <StatCard
        label="Net P&L (ETH)"
        value={totalEth.toFixed(6)}
        sub={`${ops.length} opps in window`}
        accent={totalEth >= 0 ? "good" : "bad"}
      />
      <StatCard label="Hit rate" value={`${hitRate.toFixed(1)}%`} sub="profitable opps" />
      <StatCard label="Max drawdown" value={`${maxDd.toFixed(2)}%`} sub="peak to trough" accent="bad" />
      <StatCard label="Strategy count" value={detail.strategies.length} sub="active strategies" />
    </div>
  );
}

function Badge({ tier }: { tier: "T1" | "T2" | "T3" | "missed" }) {
  const cls =
    tier === "T1"
      ? "bg-emerald-950/60 border-emerald-700/60 text-emerald-300"
      : tier === "T2"
        ? "bg-sky-950/60 border-sky-700/60 text-sky-300"
        : tier === "T3"
          ? "bg-amber-950/60 border-amber-700/60 text-amber-300"
          : "bg-rose-950/60 border-rose-700/60 text-rose-300";
  return <span className={`rounded-full border px-1.5 py-0.5 text-[10px] font-medium ${cls}`}>{tier}</span>;
}