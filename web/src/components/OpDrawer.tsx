import { useEffect, useState } from "react";
import { api, type ExplainResponse } from "../api";

interface Props {
  txHash: string | null;
  onClose: () => void;
}

function short(v: string | null | undefined, n = 16): string {
  if (!v) return "—";
  if (v.length <= n) return v;
  return `${v.slice(0, n)}…`;
}

export default function OpDrawer({ txHash, onClose }: Props) {
  const [data, setData] = useState<ExplainResponse | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);

  useEffect(() => {
    if (!txHash) return;
    setLoading(true);
    setError(null);
    setData(null);
    api
      .explorerOp(txHash)
      .then(setData)
      .catch((e: unknown) => setError(e instanceof Error ? e.message : String(e)))
      .finally(() => setLoading(false));
  }, [txHash]);

  if (!txHash) return null;

  return (
    <div className="fixed inset-0 z-40 flex justify-end bg-black/60" onClick={onClose}>
      <div
        className="h-full w-full max-w-2xl overflow-y-auto border-l border-zinc-800 bg-zinc-950 p-5"
        onClick={(e) => e.stopPropagation()}
      >
        <div className="mb-4 flex items-start justify-between gap-3">
          <div>
            <h3 className="text-sm font-medium text-zinc-200">Operation detail</h3>
            <code className="text-xs text-zinc-500">{txHash}</code>
          </div>
          <button onClick={onClose} className="text-zinc-400 hover:text-zinc-200">✕</button>
        </div>

        {loading && <p className="text-sm text-zinc-500">Loading…</p>}
        {error && <p className="text-sm text-rose-400">{error}</p>}

        {data && (
          <div className="space-y-5">
            <section>
              <h4 className="mb-2 text-xs uppercase tracking-wider text-zinc-500">
                Realized ops ({data.ops.length})
              </h4>
              {data.ops.length === 0 && <p className="text-xs text-zinc-600">None.</p>}
              {data.ops.map((op) => (
                <div key={op.id} className="mb-2 rounded-lg border border-zinc-800 bg-zinc-900/50 p-3">
                  <div className="flex items-center justify-between gap-2">
                    <span className="text-xs font-medium text-sky-300">{op.kind}</span>
                    <span className="text-xs tabular-nums text-emerald-400">
                      {op.net_profit_usd != null ? `$${op.net_profit_usd.toFixed(2)} net` : "—"}
                    </span>
                  </div>
                  <dl className="mt-2 grid grid-cols-2 gap-x-4 gap-y-1 text-xs text-zinc-400">
                    <div><dt className="text-zinc-600">block</dt><dd>{op.block_number}</dd></div>
                    <div><dt className="text-zinc-600">eoa</dt><dd>{short(op.eoa)}</dd></div>
                    <div><dt className="text-zinc-600">profit token</dt><dd>{short(op.profit_token)}</dd></div>
                    <div><dt className="text-zinc-600">confidence</dt><dd>{op.confidence}</dd></div>
                    <div><dt className="text-zinc-600">detector</dt><dd>{short(op.detector)}</dd></div>
                    <div><dt className="text-zinc-600">canonical id</dt><dd>{short(op.canonical_id, 12)}</dd></div>
                  </dl>
                </div>
              ))}
            </section>

            <section>
              <h4 className="mb-2 text-xs uppercase tracking-wider text-zinc-500">
                Rejected candidates ({data.rejected.length})
              </h4>
              {data.rejected.length === 0 && <p className="text-xs text-zinc-600">None in ±10 block window.</p>}
              {data.rejected.map((r, idx) => (
                <div key={idx} className="mb-2 rounded-lg border border-zinc-800 bg-zinc-900/50 p-3 text-xs">
                  <div className="flex items-center justify-between gap-2">
                    <span className="font-medium text-zinc-300">{r.strategy}</span>
                    <span className="text-rose-300">{r.reject_reason}</span>
                  </div>
                  <p className="mt-1 text-zinc-500">
                    block {r.block_number} · expected {r.expected_profit} wei · gas {r.gas_cost_wei} wei
                  </p>
                  {r.detail && <pre className="mt-2 overflow-x-auto rounded bg-black/40 p-2 text-[11px] text-zinc-400">{r.detail}</pre>}
                </div>
              ))}
            </section>
          </div>
        )}
      </div>
    </div>
  );
}