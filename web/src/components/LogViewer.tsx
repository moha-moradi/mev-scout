import { useEffect, useRef, useState } from "react";
import { api } from "../api";
import { usePolling } from "../hooks";

interface Props {
  jobId: string;
  intervalMs?: number;
  heightClass?: string;
}

export default function LogViewer({ jobId, intervalMs = 2000, heightClass = "h-64" }: Props) {
  const { data: lines } = usePolling<string[]>(
    () => api.jobLog(jobId, 500),
    intervalMs,
    [jobId],
  );
  const [autoScroll, setAutoScroll] = useState(true);
  const ref = useRef<HTMLPreElement>(null);

  useEffect(() => {
    if (autoScroll && ref.current) {
      ref.current.scrollTop = ref.current.scrollHeight;
    }
  }, [lines, autoScroll]);

  return (
    <div className="rounded-xl border border-zinc-800 bg-black/50">
      <div className="flex items-center justify-between border-b border-zinc-800 px-3 py-2">
        <span className="text-xs uppercase tracking-wider text-zinc-500">job log</span>
        <label className="flex items-center gap-1.5 text-xs text-zinc-400">
          <input
            type="checkbox"
            checked={autoScroll}
            onChange={(e) => setAutoScroll(e.target.checked)}
            className="accent-sky-600"
          />
          auto-scroll
        </label>
      </div>
      <pre
        ref={ref}
        onScroll={(e) => {
          const el = e.currentTarget;
          setAutoScroll(el.scrollHeight - el.scrollTop - el.clientHeight < 40);
        }}
        className={`${heightClass} overflow-auto whitespace-pre-wrap px-3 py-2 font-mono text-xs leading-5 text-zinc-300`}
      >
        {(lines ?? []).join("\n") || "Waiting for output…"}
      </pre>
    </div>
  );
}