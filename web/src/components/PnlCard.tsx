import type { PnlResponse } from "../api";

export function formatUsd(v: number | null): string {
  if (v === null) return "—";
  return `$${v.toLocaleString(undefined, { maximumFractionDigits: 2 })}`;
}

function shortToken(token: string): string {
  if (token.length <= 14) return token;
  return `${token.slice(0, 6)}…${token.slice(-4)}`;
}

export default function PnlCard({ pnl }: { pnl: PnlResponse | null }) {
  if (!pnl) {
    return (
      <div className="rounded-xl border border-dashed border-zinc-800 p-6 text-center text-sm text-zinc-500">
        No PnL breakdown for this run.
      </div>
    );
  }
  const { per_token, totals } = pnl;
  return (
    <div className="rounded-xl border border-zinc-800 bg-zinc-900/60 p-4">
      <div className="mb-3 flex items-center justify-between">
        <h3 className="text-sm font-medium text-zinc-200">Simulated PnL</h3>
        <span className="rounded bg-zinc-800 px-1.5 py-0.5 text-[10px] uppercase tracking-wider text-zinc-400">
          simulated
        </span>
      </div>
      {per_token.length === 0 ? (
        <p className="text-xs text-zinc-500">No profitable opportunities.</p>
      ) : (
        <div className="overflow-x-auto">
          <table className="w-full text-left text-xs">
            <thead className="border-b border-zinc-800 text-zinc-500">
              <tr>
                <th className="px-2 py-1.5">token</th>
                <th className="px-2 py-1.5 text-right">gross</th>
                <th className="px-2 py-1.5 text-right">gas</th>
                <th className="px-2 py-1.5 text-right">net</th>
                <th className="px-2 py-1.5 text-right">USD</th>
              </tr>
            </thead>
            <tbody className="divide-y divide-zinc-800/70">
              {per_token.map((t) => (
                <tr key={t.token}>
                  <td className="px-2 py-1.5 font-mono text-zinc-300" title={t.token}>
                    {shortToken(t.token)}
                  </td>
                  <td className="px-2 py-1.5 text-right tabular-nums text-zinc-300">{t.gross}</td>
                  <td className="px-2 py-1.5 text-right tabular-nums text-zinc-400">{t.gas}</td>
                  <td
                    className={`px-2 py-1.5 text-right tabular-nums ${Number(t.net) >= 0 ? "text-emerald-400" : "text-rose-400"}`}
                  >
                    {t.net}
                  </td>
                  <td
                    className={`px-2 py-1.5 text-right tabular-nums ${(t.usd ?? 0) >= 0 ? "text-emerald-400" : "text-rose-400"}`}
                  >
                    {formatUsd(t.usd)}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
      {totals.gross_usd !== null && (
        <div className="mt-3 flex items-center justify-between border-t border-zinc-800 pt-2 text-sm">
          <span className="text-zinc-500">gross USD</span>
          <span className="font-semibold tabular-nums text-emerald-400">{formatUsd(totals.gross_usd)}</span>
        </div>
      )}
    </div>
  );
}