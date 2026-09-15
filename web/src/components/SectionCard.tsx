interface Props {
  num: string;
  title: string;
  aside?: React.ReactNode;
  children: React.ReactNode;
}

export default function SectionCard({ num, title, aside, children }: Props) {
  return (
    <section className="rounded-xl border border-zinc-800 bg-zinc-900/60">
      <header className="flex items-center justify-between gap-3 border-b border-zinc-800/80 px-4 py-3">
        <h2 className="flex items-baseline gap-2.5 text-sm font-medium text-zinc-200">
          <span className="font-mono text-xs text-emerald-400/80">{num}</span>
          <span>{title}</span>
        </h2>
        {aside && <div className="shrink-0 text-xs text-zinc-500">{aside}</div>}
      </header>
      <div className="p-4">{children}</div>
    </section>
  );
}