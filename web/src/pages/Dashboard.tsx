import { Link } from "react-router-dom";
import { api, type HealthResponse, type OverviewRow, type StatsResponse, type SyncResponse } from "../api";
import { usePolling } from "../hooks";
import StatCard from "../components/StatCard";
import { formatUsd } from "../components/PnlCard";

export default function Dashboard() {
  const { data: health } = usePolling<HealthResponse>(api.health, 5_000, []);
  const { data: overview } = usePolling<OverviewRow>(
    () => api.explorerOverview("all"),
    15_000,
    [],
  );
  const { data: stats } = usePolling<StatsResponse>(
    () => api.explorerStats("7d"),
    30_000,
    [],
  );
  const { data: sync } = usePolling<SyncResponse>(api.sync, 10_000, []);

  return (
    <div className="space-y-6">
      <div className="flex items-center justify-between">
        <h1 className="text-xl font-semibold text-zinc-100">Dashboard</h1>
        <span className="text-xs text-zinc-500">API v{health?.version ?? "–"} · chain {health?.chain ?? "–"}</span>
      </div>

      <div className="grid grid-cols-2 gap-4 lg:grid-cols-4">
        <StatCard label="Explorer ops" value={overview?.ops ?? 0} />
        <StatCard label="Gross USD" value={formatUsd(overview?.gross_usd ?? null)} accent="good" />
        <StatCard label="Net USD" value={formatUsd(overview?.net_usd ?? null)} accent="good" />
        <StatCard label="Searchers" value={overview?.searchers ?? 0} />
      </div>

      <div className="grid grid-cols-2 gap-4 lg:grid-cols-4">
        <StatCard label="Explorer head" value={sync?.explorer_head ?? 0} />
        <StatCard label="Cache head" value={sync?.cache_head ?? 0} />
        <StatCard
          label="Last indexed"
          value={
            sync?.last_indexed
              ? new Date(sync.last_indexed * 1000).toLocaleString()
              : "never"
          }
        />
        <StatCard label="RPC providers" value={health?.rpc_provider_count ?? 0} />
      </div>

      {stats && stats.by_kind.length > 0 && (
        <div className="rounded-xl border border-zinc-800 bg-zinc-900/60 p-4">
          <h2 className="mb-3 text-sm font-medium text-zinc-200">MEV by kind (7d)</h2>
          <div className="space-y-2">
            {stats.by_kind.map((s) => (
              <div key={s.label} className="flex items-center gap-3 text-sm">
                <span className="w-28 text-zinc-400">{s.label}</span>
                <div className="h-2 flex-1 overflow-hidden rounded-full bg-zinc-800">
                  <div
                    className="h-full rounded-full bg-sky-500"
                    style={{
                      width: `${Math.min(100, (s.gross_usd / Math.max(1, stats.by_kind[0].gross_usd)) * 100)}%`,
                    }}
                  />
                </div>
                <span className="w-24 text-right tabular-nums text-zinc-300">
                  {formatUsd(s.gross_usd)}
                </span>
                <span className="w-12 text-right tabular-nums text-zinc-500">{s.ops}</span>
              </div>
            ))}
          </div>
        </div>
      )}

      <div className="grid grid-cols-2 gap-4 lg:grid-cols-4">
        {[
          { to: "/run", label: "Run a backtest" },
          { to: "/live", label: "Live monitor" },
          { to: "/explorer", label: "Explorer" },
          { to: "/jobs", label: "Jobs" },
        ].map((l) => (
          <Link
            key={l.to}
            to={l.to}
            className="rounded-xl border border-zinc-800 bg-zinc-900/60 p-4 text-sm text-zinc-300 transition-colors hover:border-sky-700 hover:bg-sky-950/30"
          >
            {l.label} →
          </Link>
        ))}
      </div>
    </div>
  );
}