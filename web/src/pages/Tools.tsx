import { useMemo, useState } from "react";
import { Link, useNavigate } from "react-router-dom";
import { api, type DoctorOutcome, type JobInfo } from "../api";
import { usePolling } from "../hooks";
import LogViewer from "../components/LogViewer";
import { useToast } from "../components/Toast";

type Field =
  | { key: string; label: string; kind: "text" | "number"; placeholder?: string; optional?: boolean; default?: string }
  | { key: string; label: string; kind: "checkbox"; default?: boolean }
  | { key: string; label: string; kind: "select"; options: string[]; default?: string };

type ToolDef = {
  id: string;
  command: string;
  title: string;
  blurb: string;
  category: string;
  fields: Field[];
  /** Build argv from form values. Positional args go first. */
  buildArgs: (v: Record<string, string | boolean>) => string[];
  /** Dedicated page instead of freeform job (optional). */
  page?: string;
};

const TOOLS: ToolDef[] = [
  {
    id: "run",
    command: "run",
    title: "Backtest run",
    blurb: "Full strategy backtest over a block range.",
    category: "Scanner",
    page: "/run",
    fields: [
      { key: "blocks", label: "blocks", kind: "number", placeholder: "10", optional: true },
      { key: "days", label: "days", kind: "number", optional: true },
      { key: "from-block", label: "from-block", kind: "number", optional: true },
      { key: "to-block", label: "to-block", kind: "number", optional: true },
      { key: "record-rejections", label: "record-rejections", kind: "checkbox" },
      { key: "batch-rpc", label: "batch-rpc", kind: "checkbox" },
    ],
    buildArgs: (v) => rangeArgs(v, ["--record-rejections", "--batch-rpc"]),
  },
  {
    id: "live",
    command: "live",
    title: "Live monitor",
    blurb: "Stream tip blocks and detect opportunities.",
    category: "Scanner",
    page: "/live",
    fields: [
      { key: "loop", label: "loop", kind: "checkbox", default: true },
      { key: "duration", label: "duration", kind: "text", placeholder: "2m", optional: true },
      { key: "poll-interval", label: "poll-interval ms", kind: "number", placeholder: "2000", optional: true },
      { key: "max-blocks", label: "max-blocks", kind: "number", optional: true },
      { key: "record-rejections", label: "record-rejections", kind: "checkbox" },
    ],
    buildArgs: (v) => flagArgs(v, ["--loop", "--duration", "--poll-interval", "--max-blocks", "--record-rejections"]),
  },
  {
    id: "fetch",
    command: "fetch",
    title: "Fetch / cache blocks",
    blurb: "Pre-cache blocks + receipts without running strategies.",
    category: "Data",
    fields: [
      { key: "blocks", label: "blocks", kind: "number", placeholder: "10", optional: true },
      { key: "days", label: "days", kind: "number", optional: true },
      { key: "block", label: "block", kind: "number", optional: true },
      { key: "from-block", label: "from-block", kind: "number", optional: true },
      { key: "to-block", label: "to-block", kind: "number", optional: true },
      { key: "batch-rpc", label: "batch-rpc", kind: "checkbox" },
      { key: "no-sig-resolve", label: "no-sig-resolve", kind: "checkbox" },
    ],
    buildArgs: (v) => rangeArgs(v, ["--batch-rpc", "--no-sig-resolve"]),
  },
  {
    id: "replay",
    command: "replay",
    title: "Replay block",
    blurb: "EVM-replay a cached block for receipt verification.",
    category: "Data",
    fields: [
      { key: "block", label: "block (required)", kind: "number", placeholder: "70000000" },
      { key: "tx-index", label: "tx-index", kind: "number", optional: true },
      { key: "analyze", label: "analyze DEX logs", kind: "checkbox" },
    ],
    buildArgs: (v) => flagArgs(v, ["--block", "--tx-index", "--analyze"]),
  },
  {
    id: "discover",
    command: "discover",
    title: "Discover pools",
    blurb: "On-chain / remote / hybrid pool discovery.",
    category: "Pools",
    page: "/pools",
    fields: [
      { key: "blocks", label: "blocks", kind: "number", optional: true },
      { key: "days", label: "days", kind: "number", optional: true },
      { key: "source", label: "source", kind: "select", options: ["onchain", "remote", "hybrid"], default: "hybrid" },
      { key: "incremental", label: "incremental", kind: "checkbox", default: true },
      { key: "enrich", label: "enrich TVL", kind: "checkbox", default: true },
      { key: "min-tvl", label: "min-tvl", kind: "number", optional: true },
      { key: "max-pools", label: "max-pools", kind: "number", optional: true },
      { key: "resolve-remote-metadata", label: "resolve-remote-metadata", kind: "checkbox" },
      { key: "json", label: "json", kind: "checkbox" },
    ],
    buildArgs: (v) =>
      rangeArgs(v, [
        "--source",
        "--incremental",
        "--enrich",
        "--min-tvl",
        "--max-pools",
        "--resolve-remote-metadata",
        "--json",
      ]),
  },
  {
    id: "validate-pools",
    command: "validate-pools",
    title: "Validate pools",
    blurb: "Recall vs GeckoTerminal reference sets.",
    category: "Pools",
    fields: [
      { key: "days", label: "days", kind: "number", placeholder: "7", default: "7" },
      { key: "source", label: "source", kind: "select", options: ["all", "gecko"], default: "all" },
      { key: "json", label: "json", kind: "checkbox", default: true },
      { key: "markdown-out", label: "markdown-out", kind: "text", placeholder: "results/pools.md", optional: true },
    ],
    buildArgs: (v) => flagArgs(v, ["--days", "--source", "--json", "--markdown-out"]),
  },
  {
    id: "tokens",
    command: "tokens",
    title: "Tokens cache",
    blurb: "List / warm token metadata; optional --enrich (Llama + CoinGecko).",
    category: "Pools",
    fields: [
      { key: "symbol", label: "symbol filter", kind: "text", optional: true },
      { key: "decimals", label: "decimals", kind: "number", optional: true },
      { key: "limit", label: "limit", kind: "number", placeholder: "100", optional: true },
      { key: "cache-only", label: "cache-only", kind: "checkbox" },
    ],
    buildArgs: (v) => flagArgs(v, ["--symbol", "--decimals", "--limit", "--cache-only"]),
  },
  {
    id: "scan",
    command: "scan",
    title: "Event scan",
    blurb: "RPC log scan: trades, transfers, flashloans, liquidations, labels.",
    category: "Scanner",
    fields: [
      { key: "blocks", label: "blocks", kind: "number", placeholder: "50", optional: true },
      { key: "days", label: "days", kind: "number", optional: true },
      {
        key: "kind",
        label: "kind",
        kind: "select",
        options: ["trades", "transfers", "flashloans", "liquidations", "labels"],
        default: "trades",
      },
      { key: "address", label: "address", kind: "text", optional: true },
      { key: "limit", label: "limit", kind: "number", optional: true },
      { key: "batch-size", label: "batch-size", kind: "number", optional: true },
      { key: "min-value", label: "min-value (wei)", kind: "text", optional: true },
    ],
    buildArgs: (v) => rangeArgs(v, ["--kind", "--address", "--limit", "--batch-size", "--min-value"]),
  },
  {
    id: "report",
    command: "report",
    title: "Report",
    blurb: "Re-render tables for a saved run.",
    category: "Results",
    page: "/results",
    fields: [{ key: "run-id", label: "run-id", kind: "text", optional: true, placeholder: "latest" }],
    buildArgs: (v) => flagArgs(v, ["--run-id"]),
  },
  {
    id: "explorer-index",
    command: "explorer index",
    title: "Explorer index",
    blurb: "Backfill or live-index realized MEV into the explorer store.",
    category: "Explorer",
    page: "/explorer",
    fields: [
      { key: "from", label: "from", kind: "number", optional: true },
      { key: "to", label: "to", kind: "number", optional: true },
      { key: "days", label: "days", kind: "number", optional: true },
      { key: "live", label: "live", kind: "checkbox" },
      { key: "duration", label: "duration", kind: "text", optional: true },
    ],
    buildArgs: (v) => flagArgs(v, ["--from", "--to", "--days", "--live", "--duration"]),
  },
  {
    id: "explorer-doctor",
    command: "explorer doctor",
    title: "Explorer doctor",
    blurb: "Probe RPC providers: latest, bulk receipts, traces.",
    category: "Explorer",
    fields: [],
    buildArgs: () => [],
  },
  {
    id: "explorer-export",
    command: "explorer export",
    title: "Explorer export",
    blurb: "Download realized ops as JSON or CSV.",
    category: "Explorer",
    fields: [
      { key: "format", label: "format", kind: "select", options: ["json", "csv"], default: "json" },
      { key: "since", label: "since", kind: "select", options: ["all", "1d", "7d", "30d"], default: "7d" },
      { key: "kinds", label: "kinds", kind: "text", optional: true, placeholder: "arb_atomic,sandwich" },
    ],
    buildArgs: (v) => flagArgs(v, ["--format", "--since", "--kinds"]),
  },
  {
    id: "explorer-validate",
    command: "explorer validate",
    title: "Explorer validate",
    blurb: "Cross-check realized ops vs scanner opportunities.",
    category: "Explorer",
    fields: [
      { key: "since", label: "since", kind: "select", options: ["all", "1d", "7d", "30d"], default: "7d" },
      { key: "match-window", label: "match-window", kind: "number", placeholder: "0", optional: true },
      { key: "run", label: "run id", kind: "text", optional: true },
      { key: "threshold-sweep", label: "threshold-sweep", kind: "checkbox" },
      { key: "emit-missing-pools", label: "emit-missing-pools", kind: "checkbox" },
      { key: "json", label: "json", kind: "checkbox", default: true },
    ],
    buildArgs: (v) =>
      flagArgs(v, ["--since", "--match-window", "--run", "--threshold-sweep", "--emit-missing-pools", "--json"]),
  },
  {
    id: "explorer-show",
    command: "explorer show",
    title: "Explorer show + trace",
    blurb: "Op detail; optional debug_traceTransaction verification.",
    category: "Explorer",
    fields: [
      { key: "tx", label: "tx hash (required)", kind: "text", placeholder: "0x…" },
      { key: "trace", label: "trace", kind: "checkbox", default: true },
    ],
    buildArgs: (v) => {
      const args: string[] = [];
      const tx = String(v.tx ?? "").trim();
      if (tx) args.push(tx);
      if (v.trace) args.push("--trace");
      return args;
    },
  },
];

function rangeArgs(v: Record<string, string | boolean>, extraKeys: string[]): string[] {
  const args: string[] = [];
  for (const k of ["days", "blocks", "block", "from-block", "to-block"]) {
    const val = v[k];
    if (typeof val === "string" && val.trim()) {
      args.push(`--${k}`, val.trim());
    }
  }
  return args.concat(flagArgs(v, extraKeys.filter((k) => !["--days", "--blocks", "--block", "--from-block", "--to-block"].includes(k))));
}

function flagArgs(v: Record<string, string | boolean>, keys: string[]): string[] {
  const args: string[] = [];
  for (const raw of keys) {
    const key = raw.replace(/^--/, "");
    const val = v[key];
    if (typeof val === "boolean") {
      if (val) args.push(`--${key}`);
    } else if (typeof val === "string" && val.trim()) {
      args.push(`--${key}`, val.trim());
    }
  }
  return args;
}

function defaultsFor(tool: ToolDef): Record<string, string | boolean> {
  const out: Record<string, string | boolean> = {};
  for (const f of tool.fields) {
    if (f.kind === "checkbox") out[f.key] = f.default ?? false;
    else if (f.kind === "select") out[f.key] = f.default ?? f.options[0] ?? "";
    else if ("default" in f && f.default) out[f.key] = f.default;
    else out[f.key] = "";
  }
  return out;
}

const CATEGORIES = ["Scanner", "Data", "Pools", "Explorer", "Results"];

export default function Tools() {
  const [activeId, setActiveId] = useState(TOOLS[0].id);
  const tool = TOOLS.find((t) => t.id === activeId) ?? TOOLS[0];
  const [values, setValues] = useState<Record<string, string | boolean>>(() => defaultsFor(TOOLS[0]));
  const [timeoutSecs, setTimeoutSecs] = useState("");
  const [starting, setStarting] = useState(false);
  const [selectedJob, setSelectedJob] = useState<string | null>(null);
  const [doctor, setDoctor] = useState<DoctorOutcome | null>(null);
  const [doctorLoading, setDoctorLoading] = useState(false);
  const toast = useToast();
  const navigate = useNavigate();
  const { data: jobs, refresh } = usePolling<JobInfo[]>(api.jobs, 4000, []);

  const categories = useMemo(() => CATEGORIES.filter((c) => TOOLS.some((t) => t.category === c)), []);

  function selectTool(id: string) {
    const t = TOOLS.find((x) => x.id === id);
    if (!t) return;
    setActiveId(id);
    setValues(defaultsFor(t));
    setDoctor(null);
  }

  async function runJob() {
    setStarting(true);
    try {
      if (tool.id === "explorer-doctor") {
        setDoctorLoading(true);
        const res = await api.explorerDoctor();
        setDoctor(res);
        toast(res.gate_ok ? "Doctor gate PASS" : "Doctor gate FAIL", res.gate_ok ? "success" : "error");
        return;
      }
      if (tool.id === "explorer-export") {
        const format = String(values.format || "json");
        const since = String(values.since || "all");
        const kinds = String(values.kinds || "");
        await api.explorerExportDownload({ format, since, kinds: kinds || undefined });
        toast("Export downloaded.", "success");
        return;
      }
      const args = tool.buildArgs(values);
      if (tool.command === "replay" && !args.includes("--block")) {
        toast("--block is required", "error");
        return;
      }
      if (tool.command === "explorer show" && args.length === 0) {
        toast("tx hash is required", "error");
        return;
      }
      const res = await api.createJob(
        tool.command,
        args,
        timeoutSecs ? Number(timeoutSecs) : undefined,
      );
      toast(`Job ${res.job_id.slice(0, 8)} started.`, "success");
      setSelectedJob(res.job_id);
      refresh();
    } catch (e) {
      toast(`Failed: ${e instanceof Error ? e.message : String(e)}`, "error");
    } finally {
      setStarting(false);
      setDoctorLoading(false);
    }
  }

  const selected = jobs?.find((j) => j.job_id === selectedJob) ?? null;

  return (
    <div className="space-y-6">
      <div className="flex flex-wrap items-end justify-between gap-3">
        <div>
          <h1 className="text-xl font-semibold text-zinc-100">Tools</h1>
          <p className="mt-1 text-sm text-zinc-500">
            Full CLI command surface via the API — every former CLI capability lives here.
          </p>
        </div>
        <Link to="/jobs" className="text-sm text-sky-400 hover:underline">
          Job history →
        </Link>
      </div>

      <div className="grid gap-6 lg:grid-cols-[240px_1fr]">
        <aside className="space-y-4">
          {categories.map((cat) => (
            <div key={cat}>
              <div className="mb-1.5 text-[10px] font-medium uppercase tracking-[0.14em] text-zinc-500">{cat}</div>
              <div className="flex flex-col gap-1">
                {TOOLS.filter((t) => t.category === cat).map((t) => (
                  <button
                    key={t.id}
                    type="button"
                    onClick={() => selectTool(t.id)}
                    className={`rounded-lg px-3 py-2 text-left text-sm transition-colors ${
                      t.id === activeId
                        ? "bg-zinc-900 text-zinc-100 shadow-[inset_0_0_0_1px_rgba(56,189,248,0.2)]"
                        : "text-zinc-400 hover:bg-zinc-900/60 hover:text-zinc-200"
                    }`}
                  >
                    {t.title}
                  </button>
                ))}
              </div>
            </div>
          ))}
        </aside>

        <section className="space-y-4 rounded-xl border border-zinc-800 bg-zinc-900/50 p-5">
          <div className="flex flex-wrap items-start justify-between gap-3">
            <div>
              <h2 className="text-lg font-medium text-zinc-100">{tool.title}</h2>
              <p className="mt-1 text-sm text-zinc-500">{tool.blurb}</p>
              <code className="mt-2 inline-block rounded border border-zinc-800 bg-zinc-950 px-2 py-0.5 font-mono text-[11px] text-emerald-300">
                {tool.command}
              </code>
            </div>
            {tool.page && (
              <button
                type="button"
                onClick={() => navigate(tool.page!)}
                className="rounded-md border border-zinc-700 px-3 py-1.5 text-xs text-zinc-300 hover:border-sky-500/50 hover:text-sky-300"
              >
                Open dedicated page
              </button>
            )}
          </div>

          {tool.fields.length > 0 && (
            <div className="grid gap-3 sm:grid-cols-2 lg:grid-cols-3">
              {tool.fields.map((f) => (
                <label key={f.key} className="block text-xs">
                  <span className="mb-1.5 block uppercase tracking-wider text-zinc-500">{f.label}</span>
                  {f.kind === "checkbox" ? (
                    <input
                      type="checkbox"
                      checked={Boolean(values[f.key])}
                      onChange={(e) => setValues((prev) => ({ ...prev, [f.key]: e.target.checked }))}
                      className="h-4 w-4 accent-emerald-400"
                    />
                  ) : f.kind === "select" ? (
                    <select
                      value={String(values[f.key] ?? "")}
                      onChange={(e) => setValues((prev) => ({ ...prev, [f.key]: e.target.value }))}
                      className="w-full rounded-md border border-zinc-700 bg-zinc-950 px-2 py-2 text-sm outline-none focus:border-emerald-400"
                    >
                      {f.options.map((o) => (
                        <option key={o} value={o}>
                          {o}
                        </option>
                      ))}
                    </select>
                  ) : (
                    <input
                      type={f.kind === "number" ? "number" : "text"}
                      value={String(values[f.key] ?? "")}
                      placeholder={f.placeholder}
                      onChange={(e) => setValues((prev) => ({ ...prev, [f.key]: e.target.value }))}
                      className="w-full rounded-md border border-zinc-700 bg-zinc-950 px-2 py-2 font-mono text-xs outline-none focus:border-emerald-400"
                    />
                  )}
                </label>
              ))}
            </div>
          )}

          {tool.id !== "explorer-doctor" && tool.id !== "explorer-export" && (
            <label className="block max-w-xs text-xs">
              <span className="mb-1.5 block uppercase tracking-wider text-zinc-500">timeout (s)</span>
              <input
                value={timeoutSecs}
                onChange={(e) => setTimeoutSecs(e.target.value)}
                placeholder="optional"
                className="w-full rounded-md border border-zinc-700 bg-zinc-950 px-2 py-2 text-sm outline-none focus:border-emerald-400"
              />
            </label>
          )}

          <div className="flex flex-wrap gap-2">
            <button
              type="button"
              disabled={starting || doctorLoading}
              onClick={() => void runJob()}
              className="rounded-md bg-emerald-400 px-4 py-2 text-sm font-semibold text-black hover:bg-emerald-300 disabled:opacity-50"
            >
              {doctorLoading || starting
                ? "Working…"
                : tool.id === "explorer-export"
                  ? "Download export"
                  : tool.id === "explorer-doctor"
                    ? "Run doctor"
                    : "Launch job"}
            </button>
          </div>

          {doctor && (
            <div className="overflow-x-auto rounded-lg border border-zinc-800">
              <table className="w-full text-left text-xs">
                <thead className="bg-zinc-950 text-zinc-500">
                  <tr>
                    <th className="px-3 py-2">provider</th>
                    <th className="px-3 py-2">latest</th>
                    <th className="px-3 py-2">archive</th>
                    <th className="px-3 py-2">bulk</th>
                    <th className="px-3 py-2">traces</th>
                    <th className="px-3 py-2">rps</th>
                  </tr>
                </thead>
                <tbody>
                  {doctor.providers.map((p) => (
                    <tr key={p.url_shown} className="border-t border-zinc-800/80 text-zinc-300">
                      <td className="px-3 py-2 font-mono">{p.url_shown}</td>
                      <td className="px-3 py-2">{p.latest}</td>
                      <td className="px-3 py-2">{p.archive}</td>
                      <td className="px-3 py-2">{p.bulk_receipts}</td>
                      <td className="px-3 py-2">{p.traces}</td>
                      <td className="px-3 py-2">{p.rps ?? "—"}</td>
                    </tr>
                  ))}
                </tbody>
              </table>
              <div className={`border-t border-zinc-800 px-3 py-2 text-sm ${doctor.gate_ok ? "text-emerald-400" : "text-rose-400"}`}>
                Gate: {doctor.gate_ok ? "PASS" : "FAIL"} · {doctor.chain} ({doctor.chain_id})
              </div>
            </div>
          )}

          {selected && (
            <div className="space-y-2">
              <div className="flex items-center justify-between text-xs text-zinc-500">
                <span className="font-mono text-zinc-300">{selected.job_id}</span>
                <span>
                  {selected.command} {selected.args.join(" ")}
                </span>
              </div>
              <LogViewer jobId={selected.job_id} />
            </div>
          )}
        </section>
      </div>
    </div>
  );
}
