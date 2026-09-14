import type { ProgressResponse } from "../api";

const STAGES = ["resolve", "fetch", "pool_init", "detect", "complete"] as const;
type Stage = (typeof STAGES)[number];

const STAGE_LABEL: Record<Stage, string> = {
  resolve: "Resolve",
  fetch: "Fetch",
  pool_init: "Pool init",
  detect: "Detect",
  complete: "Done",
};

function stageIndex(stage: string): number {
  const i = STAGES.indexOf(stage as Stage);
  return i < 0 ? 0 : i;
}

interface Props {
  progress: ProgressResponse | null;
  running: boolean;
}

export default function StageTimeline({ progress, running }: Props) {
  const i = progress ? stageIndex(progress.stage) : 0;
  let pct: number | null = null;
  if (progress?.pct !== undefined && progress.pct !== null) {
    pct = progress.pct;
  } else if (progress && progress.done !== undefined && progress.total && progress.total > 0) {
    pct = (progress.done / progress.total) * 100;
  }

  const completed = !running && progress?.stage === "complete";

  return (
    <div className="rounded-xl border border-zinc-800 bg-zinc-900/60 p-4">
      <div className="flex flex-wrap items-center gap-1.5">
        {STAGES.map((s, idx) => {
          const state = completed
            ? "done"
            : idx < i
              ? "done"
              : idx === i
                ? "active"
                : "todo";
          const cls =
            state === "done"
              ? "border-emerald-700/60 bg-emerald-950/40 text-emerald-300"
              : state === "active"
                ? "border-sky-600 bg-sky-950/50 text-sky-200 animate-pulse"
                : "border-zinc-800 bg-zinc-900 text-zinc-500";
          const detail =
            state === "active" && progress
              ? progress.done !== undefined && progress.total !== undefined
                ? ` ${progress.done}/${progress.total}`
                : ""
              : "";
          return (
            <span key={s} className={`rounded-full border px-2.5 py-1 text-xs font-medium ${cls}`}>
              {STAGE_LABEL[s]}
              {detail}
            </span>
          );
        })}
      </div>
      {pct !== null && (
        <div className="mt-3 h-1.5 w-full overflow-hidden rounded-full bg-zinc-800">
          <div
            className="h-full rounded-full bg-sky-500 transition-all duration-500"
            style={{ width: `${pct}%` }}
          />
        </div>
      )}
      {progress && progress.stage === "complete" && (
        <div className="mt-3 text-xs tabular-nums text-zinc-400">
          run {progress.run_id ?? "–"} · {progress.ops ?? 0} ops · {((progress.elapsed_ms ?? 0) / 1000).toFixed(1)}s
        </div>
      )}
    </div>
  );
}