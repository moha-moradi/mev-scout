import { useEffect, useState } from "react";
import { api, type ChainDto, type HealthResponse } from "../api";
import { usePolling } from "../hooks";
import { useToast } from "./Toast";

interface Props {
  disabled?: boolean;
}

export default function ChainSelector({ disabled }: Props) {
  const { data: chains } = usePolling<ChainDto[]>(api.chains, 30_000, []);
  const { data: health, refresh: refreshHealth } = usePolling<HealthResponse>(api.health, 5_000, []);
  const active = health?.chain ?? "";
  const toast = useToast();
  const [busy, setBusy] = useState(false);

  // Re-read health once a chain change lands so the active label updates.
  useEffect(() => {
    if (active) refreshHealth();
  }, [active, refreshHealth]);
  useEffect(() => {
    if (chains) refreshHealth();
  }, [chains, refreshHealth]);

  const jobRunning = disabled || health?.job_status.running != null;

  async function onSelect(name: string) {
    if (name === active || jobRunning || busy) return;
    setBusy(true);
    try {
      const res = await api.putConfig({ chain: name });
      toast(
        res.restarted_connections
          ? `Switched to ${name}; database connections re-resolved.`
          : `Switched to ${name}.`,
        "success",
      );
      refreshHealth();
    } catch (e) {
      toast(`Chain switch failed: ${e instanceof Error ? e.message : String(e)}`, "error");
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="relative inline-block">
      <select
        value={active}
        disabled={jobRunning}
        onChange={(e) => void onSelect(e.target.value)}
        title={jobRunning ? "Disabled while a job runs" : "Active chain"}
        className="rounded-md border border-zinc-700 bg-zinc-900 px-2 py-1.5 text-sm text-zinc-100 outline-none focus:border-sky-600 disabled:cursor-not-allowed disabled:opacity-50"
      >
        {chains?.map((c) => (
          <option key={c.name} value={c.name}>
            {c.name}
          </option>
        ))}
      </select>
      {jobRunning && (
        <span className="pointer-events-none absolute -right-1.5 -top-1.5 h-2 w-2 rounded-full bg-amber-400" />
      )}
    </div>
  );
}