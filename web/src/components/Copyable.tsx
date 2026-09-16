import { useState } from "react";
import { shortHex } from "../lib/format";

interface Props {
  value: string;
  head?: number;
  tail?: number;
  className?: string;
  mono?: boolean;
}

export default function Copyable({
  value,
  head = 6,
  tail = 4,
  className = "",
  mono = true,
}: Props) {
  const [ok, setOk] = useState(false);

  async function copy(e: React.MouseEvent) {
    e.stopPropagation();
    try {
      await navigator.clipboard.writeText(value);
      setOk(true);
      setTimeout(() => setOk(false), 1200);
    } catch {
      // clipboard unavailable
    }
  }

  return (
    <button
      type="button"
      title={value}
      onClick={copy}
      className={`inline-flex items-center gap-1 rounded px-0.5 text-left transition-colors hover:text-zinc-100 ${
        mono ? "font-mono" : ""
      } ${className}`}
    >
      <span>{shortHex(value, head, tail)}</span>
      <span className="text-[10px] text-zinc-600">{ok ? "copied" : "copy"}</span>
    </button>
  );
}
