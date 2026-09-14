import { useEffect, useRef, useState } from "react";
import { api, type JobInfo, type ProgressResponse, type PnlResponse } from "../api";
import { usePolling } from "../hooks";
import { useToast } from "../components/Toast";
import LogViewer from "../components/LogViewer";
import StageTimeline from "../components/StageTimeline";
import PnlCard from "../components/PnlCard";

export default function Live() {
  const toast = useToast();
  const [loop, setLoop] = useState(true);
  const [duration, setDuration] = useState("2m");
  const [maxBlocks, setMaxBlocks] = useState("");
  const [pollInterval, setPollInterval] = useState("2000");
  const [recordRejections, setRecordRejections] = useState(false);

  const [jobId, setJobId] = useState<string | null>(null);
  const [starting, setStarting] = useState(false);

  const { data: job } = usePolling<JobInfo | null>(
    () => (jobId ? api.job(jobId) : Promise.resolve(null)),
    jobId ? 2000 : 60_000,
    [jobId],
  );
  const { data: progress } = usePolling<ProgressResponse | null>(
    () => (jobId ? api.jobProgress(jobId) : Promise.resolve(null)),
    jobId ? 2000 : 60_000,
    [jobId],
  );

  const [pnl, setPnl] = useState<PnlResponse | null>(null);
  const lastRunId = useRef<string | null>(null);
  if (progress?.stage === "complete" && progress.run_id) lastRunId.current = progress.run_id;

  const jobRunning = job?.status === "running";

  useEffect(() => {
    if (!jobRunning || !lastRunId.current) return;
    const id = setInterval(() => {
      api.resultPnl(lastRunId.current!).then(setPnl).catch(() => undefined);
    }, 3000);
    return () => clearInterval(id);
  }, [jobRunning]);

  function buildArgs(): string[] {
    const args: string[] = [];
    if (loop) args.push("--loop");
    if (loop && duration.trim()) args.push("--duration", duration.trim());
    if (loop && maxBlocks.trim()) args.push("--max-blocks", maxBlocks.trim());
    if (!loop && pollInterval.trim()) args.push("--poll-interval", pollInterval.trim());
    if (recordRejections) args.push("--record-rejections");
    args.push("--progress", "json");
    return args;
  }

  async function start() {
    setStarting(true);
    lastRunId.current = null;
    setPnl(null);
    try {
      const res = await api.createJob("live", buildArgs());
      setJobId(res.job_id);
      toast(`Live job ${res.job_id.slice(0, 8)} started.`, "success");
    } catch (e) {
      toast(`Failed to start live: ${e instanceof Error ? e.message : String(e)}`, "error");
    } finally {
      setStarting(false);
    }
  }

  function stop() {
    if (jobId) void api.stopJob(jobId).catch(() => undefined);
  }

  return (
    <div className="space-y-6">
      <div className="flex items-center justify-between">
        <h1 className="text-xl font-semibold text-zinc-100">Live Monitor</h1>
        {jobRunning && (
          <button
            onClick={stop}
            className="rounded-md border border-rose-700 bg-rose-950/50 px-3 py-1.5 text-sm text-rose-300 hover:bg-rose-900/50"
          >
            Stop
          </button>
        )}
      </div>

      <div className="rounded-xl border border-zinc-800 bg-zinc-900/60 p-4">
        <div className="grid gap-4 sm:grid-cols-2 lg:grid-cols-4">
          <label className="flex items-center gap-2 text-sm text-zinc-300">
            <input
              type="checkbox"
              checked={loop}
              onChange={(e) => setLoop(e.target.checked)}
              className="accent-sky-600"
            />
            --loop
          </label>
          <div>
            <label className="mb-1.5 block text-xs uppercase tracking-wider text-zinc-500">--duration</label>
            <input
              value={duration}
              disabled={!loop}
              onChange={(e) => setDuration(e.target.value)}
              placeholder={loop ? "e.g. 90s, 15m, 1h" : "requires --loop"}
              className="w-full rounded-md border border-zinc-700 bg-zinc-900 px-2 py-2 text-sm text-zinc-100 outline-none focus:border-sky-600 disabled:opacity-50"
            />
          </div>
          <div>
            <label className="mb-1.5 block text-xs uppercase tracking-wider text-zinc-500">--max-blocks</label>
            <input
              value={maxBlocks}
              disabled={!loop}
              onChange={(e) => setMaxBlocks(e.target.value)}
              placeholder={loop ? "stop after N blocks" : "requires --loop"}
              className="w-full rounded-md border border-zinc-700 bg-zinc-900 px-2 py-2 text-sm text-zinc-100 outline-none focus:border-sky-600 disabled:opacity-50"
            />
          </div>
          <div>
            <label className="mb-1.5 block text-xs uppercase tracking-wider text-zinc-500">poll interval ms</label>
            <input
              type="number"
              value={pollInterval}
              onChange={(e) => setPollInterval(e.target.value)}
              className="w-full rounded-md border border-zinc-700 bg-zinc-900 px-2 py-2 text-sm text-zinc-100 outline-none focus:border-sky-600"
            />
          </div>
        </div>

        <div className="mt-3 flex flex-wrap items-center gap-x-8 gap-y-3">
          <label className="flex items-center gap-2 text-sm text-zinc-300">
            <input
              type="checkbox"
              checked={recordRejections}
              onChange={(e) => setRecordRejections(e.target.checked)}
              className="accent-sky-600"
            />
            record rejections
          </label>
          <button
            onClick={start}
            disabled={starting || jobRunning}
            className="rounded-md bg-sky-700 px-4 py-2 text-sm font-medium text-white hover:bg-sky-600 disabled:cursor-not-allowed disabled:opacity-50"
          >
            {jobRunning ? "Running…" : starting ? "Starting…" : "Start live"}
          </button>
          {jobRunning && (
            <span className="flex items-center gap-1.5 text-xs text-zinc-400">
              <span className="h-1.5 w-1.5 animate-pulse rounded-full bg-amber-400" />
              job {jobId?.slice(0, 8)}
            </span>
          )}
        </div>
      </div>

      {jobId && (
        <div className="space-y-4">
          <StageTimeline progress={progress ?? null} running={jobRunning} />
          <PnlCard pnl={pnl} />
          <LogViewer jobId={jobId} />
        </div>
      )}
    </div>
  );
}