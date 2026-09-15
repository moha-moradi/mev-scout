import { useEffect, useState } from "react";
import { api, type SanitizedConfig, type ChainDto } from "../api";
import { usePolling } from "../hooks";
import { useToast } from "../components/Toast";
import SectionCard from "../components/SectionCard";

const OUTPUT_FORMATS = ["table", "json", "csv"];
const GAS_MODELS = ["eip1559", "legacy"];
const FLASH_LOAN_PROVIDERS = ["auto", "aave", "balancer", "dodo", "uniswap_v3"];

const STRATEGIES = [
  {
    key: "two_hop_arb",
    code: "ARB",
    name: "Cross-DEX arbitrage",
    desc: "Capture the price spread for a token pair across DEX venues within a block.",
    color: "bg-sky-950/60 border-sky-700/60 text-sky-300",
  },
  {
    key: "jit",
    code: "JIT",
    name: "JIT liquidity",
    desc: "Mint LP positions around large incoming swaps on UniV3-style pools.",
    color: "bg-amber-950/60 border-amber-700/60 text-amber-300",
  },
  {
    key: "jit_arb",
    code: "JITARB",
    name: "JIT + arb (flash loan)",
    desc: "JIT LP entry combined with an arbitrage exit, capital-free via flash loan.",
    color: "bg-violet-950/60 border-violet-700/60 text-violet-300",
  },
  {
    key: "sandwich",
    code: "SANDWICH",
    name: "Sandwich attack",
    desc: "Front-run and back-run a victim swap to extract slippage within one block.",
    color: "bg-rose-950/60 border-rose-700/60 text-rose-300",
  },
  {
    key: "liquidation",
    code: "LIQ",
    name: "Liquidation",
    desc: "Lending / perp liquidations with health-factor drops across protocols.",
    color: "bg-emerald-950/60 border-emerald-700/60 text-emerald-300",
  },
];

function Field({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <div>
      <label className="mb-1 block text-xs uppercase tracking-wider text-zinc-500">{label}</label>
      {children}
    </div>
  );
}

const inputCls =
  "w-full rounded-md border border-zinc-700 bg-zinc-900 px-2 py-1.5 text-sm text-zinc-100 outline-none focus:border-emerald-400";

export default function ConfigPage() {
  const { data: cfg } = usePolling<SanitizedConfig>(api.config, 15_000, []);
  const { data: chains } = usePolling<ChainDto[]>(api.chains, 30_000, []);
  const toast = useToast();
  const [saving, setSaving] = useState(false);
  const [dirty, setDirty] = useState(false);
  const [copied, setCopied] = useState(false);

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

  const set = <K extends keyof typeof form>(k: K, v: (typeof form)[K]) => {
    setForm((f) => (f ? { ...f, [k]: v } : f));
    setDirty(true);
  };

  const activeStrategies = new Set(
    form.strategies
      .split(",")
      .map((s) => s.trim())
      .filter(Boolean),
  );

  function toggleStrategy(key: string) {
    const next = new Set(activeStrategies);
    if (next.has(key)) next.delete(key);
    else next.add(key);
    set("strategies", next.size ? [...next].join(",") : "");
  }

  const railJson = JSON.stringify(
    {
      chain: form.chain,
      gas: {
        gas_model: form.gas_model,
        gas_limit: Number(form.gas_limit),
        priority_fee_gwei: Number(form.priority_fee_gwei),
      },
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
    },
    null,
    2,
  );

  async function save() {
    if (!form) return;
    setSaving(true);
    try {
      const gas: { gas_limit: number; priority_fee_gwei: number; gas_model?: string } = {
        gas_limit: Number(form.gas_limit),
        priority_fee_gwei: Number(form.priority_fee_gwei),
      };
      if (form.gas_model !== activeCfg.gas.gas_model) gas.gas_model = form.gas_model;
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
      setDirty(false);
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

  async function copyJson() {
    try {
      await navigator.clipboard.writeText(railJson);
      setCopied(true);
      setTimeout(() => setCopied(false), 1500);
    } catch {
      // clipboard unavailable
    }
  }

  return (
    <div className="grid gap-6 lg:grid-cols-[1fr_340px]">
      <div className="space-y-6">
        <h1 className="text-xl font-semibold text-zinc-100">Config</h1>

        <SectionCard num="01" title="Chain & RPC">
          <div className="grid gap-3 sm:grid-cols-2">
            <Field label="Active chain">
              <select value={form.chain} onChange={(e) => set("chain", e.target.value)} className={inputCls}>
                {chains?.map((c) => (
                  <option key={c.name} value={c.name}>{c.name}</option>
                ))}
              </select>
            </Field>
            <Field label="RPC providers (read-only, masked)">
              <input value={`${cfg.rpc.providers} · ${cfg.rpc.hosts.join(", ")}`} readOnly className={`${inputCls} opacity-60`} />
            </Field>
          </div>
        </SectionCard>

        <SectionCard
          num="02"
          title="Strategies"
          aside={`${activeStrategies.size} / ${STRATEGIES.length} on`}
        >
          <div className="mb-4 rounded-lg border border-amber-800/60 bg-amber-950/40 px-3 py-2.5 text-xs text-amber-200">
            mev-scout runs exactly these strategies against the backend. No inventory beyond this set is
            simulated.
          </div>
          <div className="grid gap-3 sm:grid-cols-2">
            {STRATEGIES.map((s) => {
              const on = activeStrategies.has(s.key);
              return (
                <button
                  key={s.key}
                  type="button"
                  onClick={() => toggleStrategy(s.key)}
                  className={`rounded-lg border p-3 text-left transition-colors ${
                    on
                      ? "border-zinc-700 bg-zinc-900"
                      : "border-zinc-800 bg-zinc-900/40 opacity-60"
                  }`}
                >
                  <div className="flex items-center justify-between gap-2">
                    <span className={`rounded border px-1.5 py-0.5 font-mono text-[10px] ${s.color}`}>
                      {s.code}
                    </span>
                    <span
                      className={`relative h-4 w-7 rounded-full transition-colors ${
                        on ? "bg-emerald-400" : "bg-zinc-700"
                      }`}
                    >
                      <span
                        className={`absolute top-0.5 h-3 w-3 rounded-full bg-black transition-all ${
                          on ? "left-3.5" : "left-0.5"
                        }`}
                      />
                    </span>
                  </div>
                  <div className="mt-2 text-sm font-medium text-zinc-200">{s.name}</div>
                  <div className="mt-0.5 text-xs leading-relaxed text-zinc-500">{s.desc}</div>
                </button>
              );
            })}
          </div>
        </SectionCard>

        <SectionCard num="03" title="Backtest">
          <div className="grid gap-3 sm:grid-cols-2">
            <Field label="Flash loan provider">
              <select value={form.flash_loan_provider} onChange={(e) => set("flash_loan_provider", e.target.value)} className={inputCls}>
                {FLASH_LOAN_PROVIDERS.map((p) => <option key={p} value={p}>{p}</option>)}
              </select>
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
                className="accent-emerald-400"
              />
              capture pending
            </label>
          </div>
        </SectionCard>

        <SectionCard num="04" title="Gas">
          <div className="grid gap-3 sm:grid-cols-3">
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
        </SectionCard>

        <SectionCard num="05" title="Output">
          <div className="grid gap-3 sm:grid-cols-2">
            <Field label="Format">
              <select value={form.output} onChange={(e) => set("output", e.target.value)} className={inputCls}>
                {OUTPUT_FORMATS.map((f) => <option key={f} value={f}>{f}</option>)}
              </select>
            </Field>
            <Field label="DB path (read-only)">
              <input value={cfg.output.db_path} readOnly className={`${inputCls} opacity-60`} />
            </Field>
          </div>
        </SectionCard>

        <SectionCard num="06" title="Explorer">
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
        </SectionCard>
      </div>

      <div className="lg:sticky lg:top-24 lg:self-start">
        <div className="rounded-xl border border-zinc-800 bg-zinc-900/60 p-4">
          <div className="mb-3 flex items-center justify-between">
            <h2 className="text-sm font-medium text-zinc-200">API request</h2>
            <div className="flex items-center gap-2">
              {dirty && (
                <span className="rounded-full border border-amber-700/60 bg-amber-950/50 px-2 py-0.5 text-[10px] uppercase tracking-wider text-amber-300">
                  unsaved
                </span>
              )}
              <button
                onClick={copyJson}
                className="rounded border border-zinc-700 bg-zinc-900 px-2 py-0.5 text-[11px] text-zinc-400 hover:bg-zinc-800 hover:text-zinc-200"
              >
                {copied ? "copied" : "copy"}
              </button>
            </div>
          </div>
          <pre className="max-h-72 overflow-auto rounded-lg border border-zinc-800 bg-black/40 p-3 font-mono text-[11px] leading-relaxed text-zinc-400">
            {railJson}
          </pre>
          <button
            onClick={save}
            disabled={saving}
            className="mt-3 w-full rounded-md bg-emerald-400 px-4 py-2 text-sm font-semibold text-black hover:bg-emerald-300 disabled:opacity-50"
          >
            {saving ? "Saving…" : "Save config"}
          </button>
        </div>
      </div>
    </div>
  );
}