import { useState } from "react";

interface Props {
  label?: string;
  prompt?: string;
  lines: string[];
}

export default function TerminalPanel({ label, prompt = "$", lines }: Props) {
  const [copied, setCopied] = useState(false);

  async function copy() {
    try {
      await navigator.clipboard.writeText(lines.join("\n"));
      setCopied(true);
      setTimeout(() => setCopied(false), 1500);
    } catch {
      // clipboard unavailable
    }
  }

  return (
    <div className="overflow-hidden rounded-xl border border-zinc-800 bg-black/50">
      <div className="flex items-center justify-between border-b border-zinc-800/80 bg-zinc-900/60 px-3 py-2">
        <div className="flex items-center gap-1.5">
          <span className="h-2 w-2 rounded-full bg-zinc-700" />
          <span className="h-2 w-2 rounded-full bg-zinc-700" />
          <span className="h-2 w-2 rounded-full bg-zinc-700" />
        </div>
        <span className="text-[11px] uppercase tracking-wider text-zinc-500">{label ?? "terminal"}</span>
        <button
          onClick={copy}
          className="rounded border border-zinc-700 bg-zinc-900 px-2 py-0.5 text-[11px] text-zinc-400 hover:bg-zinc-800 hover:text-zinc-200"
        >
          {copied ? "copied" : "copy"}
        </button>
      </div>
      <pre className="overflow-x-auto p-3 font-mono text-xs leading-relaxed text-zinc-300">
        {lines.map((l, i) => (
          <div key={i}>
            <span className="select-none text-emerald-400/60">{prompt} </span>
            {l}
          </div>
        ))}
      </pre>
    </div>
  );
}