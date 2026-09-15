import { NavLink, Outlet, useLocation } from "react-router-dom";
import ChainSelector from "./ChainSelector";
import ErrorBoundary from "./ErrorBoundary";
import { usePolling } from "../hooks";
import { api, type HealthResponse, type SyncResponse } from "../api";

const NAV = [
  { to: "/", label: "Dashboard", end: true },
  { to: "/run", label: "Backtest Runner" },
  { to: "/live", label: "Live Monitor" },
  { to: "/explorer", label: "Explorer" },
  { to: "/pools", label: "Pools" },
  { to: "/jobs", label: "Jobs" },
  { to: "/results", label: "Results" },
  { to: "/config", label: "Config" },
];

function useHead() {
  const { data: health } = usePolling<HealthResponse>(api.health, 5_000, []);
  const { data: sync } = usePolling<SyncResponse>(api.sync, 5_000, []);
  return { health, sync };
}

export default function Layout() {
  const location = useLocation();
  const current = NAV.find(
    (n) => (n.end && n.to === location.pathname) || (!n.end && location.pathname.startsWith(n.to)),
  );
  const { health, sync } = useHead();
  const running = health?.job_status.running;
  const head = sync?.explorer_head ?? sync?.cache_head;

  return (
    <div className="min-h-screen bg-zinc-950 font-sans text-zinc-100">
      <aside className="fixed inset-y-0 left-0 z-30 hidden w-60 flex-col border-r border-zinc-800/80 bg-zinc-950/90 p-4 lg:flex">
        <div className="mb-6 flex items-center gap-2 px-1">
          <span className="text-lg font-semibold tracking-tight text-emerald-400">mev-scout</span>
          <span className="rounded bg-zinc-800 px-1.5 py-0.5 text-[10px] uppercase tracking-wider text-zinc-500">
            ui
          </span>
        </div>
        <nav className="flex flex-col gap-1">
          {NAV.map((n) => (
            <NavLink
              key={n.to}
              to={n.to}
              end={n.end}
              className={({ isActive }) =>
                `relative rounded-md px-3 py-2 text-sm transition-colors ${
                  isActive
                    ? "bg-zinc-900 font-medium text-emerald-400"
                    : "text-zinc-400 hover:bg-zinc-900/60 hover:text-zinc-200"
                }`
              }
            >
              {({ isActive }) => (
                <>
                  {isActive && (
                    <span className="absolute left-0 top-1/2 h-4 w-0.5 -translate-y-1/2 rounded-full bg-emerald-400" />
                  )}
                  {n.label}
                </>
              )}
            </NavLink>
          ))}
        </nav>
        <div className="mt-auto border-t border-zinc-800/80 pt-3 font-mono text-[11px] tabular-nums">
          <div className="flex items-center justify-between py-1">
            <span className="uppercase tracking-wider text-zinc-500">block sync</span>
            <span className="text-zinc-300">{head !== undefined ? head.toLocaleString() : "–"}</span>
          </div>
          <div className="flex items-center justify-between py-1">
            <span className="uppercase tracking-wider text-zinc-500">chain</span>
            <span className="text-zinc-300">{health?.chain ?? "–"}</span>
          </div>
          <div className="flex items-center justify-between py-1">
            <span className="uppercase tracking-wider text-zinc-500">rpc</span>
<span className="text-zinc-300">
              {health !== null && health !== undefined ? `${health.rpc_provider_count} nodes` : "–"}
            </span>
          </div>
        </div>
      </aside>

      <div className="lg:pl-60">
        <header className="sticky top-0 z-20 border-b border-zinc-800/80 bg-zinc-950/90 backdrop-blur">
          <div className="flex items-center justify-between gap-3 px-4 py-3 lg:px-8">
            <div className="flex items-center gap-4">
              {running ? (
                <a
                  href={`/jobs#${running}`}
                  className="flex items-center gap-1.5 rounded-full border border-amber-600/50 bg-amber-950/50 px-2.5 py-1 text-xs text-amber-300"
                >
                  <span className="h-1.5 w-1.5 animate-pulse rounded-full bg-amber-400" />
                  RUNNING · {running.slice(0, 8)}
                </a>
              ) : (
                <span className="flex items-center gap-1.5 rounded-full border border-zinc-800 px-2.5 py-1 text-xs text-zinc-500">
                  <span className="h-1.5 w-1.5 rounded-full bg-zinc-600" />
                  IDLE
                </span>
              )}
            </div>
            <div className="flex items-center gap-3">
              <ChainSelector />
              <NavLink
                to="/run"
                className="rounded-md bg-emerald-400 px-4 py-1.5 text-sm font-semibold text-black transition-colors hover:bg-emerald-300"
              >
                New simulation
              </NavLink>
            </div>
          </div>
        </header>
        <main className="mx-auto w-full max-w-[1600px] px-4 py-6 lg:px-8">
          <div className="mb-5 flex items-center gap-2 text-[11px] uppercase tracking-wider">
            <span className="h-3 w-0.5 rounded-full bg-sky-400/80" />
            <span className="text-zinc-500">{health?.chain ?? "…"}</span>
            <span className="text-zinc-700">/</span>
            <span className="text-zinc-300">{current?.label ?? "…"}</span>
          </div>
          <ErrorBoundary>
            <Outlet />
          </ErrorBoundary>
        </main>
      </div>
    </div>
  );
}