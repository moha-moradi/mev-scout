import { useRef, useState } from "react";
import { api, type FeedRow, type JobInfo, type OverviewRow, type StatsRow } from "../api";
import { usePolling } from "../hooks";
import StatCard from "../components/StatCard";
import OpDrawer from "../components/OpDrawer";
import { useToast } from "../components/Toast";

const KINDS = ["arb_atomic", "sandwich", "liquidation", "jit", "jit_arb", "unknown"];
const KIND_COLOR: Record<string, string> = {
  arb_atomic: "bg-sky-950/60 border-sky-700/60 text-sky-300",
  sandwich: "bg-rose-950/60 border-rose-700/60 text-rose-300",
  liquidation: "bg-emerald-950/60 border-emerald-700/60 text-emerald-300",
  jit: "bg-amber-950/60 border-amber-700/60 text-amber-300",
  jit_arb: "bg-violet-950/60 border-violet-700/60 text-violet-300",
  unknown: "border-zinc-700 bg-zinc-800 text-zinc-300",
};

function short(v: string | null | undefined, n = 18): string {
  if (!v) return "—";
  if (v.length <= n) return v;
  return `${v.slice(0, n)}…`;
}

export default function Explorer() {
  const [mode, setMode] = useState<"past" | "live">("live");
  const [kinds, setKinds] = useState<string[]>([]);
  const [q, setQ] = useState("");
  const toast = useToast();
  const [drawerTx, setDrawerTx] = useState<string | null>(null);

  const { data: overview } = usePolling<OverviewRow>(
    () => api.explorerOverview("all"),
    15_000,
    [],
  );
  const { data: feed } = usePolling<FeedRow[]>(
    () => api.explorerFeed({ limit: 40, kinds: kinds.join(","), q: q || undefined }),
    5000,
    [kinds, q],
  );
  const { data: top } = usePolling<StatsRow[]>(
    () => api.explorerTop("sender", "all", 10),
    30_000,
    [],
  );

  const { data: jobs } = usePolling<JobInfo[]>(api.jobs, 5000, []);
  const liveIndexJob = useRef<string | null>(null);
  if (jobs) {
    liveIndexJob.current =
      jobs.find(
        (j) => j.command === "explorer index" && (j.status === "running" || j.args.includes("--live")),
      )?.job_id ?? null;
  }

  const runningLiveIndex = Boolean(liveIndexJob.current);

  async function toggleLiveIndex() {
    if (runningLiveIndex) {
      if (liveIndexJob.current) {
        try {
          await api.stopJob(liveIndexJob.current);
          toast("Live indexer stopped.", "success");
        } catch (e) {
          toast(`Failed to stop: ${e instanceof Error ? e.message : String(e)}`, "error");
        }
      }
      return;
    }
    try {
      const res = await api.createJob("explorer index", ["--live"]);
      toast(`Indexer job ${res.job_id.slice(0, 8)} started.`, "success");
    } catch (e) {
      toast(`Failed to start: ${e instanceof Error ? e.message : String(e)}`, "error");
    }
  }

  return (
    <div className="space-y-6">
      <div className="flex flex-wrap items-center justify-between gap-3">
        <h1 className="text-xl font-semibold text-zinc-100">Explorer</h1>
        <div className="flex items-center gap-3">
          <div className="flex rounded-lg border border-zinc-800 bg-zinc-900 p-1 text-sm">
            {(["live", "past"] as const).map((m) => (
              <button
                key={m}
                onClick={() => setMode(m)}
                className={`rounded-md px-3 py-1 capitalize ${
                  mode === m ? "bg-zinc-700 text-zinc-100" : "text-zinc-400 hover:text-zinc-200"
                }`}
              >
                {m}
              </button>
            ))}
          </div>
          <button
            onClick={toggleLiveIndex}
            disabled={mode === "past"}
            className={`rounded-md border px-3 py-1.5 text-sm disabled:cursor-not-allowed disabled:opacity-50 ${
              runningLiveIndex
                ? "border-rose-700 bg-rose-950/50 text-rose-300 hover:bg-rose-900/50"
                : "border-sky-700 bg-sky-950/50 text-sky-300 hover:bg-sky-900/50"
            }`}
          >
            {runningLiveIndex ? "Stop indexer" : "Start live indexer"}
          </button>
        </div>
      </div>

      <div className="grid grid-cols-2 gap-4 lg:grid-cols-4">
        <StatCard label="Ops" value={overview?.ops ?? 0} />
        <StatCard label="Gross USD" value={`$${overview?.gross_usd.toLocaleString() ?? 0}`} accent="good" />
        <StatCard label="Net USD" value={`$${overview?.net_usd.toLocaleString() ?? 0}`} accent="good" />
        <StatCard label="Highest single" value={`$${overview?.highest_single_usd.toLocaleString() ?? 0}`} />
      </div>

      {top && top.length > 0 && (
        <div className="rounded-xl border border-zinc-800 bg-zinc-900/60 p-4">
          <h2 className="mb-3 text-sm font-medium text-zinc-200">Top searchers</h2>
          <div className="flex flex-wrap gap-2">
            {top.map((s) => (
              <span key={s.label} className="rounded-lg border border-zinc-800 bg-zinc-950/60 px-3 py-1.5 text-xs">
                <span className="font-mono text-zinc-300">{short(s.label)}</span>
                <span className="ml-2 tabular-nums text-emerald-400">${s.gross_usd.toLocaleString()}</span>
                <span className="ml-2 text-zinc-500">{s.ops} ops</span>
              </span>
            ))}
          </div>
        </div>
      )}

      <div className="rounded-xl border border-zinc-800 overflow-hidden">
        <div className="flex flex-wrap items-center gap-2 border-b border-zinc-800 bg-zinc-900/60 px-4 py-2.5">
          <span className="text-xs uppercase tracking-wider text-zinc-500">feed</span>
          <div className="flex flex-wrap gap-1.5">
            {KINDS.map((k) => (
              <button
                key={k}
                onClick={() =>
                  setKinds((ks) => (ks.includes(k) ? ks.filter((x) => x !== k) : [...ks, k]))
                }
                className={`rounded-full border px-2 py-0.5 text-[11px] ${
                  kinds.includes(k)
                    ? KIND_COLOR[k]
                    : "border-zinc-800 bg-zinc-900 text-zinc-500 hover:text-zinc-300"
                }`}
              >
                {k}
              </button>
            ))}
          </div>
          <input
            value={q}
            onChange={(e) => setQ(e.target.value)}
            placeholder="filter hash / eoa / token"
            className="ml-auto w-56 rounded-md border border-zinc-700 bg-zinc-900 px-2 py-1 text-xs text-zinc-100 outline-none focus:border-emerald-400"
          />
        </div>
        <table className="w-full text-left text-sm">
          <thead className="border-b border-zinc-800 bg-zinc-900/40 text-xs uppercase tracking-wider text-zinc-500">
            <tr>
              <th className="px-3 py-2">ts</th>
              <th className="px-3 py-2">block</th>
              <th className="px-3 py-2">kind</th>
              <th className="px-3 py-2">eoa</th>
              <th className="px-3 py-2 text-right">net USD</th>
              <th className="px-3 py-2">tx</th>
            </tr>
          </thead>
          <tbody className="divide-y divide-zinc-800/70">
            {(feed ?? []).map((f, idx) => (
              <tr key={idx} className="cursor-pointer hover:bg-zinc-900/40" onClick={() => setDrawerTx(f.tx_hash)}>
                <td className="px-3 py-2 tabular-nums text-zinc-500">
                  {new Date(f.ts * 1000).toLocaleTimeString()}
                </td>
                <td className="px-3 py-2 tabular-nums text-zinc-300">{f.block_number}</td>
                <td className="px-3 py-2">
                  <span className={`rounded-full border px-2 py-0.5 text-[11px] ${KIND_COLOR[f.kind] ?? KIND_COLOR.unknown}`}>
                    {f.kind}
                  </span>
                </td>
                <td className="px-3 py-2 font-mono text-xs text-zinc-400">{short(f.eoa, 12)}</td>
                <td className="px-3 py-2 text-right tabular-nums text-emerald-400">
                  {f.net_profit_usd != null ? `$${f.net_profit_usd.toFixed(2)}` : "—"}
                </td>
                <td className="px-3 py-2 font-mono text-[11px] text-zinc-500">{short(f.tx_hash, 20)}</td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>

      <OpDrawer txHash={drawerTx} onClose={() => setDrawerTx(null)} />
    </div>
  );
}