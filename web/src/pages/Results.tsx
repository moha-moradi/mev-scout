import { lazy, Suspense, useEffect, useState } from "react";
import { Link, useParams } from "react-router-dom";
import { api, type PnlResponse, type ResultsRow, type RunDetail, type ValidationResponse } from "../api";
import { usePolling } from "../hooks";
import DataTable, { type Column } from "../components/DataTable";
import ValidationPanel from "../components/ValidationPanel";
import PnlCard from "../components/PnlCard";
import StatCard from "../components/StatCard";
import TerminalPanel from "../components/TerminalPanel";

const RunCharts = lazy(() => import("../components/RunCharts"));

function shortId(id: string): string {
  return `${id.slice(0, 12)}…`;
}

export default function Results() {
  const { runId } = useParams();
  const [selected, setSelected] = useState<string | null>(runId ?? null);

  const { data } = usePolling<import("../api").Paginated<ResultsRow>>(
    () => api.results(0, 100),
    15_000,
    [],
  );

  useEffect(() => {
    if (runId) setSelected(runId);
  }, [runId]);

  const rows = data?.items ?? [];

  const columns: Column<ResultsRow>[] = [
    {
      key: "run_id",
      header: "run",
      cell: (r) => (
        <Link
          to={`/results/${r.run_id}`}
          onClick={() => setSelected(r.run_id)}
          className="font-mono text-xs text-sky-300 hover:underline"
        >
          {shortId(r.run_id)}
        </Link>
      ),
      sortValue: (r) => r.run_id,
    },
    { key: "chain", header: "chain", cell: (r) => r.chain, sortValue: (r) => r.chain },
    { key: "range_mode", header: "mode", cell: (r) => r.range_mode, sortValue: (r) => r.range_mode },
    {
      key: "range",
      header: "blocks",
      align: "right",
      cell: (r) => (
        <span className="tabular-nums text-zinc-300">
          {r.start_block}–{r.end_block}
        </span>
      ),
      sortValue: (r) => r.start_block,
    },
    {
      key: "total_ops",
      header: "ops",
      align: "right",
      cell: (r) => <span className="tabular-nums text-emerald-400">{r.total_ops}</span>,
      sortValue: (r) => r.total_ops,
    },
    {
      key: "resolved_at",
      header: "when",
      cell: (r) => <span className="tabular-nums text-zinc-400">{new Date(r.resolved_at * 1000).toLocaleString()}</span>,
      sortValue: (r) => r.resolved_at,
    },
  ];

  return (
    <div className="space-y-6">
      <h1 className="text-xl font-semibold text-zinc-100">Results &amp; history</h1>
      <DataTable<ResultsRow>
        columns={columns}
        rows={rows}
        rowKey={(r) => r.run_id}
        empty="No runs yet."
        action={
          <Link
            to="/run"
            className="inline-block rounded-md bg-emerald-400 px-3 py-1.5 text-sm font-semibold text-black hover:bg-emerald-300"
          >
            Run a backtest
          </Link>
        }
      />

      {selected && <RunDetailCard runId={selected} />}
    </div>
  );
}

function RunDetailCard({ runId }: { runId: string }) {
  const { data: detail } = usePolling<RunDetail | null>(
    () => api.result(runId).catch(() => null),
    10_000,
    [runId],
  );
  const { data: validation } = usePolling<ValidationResponse | null>(
    () => api.resultValidation(runId).catch(() => null),
    10_000,
    [runId],
  );
  const { data: pnl } = usePolling<PnlResponse | null>(
    () => api.resultPnl(runId).catch(() => null),
    10_000,
    [runId],
  );

  if (!detail) return null;

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
    <div className="space-y-4">
      <div className="rounded-xl border border-zinc-800 bg-zinc-900/60 px-4 py-3">
        <div className="mb-2 flex flex-wrap items-center gap-2 text-sm">
          <span className="rounded bg-zinc-800 px-2 py-0.5 font-mono text-xs">{detail.run_id}</span>
          <span>{detail.range_mode}</span>
          <span className="tabular-nums">blocks {detail.start_block}–{detail.end_block}</span>
          <span className="text-zinc-600">·</span>
          <span>{detail.chain}</span>
        </div>
        <div className="flex flex-wrap gap-1.5">
          {detail.strategies.map((s) => (
            <span key={s} className="rounded-full border border-zinc-700 bg-zinc-800 px-2 py-0.5 text-xs text-zinc-300">
              {s}
            </span>
          ))}
        </div>
      </div>

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

      <Suspense
        fallback={<p className="text-xs text-zinc-500">Loading charts…</p>}
      >
        <RunCharts detail={detail} />
      </Suspense>
      <TerminalPanel
        label="run summary"
        lines={[
          `mev-scout run --chain ${detail.chain} --from-block ${detail.start_block} --to-block ${detail.end_block}`,
          `→ strategies: ${detail.strategies.join(", ")}`,
          `→ flash loan provider: ${detail.flash_loan_provider}`,
          `→ net P&L ${totalEth.toFixed(4)} ETH · hit rate ${hitRate.toFixed(1)}%`,
        ]}
      />

      <ValidationPanel validation={validation} candidates={detail.opportunities} runId={detail.run_id} />
      <PnlCard pnl={pnl} />

      <div className="rounded-xl border border-zinc-800 overflow-x-auto">
        <div className="border-b border-zinc-800 bg-zinc-900/60 px-4 py-2.5 text-sm font-medium text-zinc-200">
          Opportunities ({detail.opportunities.length})
        </div>
        <table className="w-full text-left text-sm">
          <thead className="border-b border-zinc-800 bg-zinc-900/40 text-xs uppercase tracking-wider text-zinc-500">
            <tr>
              <th className="px-3 py-2">block</th>
              <th className="px-3 py-2">strategy</th>
              <th className="px-3 py-2">pool A</th>
              <th className="px-3 py-2">token out</th>
              <th className="px-3 py-2 text-right">expected profit</th>
              <th className="px-3 py-2 text-right">gas</th>
            </tr>
          </thead>
          <tbody className="divide-y divide-zinc-800/70">
            {detail.opportunities.map((o, idx) => (
              <tr key={idx} className="hover:bg-zinc-900/40">
                <td className="px-3 py-2 tabular-nums text-zinc-300">{o.block_number}</td>
                <td className="px-3 py-2 text-zinc-300">{o.strategy}</td>
                <td className="px-3 py-2 font-mono text-xs text-zinc-400">{`${o.pool_a.slice(0, 10)}…`}</td>
                <td className="px-3 py-2 font-mono text-xs text-zinc-400">{`${o.token_out.slice(0, 10)}…`}</td>
                <td className="px-3 py-2 text-right tabular-nums text-emerald-400">{o.expected_profit}</td>
                <td className="px-3 py-2 text-right tabular-nums text-zinc-400">{o.gas_cost_wei}</td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>
    </div>
  );
}