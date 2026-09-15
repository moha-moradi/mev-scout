import { useState } from "react";
import { Link } from "react-router-dom";
import { api, type JobInfo } from "../api";
import { usePolling } from "../hooks";
import DataTable, { type Column } from "../components/DataTable";
import LogViewer from "../components/LogViewer";
import { useToast } from "../components/Toast";

const COMMANDS = ["run", "live", "discover", "tokens", "scan", "report", "explorer index"];
const STATUS_CLASS: Record<string, string> = {
  running: "border-amber-700/60 bg-amber-950/40 text-amber-300",
  finished: "border-emerald-700/60 bg-emerald-950/40 text-emerald-300",
  failed: "border-rose-700/60 bg-rose-950/40 text-rose-300",
  killed: "border-zinc-700 bg-zinc-900 text-zinc-400",
};

const PRESETS: Record<string, { args: string[]; hint: string }> = {
  run: { args: ["--blocks", "10", "--progress", "json"], hint: "last 10 blocks" },
  live: { args: ["--loop", "--duration", "2m", "--progress", "json"], hint: "2 minutes" },
  discover: { args: ["--incremental"], hint: "resume from cached head" },
  tokens: { args: [], hint: "token scanner (default window)" },
  scan: { args: [], hint: "block scanner (default)" },
  report: { args: [], hint: "latest run" },
  "explorer index": { args: ["--live"], hint: "live indexing" },
};

function fmtTime(iso: string | null): string {
  return iso ? new Date(iso).toLocaleString() : "—";
}

export default function Jobs() {
  const [command, setCommand] = useState("run");
  const [args, setArgs] = useState(PRESETS.run.args.join(" "));
  const [timeoutSecs, setTimeoutSecs] = useState("");
  const [selected, setSelected] = useState<string | null>(null);
  const toast = useToast();
  const [starting, setStarting] = useState(false);

  const { data: jobs, refresh } = usePolling<JobInfo[]>(api.jobs, 4000, []);
  const selectedJob = jobs?.find((j) => j.job_id === selected) ?? null;

  const columns: Column<JobInfo>[] = [
    {
      key: "job_id",
      header: "job",
      cell: (r) => (
        <button onClick={() => setSelected(r.job_id)} className="font-mono text-xs text-sky-300 hover:underline">
          {r.job_id}
        </button>
      ),
      sortValue: (r) => r.job_id,
    },
    { key: "command", header: "command", cell: (r) => r.command, sortValue: (r) => r.command },
    {
      key: "status",
      header: "status",
      cell: (r) => (
        <span className={`rounded-full border px-2 py-0.5 text-[11px] ${STATUS_CLASS[r.status] ?? STATUS_CLASS.killed}`}>
          {r.status}
        </span>
      ),
      sortValue: (r) => r.status,
    },
    { key: "run_id", header: "run id", cell: (r) => <span className="font-mono text-xs text-zinc-400">{r.run_id ?? "—"}</span> },
    { key: "created_at", header: "created", cell: (r) => <span className="tabular-nums text-zinc-400">{fmtTime(r.created_at)}</span>, sortValue: (r) => r.created_at },
    { key: "finished_at", header: "finished", cell: (r) => <span className="tabular-nums text-zinc-500">{fmtTime(r.finished_at)}</span>, sortValue: (r) => r.finished_at ?? "" },
    { key: "exit_code", header: "exit", align: "right", cell: (r) => <span className="tabular-nums text-zinc-400">{r.exit_code ?? "—"}</span> },
    {
      key: "actions",
      header: "",
      align: "right",
      cell: (r) =>
        r.status === "running" ? (
          <button
            onClick={(e) => {
              e.stopPropagation();
              void api.stopJob(r.job_id).then(refresh).catch(() => undefined);
            }}
            className="rounded border border-rose-700 bg-rose-950/40 px-2 py-0.5 text-[11px] text-rose-300 hover:bg-rose-900/40"
          >
            stop
          </button>
        ) : null,
    },
  ];

  async function create() {
    setStarting(true);
    try {
      const parsed = args.split(/\s+/).filter(Boolean);
      const res = await api.createJob(
        command,
        parsed,
        timeoutSecs ? Number(timeoutSecs) : undefined,
      );
      toast(`Job ${res.job_id.slice(0, 8)} started.`, "success");
      setSelected(res.job_id);
      refresh();
    } catch (e) {
      toast(`Failed: ${e instanceof Error ? e.message : String(e)}`, "error");
    } finally {
      setStarting(false);
    }
  }

  return (
    <div className="space-y-6">
      <h1 className="text-xl font-semibold text-zinc-100">Jobs</h1>

      <div className="rounded-xl border border-zinc-800 bg-zinc-900/60 p-4">
        <h2 className="mb-3 text-sm font-medium text-zinc-200">Launch command</h2>
        <div className="grid gap-3 sm:grid-cols-2 lg:grid-cols-4">
          <div>
            <label className="mb-1.5 block text-xs uppercase tracking-wider text-zinc-500">command</label>
            <select
              value={command}
              onChange={(e) => {
                setCommand(e.target.value);
                setArgs(PRESETS[e.target.value].args.join(" "));
              }}
              className="w-full rounded-md border border-zinc-700 bg-zinc-900 px-2 py-2 text-sm outline-none focus:border-emerald-400"
            >
              {COMMANDS.map((c) => (
                <option key={c} value={c}>{c}</option>
              ))}
            </select>
          </div>
          <div className="lg:col-span-2">
            <label className="mb-1.5 block text-xs uppercase tracking-wider text-zinc-500">args</label>
            <input
              value={args}
              onChange={(e) => setArgs(e.target.value)}
              placeholder="--blocks 10 --progress json"
              className="w-full rounded-md border border-zinc-700 bg-zinc-900 px-2 py-2 font-mono text-xs outline-none focus:border-emerald-400"
            />
          </div>
          <div>
            <label className="mb-1.5 block text-xs uppercase tracking-wider text-zinc-500">timeout (s)</label>
            <input
              value={timeoutSecs}
              onChange={(e) => setTimeoutSecs(e.target.value)}
              placeholder="optional"
              className="w-full rounded-md border border-zinc-700 bg-zinc-900 px-2 py-2 text-sm outline-none focus:border-emerald-400"
            />
          </div>
        </div>
        <p className="mt-2 text-xs text-zinc-600">Preset args: {PRESETS[command].hint}</p>
        <button
          onClick={create}
          disabled={starting}
          className="mt-3 rounded-md bg-emerald-400 px-4 py-2 text-sm font-semibold text-black hover:bg-emerald-300 disabled:opacity-50"
        >
          {starting ? "Starting…" : "Create job"}
        </button>
      </div>

      <DataTable<JobInfo>
        columns={columns}
        rows={jobs ?? []}
        rowKey={(r) => r.job_id}
        empty="No jobs yet."
        action={
          <Link
            to="/run"
            className="inline-block rounded-md bg-emerald-400 px-3 py-1.5 text-sm font-semibold text-black hover:bg-emerald-300"
          >
            Run a backtest
          </Link>
        }
      />

      {selectedJob && (
        <div className="space-y-3">
          <div className="flex items-center justify-between">
            <h2 className="font-mono text-sm text-zinc-200">{selectedJob.job_id}</h2>
            <span className="text-xs text-zinc-500">
              {selectedJob.command} {selectedJob.args.join(" ")}
            </span>
          </div>
          <LogViewer jobId={selectedJob.job_id} />
        </div>
      )}
    </div>
  );
}