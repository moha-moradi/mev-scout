import { NavLink, Outlet } from "react-router-dom";
import ChainSelector from "./ChainSelector";
import ErrorBoundary from "./ErrorBoundary";
import { usePolling } from "../hooks";
import { api, type HealthResponse } from "../api";

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

function HealthBadge() {
  const { data: health } = usePolling<HealthResponse>(api.health, 5_000, []);
  const cacheOk = health?.db_status.cache === "ok" || health?.db_status.cache.includes("reads");
  const explorerOk =
    health?.db_status.explorer === "ok" || health?.db_status.explorer.includes("reads");
  const running = health?.job_status.running;

  return (
    <div className="flex items-center gap-4 text-xs text-zinc-400">
      <span
        title="cache DB"
        className={`flex items-center gap-1.5 ${cacheOk ? "text-emerald-400" : "text-amber-400"}`}
      >
        <span className={`h-1.5 w-1.5 rounded-full ${cacheOk ? "bg-emerald-400" : "bg-amber-400"}`} />
        cache {health?.db_status.cache ?? "–"}
      </span>
      <span
        title="explorer DB"
        className={`flex items-center gap-1.5 ${explorerOk ? "text-emerald-400" : "text-amber-400"}`}
      >
        <span className={`h-1.5 w-1.5 rounded-full ${explorerOk ? "bg-emerald-400" : "bg-amber-400"}`} />
        explorer {health?.db_status.explorer ?? "–"}
      </span>
      {running && (
        <a
          href={`/jobs#${running}`}
          className="flex items-center gap-1.5 rounded-full border border-amber-600/50 bg-amber-950/50 px-2 py-0.5 text-amber-300"
        >
          <span className="h-1.5 w-1.5 animate-pulse rounded-full bg-amber-400" />
          job {running.slice(0, 8)}
        </a>
      )}
    </div>
  );
}

export default function Layout() {
  return (
    <div className="min-h-screen bg-zinc-950 text-zinc-100">
      <aside className="fixed inset-y-0 left-0 hidden w-60 border-r border-zinc-800/80 bg-zinc-950/90 p-4 lg:block">
        <div className="mb-6 flex items-center gap-2 px-1">
          <span className="text-lg font-semibold tracking-tight text-zinc-50">mev-scout</span>
          <span className="rounded bg-zinc-800 px-1.5 py-0.5 text-[10px] uppercase tracking-wider text-zinc-400">
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
                `rounded-md px-3 py-2 text-sm transition-colors ${
                  isActive
                    ? "bg-sky-950/60 font-medium text-sky-200"
                    : "text-zinc-400 hover:bg-zinc-900 hover:text-zinc-200"
                }`
              }
            >
              {n.label}
            </NavLink>
          ))}
        </nav>
      </aside>

      <div className="lg:pl-60">
        <header className="sticky top-0 z-20 border-b border-zinc-800/80 bg-zinc-950/90 backdrop-blur">
          <div className="flex items-center justify-between gap-3 px-4 py-3 lg:px-8">
            <HealthBadge />
            <ChainSelector />
          </div>
        </header>
        <main className="mx-auto max-w-7xl px-4 py-6 lg:px-8">
          <ErrorBoundary>
            <Outlet />
          </ErrorBoundary>
        </main>
      </div>
    </div>
  );
}