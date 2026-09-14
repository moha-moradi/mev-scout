import { useEffect, useState } from "react";
import { api, type SanitizedConfig, type ChainDto } from "../api";
import { usePolling } from "../hooks";
import { useToast } from "../components/Toast";

const OUTPUT_FORMATS = ["table", "json", "csv"];
const GAS_MODELS = ["eip1559", "legacy"];
const FLASH_LOAN_PROVIDERS = ["auto", "aave", "balancer", "dodo", "uniswap_v3"];

function Field({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <div>
      <label className="mb-1 block text-xs uppercase tracking-wider text-zinc-500">{label}</label>
      {children}
    </div>
  );
}

const inputCls =
  "w-full rounded-md border border-zinc-700 bg-zinc-900 px-2 py-1.5 text-sm text-zinc-100 outline-none focus:border-sky-600";

export default function ConfigPage() {
  const { data: cfg } = usePolling<SanitizedConfig>(api.config, 15_000, []);
  const { data: chains } = usePolling<ChainDto[]>(api.chains, 30_000, []);
  const toast = useToast();
  const [saving, setSaving] = useState(false);

  const [form, setForm] = useState<
    | {
        chain: string;
        flash_loan_provider: string;
        strategies: string;
        max_pairs_per_token: string;
        proximity_window: string;
        capture_pending: boolean;
        min_profit_wei: string;
        max_candidates_per_tx: string;
        gas_model: string;
        gas_limit: string;
        priority_fee_gwei: string;
        output: string;
        confirmations: string;
        poll_interval_ms: string;
        checkpoint_every: string;
      }
    | null
  >(null);

  useEffect(() => {
    if (cfg && !form) {
      setForm({
        chain: cfg.chain,
        flash_loan_provider: cfg.backtest.flash_loan_provider,
        strategies: cfg.backtest.strategies,
        max_pairs_per_token: String(cfg.backtest.max_pairs_per_token),
        proximity_window: String(cfg.backtest.proximity_window),
        capture_pending: cfg.backtest.capture_pending,
        min_profit_wei: String(cfg.backtest.min_profit_wei),
        max_candidates_per_tx: String(cfg.backtest.max_candidates_per_tx),
        gas_model: cfg.gas.gas_model,
        gas_limit: String(cfg.gas.gas_limit),
        priority_fee_gwei: String(cfg.gas.priority_fee_gwei),
        output: cfg.output.output,
        confirmations: String(cfg.explorer.confirmations),
        poll_interval_ms: String(cfg.explorer.poll_interval_ms),
        checkpoint_every: String(cfg.explorer.checkpoint_every),
      });
    }
  }, [cfg, form]);

  if (!cfg || !form) {
    return <p className="text-sm text-zinc-500">Loading config…</p>;
  }
  const activeCfg = cfg;

  const set = <K extends keyof typeof form>(k: K, v: (typeof form)[K]) =>
    setForm((f) => (f ? { ...f, [k]: v } : f));

  async function save() {
    if (!form) return;
    setSaving(true);
    try {
      const gas = {
        gas_limit: Number(form.gas_limit),
        priority_fee_gwei: Number(form.priority_fee_gwei),
      };
      if (form.gas_model !== activeCfg.gas.gas_model) (gas as { gas_model?: string }).gas_model = form.gas_model;
      const res = await api.putConfig({
        chain: form.chain !== activeCfg.chain ? form.chain : undefined,
        gas,
        backtest: {
          flash_loan_provider: form.flash_loan_provider,
          strategies: form.strategies,
          max_pairs_per_token: Number(form.max_pairs_per_token),
          proximity_window: Number(form.proximity_window),
          capture_pending: form.capture_pending,
          min_profit_wei: Number(form.min_profit_wei),
          max_candidates_per_tx: Number(form.max_candidates_per_tx),
        },
        output: { output: form.output },
        explorer: {
          confirmations: Number(form.confirmations),
          poll_interval_ms: Number(form.poll_interval_ms),
          checkpoint_every: Number(form.checkpoint_every),
        },
      });
      toast(
        res.restarted_connections
          ? "Saved. Database connections re-resolved."
          : "Config saved.",
        "success",
      );
    } catch (e) {
      toast(`Save failed: ${e instanceof Error ? e.message : String(e)}`, "error");
    } finally {
      setSaving(false);
    }
  }

  return (
    <div className="space-y-6">
      <div className="flex items-center justify-between">
        <h1 className="text-xl font-semibold text-zinc-100">Config</h1>
        <button
          onClick={save}
          disabled={saving}
          className="rounded-md bg-sky-700 px-4 py-2 text-sm font-medium text-white hover:bg-sky-600 disabled:opacity-50"
        >
          {saving ? "Saving…" : "Save"}
        </button>
      </div>

      <div className="grid gap-4 lg:grid-cols-2">
        <div className="rounded-xl border border-zinc-800 bg-zinc-900/60 p-4">
          <h2 className="mb-3 text-sm font-medium text-zinc-200">Chain</h2>
          <Field label="Active chain">
            <select value={form.chain} onChange={(e) => set("chain", e.target.value)} className={inputCls}>
              {chains?.map((c) => (
                <option key={c.name} value={c.name}>{c.name}</option>
              ))}
            </select>
          </Field>
          <div className="mt-3">
            <Field label="RPC providers (read-only, masked)">
              <input value={`${cfg.rpc.providers} · ${cfg.rpc.hosts.join(", ")}`} readOnly className={`${inputCls} opacity-60`} />
            </Field>
          </div>
        </div>

        <div className="rounded-xl border border-zinc-800 bg-zinc-900/60 p-4">
          <h2 className="mb-3 text-sm font-medium text-zinc-200">Backtest</h2>
          <div className="grid gap-3 sm:grid-cols-2">
            <Field label="Flash loan provider">
              <select value={form.flash_loan_provider} onChange={(e) => set("flash_loan_provider", e.target.value)} className={inputCls}>
                {FLASH_LOAN_PROVIDERS.map((p) => <option key={p} value={p}>{p}</option>)}
              </select>
            </Field>
            <Field label="Strategies (comma-separated)">
              <input value={form.strategies} onChange={(e) => set("strategies", e.target.value)} className={inputCls} />
            </Field>
            <Field label="Max pairs per token">
              <input type="number" value={form.max_pairs_per_token} onChange={(e) => set("max_pairs_per_token", e.target.value)} className={inputCls} />
            </Field>
            <Field label="Proximity window">
              <input type="number" value={form.proximity_window} onChange={(e) => set("proximity_window", e.target.value)} className={inputCls} />
            </Field>
            <Field label="Min profit wei">
              <input type="number" value={form.min_profit_wei} onChange={(e) => set("min_profit_wei", e.target.value)} className={inputCls} />
            </Field>
            <Field label="Max candidates per tx">
              <input type="number" value={form.max_candidates_per_tx} onChange={(e) => set("max_candidates_per_tx", e.target.value)} className={inputCls} />
            </Field>
            <label className="flex items-center gap-2 self-end pb-1.5 text-sm text-zinc-300">
              <input
                type="checkbox"
                checked={form.capture_pending}
                onChange={(e) => set("capture_pending", e.target.checked)}
                className="accent-sky-600"
              />
              capture pending
            </label>
          </div>
        </div>

        <div className="rounded-xl border border-zinc-800 bg-zinc-900/60 p-4">
          <h2 className="mb-3 text-sm font-medium text-zinc-200">Gas</h2>
          <div className="grid gap-3 sm:grid-cols-2">
            <Field label="Gas model">
              <select value={form.gas_model} onChange={(e) => set("gas_model", e.target.value)} className={inputCls}>
                {GAS_MODELS.map((m) => <option key={m} value={m}>{m}</option>)}
              </select>
            </Field>
            <Field label="Gas limit">
              <input type="number" value={form.gas_limit} onChange={(e) => set("gas_limit", e.target.value)} className={inputCls} />
            </Field>
            <Field label="Priority fee (gwei)">
              <input type="number" step="0.1" value={form.priority_fee_gwei} onChange={(e) => set("priority_fee_gwei", e.target.value)} className={inputCls} />
            </Field>
          </div>
        </div>

        <div className="rounded-xl border border-zinc-800 bg-zinc-900/60 p-4">
          <h2 className="mb-3 text-sm font-medium text-zinc-200">Output</h2>
          <Field label="Format">
            <select value={form.output} onChange={(e) => set("output", e.target.value)} className={inputCls}>
              {OUTPUT_FORMATS.map((f) => <option key={f} value={f}>{f}</option>)}
            </select>
          </Field>
          <div className="mt-3">
            <Field label="DB path (read-only)">
              <input value={cfg.output.db_path} readOnly className={`${inputCls} opacity-60`} />
            </Field>
          </div>
        </div>

        <div className="rounded-xl border border-zinc-800 bg-zinc-900/60 p-4">
          <h2 className="mb-3 text-sm font-medium text-zinc-200">Explorer</h2>
          <div className="grid gap-3 sm:grid-cols-3">
            <Field label="Confirmations">
              <input type="number" value={form.confirmations} onChange={(e) => set("confirmations", e.target.value)} className={inputCls} />
            </Field>
            <Field label="Poll interval (ms)">
              <input type="number" value={form.poll_interval_ms} onChange={(e) => set("poll_interval_ms", e.target.value)} className={inputCls} />
            </Field>
            <Field label="Checkpoint every">
              <input type="number" value={form.checkpoint_every} onChange={(e) => set("checkpoint_every", e.target.value)} className={inputCls} />
            </Field>
          </div>
        </div>
      </div>
    </div>
  );
}