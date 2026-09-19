import { useState } from "react";
import { Link } from "react-router-dom";
import { api, type HealthResponse, type PoolInfo } from "../api";
import { usePolling } from "../hooks";
import DataTable, { type Column } from "../components/DataTable";
import Copyable from "../components/Copyable";
import DiscoverForm from "../components/DiscoverForm";
import { useToast } from "../components/Toast";
import { formatAmmType, formatBlockCreated, formatUsd } from "../lib/format";

function TokenCell({ symbol, address }: { symbol: string | null; address: string }) {
  return (
    <div className="flex min-w-[7rem] flex-col gap-0.5">
      <span className="text-sm font-medium text-zinc-200">{symbol || "unknown"}</span>
      <Copyable value={address} head={4} tail={4} className="text-[11px] text-zinc-500" />
    </div>
  );
}

function protocolLabel(r: PoolInfo): string {
  const amm = formatAmmType(r.type);
  const name = (r.dex_name ?? "").trim();
  if (!name) return amm;
  const normalized = name.replace(/\s+/g, "").toLowerCase();
  const ammNorm = amm.replace(/\s+/g, "").toLowerCase();
  if (normalized === ammNorm || normalized === r.type.replace(/_/g, "")) return amm;
  return name;
}

export default function Pools() {
  const [q, setQ] = useState("");
  const [dex, setDex] = useState("");
  const [token, setToken] = useState("");
  const [minTvl, setMinTvl] = useState("");
  const [sort, setSort] = useState("tvl_usd");
  const [order, setOrder] = useState<"asc" | "desc">("desc");
  const toast = useToast();
  const [discovering, setDiscovering] = useState(false);
  const [showDiscover, setShowDiscover] = useState(true);

  const { data: health } = usePolling<HealthResponse>(api.health, 15_000, []);
  const chain = health?.chain;

  const { data, loading, error, refresh } = usePolling(
    () =>
      api.pools({
        q: q || undefined,
        dex: dex || undefined,
        token: token || undefined,
        min_tvl: minTvl ? Number(minTvl) : undefined,
        sort,
        order,
        limit: 500,
      }),
    10_000,
    [q, dex, token, minTvl, sort, order],
  );

  const rows = data?.items ?? [];
  const withTvl = rows.filter((r) => r.tvl_usd != null).length;
  const tvlMissing = rows.length > 0 && withTvl === 0;

  const columns: Column<PoolInfo>[] = [
    {
      key: "protocol",
      header: "protocol",
      cell: (r) => (
        <div className="flex min-w-[8rem] flex-col gap-0.5">
          <span className="text-sm font-medium text-zinc-100">{protocolLabel(r)}</span>
          <Copyable value={r.address} head={6} tail={4} className="text-[11px] text-zinc-500" />
        </div>
      ),
      sortValue: (r) => protocolLabel(r),
    },
    {
      key: "type",
      header: "amm type",
      cell: (r) => (
        <span className="rounded border border-zinc-700/80 bg-zinc-900 px-1.5 py-0.5 text-[11px] text-zinc-400">
          {formatAmmType(r.type)}
        </span>
      ),
      sortValue: (r) => r.type,
    },
    {
      key: "token0",
      header: "token A",
      cell: (r) => <TokenCell symbol={r.token0_symbol} address={r.token0} />,
      sortValue: (r) => r.token0_symbol ?? r.token0,
    },
    {
      key: "token1",
      header: "token B",
      cell: (r) => <TokenCell symbol={r.token1_symbol} address={r.token1} />,
      sortValue: (r) => r.token1_symbol ?? r.token1,
    },
    {
      key: "tvl_usd",
      header: "TVL",
      align: "right",
      cell: (r) =>
        r.tvl_usd != null ? (
          <span className="tabular-nums text-emerald-400">{formatUsd(r.tvl_usd)}</span>
        ) : (
          <span className="text-zinc-600">—</span>
        ),
      sortValue: (r) => r.tvl_usd ?? -1,
    },
    {
      key: "volume_usd_24h",
      header: "24h vol",
      align: "right",
      cell: (r) =>
        r.volume_usd_24h != null ? (
          <span className="tabular-nums text-zinc-300">{formatUsd(r.volume_usd_24h)}</span>
        ) : (
          <span className="text-zinc-600">—</span>
        ),
      sortValue: (r) => r.volume_usd_24h ?? -1,
    },
    {
      key: "fee",
      header: "fee",
      align: "right",
      cell: (r) => <span className="tabular-nums text-zinc-300">{r.fee} bps</span>,
      sortValue: (r) => r.fee,
    },
    {
      key: "creation_block",
      header: "created",
      align: "right",
      cell: (r) => (
        <span className="whitespace-nowrap tabular-nums text-zinc-400" title={`block ${r.creation_block}`}>
          {formatBlockCreated(chain, r.creation_block)}
        </span>
      ),
      sortValue: (r) => r.creation_block,
    },
  ];

  async function runDiscover(args: string[]) {
    setDiscovering(true);
    try {
      const res = await api.createJob("discover", args);
      toast(`Discover job ${res.job_id.slice(0, 8)} started.`, "success");
      setTimeout(() => refresh(), 4_000);
    } catch (e) {
      toast(`Failed: ${e instanceof Error ? e.message : String(e)}`, "error");
    } finally {
      setDiscovering(false);
    }
  }

  return (
    <div className="space-y-6">
      <div className="flex flex-wrap items-center justify-between gap-3">
        <h1 className="text-xl font-semibold text-zinc-100">Pools</h1>
        <div className="flex flex-wrap items-center gap-2">
          <Link to="/jobs" className="text-xs text-zinc-500 hover:text-zinc-300">
            job history →
          </Link>
          <button
            type="button"
            onClick={() => setShowDiscover((v) => !v)}
            className="rounded-md border border-zinc-700 px-3 py-1.5 text-sm text-zinc-300 hover:border-zinc-500"
          >
            {showDiscover ? "Hide discover" : "Discover pools"}
          </button>
        </div>
      </div>

      {showDiscover && (
        <div className="rounded-xl border border-zinc-800 bg-zinc-900/60 p-4">
          <h2 className="mb-3 text-sm font-medium text-zinc-200">Pool discovery</h2>
          <p className="mb-4 text-xs text-zinc-500">
            Same flags as <span className="font-mono text-zinc-400">mev-scout discover</span>. Choose
            source, block range (for onchain/hybrid), enrich, and advanced RPC options.
          </p>
          <DiscoverForm
            submitting={discovering}
            submitLabel="Start discovery"
            onSubmit={runDiscover}
          />
        </div>
      )}

      {tvlMissing && (
        <div className="rounded-lg border border-amber-800/60 bg-amber-950/30 px-3 py-2 text-sm text-amber-200/90">
          TVL is empty because these pools came from on-chain activity only. Run discovery with{" "}
          <span className="font-medium">--source remote</span> or{" "}
          <span className="font-medium">--enrich</span> to attach GeckoTerminal metrics.
        </div>
      )}

      <div className="grid gap-3 rounded-xl border border-zinc-800 bg-zinc-900/60 p-4 sm:grid-cols-2 lg:grid-cols-5">
        <input
          value={q}
          onChange={(e) => setQ(e.target.value)}
          placeholder="search address"
          className="rounded-md border border-zinc-700 bg-zinc-900 px-2 py-2 text-sm outline-none focus:border-emerald-400"
        />
        <input
          value={dex}
          onChange={(e) => setDex(e.target.value)}
          placeholder="protocol (e.g. pharaoh)"
          className="rounded-md border border-zinc-700 bg-zinc-900 px-2 py-2 text-sm outline-none focus:border-emerald-400"
        />
        <input
          value={token}
          onChange={(e) => setToken(e.target.value)}
          placeholder="token symbol"
          className="rounded-md border border-zinc-700 bg-zinc-900 px-2 py-2 text-sm outline-none focus:border-emerald-400"
        />
        <input
          value={minTvl}
          onChange={(e) => setMinTvl(e.target.value)}
          placeholder="min TVL USD"
          className="rounded-md border border-zinc-700 bg-zinc-900 px-2 py-2 text-sm outline-none focus:border-emerald-400"
        />
        <select
          value={`${sort}:${order}`}
          onChange={(e) => {
            const [s, o] = e.target.value.split(":");
            setSort(s);
            setOrder(o as "asc" | "desc");
          }}
          className="rounded-md border border-zinc-700 bg-zinc-900 px-2 py-2 text-sm outline-none focus:border-emerald-400"
        >
          <option value="tvl_usd:desc">TVL ↓</option>
          <option value="tvl_usd:asc">TVL ↑</option>
          <option value="volume_usd_24h:desc">24h volume ↓</option>
          <option value="creation_block:desc">newest</option>
        </select>
      </div>

      {error && <p className="text-sm text-rose-400">{error}</p>}
      <div className="text-sm">
        <DataTable<PoolInfo>
          columns={columns}
          rows={rows}
          rowKey={(r) => r.address}
          empty={loading ? "Loading…" : "No pools indexed."}
          action={
            <button
              type="button"
              onClick={() => {
                setShowDiscover(true);
                window.scrollTo({ top: 0, behavior: "smooth" });
              }}
              className="inline-block rounded-md bg-emerald-400 px-3 py-1.5 text-sm font-semibold text-black hover:bg-emerald-300"
            >
              Discover pools
            </button>
          }
        />
        <p className="mt-2 text-xs text-zinc-600">
          {data?.total ?? 0} total pools
          {rows.length > 0 ? ` · ${withTvl}/${rows.length} with TVL` : ""}
        </p>
      </div>
    </div>
  );
}
