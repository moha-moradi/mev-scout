import type { MevOpportunity, ValidationResponse } from "../api";

export function tierBadge(opp: MevOpportunity): "T1" | "T2" | "T3" | "missed" {
  // Heuristic from the CLI's tiered matching (pool_a canonical match, etc.).
  // Realized classification lives in the API validation report; this is a
  // display-only shim for candidates without a realized op.
  return opp.details && (opp.details as { tier?: string }).tier
    ? ((opp.details as { tier: string }).tier as "T1" | "T2" | "T3")
    : opp.tx_index != null
    ? "T1"
    : "missed";
}

interface Props {
  validation: ValidationResponse | null;
  candidates: MevOpportunity[];
  runId?: string;
  onValidate?: () => void;
  validating?: boolean;
}

export default function ValidationPanel({
  validation,
  candidates,
  runId,
  onValidate,
  validating,
}: Props) {
  if (!validation) {
    return (
      <div className="rounded-xl border border-dashed border-zinc-800 p-6 text-center text-sm text-zinc-500">
        <p>Run not validated yet.</p>
        {onValidate && (
          <button
            type="button"
            disabled={validating}
            onClick={onValidate}
            className="mt-3 rounded-md border border-sky-700/50 bg-sky-950/30 px-3 py-1.5 text-xs text-sky-300 hover:bg-sky-900/40 disabled:opacity-50"
          >
            {validating ? "Starting…" : "Run explorer validate"}
          </button>
        )}
      </div>
    );
  }
  const v = validation;
  const cov = validation.explorer_coverage;

  const cards = [
    { label: "T1 (realized)", value: v.tier1, color: "text-emerald-400" },
    { label: "T2 (partial)", value: v.tier2, color: "text-sky-400" },
    { label: "T3", value: v.tier3, color: "text-amber-400" },
    { label: "missed", value: v.missed, color: "text-rose-400" },
  ];

  const counts = {
    t1: candidates.filter((c) => tierBadge(c) === "T1").length,
    t2: candidates.filter((c) => tierBadge(c) === "T2").length,
    t3: candidates.filter((c) => tierBadge(c) === "T3").length,
    missed: candidates.filter((c) => tierBadge(c) === "missed").length,
  };

  return (
    <div className="rounded-xl border border-zinc-800 bg-zinc-900/60 p-4">
      <div className="mb-3 flex flex-wrap items-center justify-between gap-2">
        <h3 className="text-sm font-medium text-zinc-200">Validation</h3>
        <div className="flex items-center gap-2">
          {onValidate && (
            <button
              type="button"
              disabled={validating}
              onClick={onValidate}
              className="rounded-md border border-sky-700/50 bg-sky-950/30 px-2.5 py-1 text-[11px] text-sky-300 hover:bg-sky-900/40 disabled:opacity-50"
            >
              {validating ? "Starting…" : "Re-run validate"}
            </button>
          )}
          <span className="text-xs text-zinc-500">
            coverage {cov.blocks_indexed}/{cov.blocks_total} blocks
            {cov.covered ? " ✓" : " (partial)"}
          </span>
        </div>
      </div>
      <div className="grid grid-cols-2 gap-3 sm:grid-cols-4">
        {cards.map((c) => (
          <div key={c.label} className="rounded-lg border border-zinc-800 bg-zinc-950/60 p-3">
            <div className="text-[10px] uppercase tracking-wider text-zinc-500">{c.label}</div>
            <div className={`mt-1 text-xl font-semibold tabular-nums ${c.color}`}>
              {c.value}
              <span className="ml-1.5 text-xs font-normal text-zinc-500">
                {counts[c.label.toLowerCase() as keyof typeof counts]}/{candidates.length}
              </span>
            </div>
          </div>
        ))}
      </div>
      {!cov.covered && runId && (
        <p className="mt-3 text-xs text-amber-300/80">
          Explorer does not fully cover this range — index it to get full recall attribution.
        </p>
      )}
    </div>
  );
}
