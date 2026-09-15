import { useMemo } from "react";
import {
  ResponsiveContainer,
  AreaChart,
  Area,
  BarChart,
  Bar,
  XAxis,
  YAxis,
  Tooltip,
  CartesianGrid,
} from "recharts";
import type { RunDetail } from "../api";

interface Point {
  i: number;
  block: number;
  equity: number;
  dd: number;
  opps: number;
}

function toEth(wei: string): number {
  return Number(wei) / 1e18;
}

export default function RunCharts({ detail }: { detail: RunDetail }) {
  const data = useMemo<Point[]>(() => {
    const byBlock = new Map<number, number[]>();
    detail.opportunities.forEach((o) => {
      const arr = byBlock.get(o.block_number) ?? [];
      arr.push(toEth(o.expected_profit));
      byBlock.set(o.block_number, arr);
    });

    const blocks = [...byBlock.keys()].sort((a, b) => a - b);
    let acc = 0;
    let peak = 0;
    return blocks.map((b, i) => {
      const profits = byBlock.get(b) ?? [];
      acc += profits.reduce((s, p) => s + p, 0);
      peak = Math.max(peak, acc);
      const dd = peak > 0 ? ((acc - peak) / peak) * 100 : 0;
      return { i, block: b, equity: acc, dd, opps: profits.length };
    });
  }, [detail.opportunities]);

  if (data.length === 0) {
    return (
      <div className="rounded-xl border border-dashed border-zinc-800 p-6 text-center text-sm text-zinc-500">
        No time series to plot — this run produced no opportunities.
      </div>
    );
  }

  return (
    <div className="grid gap-4 lg:grid-cols-2">
      <div className="rounded-xl border border-zinc-800 bg-zinc-900/60 p-4">
        <h3 className="mb-3 text-sm font-medium text-zinc-200">Equity curve (ETH, cumulative)</h3>
        <ResponsiveContainer width="100%" height={220}>
          <AreaChart data={data} margin={{ top: 4, right: 8, bottom: 0, left: 0 }}>
            <defs>
              <linearGradient id="eq" x1="0" y1="0" x2="0" y2="1">
                <stop offset="0%" stopColor="#00ff94" stopOpacity={0.35} />
                <stop offset="100%" stopColor="#00ff94" stopOpacity={0.02} />
              </linearGradient>
            </defs>
            <CartesianGrid stroke="#1e2330" vertical={false} />
            <XAxis
              dataKey="i"
              tick={{ fill: "#64748b", fontSize: 10 }}
              tickLine={false}
              axisLine={{ stroke: "#1e2330" }}
            />
            <YAxis
              tick={{ fill: "#64748b", fontSize: 10 }}
              tickLine={false}
              axisLine={false}
              width={72}
            />
            <Tooltip
              contentStyle={{ background: "transparent", border: "none" }}
              wrapperStyle={{ outline: "none" }}
              labelFormatter={(l) =>
                `block ${(data[Number(l) as number] as Point | undefined)?.block ?? "?"}`
              }
              formatter={(v) => [Number(v).toFixed(6), "equity (ETH)"]}
            />
            <Area type="monotone" dataKey="equity" stroke="#00ff94" strokeWidth={2} fill="url(#eq)" />
          </AreaChart>
        </ResponsiveContainer>
      </div>

      <div className="rounded-xl border border-zinc-800 bg-zinc-900/60 p-4">
        <h3 className="mb-3 text-sm font-medium text-zinc-200">Drawdown (%)</h3>
        <ResponsiveContainer width="100%" height={220}>
          <AreaChart data={data} margin={{ top: 4, right: 8, bottom: 0, left: 0 }}>
            <defs>
              <linearGradient id="dd" x1="0" y1="0" x2="0" y2="1">
                <stop offset="0%" stopColor="#f472b6" stopOpacity={0.3} />
                <stop offset="100%" stopColor="#f472b6" stopOpacity={0.02} />
              </linearGradient>
            </defs>
            <CartesianGrid stroke="#1e2330" vertical={false} />
            <XAxis
              dataKey="i"
              tick={{ fill: "#64748b", fontSize: 10 }}
              tickLine={false}
              axisLine={{ stroke: "#1e2330" }}
            />
            <YAxis
              tick={{ fill: "#64748b", fontSize: 10 }}
              tickLine={false}
              axisLine={false}
              width={52}
            />
            <Tooltip
              contentStyle={{ background: "transparent", border: "none" }}
              wrapperStyle={{ outline: "none" }}
              formatter={(v) => [Number(v).toFixed(2) + "%", "drawdown"]}
            />
            <Area type="monotone" dataKey="dd" stroke="#f472b6" strokeWidth={2} fill="url(#dd)" />
          </AreaChart>
        </ResponsiveContainer>
      </div>

      <div className="rounded-xl border border-zinc-800 bg-zinc-900/60 p-4 lg:col-span-2">
        <h3 className="mb-3 text-sm font-medium text-zinc-200">Opportunities per block</h3>
        <ResponsiveContainer width="100%" height={180}>
          <BarChart data={data} margin={{ top: 4, right: 8, bottom: 0, left: 0 }}>
            <CartesianGrid stroke="#1e2330" vertical={false} />
            <XAxis
              dataKey="i"
              tick={{ fill: "#64748b", fontSize: 10 }}
              tickLine={false}
              axisLine={{ stroke: "#1e2330" }}
            />
            <YAxis
              tick={{ fill: "#64748b", fontSize: 10 }}
              tickLine={false}
              axisLine={false}
              width={48}
            />
            <Tooltip
              contentStyle={{ background: "transparent", border: "none" }}
              wrapperStyle={{ outline: "none" }}
              labelFormatter={(l) =>
                `block ${(data[Number(l) as number] as Point | undefined)?.block ?? "?"}`
              }
              formatter={(v) => [Number(v), "opps"]}
            />
            <Bar dataKey="opps" fill="#22d3ee" radius={[2, 2, 0, 0]} />
          </BarChart>
        </ResponsiveContainer>
      </div>
    </div>
  );
}