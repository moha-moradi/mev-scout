import { useState } from "react";
import { api, type PoolInfo } from "../api";
import { usePolling } from "../hooks";
import DataTable, { type Column } from "../components/DataTable";
import { useToast } from "../components/Toast";

function shortAddr(a: string): string {
  return `${a.slice(0, 8)}…${a.slice(-6)}`;
}

function fmtUsd(v: number): string {
  return v >= 1_000_000
    ? `$${(v / 1_000_000).toFixed(2)}M`
    : v >= 1_000
      ? `$${(v / 1_000).toFixed(1)}k`
      : `$${v.toFixed(2)}`;
}

export default function Pools() {
  const [q, setQ] = useState("");
  const [dex, setDex] = useState("");
  const [token, setToken] = useState("");
  const [minTvl, setMinTvl] = useState("");
  const [sort, setSort] = useState("tvl_usd");
  const [order, setOrder] = useState<"asc" | "desc">("desc");
  const toast = useToast();
  const [enriching, setEnriching] = useState(false);

  const { data, loading, error } = usePolling(
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

  const columns: Column<PoolInfo>[] = [
    {
      key: "address",
      header: "address",
      cell: (r) => <span className="font-mono text-xs text-zinc-300">{shortAddr(r.address)}</span>,
      sortValue: (r) => r.address,
    },
    { key: "dex", header: "dex", cell: (r) => r.dex_name ?? r.type, sortValue: (r) => r.dex_name ?? r.type },
    {
      key: "token0",
      header: "token A",
      cell: (r) => (
        <span className="font-mono text-xs text-zinc-400">
          {r.token0_symbol ? `${r.token0_symbol} ` : ""}
          {shortAddr(r.token0)}
        </span>
      ),
      sortValue: (r) => r.token0,
    },
    {
      key: "token1",
      header: "token B",
      cell: (r) => (
        <span className="font-mono text-xs text-zinc-400">
          {r.token1_symbol ? `${r.token1_symbol} ` : ""}
          {shortAddr(r.token1)}
        </span>
      ),
      sortValue: (r) => r.token1,
    },
    {
      key: "tvl_usd",
      header: "TVL",
      align: "right",
      cell: (r) => (r.tvl_usd != null ? <span className="text-emerald-400">{fmtUsd(r.tvl_usd)}</span> : <span className="text-zinc-500">—</span>),
      sortValue: (r) => r.tvl_usd ?? 0,
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
        <span className="tabular-nums text-zinc-400">{r.creation_block > 0 ? r.creation_block : "—"}</span>
      ),
      sortValue: (r) => r.creation_block,
    },
  ];

  async function enrich() {
    setEnriching(true);
    try {
      const res = await api.createJob("discover", ["--incremental", "--enrich", "--json"]);
      toast(`Discovery+enrich job ${res.job_id.slice(0, 8)} started.`, "success");
    } catch (e) {
      toast(`Failed: ${e instanceof Error ? e.message : String(e)}`, "error");
    } finally {
      setEnriching(false);
    }
  }

  return (
    <div className="space-y-6">
      <div className="flex flex-wrap items-center justify-between gap-3">
        <h1 className="text-xl font-semibold text-zinc-100">Pools</h1>
        <button
          onClick={enrich}
          disabled={enriching}
          className="rounded-md border border-sky-700 bg-sky-950/50 px-3 py-1.5 text-sm text-sky-300 hover:bg-sky-900/50 disabled:opacity-50"
        >
          {enriching ? "Starting…" : "Discover + enrich"}
        </button>
      </div>

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
          placeholder="dex"
          className="rounded-md border border-zinc-700 bg-zinc-900 px-2 py-2 text-sm outline-none focus:border-emerald-400"
        />
        <input
          value={token}
          onChange={(e) => setToken(e.target.value)}
          placeholder="token"
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
              onClick={enrich}
              disabled={enriching}
              className="inline-block rounded-md bg-emerald-400 px-3 py-1.5 text-sm font-semibold text-black hover:bg-emerald-300 disabled:opacity-50"
            >
              {enriching ? "Starting…" : "Start indexer"}
            </button>
          }
        />
        <p className="mt-2 text-xs text-zinc-600">{data?.total ?? 0} total pools</p>
      </div>
    </div>
  );
}