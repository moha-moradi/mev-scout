import { colorFromHex, parseRoute, routeTokens, shortHex, type RouteHop } from "../lib/format";

interface Props {
  routeJson?: string | null;
  hops?: RouteHop[];
  className?: string;
  compact?: boolean;
}

function TokenChip({ addr, compact }: { addr: string; compact?: boolean }) {
  return (
    <span
      className="inline-flex items-center gap-1.5"
      title={addr}
    >
      <span
        className="inline-block rounded-full ring-1 ring-black/40"
        style={{
          width: compact ? 16 : 20,
          height: compact ? 16 : 20,
          background: colorFromHex(addr),
        }}
      />
      {!compact && (
        <span className="font-mono text-[11px] text-zinc-300">{shortHex(addr, 2, 3)}</span>
      )}
    </span>
  );
}

export default function RoutePath({ routeJson, hops, className = "", compact = true }: Props) {
  const list = hops ?? parseRoute(routeJson);
  const tokens = routeTokens(list);

  if (tokens.length === 0) {
    if (list.length > 0) {
      return (
        <span className={`inline-flex flex-wrap items-center gap-1 ${className}`}>
          {list.map((h, i) => (
            <span key={i} className="inline-flex items-center gap-1 text-[11px] text-zinc-400">
              {i > 0 && <span className="text-zinc-600">→</span>}
              <span className="rounded border border-zinc-800 bg-zinc-900 px-1.5 py-0.5 font-mono">
                {h.amm ?? "pool"}
              </span>
            </span>
          ))}
        </span>
      );
    }
    return <span className={`text-xs text-zinc-600 ${className}`}>—</span>;
  }

  return (
    <span className={`inline-flex flex-wrap items-center gap-1 ${className}`}>
      {tokens.map((t, i) => (
        <span key={`${t}-${i}`} className="inline-flex items-center gap-1">
          {i > 0 && <span className="text-[10px] text-zinc-500">→</span>}
          <TokenChip addr={t} compact={compact} />
        </span>
      ))}
    </span>
  );
}
