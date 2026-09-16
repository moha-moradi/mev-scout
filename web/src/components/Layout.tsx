import { NavLink, Outlet, useLocation } from "react-router-dom";
import ChainSelector from "./ChainSelector";
import ErrorBoundary from "./ErrorBoundary";
import { usePolling } from "../hooks";
import { api, type HealthResponse, type SyncResponse } from "../api";

const NAV = [
  { to: "/", label: "Dashboard", end: true, icon: "grid" },
  { to: "/run", label: "Backtest Runner", icon: "chart" },
  { to: "/live", label: "Live Monitor", icon: "pulse" },
  { to: "/explorer", label: "Explorer", icon: "target" },
  { to: "/pools", label: "Pools", icon: "layers" },
  { to: "/jobs", label: "Jobs", icon: "clock" },
  { to: "/results", label: "Results", icon: "report" },
  { to: "/config", label: "Config", icon: "sliders" },
] as const;

function NavIcon({ name }: { name: (typeof NAV)[number]["icon"] }) {
  const common = {
    width: 16,
    height: 16,
    viewBox: "0 0 24 24",
    fill: "none",
    stroke: "currentColor",
    strokeWidth: 1.75,
    strokeLinecap: "round" as const,
    strokeLinejoin: "round" as const,
    "aria-hidden": true,
  };
  switch (name) {
    case "grid":
      return (
        <svg {...common}>
          <rect x="3" y="3" width="7" height="7" rx="1" />
          <rect x="14" y="3" width="7" height="7" rx="1" />
          <rect x="3" y="14" width="7" height="7" rx="1" />
          <rect x="14" y="14" width="7" height="7" rx="1" />
        </svg>
      );
    case "chart":
      return (
        <svg {...common}>
          <path d="M4 19V5M4 19h16" />
          <path d="M8 15l3-4 3 2 5-7" />
        </svg>
      );
    case "pulse":
      return (
        <svg {...common}>
          <path d="M3 12h4l2-5 4 10 2-5h6" />
        </svg>
      );
    case "target":
      return (
        <svg {...common}>
          <circle cx="12" cy="12" r="8" />
          <circle cx="12" cy="12" r="3" />
        </svg>
      );
    case "layers":
      return (
        <svg {...common}>
          <path d="M12 3l9 5-9 5-9-5 9-5z" />
          <path d="M3 12l9 5 9-5" />
          <path d="M3 16l9 5 9-5" />
        </svg>
      );
    case "clock":
      return (
        <svg {...common}>
          <circle cx="12" cy="12" r="8" />
          <path d="M12 8v5l3 2" />
        </svg>
      );
    case "report":
      return (
        <svg {...common}>
          <path d="M4 19V5h11l5 5v9H4z" />
          <path d="M14 5v5h5" />
          <path d="M8 13h8M8 16h5" />
        </svg>
      );
    case "sliders":
      return (
        <svg {...common}>
          <path d="M4 7h10M18 7h2M4 17h2M10 17h10" />
          <circle cx="16" cy="7" r="2.25" />
          <circle cx="8" cy="17" r="2.25" />
        </svg>
      );
  }
}

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
  const apiOk = health != null;

  return (
    <div className="min-h-screen bg-zinc-950 font-sans text-zinc-100">
      <aside className="fixed inset-y-0 left-0 z-30 flex w-56 flex-col border-r border-zinc-800/80 bg-zinc-950/95 p-3 sm:w-60 sm:p-4">
        <div className="mb-5 flex items-center gap-2 px-1">
          <span className="text-lg font-semibold tracking-tight text-emerald-400">mev-scout</span>
          {health?.chain && (
            <span className="rounded border border-sky-500/40 bg-sky-950/40 px-1.5 py-0.5 text-[10px] uppercase tracking-wider text-sky-300">
              {health.chain}
            </span>
          )}
        </div>

        <div className="mb-5 px-1">
          <div className="mb-1.5 text-[10px] font-medium uppercase tracking-[0.14em] text-zinc-500">
            Chain
          </div>
          <div className="w-full [&_select]:w-full [&_select]:border-zinc-800 [&_select]:bg-zinc-900/80 [&_select]:py-2">
            <ChainSelector />
          </div>
        </div>

        <nav className="flex flex-col gap-1">
          {NAV.map((n) => (
            <NavLink
              key={n.to}
              to={n.to}
              end={n.end}
              className={({ isActive }) =>
                `relative flex items-center gap-2.5 rounded-xl px-3 py-2 text-sm transition-colors ${
                  isActive
                    ? "bg-zinc-900/90 font-medium text-zinc-100 shadow-[inset_0_0_0_1px_rgba(56,189,248,0.12)]"
                    : "text-zinc-400 hover:bg-zinc-900/50 hover:text-zinc-200"
                }`
              }
            >
              {({ isActive }) => (
                <>
                  {isActive && (
                    <span
                      className="absolute left-0 top-1/2 h-[58%] w-[3px] -translate-y-1/2 rounded-full bg-sky-400 shadow-[0_0_10px_rgba(56,189,248,0.85)]"
                      aria-hidden
                    />
                  )}
                  <span className={isActive ? "text-sky-300" : "text-zinc-500"}>
                    <NavIcon name={n.icon} />
                  </span>
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

      <div className="pl-56 sm:pl-60">
        <header className="sticky top-0 z-20 border-b border-zinc-800/80 bg-zinc-950/90 backdrop-blur">
          <div className="flex items-center justify-between gap-3 px-4 py-3 lg:px-8">
            <div className="flex items-center gap-3">
              <span
                className={`rounded-md px-2 py-1 text-[11px] font-semibold uppercase tracking-wider ${
                  apiOk
                    ? "bg-emerald-400 text-black"
                    : "border border-zinc-800 bg-zinc-900 text-zinc-500"
                }`}
              >
                api
              </span>
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
                  <span className="h-1.5 w-1.5 rounded-full bg-emerald-400" />
                  IDLE
                </span>
              )}
            </div>
            <div className="flex items-center gap-3">
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
