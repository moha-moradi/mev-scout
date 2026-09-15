import { useMemo, useState } from "react";

export interface Column<T> {
  key: string;
  header: string;
  cell: (row: T) => React.ReactNode;
  sortValue?: (row: T) => number | string;
  align?: "left" | "right";
  className?: string;
}

interface Props<T> {
  columns: Column<T>[];
  rows: T[];
  rowKey: (row: T) => string;
  empty?: string;
  action?: React.ReactNode;
}

export default function DataTable<T>({ columns, rows, rowKey, empty, action }: Props<T>) {
  const [sortKey, setSortKey] = useState<string | null>(null);
  const [sortDir, setSortDir] = useState<"asc" | "desc">("desc");

  const sorted = useMemo(() => {
    if (!sortKey) return rows;
    const col = columns.find((c) => c.key === sortKey);
    if (!col?.sortValue) return rows;
    const sv = col.sortValue;
    const dir = sortDir;
    return [...rows].sort((a, b) => {
      const va = sv(a);
      const vb = sv(b);
      const cmp = typeof va === "number" && typeof vb === "number"
        ? va - vb
        : String(va).localeCompare(String(vb));
      return dir === "asc" ? cmp : -cmp;
    });
  }, [columns, rows, sortKey, sortDir]);

  function toggleSort(key: string) {
    if (sortKey === key) {
      setSortDir((d) => (d === "asc" ? "desc" : "asc"));
    } else {
      setSortKey(key);
      setSortDir("desc");
    }
  }

  if (rows.length === 0) {
    return (
      <div className="rounded-xl border border-dashed border-zinc-800 p-10 text-center">
        <div className="text-3xl text-zinc-700">∅</div>
        <p className="mt-2 text-sm text-zinc-500">{empty ?? "No data"}</p>
        {action && <div className="mt-4">{action}</div>}
      </div>
    );
  }

  return (
    <div className="overflow-x-auto rounded-xl border border-zinc-800">
      <table className="w-full text-left text-sm">
        <thead className="border-b border-zinc-800 bg-zinc-900/60">
          <tr>
            {columns.map((c) => (
              <th
                key={c.key}
                onClick={() => c.sortValue && toggleSort(c.key)}
                className={`px-3 py-2 text-xs uppercase tracking-wider text-zinc-500 ${
                  c.align === "right" ? "text-right" : "text-left"
                } ${c.sortValue ? "cursor-pointer select-none hover:text-zinc-300" : ""}`}
              >
                {c.header}
                {sortKey === c.key && (sortDir === "asc" ? " ↑" : " ↓")}
              </th>
            ))}
          </tr>
        </thead>
        <tbody className="divide-y divide-zinc-800/70">
          {sorted.map((row) => (
            <tr key={rowKey(row)} className="hover:bg-zinc-900/40">
              {columns.map((c) => (
                <td
                  key={c.key}
                  className={`px-3 py-2 tabular-nums ${c.align === "right" ? "text-right" : "text-left"} ${c.className ?? ""}`}
                >
                  {c.cell(row)}
                </td>
              ))}
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}