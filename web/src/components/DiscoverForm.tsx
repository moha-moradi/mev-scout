import { useMemo, useState } from "react";
import {
  buildDiscoverArgs,
  DEFAULT_DISCOVER_STATE,
  needsBlockRange,
  validateDiscoverState,
  type DiscoverFormState,
  type DiscoverySource,
  type RangeMode,
} from "../lib/discoverArgs";

const FIELD =
  "w-full rounded-md border border-zinc-700 bg-zinc-900 px-2 py-2 text-sm text-zinc-100 outline-none focus:border-emerald-400";
const LABEL = "mb-1.5 block text-xs uppercase tracking-wider text-zinc-500";
const CHECK =
  "flex items-center gap-2 text-sm text-zinc-300 select-none cursor-pointer";

type Props = {
  initial?: Partial<DiscoverFormState>;
  /** When set, form is controlled from outside (e.g. Jobs syncing argv). */
  value?: DiscoverFormState;
  onChange?: (state: DiscoverFormState, args: string[]) => void;
  onSubmit?: (args: string[]) => void | Promise<void>;
  submitting?: boolean;
  submitLabel?: string;
  /** Hide the primary submit button (parent provides its own). */
  hideSubmit?: boolean;
  showArgPreview?: boolean;
};

export default function DiscoverForm({
  initial,
  value,
  onChange,
  onSubmit,
  submitting = false,
  submitLabel = "Start discovery",
  hideSubmit = false,
  showArgPreview = true,
}: Props) {
  const [local, setLocal] = useState<DiscoverFormState>({
    ...DEFAULT_DISCOVER_STATE,
    ...initial,
  });
  const [advanced, setAdvanced] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const state = value ?? local;

  function update(patch: Partial<DiscoverFormState>) {
    const next = { ...state, ...patch };
    if (!value) setLocal(next);
    onChange?.(next, buildDiscoverArgs(next));
  }

  const args = useMemo(() => buildDiscoverArgs(state), [state]);
  const rangeNeeded = needsBlockRange(state.source);

  async function submit() {
    const err = validateDiscoverState(state);
    if (err) {
      setError(err);
      return;
    }
    setError(null);
    await onSubmit?.(args);
  }

  return (
    <div className="space-y-4">
      <div className="grid gap-3 sm:grid-cols-2 lg:grid-cols-4">
        <div>
          <label className={LABEL}>source (--source)</label>
          <select
            value={state.source}
            onChange={(e) => update({ source: e.target.value as DiscoverySource })}
            className={FIELD}
          >
            <option value="remote">remote — GeckoTerminal / DexScreener</option>
            <option value="onchain">onchain — factory getLogs</option>
            <option value="hybrid">hybrid — union of both</option>
          </select>
        </div>

        <div className={rangeNeeded ? "" : "opacity-50"}>
          <label className={LABEL}>block range</label>
          <select
            value={state.rangeMode}
            disabled={!rangeNeeded}
            onChange={(e) => update({ rangeMode: e.target.value as RangeMode })}
            className={FIELD}
            title={
              rangeNeeded
                ? undefined
                : "Block range is only used for onchain / hybrid sources"
            }
          >
            <option value="auto">auto (config lookback / start block)</option>
            <option value="blocks">--blocks (last N)</option>
            <option value="days">--days (last N days)</option>
            <option value="range">--from-block / --to-block</option>
            <option value="block">--block (single)</option>
          </select>
        </div>

        {rangeNeeded && state.rangeMode === "blocks" && (
          <div>
            <label className={LABEL}>--blocks</label>
            <input
              type="number"
              min={1}
              value={state.blocks}
              onChange={(e) => update({ blocks: e.target.value })}
              className={FIELD}
            />
          </div>
        )}
        {rangeNeeded && state.rangeMode === "days" && (
          <div>
            <label className={LABEL}>--days</label>
            <input
              type="number"
              min={1}
              max={365}
              value={state.days}
              onChange={(e) => update({ days: e.target.value })}
              className={FIELD}
            />
          </div>
        )}
        {rangeNeeded && state.rangeMode === "block" && (
          <div>
            <label className={LABEL}>--block</label>
            <input
              type="number"
              min={1}
              value={state.block}
              onChange={(e) => update({ block: e.target.value })}
              className={FIELD}
            />
          </div>
        )}
        {rangeNeeded && state.rangeMode === "range" && (
          <>
            <div>
              <label className={LABEL}>--from-block</label>
              <input
                type="number"
                min={0}
                value={state.fromBlock}
                onChange={(e) => update({ fromBlock: e.target.value })}
                className={FIELD}
              />
            </div>
            <div>
              <label className={LABEL}>--to-block</label>
              <input
                type="number"
                min={0}
                value={state.toBlock}
                onChange={(e) => update({ toBlock: e.target.value })}
                className={FIELD}
              />
            </div>
          </>
        )}
      </div>

      <div className="flex flex-wrap gap-x-6 gap-y-2">
        <label className={CHECK}>
          <input
            type="checkbox"
            checked={state.enrich}
            onChange={(e) => update({ enrich: e.target.checked })}
            className="accent-emerald-400"
          />
          --enrich (TVL / volume metrics)
        </label>
        <label className={CHECK}>
          <input
            type="checkbox"
            checked={state.incremental}
            disabled={!rangeNeeded}
            onChange={(e) => update({ incremental: e.target.checked })}
            className="accent-emerald-400"
          />
          --incremental
        </label>
        <label className={CHECK}>
          <input
            type="checkbox"
            checked={state.resolveRemoteMetadata}
            onChange={(e) => update({ resolveRemoteMetadata: e.target.checked })}
            className="accent-emerald-400"
          />
          --resolve-remote-metadata
        </label>
        <label className={CHECK}>
          <input
            type="checkbox"
            checked={state.json}
            onChange={(e) => update({ json: e.target.checked })}
            className="accent-emerald-400"
          />
          --json
        </label>
      </div>

      <div className="grid gap-3 sm:grid-cols-2 lg:grid-cols-4">
        <div>
          <label className={LABEL}>--min-tvl</label>
          <input
            type="number"
            min={0}
            step="any"
            value={state.minTvl}
            onChange={(e) => update({ minTvl: e.target.value })}
            placeholder="0"
            className={FIELD}
          />
        </div>
        <div>
          <label className={LABEL}>--max-pools</label>
          <input
            type="number"
            min={1}
            value={state.maxPools}
            onChange={(e) => update({ maxPools: e.target.value })}
            className={FIELD}
          />
        </div>
      </div>

      <button
        type="button"
        onClick={() => setAdvanced((v) => !v)}
        className="text-xs text-zinc-500 hover:text-zinc-300"
      >
        {advanced ? "▾ hide advanced" : "▸ advanced (--batch-size, --rpc-concurrency, …)"}
      </button>

      {advanced && (
        <div className="grid gap-3 rounded-lg border border-zinc-800 bg-zinc-950/40 p-3 sm:grid-cols-2 lg:grid-cols-4">
          <div>
            <label className={LABEL}>--batch-size</label>
            <input
              type="number"
              min={1}
              value={state.batchSize}
              onChange={(e) => update({ batchSize: e.target.value })}
              className={FIELD}
            />
          </div>
          <div>
            <label className={LABEL}>--rpc-concurrency</label>
            <input
              type="number"
              min={1}
              value={state.rpcConcurrency}
              onChange={(e) => update({ rpcConcurrency: e.target.value })}
              className={FIELD}
            />
          </div>
          <div>
            <label className={LABEL}>--solidly-fee-bps</label>
            <input
              type="number"
              min={0}
              value={state.solidlyFeeBps}
              onChange={(e) => update({ solidlyFeeBps: e.target.value })}
              placeholder="default 30"
              className={FIELD}
            />
          </div>
          <div className="flex items-end pb-2">
            <label className={CHECK}>
              <input
                type="checkbox"
                checked={state.healthCheck}
                onChange={(e) => update({ healthCheck: e.target.checked })}
                className="accent-emerald-400"
              />
              --health-check
            </label>
          </div>
        </div>
      )}

      {showArgPreview && (
        <p className="rounded-md border border-zinc-800 bg-zinc-950/50 px-3 py-2 font-mono text-[11px] text-zinc-500 break-all">
          discover {args.join(" ")}
        </p>
      )}

      {error && <p className="text-sm text-rose-400">{error}</p>}

      {!hideSubmit && onSubmit && (
        <button
          type="button"
          onClick={() => void submit()}
          disabled={submitting}
          className="rounded-md bg-emerald-400 px-4 py-2 text-sm font-semibold text-black hover:bg-emerald-300 disabled:opacity-50"
        >
          {submitting ? "Starting…" : submitLabel}
        </button>
      )}
    </div>
  );
}
