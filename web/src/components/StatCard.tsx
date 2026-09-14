interface Props {
  label: string;
  value: string | number;
  delta?: string;
  accent?: "default" | "good" | "bad";
}

export default function StatCard({ label, value, delta, accent = "default" }: Props) {
  const accentClass =
    accent === "good" ? "text-emerald-400" : accent === "bad" ? "text-rose-400" : "text-zinc-100";
  return (
    <div className="rounded-xl border border-zinc-800 bg-zinc-900/60 p-4">
      <div className="text-xs uppercase tracking-wider text-zinc-500">{label}</div>
      <div className={`mt-1.5 text-2xl font-semibold tabular-nums ${accentClass}`}>{value}</div>
      {delta !== undefined && (
        <div
          className={`mt-1 text-xs tabular-nums ${
            delta.startsWith("-") ? "text-rose-400" : "text-emerald-400"
          }`}
        >
          {delta}
        </div>
      )}
    </div>
  );
}