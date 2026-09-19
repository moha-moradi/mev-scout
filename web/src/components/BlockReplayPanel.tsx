import { useState } from "react";
import { Link } from "react-router-dom";
import { api, type FeedRow, type JobInfo } from "../api";
import { usePolling } from "../hooks";
import LogViewer from "./LogViewer";
import { useToast } from "./Toast";
import { shortHex } from "../lib/format";

interface Props {
  block: number;
  feedRows: FeedRow[];
  onClose: () => void;
  onOpenTx: (txHash: string) => void;
}

export default function BlockReplayPanel({ block, feedRows, onClose, onOpenTx }: Props) {
  const toast = useToast();
  const [analyze, setAnalyze] = useState(true);
  const [txIndex, setTxIndex] = useState("");
  const [starting, setStarting] = useState(false);
  const [jobId, setJobId] = useState<string | null>(null);

  const opsInBlock = feedRows.filter((r) => r.block_number === block);
  const { data: job } = usePolling<JobInfo | null>(
    () => (jobId ? api.job(jobId) : Promise.resolve(null)),
    jobId ? 2000 : 60_000,
    [jobId],
  );

  async function replay() {
    setStarting(true);
    try {
      const args = ["--block", String(block)];
      if (txIndex.trim()) args.push("--tx-index", txIndex.trim());
      if (analyze) args.push("--analyze");
      const res = await api.createJob("replay", args);
      setJobId(res.job_id);
      toast(`Replay job ${res.job_id.slice(0, 8)} started.`, "success");
    } catch (e) {
      toast(`Failed: ${e instanceof Error ? e.message : String(e)}`, "error");
    } finally {
      setStarting(false);
    }
  }

  return (
    <div className="fixed inset-0 z-40 flex justify-end bg-black/60 backdrop-blur-[2px]" onClick={onClose}>
      <div
        className="flex h-full w-full max-w-lg flex-col border-l border-zinc-800 bg-zinc-950 shadow-2xl shadow-black/50"
        onClick={(e) => e.stopPropagation()}
      >
        <div className="flex items-start justify-between gap-3 border-b border-zinc-800 px-5 py-4">
          <div>
            <h3 className="text-sm font-semibold text-zinc-100">Block {block.toLocaleString()}</h3>
            <p className="mt-1 text-xs text-zinc-500">
              MEV ops in feed · EVM replay for receipt verification
            </p>
          </div>
          <button
            type="button"
            onClick={onClose}
            className="rounded-md border border-zinc-800 px-2 py-1 text-zinc-400 hover:border-zinc-700 hover:text-zinc-200"
          >
            ✕
          </button>
        </div>

        <div className="flex-1 space-y-5 overflow-y-auto px-5 py-4">
          <section>
            <h4 className="mb-2 text-[10px] uppercase tracking-wider text-zinc-500">
              Ops in this block ({opsInBlock.length})
            </h4>
            {opsInBlock.length === 0 ? (
              <p className="text-xs text-zinc-600">No indexed MEV ops for this block in the current feed.</p>
            ) : (
              <ul className="divide-y divide-zinc-800/80 rounded-lg border border-zinc-800">
                {opsInBlock.map((op) => (
                  <li key={op.tx_hash}>
                    <button
                      type="button"
                      onClick={() => onOpenTx(op.tx_hash)}
                      className="flex w-full items-center justify-between gap-2 px-3 py-2 text-left text-xs hover:bg-zinc-900/60"
                    >
                      <span className="font-mono text-zinc-300">{shortHex(op.tx_hash, 6, 4)}</span>
                      <span className="text-zinc-500">{op.kind}</span>
                    </button>
                  </li>
                ))}
              </ul>
            )}
          </section>

          <section className="rounded-xl border border-zinc-800 bg-zinc-900/40 p-4">
            <h4 className="mb-3 text-sm font-medium text-zinc-200">Replay block</h4>
            <label className="mb-3 block text-xs">
              <span className="mb-1.5 block uppercase tracking-wider text-zinc-500">tx-index (optional)</span>
              <input
                type="number"
                value={txIndex}
                onChange={(e) => setTxIndex(e.target.value)}
                placeholder="all txs"
                className="w-full rounded-md border border-zinc-700 bg-zinc-950 px-2 py-2 font-mono text-xs outline-none focus:border-emerald-400"
              />
            </label>
            <label className="mb-4 flex items-center gap-2 text-sm text-zinc-300">
              <input
                type="checkbox"
                checked={analyze}
                onChange={(e) => setAnalyze(e.target.checked)}
                className="accent-emerald-400"
              />
              analyze DEX logs
            </label>
            <button
              type="button"
              disabled={starting}
              onClick={() => void replay()}
              className="rounded-md bg-emerald-400 px-3 py-1.5 text-sm font-semibold text-black hover:bg-emerald-300 disabled:opacity-50"
            >
              {starting ? "Starting…" : "Replay block"}
            </button>
            <p className="mt-2 text-[11px] text-zinc-600">
              Block must be cached (backtest/live fetch it as part of the run).{" "}
              <Link to="/jobs" className="text-sky-500 hover:underline">
                Job logs →
              </Link>
            </p>
          </section>

          {jobId && (
            <section className="space-y-2">
              <div className="flex items-center justify-between text-xs text-zinc-500">
                <span className="font-mono text-zinc-300">{jobId}</span>
                <span>{job?.status ?? "…"}</span>
              </div>
              <LogViewer jobId={jobId} heightClass="h-48" />
            </section>
          )}
        </div>
      </div>
    </div>
  );
}
