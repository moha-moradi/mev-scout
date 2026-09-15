import { colorFromHex, shortHex } from "../lib/format";

interface Props {
  seed: string;
  size?: number;
  className?: string;
}

/** Tiny deterministic avatar — no external assets. */
export default function Identicon({ seed, size = 22, className = "" }: Props) {
  const c1 = colorFromHex(seed);
  const c2 = colorFromHex(seed.split("").reverse().join(""));
  const label = shortHex(seed, 1, 1).replace("0x", "").replace("…", "").slice(0, 2).toUpperCase();

  return (
    <span
      className={`inline-flex shrink-0 items-center justify-center rounded-full text-[9px] font-semibold text-black/80 ${className}`}
      style={{
        width: size,
        height: size,
        background: `linear-gradient(135deg, ${c1}, ${c2})`,
      }}
      aria-hidden
    >
      {label}
    </span>
  );
}
