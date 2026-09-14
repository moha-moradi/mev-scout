import { useEffect, useState } from "react";
import { Link, useParams } from "react-router-dom";
import { api, type PnlResponse, type ResultsRow, type RunDetail, type ValidationResponse } from "../api";
import { usePolling } from "../hooks";
import DataTable, { type Column } from "../components/DataTable";
import ValidationPanel from "../components/ValidationPanel";
import PnlCard from "../components/PnlCard";

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
        empty="No runs yet — run a backtest first."
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