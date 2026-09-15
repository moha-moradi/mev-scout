interface Props {
  num: string;
  title: string;
  aside?: React.ReactNode;
  children: React.ReactNode;
}

export default function SectionCard({ num, title, aside, children }: Props) {
  return (
    <section className="rounded-[var(--radius-card)] border border-zinc-800 bg-zinc-900/60">
      <header className="flex items-center justify-between gap-3 border-b border-zinc-800/80 px-4 py-3">
        <h2 className="flex items-baseline gap-2.5 text-sm font-medium text-zinc-200">
          <span className="font-mono text-xs tabular-nums text-emerald-400/90">{num}</span>
          <span className="text-zinc-500">·</span>
          <span>{title}</span>
        </h2>
        {aside && (
          <div className="shrink-0 font-mono text-[11px] uppercase tracking-wider text-zinc-500">
            {aside}
          </div>
        )}
      </header>
      <div className="p-4">{children}</div>
    </section>
  );
}