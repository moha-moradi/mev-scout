interface Props {
  label: string;
  value: string | number;
  delta?: string;
  sub?: string;
  accent?: "default" | "good" | "bad";
}

export default function StatCard({ label, value, delta, sub, accent = "default" }: Props) {
  const accentClass =
    accent === "good" ? "text-emerald-400" : accent === "bad" ? "text-rose-400" : "text-zinc-100";
  return (
    <div className="rounded-[var(--radius-card)] border border-zinc-800 bg-zinc-900/60 p-4">
      <div className="text-[10px] uppercase tracking-wider text-zinc-500">{label}</div>
      <div className={`mt-1.5 font-mono text-2xl font-semibold tabular-nums ${accentClass}`}>{value}</div>
      {sub !== undefined && <div className="mt-0.5 text-xs text-zinc-500">{sub}</div>}
      {delta !== undefined && (
        <div
          className={`mt-1 font-mono text-xs tabular-nums ${
            delta.startsWith("-") ? "text-rose-400" : "text-emerald-400"
          }`}
        >
          {delta}
        </div>
      )}
    </div>
  );
}