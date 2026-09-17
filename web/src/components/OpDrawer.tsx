import { useEffect, useMemo, useState } from "react";
import { api, type ExplainResponse, type MevOpRow } from "../api";
import Copyable from "./Copyable";
import Identicon from "./Identicon";
import RoutePath from "./RoutePath";
import {
  addressExplorerUrl,
  formatUsd,
  formatUsdSigned,
  parseRoute,
  relativeTime,
  shortHex,
  txExplorerUrl,
} from "../lib/format";

interface Props {
  txHash: string | null;
  chain?: string;
  onClose: () => void;
}

function Metric({
  label,
  value,
  accent,
  sub,
}: {
  label: string;
  value: string;
  accent?: "good" | "bad" | "muted";
  sub?: string;
}) {
  const color =
    accent === "good"
      ? "text-emerald-400"
      : accent === "bad"
        ? "text-rose-400"
        : accent === "muted"
          ? "text-zinc-400"
          : "text-zinc-100";
  return (
    <div className="rounded-lg border border-zinc-800 bg-zinc-900/70 px-3 py-2.5">
      <div className="text-[10px] uppercase tracking-wider text-zinc-500">{label}</div>
      <div className={`mt-1 font-mono text-lg font-semibold tabular-nums ${color}`}>{value}</div>
      {sub && <div className="mt-0.5 text-[11px] text-zinc-600">{sub}</div>}
    </div>
  );
}

function profitability(op: MevOpRow): {
  label: string;
  accent: "good" | "bad" | "muted";
  margin: string | null;
} {
  const net = op.net_profit_usd;
  const gross = op.profit_usd;
  const gas = op.gas_cost_usd;
  if (net == null) return { label: "Unknown", accent: "muted", margin: null };
  const margin =
    gross != null && gross !== 0
      ? `${(((net ?? 0) / Math.abs(gross)) * 100).toFixed(1)}% of gross`
      : gas != null && gas > 0
        ? `${((net / gas) * 100).toFixed(0)}% over gas`
        : null;
  if (net > 0) return { label: "Profitable", accent: "good", margin };
  if (net < 0) return { label: "Unprofitable", accent: "bad", margin };
  return { label: "Break-even", accent: "muted", margin };
}

function victims(raw: string | null): string[] {
  if (!raw) return [];
  try {
    const parsed = JSON.parse(raw) as unknown;
    if (Array.isArray(parsed)) return parsed.map(String);
  } catch {
    return raw
      .split(/[,\s]+/)
      .map((s) => s.trim())
      .filter(Boolean);
  }
  return [];
}

export default function OpDrawer({ txHash, chain, onClose }: Props) {
  const [data, setData] = useState<ExplainResponse | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const [tracing, setTracing] = useState(false);

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

  async function runTrace() {
    if (!txHash) return;
    setTracing(true);
    setError(null);
    try {
      const res = await api.explorerOp(txHash, true);
      setData(res);
    } catch (e: unknown) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setTracing(false);
    }
  }

  const primary = data?.ops[0];
  const status = useMemo(() => (primary ? profitability(primary) : null), [primary]);
  const hops = useMemo(() => parseRoute(primary?.route_json), [primary]);
  const victimList = useMemo(() => victims(primary?.victim_hashes ?? null), [primary]);
  const explorerTx = txHash ? txExplorerUrl(chain, txHash) : null;

  if (!txHash) return null;

  return (
    <div className="fixed inset-0 z-40 flex justify-end bg-black/60 backdrop-blur-[2px]" onClick={onClose}>
      <div
        className="flex h-full w-full max-w-xl flex-col border-l border-zinc-800 bg-zinc-950 shadow-2xl shadow-black/50"
        onClick={(e) => e.stopPropagation()}
      >
        <div className="flex items-start justify-between gap-3 border-b border-zinc-800 px-5 py-4">
          <div className="min-w-0">
            <div className="flex items-center gap-2">
              <h3 className="text-sm font-semibold text-zinc-100">Transaction detail</h3>
              {status && (
                <span
                  className={`rounded-full border px-2 py-0.5 text-[10px] uppercase tracking-wider ${
                    status.accent === "good"
                      ? "border-emerald-700/60 bg-emerald-950/50 text-emerald-300"
                      : status.accent === "bad"
                        ? "border-rose-700/60 bg-rose-950/50 text-rose-300"
                        : "border-zinc-700 bg-zinc-900 text-zinc-400"
                  }`}
                >
                  {status.label}
                </span>
              )}
            </div>
            <div className="mt-1 flex flex-wrap items-center gap-2 text-xs text-zinc-500">
              <Copyable value={txHash} className="text-zinc-400" head={8} tail={6} />
              {explorerTx && (
                <a
                  href={explorerTx}
                  target="_blank"
                  rel="noreferrer"
                  className="text-emerald-400/80 hover:text-emerald-300"
                >
                  Open explorer ↗
                </a>
              )}
            </div>
          </div>
          <div className="flex shrink-0 items-center gap-2">
            <button
              type="button"
              disabled={tracing || loading}
              onClick={() => void runTrace()}
              className="rounded-md border border-sky-700/50 bg-sky-950/40 px-2.5 py-1 text-[11px] text-sky-300 hover:bg-sky-900/40 disabled:opacity-50"
            >
              {tracing ? "Tracing…" : data?.trace ? "Re-trace" : "Trace"}
            </button>
            <button
              onClick={onClose}
              className="rounded-md border border-zinc-800 px-2 py-1 text-zinc-400 hover:border-zinc-700 hover:text-zinc-200"
            >
              ✕
            </button>
          </div>
        </div>

        <div className="flex-1 overflow-y-auto px-5 py-4">
          {loading && <p className="text-sm text-zinc-500">Loading…</p>}
          {error && <p className="text-sm text-rose-400">{error}</p>}

          {data && primary && (
            <div className="space-y-6">
              {data.trace && (
                <section className="rounded-xl border border-sky-900/50 bg-sky-950/20 p-4">
                  <h4 className="mb-2 text-[10px] uppercase tracking-wider text-sky-400">
                    Trace verification
                  </h4>
                  <pre className="whitespace-pre-wrap font-mono text-[11px] text-zinc-300">{data.trace}</pre>
                </section>
              )}
              <div className="grid grid-cols-3 gap-2">
                <Metric
                  label="Revenue"
                  value={formatUsd(primary.profit_usd)}
                  sub="gross profit"
                />
                <Metric
                  label="Spent"
                  value={formatUsd(primary.gas_cost_usd)}
                  accent="muted"
                  sub="gas cost"
                />
                <Metric
                  label="Net profit"
                  value={formatUsdSigned(primary.net_profit_usd)}
                  accent={status?.accent === "good" ? "good" : status?.accent === "bad" ? "bad" : "muted"}
                  sub={status?.margin ?? undefined}
                />
              </div>

              <section className="rounded-xl border border-zinc-800 bg-zinc-900/40 p-4">
                <h4 className="mb-3 text-[10px] uppercase tracking-wider text-zinc-500">Economics</h4>
                <dl className="grid grid-cols-2 gap-x-4 gap-y-3 text-xs">
                  <div>
                    <dt className="text-zinc-600">Profit token</dt>
                    <dd className="mt-0.5">
                      {primary.profit_token ? (
                        <Copyable value={primary.profit_token} className="text-zinc-300" />
                      ) : (
                        "—"
                      )}
                    </dd>
                  </div>
                  <div>
                    <dt className="text-zinc-600">Profit amount</dt>
                    <dd className="mt-0.5 font-mono tabular-nums text-zinc-300">
                      {primary.profit_amount ?? "—"}
                    </dd>
                  </div>
                  <div>
                    <dt className="text-zinc-600">Gas / gross</dt>
                    <dd className="mt-0.5 font-mono tabular-nums text-zinc-300">
                      {primary.gas_cost_usd != null &&
                      primary.profit_usd != null &&
                      primary.profit_usd !== 0
                        ? `${((primary.gas_cost_usd / Math.abs(primary.profit_usd)) * 100).toFixed(1)}%`
                        : "—"}
                    </dd>
                  </div>
                  <div>
                    <dt className="text-zinc-600">Confidence</dt>
                    <dd className="mt-0.5 capitalize text-zinc-300">{primary.confidence}</dd>
                  </div>
                </dl>
              </section>

              <section>
                <h4 className="mb-2 text-[10px] uppercase tracking-wider text-zinc-500">Route</h4>
                <div className="rounded-xl border border-zinc-800 bg-zinc-900/40 p-4">
                  <RoutePath hops={hops} compact={false} />
                  {hops.length > 0 && (
                    <ul className="mt-3 space-y-2 border-t border-zinc-800/80 pt-3">
                      {hops.map((h, i) => (
                        <li key={i} className="flex flex-wrap items-center gap-2 text-[11px] text-zinc-400">
                          <span className="rounded border border-zinc-800 bg-zinc-950 px-1.5 py-0.5 font-mono text-zinc-300">
                            {h.amm ?? `hop ${i + 1}`}
                          </span>
                          {h.token_in && (
                            <span className="font-mono">{shortHex(h.token_in, 4, 4)}</span>
                          )}
                          <span className="text-zinc-600">→</span>
                          {h.token_out && (
                            <span className="font-mono">{shortHex(h.token_out, 4, 4)}</span>
                          )}
                          {(h.amount_in || h.amount_out) && (
                            <span className="ml-auto tabular-nums text-zinc-500">
                              {h.amount_in ? `in ${h.amount_in}` : ""}
                              {h.amount_in && h.amount_out ? " · " : ""}
                              {h.amount_out ? `out ${h.amount_out}` : ""}
                            </span>
                          )}
                        </li>
                      ))}
                    </ul>
                  )}
                  {hops.length === 0 && <p className="text-xs text-zinc-600">No route data stored.</p>}
                </div>
              </section>

              <section className="rounded-xl border border-zinc-800 bg-zinc-900/40 p-4">
                <h4 className="mb-3 text-[10px] uppercase tracking-wider text-zinc-500">Context</h4>
                <dl className="grid grid-cols-2 gap-x-4 gap-y-3 text-xs">
                  <div>
                    <dt className="text-zinc-600">Kind</dt>
                    <dd className="mt-0.5 text-sky-300">{primary.kind}</dd>
                  </div>
                  <div>
                    <dt className="text-zinc-600">Block</dt>
                    <dd className="mt-0.5 font-mono tabular-nums text-zinc-300">
                      {primary.block_number.toLocaleString()}
                      {primary.tx_index != null ? ` · #${primary.tx_index}` : ""}
                    </dd>
                  </div>
                  <div>
                    <dt className="text-zinc-600">Time</dt>
                    <dd className="mt-0.5 text-zinc-300">{relativeTime(primary.ts)}</dd>
                  </div>
                  <div>
                    <dt className="text-zinc-600">Detector</dt>
                    <dd className="mt-0.5 text-zinc-300">{primary.detector ?? "—"}</dd>
                  </div>
                  <div className="col-span-2">
                    <dt className="text-zinc-600">Sender</dt>
                    <dd className="mt-1 flex items-center gap-2">
                      <Identicon seed={primary.eoa} />
                      <Copyable value={primary.eoa} className="text-zinc-300" />
                      {addressExplorerUrl(chain, primary.eoa) && (
                        <a
                          href={addressExplorerUrl(chain, primary.eoa)!}
                          target="_blank"
                          rel="noreferrer"
                          className="text-[11px] text-emerald-400/80 hover:text-emerald-300"
                        >
                          ↗
                        </a>
                      )}
                    </dd>
                  </div>
                  {primary.contract && (
                    <div className="col-span-2">
                      <dt className="text-zinc-600">Contract</dt>
                      <dd className="mt-0.5">
                        <Copyable value={primary.contract} className="text-zinc-300" />
                      </dd>
                    </div>
                  )}
                  {primary.canonical_id && (
                    <div className="col-span-2">
                      <dt className="text-zinc-600">Canonical id</dt>
                      <dd className="mt-0.5">
                        <Copyable value={primary.canonical_id} className="text-zinc-400" head={10} tail={6} />
                      </dd>
                    </div>
                  )}
                </dl>
              </section>

              {victimList.length > 0 && (
                <section>
                  <h4 className="mb-2 text-[10px] uppercase tracking-wider text-zinc-500">
                    Victims ({victimList.length})
                  </h4>
                  <ul className="space-y-1.5">
                    {victimList.map((v) => (
                      <li
                        key={v}
                        className="rounded-lg border border-zinc-800 bg-zinc-900/40 px-3 py-2 text-xs"
                      >
                        <Copyable value={v} className="text-zinc-300" head={8} tail={6} />
                      </li>
                    ))}
                  </ul>
                </section>
              )}

              {data.ops.length > 1 && (
                <section>
                  <h4 className="mb-2 text-[10px] uppercase tracking-wider text-zinc-500">
                    Related ops ({data.ops.length})
                  </h4>
                  {data.ops.map((op) => (
                    <div key={op.id} className="mb-2 rounded-lg border border-zinc-800 bg-zinc-900/50 p-3">
                      <div className="flex items-center justify-between gap-2">
                        <span className="text-xs font-medium text-sky-300">{op.kind}</span>
                        <span
                          className={`text-xs tabular-nums ${
                            (op.net_profit_usd ?? 0) >= 0 ? "text-emerald-400" : "text-rose-400"
                          }`}
                        >
                          {formatUsdSigned(op.net_profit_usd)}
                        </span>
                      </div>
                      <p className="mt-1 text-[11px] text-zinc-500">
                        block {op.block_number} · gas {formatUsd(op.gas_cost_usd)} · gross{" "}
                        {formatUsd(op.profit_usd)}
                      </p>
                    </div>
                  ))}
                </section>
              )}

              <section>
                <h4 className="mb-2 text-[10px] uppercase tracking-wider text-zinc-500">
                  Rejected candidates ({data.rejected.length})
                </h4>
                {data.rejected.length === 0 && (
                  <p className="text-xs text-zinc-600">None in ±10 block window.</p>
                )}
                {data.rejected.map((r, idx) => (
                  <div key={idx} className="mb-2 rounded-lg border border-zinc-800 bg-zinc-900/50 p-3 text-xs">
                    <div className="flex items-center justify-between gap-2">
                      <span className="font-medium text-zinc-300">{r.strategy}</span>
                      <span className="rounded border border-rose-900/50 bg-rose-950/40 px-1.5 py-0.5 text-rose-300">
                        {r.reject_reason}
                      </span>
                    </div>
                    <p className="mt-1.5 text-zinc-500">
                      block {r.block_number} · expected {r.expected_profit} wei · gas {r.gas_cost_wei} wei
                    </p>
                    {r.detail && (
                      <pre className="mt-2 overflow-x-auto rounded bg-black/40 p-2 text-[11px] text-zinc-400">
                        {r.detail}
                      </pre>
                    )}
                  </div>
                ))}
              </section>

              {primary.details_json && (
                <section>
                  <h4 className="mb-2 text-[10px] uppercase tracking-wider text-zinc-500">Raw details</h4>
                  <pre className="overflow-x-auto rounded-lg border border-zinc-800 bg-black/40 p-3 font-mono text-[11px] text-zinc-400">
                    {(() => {
                      try {
                        return JSON.stringify(JSON.parse(primary.details_json), null, 2);
                      } catch {
                        return primary.details_json;
                      }
                    })()}
                  </pre>
                </section>
              )}
            </div>
          )}

          {data && !primary && !loading && (
            <div className="space-y-4">
              <p className="text-sm text-zinc-400">No realized ops for this transaction.</p>
              {data.rejected.length > 0 && (
                <section>
                  <h4 className="mb-2 text-[10px] uppercase tracking-wider text-zinc-500">
                    Rejected candidates ({data.rejected.length})
                  </h4>
                  {data.rejected.map((r, idx) => (
                    <div key={idx} className="mb-2 rounded-lg border border-zinc-800 bg-zinc-900/50 p-3 text-xs">
                      <div className="flex justify-between gap-2">
                        <span className="text-zinc-300">{r.strategy}</span>
                        <span className="text-rose-300">{r.reject_reason}</span>
                      </div>
                    </div>
                  ))}
                </section>
              )}
            </div>
          )}
        </div>
      </div>
    </div>
  );
}
