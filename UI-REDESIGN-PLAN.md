# MEVSCOPE UI Analysis & Improvement Plan

**Date:** 2025-09-15
**Source:** https://mev-forge.lovable.app
**Target:** mev-scout

---

## MEVSCOPE Pages Reviewed

| Page | Status |
|------|--------|
| Configure | Content-rich, main configuration page |
| Pipeline | Empty state placeholder |
| Opportunities | Empty state placeholder |
| Report | Empty state placeholder |
| Backtest | Equity curve, drawdown, daily opportunity charts |
| History | Empty state placeholder |

---

## Key UI Patterns Worth Adopting

### 1. Top-level Status Bar / KPI Strip

MEVSCOPE shows live status in **two** places:
- Header: `API` pill · `IDLE`/`RUNNING` · persistent **New simulation** CTA
- **Sidebar footer:** `BLOCK SYNC` · `ETH $…` · `RPC …ms` (monospace)

**Our current header** only shows a health badge + chain selector; sidebar has no footer metrics.

**Action:** Add sidebar-footer KPIs (block # now; price/latency when backend exists) plus an explicit IDLE/RUNNING pill and persistent Run CTA in the header.

---

### 2. Numbered Section Headings with Dots

Sections formatted as:
```
01 · Block window
02 · Strategies
03 · Builders & Routing
```

This gives the configure page a structured, sequential feel.

**Action:** Replace flat form layout in Config page with numbered/sectioned layout.

---

### 3. Strategy Cards with Category Grouping + Toggle Pills

Each strategy has:
- A short code tag (`SKIM`, `SYNC`, `ARB`, `SANDWICH`)
- A description
- An "Expected trace" link
- An on/off toggle
- Category headers with count (`1 / 6 on`)

**This is far better than our current flat toggle list.**

**Action:** Adopt a card-based strategy selector with category headers and individual toggles.

---

### 4. Builder Cards with Live Bar Charts

Flashbots 40%, Titan 26%, Beaver 18% — shown as a horizontal bar breakdown with latency stats (`lat 22 ± 6 ms · prior 36 %`).

**Action:** Show builder/relay stats this way in Config or Dashboard.

---

### 5. Latency Budget Visualization

Horizontal stacked bar showing:
```
Ingest 18ms → Decode 8ms → Simulate 18ms → Sign 2ms → Submit 20ms
```
vs total block time budget (12000ms).

**Action:** Add pipeline timing visualization to RunBacktest or Live pages.

---

### 6. Auto-resolved Economics Panel

Gas price, builder tip, min profit threshold — all marked `Auto` with no manual overrides. Computed values with a Refresh button.

**Action:** Replace editable fields with auto-computed display where appropriate.

---

### 7. API Request Copy Block

JSON code block with a Copy button showing the equivalent CLI/API request.

**Action:** Add to Config or Results pages.

---

### 8. Concise Empty States with Single CTA

"Configure a run and execute it to see the pipeline" → "Start now" button.

**Action:** Simplify empty states in Jobs, Results pages.

---

### 9. DEX Venue Chips

Uniswap v2, v3, SushiSwap, Curve, Balancer — selectable chips with protocol type and truncated address.

**Action:** Replace table rows with compact chip-style selectors.

---

### 10. Backtest Charts

The Backtest page has 3 charts:
- Equity curve
- Drawdown
- Daily opportunities

**We have recharts in our dependencies but never use it.**

**Action:** Add equity curve, drawdown, and daily opportunity count charts to backtest results.

---

## Things We Do Better (Keep Ours)

| Feature | Why |
|---------|-----|
| Left sidebar navigation | Same pattern as MEVSCOPE (they also use a left sidebar + icons). Keep ours; improve active-state + footer KPIs to match their polish |
| DataTable with sort | MEVSCOPE has no tables; we have a proper sortable DataTable |
| Drawer for opportunity detail | Our OpDrawer pattern is good |
| Log viewer | MEVSCOPE has no pipeline log viewer |
| Stage timeline | Our progress visualization is more detailed than MEVSCOPE's empty Pipeline page |
| Real backend wiring | MEVSCOPE Configure is demo-dense (45 strategies, builder shares, risk knobs). Prefer their *visual language*, not every fake control |

---

## Summary of Recommendations

| Priority | Idea | Pages Affected |
|----------|------|----------------|
| **High** | Use recharts for equity curve, drawdown, daily opps in backtest results | RunBacktest, Results |
| **High** | Strategy card grid with category groups + toggle pills + short codes | Config |
| **High** | Builder/relay stats with horizontal bar breakdown | Config, Dashboard |
| **High** | Design-token remap in `index.css` (slate-tinted palette + neon primary + Inter/JetBrains Mono) | All pages |
| **High** | Sidebar footer KPIs + neon brand + green active-nav rail | Layout |
| **High** | Segmented controls for small option sets; capability honesty banner | Config, RunBacktest |
| **High** | Primary CTAs = black text on neon green | All pages |
| **Medium** | Latency budget stacked bar visualization | RunBacktest, Live |
| **Medium** | Add ETH price + block number + RPC latency (block # now; price/latency backend-gated) | Layout |
| **Medium** | Numbered section headings for Config page | Config |
| **Medium** | "Auto-resolved" economics panel pattern | Config |
| **Medium** | Sticky JSON-preview rail + persistent Run CTA + dirty-state + mobile stack | Config |
| **Medium** | Backtest KPI cards (Net P&L / hit rate / max DD) + `Re-run`/`Export` actions | RunBacktest, Results |
| **Medium** | Terminal run-summary panel echoing CLI args | RunBacktest, Results |
| **Medium** | Header IDLE/RUNNING cluster + breadcrumb; empty states with large icons | Layout, All pages |
| **Medium** | Chart color tokens (equity green / DD magenta / opps cyan) | RunBacktest, Results |
| **Low** | API request copy block (JSON + copy button) | Config, Results |
| **Low** | DEX venue chips with protocol tags | Config |
| **Low** | Concise empty states with single CTA button | All pages |
| **Low** | Live estimates (est. blocks/txs) under window selector | Config |
| **Low** | Copy-to-clipboard on addresses/hashes | All pages |
| **Low** | "Expected trace" / Advanced disclosure per strategy; disabled-but-visible venues | Config |
| **Low** | Do-not-copy: aggregator grid, fake adversarial knobs, 45-strategy catalogue | — |

---

## Current mev-scout UI Architecture

- **Framework:** React 19 + TypeScript + Vite 6
- **Styling:** Tailwind CSS v4 (CSS-first config, inline utility classes)
- **Routing:** react-router-dom v7 (createBrowserRouter)
- **State:** Component-local + custom `usePolling` hook
- **UI Library:** Hand-rolled components (no shadcn/MUI/Radix)
- **Tables:** Custom `DataTable` component with sort
- **Charts:** recharts in dependencies but unused
- **Theme:** Dark-only (`color-scheme: dark`, `bg-zinc-950`)
- **Palette:** Zinc surfaces, sky (primary), emerald (profit), rose (loss), amber (warning)
  → **being replaced** by the MEVSCOPE-derived token set below.

---

## Additions (review pass — 2025-09-15)

### A. Extracted MEVSCOPE design tokens (the single biggest visual gap)

The plan lists *patterns* but not the *palette itself*. These were pulled from
their bundled CSS (`assets/styles-t8ny5G8u.css`) and are the main reason their
UI reads better at equal layout quality:

| Token | Value | Role |
|-------|-------|------|
| `--page` | `#0a0b0d` | page background (deeper than zinc-950) |
| `--panel` | `#111318` | card/panel background |
| `--surface` / `--surface-2` | `#181c24` / `#0d1017` | elevated / inset surfaces |
| `--line` | `#1e2330` | **slate-tinted** hairline border (not neutral gray) |
| `--ink` / `--ink-dim` / `--ink-mute` | `#e2e8f0` / `#64748b` / `#334155` | 3-step text hierarchy |
| `--acc-green` | `#00ff94` | **primary**: CTAs, active nav, live dots, profit |
| `--acc-red` | `#f87171` | destructive / drawdown |
| other accents | amber `#f59e0b`, blue `#38bdf8`, purple `#a78bfa`, orange `#fb923c`, cyan `#22d3ee`, pink `#f472b6`, teal `#2dd4bf` | per-venue / per-strategy coding |
| radius | `0.625rem` | card radius |
| scrollbars | 8px, track = page, thumb = `--line` | themed |

**Action (cheap & high-leverage):** define these in `web/src/index.css` via a
Tailwind v4 `@theme` block and **remap the zinc/emerald scales** onto them
(zinc-950→page, zinc-900→panel, zinc-800→line, zinc-500→ink-dim, …,
emerald-400→`#00ff94`). Every existing component re-skins with zero
class-name rewrites; only primary CTA buttons need an explicit swap
(black text on neon green, like their `--primary-foreground: #000`).

### B. Typography & micro-polish

- Fonts: **Inter** (sans) + **JetBrains Mono** (mono) — mono for every number,
  address, hash and label; pair with `tabular-nums` (we already do this).
- Uppercase micro-labels with wide tracking for section/field labels.
- Custom keyframes: `blink` (live indicators) and `pulse-dot` (running jobs).
- Copy-to-clipboard on all addresses/hashes/JSON (truncated `0x7a25…488D`,
  full value on hover/copy).

**Action:** load both Google Fonts + scrollbar/keyframe rules in `index.css`;
add a tiny `Copyable` component for hex strings.

### C. Sticky action rail on Config (upgrade of pattern #7)

MEVSCOPE's Configure page is not just "a JSON block with Copy": it is a
**sticky right rail** visible while scrolling all sections, containing a live
JSON preview of the request that updates with every field change, plus the
persistent **"Run simulation →"** CTA.

**Action:** two-column Config layout — numbered sections scroll on the left;
the right rail sticks (`lg:sticky lg:top-20`) with: live `putConfig` body
preview, Copy button, Save CTA, and a "Run backtest" shortcut.
Priority: **Medium**.

### D. Live estimates while configuring

Their Block window section recomputes as you type: `Est. blocks 216,000 ·
Est. transactions 38,880,000 · Date range Aug 16 → Sep 15`, and the RPC field
has an inline **"Test → 42ms"** ping button.

**Action:** derive estimates client-side (blocks ≈ days × 7200 on 12s chains;
txs ≈ blocks × ~180) and show them under the window selector. The RPC ping
needs a backend probe endpoint — backlog item, not UI-only.

### E. Backtest KPI cards + presets (upgrade of pattern #10)

Charts alone under-sell it. Their Backtest page pairs charts with:
- 4 KPI cards with contextual subtitles: `Net P&L $21,036.58 / 90d window`,
  `Hit rate 54.4% / profitable days`, `Sharpe 3.05 / annualised, rf=0`,
  `Max drawdown −2.99% / peak-to-trough`
- Config strip presets: **Cons / Bal / Agg** + `Seed` field
- Actions: **Re-run** and **Export**
- A terminal-style run summary (`$ mevscope backtest --chain ethereum …`)

**Action:** add a `sub` prop to our `StatCard`; compute hit-rate / max-DD /
cumulative profit from run opportunities + PnL; use the `TerminalPanel`
component echoing the equivalent `mev-scout run …` args; Export = CSV of
opportunities (client-side blob download).

### F. Data-availability caveats (reality check before implementing)

Several plan items are constrained by what our API actually exposes — flag
these instead of faking data in the UI:

| Plan item | Data status in mev-scout |
|-----------|--------------------------|
| ETH price in top bar | **Not available** — `HealthResponse`/`SyncResponse` expose no price. Needs a backend endpoint or external feed. |
| RPC latency in top bar | **Not available** — only `rpc_provider_count`. Needs a backend probe (ties into D's "Test" button). |
| Current block number | **Available now** via `GET /sync` (`explorer_head` / `cache_head`). |
| Builder/relay share bars (#4) | **No builder data in API.** Backlog a backend collector, or render placeholder bars clearly marked "coming soon". |
| Strategy cards (#3) | Backend supports exactly **5** strategies: `two_hop_arb`, `jit`, `jit_arb`, `sandwich`, `liquidation` (config stores a comma-separated string in `SanitizedConfig.backtest.strategies`). The card grid should toggle these 5 and write back the CSV — not invent MEVSCOPE's 45. |
| Equity/drawdown charts (#10) | No time-series endpoint; derive client-side from `RunDetail.opportunities` (cumulative `expected_profit` by block). Fine for a first pass. |
| Latency budget bar (#5) | `ProgressResponse` has stage/done/total/elapsed, but no per-phase ms — approximate from stage transitions or backlog. |

### G. Empty-state CTA mapping (pattern #8, concretised)

| Page | Empty message | CTA → target |
|------|---------------|--------------|
| Jobs | No jobs yet | "Run a backtest" → `/run` |
| Results | No runs yet | "Run a backtest" → `/run` |
| Live | Not started | "Start live" (in-page) |
| Pools | No pools indexed | "Start indexer" → creates `explorer index` job |

**Action:** extend `DataTable` with an `action?: ReactNode` prop rendered
inside the dashed empty box, and reuse it in page-level empty states.

### H. Also worth noting

- MEVSCOPE code-splits each Configure section into its own chunk
  (`SectionCard-*`, `Field-*`, `TerminalPanel-*`). Once our Config grows,
  consider `React.lazy` for the heavier sections.
- A breadcrumb in the header (`Ethereum / Configure`) is a cheap orientation
  aid — add it under our existing header.
- The "Auto" badges on derived values (pattern #6) pair well with read-only
  fields: keep our read-only inputs, add the badge + a Refresh action that
  re-polls `/config`.

---

## Implementation status (2025-09-15)

| Item | Status |
|------|--------|
| Design tokens in `index.css` (zinc/emerald remap, Inter + JetBrains Mono fonts, scrollbars, keyframes) | ✅ done |
| Sidebar neon brand + active nav accent bar + footer KPIs (block sync, chain, rpc) | ✅ done |
| Header IDLE/RUNNING pill + persistent "New simulation" CTA (black on neon) | ✅ done |
| Page breadcrumb (`chain / page`) | ✅ done |
| Primary CTA buttons: black text on `#00ff94` across all pages | ✅ done |
| Focus borders + checkboxes remapped to emerald-400 | ✅ done |
| `SectionCard` (numbered sections) | ✅ done |
| `TerminalPanel` (CLI echo box with copy button) | ✅ done |
| `RunCharts` (equity curve + drawdown + daily opps, recharts, lazy-loaded) | ✅ done |
| `StatCard` sub prop | ✅ done |
| `DataTable` empty-state action prop | ✅ done |
| Empty states with CTA links (Jobs → /run, Results → /run, Pools → Start indexer) | ✅ done |
| Config: numbered sections + strategy toggle cards + honesty banner + sticky JSON rail (dirty state + copy) | ✅ done |
| Results: KPI cards (Net P&L / hit rate / max DD) + RunCharts + TerminalPanel | ✅ done |
| RunBacktest: KPI summary + RunCharts + TerminalPanel + TerminalPanel CLI echo | ✅ done |
| Code-splitting: `RunCharts` lazy via `React.lazy` + `Suspense` | ✅ done |
| ETH price / RPC latency / builder stats | ⛔ blocked on backend endpoints |
| Per-stage latency budget bar (Ingest→Decode→Simulate→Sign→Submit) | ⛔ blocked — ProgressResponse has no per-phase ms |

---

## Additions (second review pass — live site walkthrough)

Reviewed live at https://mev-forge.lovable.app (Configure, Pipeline, Opportunities,
Report, Backtest, History). Plan items below are **suggestions only** — no code
changes in this pass.

### I. Corrections to earlier assumptions

1. **MEVSCOPE uses a left sidebar too** (not top-nav-only). Brand + chain selector
   sit at the top; nav links have line icons; **live KPIs live in the sidebar
   footer** (`BLOCK SYNC` · `ETH $…` · `RPC 42ms`), not only in a top strip.
2. **Don't clone Configure density.** Their page is a product demo: 45 strategies,
   builder market-share cards, aggregator routers, mempool tiles, risk/adversarial
   knobs. Many controls are illustrative. For mev-scout, copy the **visual system**
   and the patterns that map to real config/API fields; leave the rest as
   backlog or "coming soon" placeholders.

### J. Layout / chrome (high leverage, under-specified before)

| Idea | Detail | Priority |
|------|--------|----------|
| Sidebar footer KPI stack | Move (or mirror) sync head / health / latency under the nav, monospace labels. Matches MEVSCOPE and frees header space. Block # available now via `/sync`; price/latency still backend-blocked. | **High** |
| Neon brand mark | Product name in accent green in the sidebar (their `MEVSCOPE` treatment). Our `mev-scout` + muted `ui` pill currently reads too quiet. | **High** |
| Active nav: left accent bar + green text | Replace sky-blue active state with neon primary + thin left rail (matches their Configure highlight). | **High** |
| Header status cluster | Always show: API/health pill · `IDLE` / `RUNNING` pulse · primary **Run** CTA (`New simulation` equivalent). We already have a job pill when running; make IDLE explicit too. | **Medium** |
| Page breadcrumb | `CHAIN / Page` under header (already listed Low — bump usefulness after seeing it on every page). | **Medium** |
| Empty states with large faded icons | Pipeline/Opportunities/Report/History all use a big muted icon + one sentence + one CTA. Upgrade ours beyond dashed text boxes. | **Medium** |

### K. Configure interaction patterns (beyond cards + sticky rail)

| Idea | Detail | Priority |
|------|--------|----------|
| Segmented controls for ≤5 options | Window mode (`Last N days` / `Block range` / `Single block`), tip mode, gas model, flash-loan provider, Cons/Bal/Agg — prefer segmented pills over `<select>` where the option set is small. | **High** |
| Strategy accordion + `N / M on` | Category headers collapse; show count of enabled strategies. With only 5 strategies we can use 1–2 groups (e.g. Arb / Positional / Liq) still — keeps the UI scannable. | **High** |
| Capability honesty banner | Amber callout: which strategies actually run against the backend. Mirrors their "API mode: only arb, jit…" note. Essential so we don't imply 45 strategies. | **High** |
| Short strategy codes + 1-line blurb | `ARB`, `JIT`, `JITARB`, `SANDWICH`, `LIQ` tags + one-sentence description on each card (we already planned cards; make copy explicit). | **Medium** |
| "Expected trace" / docs affordance | Optional expand under each strategy showing a sample call sequence or link to docs — educational, low cost, no backend. | **Low** |
| Nested "Advanced" per strategy | Keep proximity window / max candidates / flash-loan overrides behind an Advanced disclosure so the main grid stays calm. | **Medium** |
| Unavailable options stay visible but disabled | Grayed builder/venue chips with tooltip ("not on this chain" / "coming soon") instead of hiding — teaches the surface area without faking data. | **Low** |
| Dirty-state on sticky rail | When form ≠ last saved config, badge the JSON rail `Unsaved` and disable or warn on Run until Save. | **Medium** |
| Sticky rail collapses on small screens | Stack JSON + Run **below** sections on `<lg`; don't squeeze a two-column layout on mobile. | **Medium** |
| Section meta footers | Under forms: `3 strategies active · block time 12s` (derive from chain config). Cheap orientation cue used on their Backtest strip. | **Low** |

### L. Visual / motion polish not yet listed

| Idea | Detail | Priority |
|------|--------|----------|
| Chart color tokens | Equity = `--acc-green`, drawdown = magenta/rose (`#f472b6` / `--acc-red`), daily opps = cyan (`#22d3ee`). Codify in theme so Recharts series stay consistent. | **Medium** |
| Primary CTA = black on neon | Their Run / New simulation buttons use black label on `#00ff94`. Enforce for all primary actions (not white-on-green). | **High** |
| Focus / section accent | Subtle slate-blue or green hairline glow on the section currently in view (they use a soft blue edge on Block window). Optional; don't overdo glow. | **Low** |
| Nav / status icons | Thin stroke icons next to sidebar labels (and empty states). Improves scanability once we have 8 nav items. | **Low** |
| `blink` / `pulse-dot` usage map | Live: RPC healthy = solid green; job running = amber pulse; IDLE = muted gray dot. Already have keyframes — define when each applies. | **Low** |

### M. Backtest / Results extras

| Idea | Detail | Priority |
|------|--------|----------|
| Config strip above charts | Window days · starting capital · profile presets · seed · Re-run / Export in one row (they do this well). Maps to RunBacktest + Results. | **Medium** |
| KPI subtitle always contextual | e.g. `90d window`, `profitable days`, `peak-to-trough` — already in E; keep as a hard requirement for StatCard `sub`. | **Medium** |
| Daily opportunities chart | Third chart on Backtest; plan mentioned it — confirm we ship all three, not only equity + drawdown. | **Medium** |
| Export = opportunities CSV | Client-side blob; no backend needed. Pair with Re-run that re-POSTs the same job args. | **Medium** |

### N. Explicit non-goals / do-not-copy (important)

Avoid spending cycles on MEVSCOPE surfaces that are mostly decorative relative to our API:

- Builder market-share cards with live `%` / `prior` (no data — already flagged in F).
- DEX aggregator router grid + share sliders.
- Mempool source tiles (MEV-Share, Merkle, CEX WS…) unless Live gains real feeds.
- Risk & Competition adversarial / censored-builder / reorg knobs unless the runner consumes them.
- 45-strategy catalogue and cross-domain / NFT / governance niches.

If we want the *look* of those sections later, ship a single "Roadmap" panel listing unsupported capabilities rather than disabled fake controls everywhere.

### O. Suggested implementation order (refined)

1. **Visual baseline:** tokens + fonts + neon CTAs + sidebar brand/active state *(partially done)*.
2. **Chrome:** sidebar footer KPIs (block sync now), IDLE/RUNNING + persistent Run, breadcrumb, empty-state icons.
3. **Config IA:** numbered sections + segmented window controls + strategy cards/accordion + honesty banner + sticky JSON rail (with dirty state + mobile stack).
4. **Results/Backtest:** KPI row + 3 charts + terminal summary + Re-run/Export.
5. **Backend-gated:** ETH price, RPC ping, builder stats — only after endpoints exist.

### P. Open questions for product

- Should Config and Backtest Runner stay separate pages, or merge toward MEVSCOPE's "Configure → Run" single flow with Results/Jobs as outcomes?
- Do we expose block-range / single-block window modes in the UI even if the runner today is mostly days/block-span via CLI args?
- Brand string: keep `mev-scout` or introduce a display name closer to MEVSCOPE's weight?
