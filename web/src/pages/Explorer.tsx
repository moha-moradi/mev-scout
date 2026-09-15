import { useRef, useState } from "react";
import { api, type FeedRow, type HealthResponse, type JobInfo, type OverviewRow, type StatsRow } from "../api";
import { usePolling } from "../hooks";
import StatCard from "../components/StatCard";
import OpDrawer from "../components/OpDrawer";
import Identicon from "../components/Identicon";
import RoutePath from "../components/RoutePath";
import { useToast } from "../components/Toast";
import {
  colorFromHex,
  formatUsd,
  formatUsdSigned,
  relativeTime,
  shortHex,
  txExplorerUrl,
} from "../lib/format";

const KINDS = ["arb_atomic", "sandwich", "liquidation", "jit", "jit_arb", "unknown"];
const KIND_COLOR: Record<string, string> = {
  arb_atomic: "bg-sky-950/60 border-sky-700/60 text-sky-300",
  sandwich: "bg-rose-950/60 border-rose-700/60 text-rose-300",
  liquidation: "bg-emerald-950/60 border-emerald-700/60 text-emerald-300",
  jit: "bg-amber-950/60 border-amber-700/60 text-amber-300",
  jit_arb: "bg-violet-950/60 border-violet-700/60 text-violet-300",
  unknown: "border-zinc-700 bg-zinc-800 text-zinc-300",
};

function ExternalLinkIcon() {
  return (
    <svg width="12" height="12" viewBox="0 0 24 24" fill="none" className="opacity-60" aria-hidden>
      <path
        d="M14 5h5v5M19 5l-9 9M10 5H6a1 1 0 0 0-1 1v12a1 1 0 0 0 1 1h12a1 1 0 0 0 1-1v-4"
        stroke="currentColor"
        strokeWidth="1.75"
        strokeLinecap="round"
        strokeLinejoin="round"
      />
    </svg>
  );
}

function TokenCell({ token }: { token: string | null }) {
  if (!token) return <span className="text-zinc-600">—</span>;
  return (
    <span className="inline-flex items-center gap-2" title={token}>
      <span
        className="inline-block h-5 w-5 rounded-full ring-1 ring-black/30"
        style={{ background: colorFromHex(token) }}
      />
      <span className="font-mono text-xs text-zinc-200">{shortHex(token, 2, 3)}</span>
    </span>
  );
}

export default function Explorer() {
  const [mode, setMode] = useState<"past" | "live">("live");
  const [kinds, setKinds] = useState<string[]>([]);
  const [q, setQ] = useState("");
  const toast = useToast();
  const [drawerTx, setDrawerTx] = useState<string | null>(null);

  const { data: health } = usePolling<HealthResponse>(api.health, 10_000, []);
  const chain = health?.chain;

  const { data: overview } = usePolling<OverviewRow>(
    () => api.explorerOverview("all"),
    15_000,
    [],
  );
  const { data: feed } = usePolling<FeedRow[]>(
    () => api.explorerFeed({ limit: 60, kinds: kinds.join(","), q: q || undefined }),
    4000,
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

  const rows = feed ?? [];

  return (
    <div className="space-y-6">
      <div className="flex flex-wrap items-center justify-between gap-3">
        <div className="flex items-center gap-3">
          <h1 className="text-xl font-semibold tracking-tight text-zinc-100">
            {mode === "live" ? "MEV Live" : "Explorer"}
          </h1>
          {mode === "live" && runningLiveIndex && (
            <span className="flex items-center gap-1.5 rounded-full border border-emerald-800/60 bg-emerald-950/40 px-2.5 py-0.5 text-[11px] text-emerald-300">
              <span className="h-1.5 w-1.5 animate-[pulse-dot_1.4s_ease-in-out_infinite] rounded-full bg-emerald-400" />
              indexing
            </span>
          )}
        </div>
        <div className="flex items-center gap-3">
          <div className="flex rounded-lg border border-zinc-800 bg-zinc-900/80 p-1 text-sm">
            {(["live", "past"] as const).map((m) => (
              <button
                key={m}
                onClick={() => setMode(m)}
                className={`rounded-md px-3 py-1 capitalize transition-colors ${
                  mode === m
                    ? "bg-emerald-400 font-semibold text-black"
                    : "text-zinc-400 hover:text-zinc-200"
                }`}
              >
                {m}
              </button>
            ))}
          </div>
          <button
            onClick={toggleLiveIndex}
            disabled={mode === "past"}
            className={`rounded-md border px-3 py-1.5 text-sm font-medium disabled:cursor-not-allowed disabled:opacity-40 ${
              runningLiveIndex
                ? "border-rose-700/70 bg-rose-950/40 text-rose-300 hover:bg-rose-900/40"
                : "border-emerald-700/50 bg-emerald-950/30 text-emerald-300 hover:bg-emerald-900/40"
            }`}
          >
            {runningLiveIndex ? "Stop indexer" : "Start live indexer"}
          </button>
        </div>
      </div>

      {mode === "past" && (
        <>
          <div className="grid grid-cols-2 gap-4 lg:grid-cols-4">
            <StatCard label="Ops" value={overview?.ops ?? 0} />
            <StatCard
              label="Gross USD"
              value={`$${overview?.gross_usd.toLocaleString() ?? 0}`}
              accent="good"
            />
            <StatCard
              label="Net USD"
              value={`$${overview?.net_usd.toLocaleString() ?? 0}`}
              accent="good"
            />
            <StatCard
              label="Highest single"
              value={`$${overview?.highest_single_usd.toLocaleString() ?? 0}`}
            />
          </div>

          {top && top.length > 0 && (
            <div className="rounded-xl border border-zinc-800 bg-zinc-900/60 p-4">
              <h2 className="mb-3 text-xs font-medium uppercase tracking-wider text-zinc-500">
                Top searchers
              </h2>
              <div className="flex flex-wrap gap-2">
                {top.map((s) => (
                  <span
                    key={s.label}
                    className="inline-flex items-center gap-2 rounded-lg border border-zinc-800 bg-zinc-950/60 px-3 py-1.5 text-xs"
                  >
                    <Identicon seed={s.label} size={18} />
                    <span className="font-mono text-zinc-300">{shortHex(s.label)}</span>
                    <span className="tabular-nums text-emerald-400">
                      ${s.gross_usd.toLocaleString()}
                    </span>
                    <span className="text-zinc-500">{s.ops} ops</span>
                  </span>
                ))}
              </div>
            </div>
          )}
        </>
      )}

      <div className="overflow-hidden rounded-xl border border-zinc-800 bg-zinc-950/40">
        <div className="flex flex-wrap items-center gap-2 border-b border-zinc-800 bg-zinc-900/50 px-4 py-3">
          <div className="relative min-w-[220px] flex-1">
            <input
              value={q}
              onChange={(e) => setQ(e.target.value)}
              placeholder="Search transaction by hash…"
              className="w-full rounded-lg border border-zinc-800 bg-zinc-950/80 py-2 pl-3 pr-9 text-sm text-zinc-100 outline-none placeholder:text-zinc-600 focus:border-emerald-400/70"
            />
            <span className="pointer-events-none absolute right-3 top-1/2 -translate-y-1/2 text-zinc-600">
              <svg width="14" height="14" viewBox="0 0 24 24" fill="none" aria-hidden>
                <circle cx="11" cy="11" r="7" stroke="currentColor" strokeWidth="1.75" />
                <path d="M20 20l-3.5-3.5" stroke="currentColor" strokeWidth="1.75" strokeLinecap="round" />
              </svg>
            </span>
          </div>
          <div className="flex flex-wrap gap-1.5">
            {KINDS.map((k) => (
              <button
                key={k}
                onClick={() =>
                  setKinds((ks) => (ks.includes(k) ? ks.filter((x) => x !== k) : [...ks, k]))
                }
                className={`rounded-full border px-2.5 py-0.5 text-[11px] transition-colors ${
                  kinds.includes(k)
                    ? KIND_COLOR[k]
                    : "border-zinc-800 bg-zinc-900 text-zinc-500 hover:text-zinc-300"
                }`}
              >
                {k}
              </button>
            ))}
          </div>
        </div>

        <div className="overflow-x-auto">
          <table className="w-full min-w-[980px] text-left text-sm">
            <thead className="border-b border-zinc-800/80 text-[11px] uppercase tracking-wider text-zinc-500">
              <tr>
                <th className="px-3 py-3 font-medium">Time</th>
                <th className="px-3 py-3 font-medium">Token</th>
                <th className="px-3 py-3 font-medium">Hash</th>
                <th className="px-3 py-3 font-medium">Route</th>
                <th className="px-3 py-3 font-medium">Kind</th>
                <th className="px-3 py-3 text-right font-medium">Spent</th>
                <th className="px-3 py-3 text-right font-medium">Revenue</th>
                <th className="px-3 py-3 text-right font-medium">Profit</th>
                <th className="px-3 py-3 font-medium">Sender</th>
                <th className="px-3 py-3 text-right font-medium">Block</th>
              </tr>
            </thead>
            <tbody className="divide-y divide-zinc-800/60">
              {rows.length === 0 && (
                <tr>
                  <td colSpan={10} className="px-4 py-16 text-center">
                    <div className="mx-auto max-w-sm space-y-2">
                      <p className="text-sm text-zinc-400">No MEV operations in the feed yet.</p>
                      <p className="text-xs text-zinc-600">
                        Start the live indexer to stream new blocks into the explorer store.
                      </p>
                    </div>
                  </td>
                </tr>
              )}
              {rows.map((f) => {
                const net = f.net_profit_usd;
                const gross = f.profit_usd;
                const gas = f.gas_cost_usd;
                const profitable = (net ?? 0) > 0;
                const explorer = txExplorerUrl(chain, f.tx_hash);
                return (
                  <tr
                    key={`${f.tx_hash}-${f.block_number}-${f.ts}`}
                    className="cursor-pointer transition-colors hover:bg-zinc-900/50"
                    onClick={() => setDrawerTx(f.tx_hash)}
                  >
                    <td className="whitespace-nowrap px-3 py-2.5 tabular-nums text-zinc-500">
                      {relativeTime(f.ts)}
                    </td>
                    <td className="px-3 py-2.5">
                      <TokenCell token={f.profit_token} />
                    </td>
                    <td className="px-3 py-2.5">
                      <span className="inline-flex items-center gap-1.5 font-mono text-xs text-zinc-200">
                        {shortHex(f.tx_hash, 4, 4)}
                        {explorer && (
                          <a
                            href={explorer}
                            target="_blank"
                            rel="noreferrer"
                            onClick={(e) => e.stopPropagation()}
                            className="text-zinc-500 hover:text-emerald-400"
                            title="Open in explorer"
                          >
                            <ExternalLinkIcon />
                          </a>
                        )}
                      </span>
                    </td>
                    <td className="px-3 py-2.5">
                      <RoutePath routeJson={f.route_json} />
                    </td>
                    <td className="px-3 py-2.5">
                      <span
                        className={`rounded-full border px-2 py-0.5 text-[11px] ${
                          KIND_COLOR[f.kind] ?? KIND_COLOR.unknown
                        }`}
                      >
                        {f.kind}
                      </span>
                    </td>
                    <td className="whitespace-nowrap px-3 py-2.5 text-right tabular-nums text-zinc-400">
                      {formatUsd(gas)}
                    </td>
                    <td className="whitespace-nowrap px-3 py-2.5 text-right tabular-nums text-zinc-300">
                      {formatUsd(gross)}
                    </td>
                    <td
                      className={`whitespace-nowrap px-3 py-2.5 text-right font-medium tabular-nums ${
                        profitable ? "text-emerald-400" : net != null && net < 0 ? "text-rose-400" : "text-zinc-400"
                      }`}
                    >
                      {formatUsdSigned(net)}
                    </td>
                    <td className="px-3 py-2.5">
                      <span className="inline-flex items-center gap-2" title={f.eoa}>
                        <Identicon seed={f.eoa} />
                        <span className="font-mono text-xs text-zinc-300">{shortHex(f.eoa, 4, 4)}</span>
                      </span>
                    </td>
                    <td className="whitespace-nowrap px-3 py-2.5 text-right font-mono text-xs tabular-nums text-zinc-500">
                      {f.block_number.toLocaleString()}
                    </td>
                  </tr>
                );
              })}
            </tbody>
          </table>
        </div>
      </div>

      <OpDrawer txHash={drawerTx} chain={chain} onClose={() => setDrawerTx(null)} />
    </div>
  );
}
