# پلن جامع پوشش شکافها: Atomic Capital-Free DeFi Arbitrage Engine

> **تاریخ مبنا:** 2026-11-21  
> **منابع پلن:** (۱) **نتایج** تاکسونومی ۱۳استراتژی مدل دیگری طبق خلاصهای که از گفتگو در اختیارم بود (نه متن کامل لیست ۱۳گانه — رجوع شود به §2.4)، (۲) ۸ تصحیح باقیمانده و تمام تصمیمات حذف/Expansion همان گفتگو، (۳) بررسی من از کد واقعی ماژولهای `core/src/explorer/**`, `core/src/jobs/live.rs`, `core/src/types/opportunity.rs`, `core/src/explorer/store/**` و مستندات `docs/`، (۴) بازبینی دوم با متن کامل `live.rs` (رجوع شود به §1.3).  
> **هدف:** تبدیل تمام مواردی که «گفته شد ولی در کد جا نیفتاد» به کاربستههای قابلاجرا، با تعیین فایل مقصد، معیار پذیرش و ترتیب وابستگی.  
> **وضعیت:** پیادهسازی مرحلهای آغاز و بخش عمدهٔ آن انجام شد (به تاریخ 2026-11-23). عمق بررسی کد: **ساختاری/هدفمند** (فایلها و توابع کلیدی خوانده شدند)، نه audit خطبهخط تمام submoduleها. جزئیات پیشرفت در §0.1.

---

## 0. اصول حاکم (Non-negotiable)

| اصل | قرار |
|---|---|
| دامنه | **DeFi-only**. CEX–DEX حذف شد. |
| اتمیّت | **یک تراکنش EVM**: Borrow → Callback → تمام Swapها → Repay(capital+fee) → Net-Profit Validation؛ هر شکست = `REVERT` کامل. |
| سرمایه | Capital-Free یعنی funding از Flash Loan / Flash Swap؛ **gas و priority fee هنوز پرداخت میشوند** و نباید «capitalless» تفسیر شوند. |
| زنجیره | Avalanche C-Chain، `chain_id = 43114`. |
| استک | Rust + Alloy + REVM؛ Simulation-First، Protocol-Agnostic، Modular/Adapter-Based، Graph-Based. |
| سه موتور | **Discovery**، **Atomic Funding & Execution**، **Simulation & Profit Optimization**. |
| لایه funding | Balancer V2، Aave V3، Uniswap V3، LFJ LB، flash swapهای V2-compatible و احتمالاً forkهای V3. **Flash Loan لایه funding است، نه خانواده آرب.** |
| Aggregator | 1inch / KyberSwap / OpenOcean / ParaSwap(Velora) = **Routing Layer**، نه Pool source. Odos حذف شد. |
| PBS | Avalanche C-Chain به طور پیشفرض MEV-Boost/PBS ندارد → `priority_fee`، نه `BuilderTip`؛ `builder_bid: Option<Null>`. |

### 0.1 وضعیت پیادهسازی (2026-11-23)

| Section | موارد اعمالشده | وضعیت |
|---|---|---|
| C-05 / M-03 / M-06 / M-07 | فرمولcanonical `compute_net_profit`، نرخ AVAX هم-block + `manipulation_risk`، canonical-ID بعد از tag stamping، quote context (nonce/balance/allowance) و gate readonly | ✅ |
| A-03 / A-04 / C-03 | premium لحظهای Aave/Balancer با fallback، sweep کاندید-L (پیشفرض خاموش) با تگ optimizer، Split-Route تحت محور optimizer | ✅ |
| A-07 / A-08 / A-09 | تگ `out_of_core` (محور Scope)، GMX/sAVAX/Pendle با تگ forensic حفظ و از sim/exec/ranking خارج (skip شمارششده) | ✅ |
| C-07 / C-08 / M-09 | re-quote pending + `quote_expires_at` + تست پذیرش، متریکهای quote/competition در `paper_sessions` و خروجی session | ✅ |
| C-01/C-02/C-04/C-06/M-01/M-02/M-04/M-05/M-08/M-10 | پیشتر در کد پیاده یا اسنادشان اصلاح شدهبود | ✅ (پیشین) |
| §2.4 جدول ۱۳استراتژی | نیاز به متن اصلی لیست دارد | ⏸ منتظر ورودی کاربر |

> **توجه:** Split-Route بهصورت پیشفرض خاموش است تا زمانی که تخصیص کسری دومین مسیر در `MevOpportunity` و exec planner قابل نمایش/اجرا شود. تگ `optimizer:split_route` فقط محور attribution است.

---

## 1. مرز دامنه (Scope Guards)

### 1.1 خارج از core (حذف یا Strategy جداگانه)

| مورد | دلیل | جایگاه |
|---|---|---|
| Cross-chain / Bridge Arbitrage | تأیید نهایی (confirmation) داخل یک تراکنش EVM قابل انتظار نیست؛ HTLC/Intent/Escrow اتمیّت همان EVM را حل نمیکند | حذف |
| CEX–DEX | خارج از DeFi | حذف |
| Perpetual / Funding / GMX Spot–Perp | GMX دو تراکنش است (`request` + `keeper execution`) → نقض اتمیّت | حذف |
| Sandwich / JIT / Liquidation / Yield-Rate | خارج از core؛ Liquidation **Strategy جداگانه** با executor قابلتوسعه میماند | `jit_accounting.rs`, `benqi_liq.rs` با تگ out-of-core |
| Backrun / Oracle–AMM | «قابل توسعه / قابل بررسی»، نه core | لایه expansion |
| Split-Route | Optimizer/Execution mode است، **نه** `route_shape` | محور optimizer |

### 1.2 آنچه از قبل درست است — دوباره نسازیم

| وجود دارد | محل |
|---|---|
| `priority_fee_gwei` در gas config | `types/strategy.rs:239`, `config/settings.rs:52`, `config/validation.rs` |
| `FlashLoanProvider` + `fee_rate_bps()` | `config/settings.rs:66`, `types/strategy.rs:307` |
| `DetectionPath::{Replay, Pending, LogOnly}` | `types/opportunity.rs:12` |
| `LoopStats.total_pending_txs` | `jobs/live.rs:1116` |
| `revert_gas_wei`, `state_drift`, `reconcile_sim_vs_exec` | `paper/types.rs:291`, `paper/ledger.rs:545`, `paper/recon.rs` |
| `MevEvent.flashloan_fee_wei` | `explorer/types.rs:214` |
| Backrun به صورت **tag** بر `ArbAtomic` (نه MevKind جدا) | `explorer/types.rs:124`, `classify/causal.rs` |
| `is_forensic_action()` برای جدا نگه داشتن P&L LP/liq | `explorer/types.rs:70` |
| `RejectReason::BuilderTip` (معنای attribution، نه gas bidding) | `explorer/reject.rs:40` — نام را نگه دار، کامنتش را اصلاح کن |

### 1.3 بازبینی دوم با متن کامل `live.rs` — اصلاحاتی روی پلن

| مورد | آنچه واقعاً در کد هست | اصلاح اعمالشده |
|---|---|---|
| `revert_gas_wei` | `render_ledger_summary` **از قبل** خط `exec:` شامل `revert gas`، `loss` و `setup` را چاپ میکند؛ `recon:`، `caps:`، `blocks:` و جدولهای `by strategy`/`by tag` هم موجوداند | **M-09 تصحیح شد:** تنها سه خط جدید لازم است: funding fee، quote expiry/drift و competition loss. بقیه را دوباره نسازیم. |
| `SimPolicy.fee_bps` | `resolve_sim_policy` (خط 143) fee_bps را از `FlashLoanProvider::fee_rate_bps()` میگیرد | **C-05 تصحیح شد:** fee در مسیر sim لحاظ است ولی نه روی `MevOpportunity` و نه بهعنوان جزء جداگانه `LedgerResult`. WP1 فقط «صریحسازی + کسر revert gas + فیلد opportunity». |
| `capture_pending` | `with_capture_pending(config.backtest.capture_pending)` در `LiveContext::new` وصل است | **C-07 تصحیح شد:** feed وجود دارد؛ تنها **Re-quote trigger** و `quote_expires_at` ناقصاند. |
| `ArbTagPolicy.flash_financed` | `flash_financed: ledger.sim.flash_loan` به policy تشخیص داده میشود | **M-05 تصحیح شد:** پیش از تعریف محور funding، تگ emitted توسط `mev/detectors` بررسی شود؛ بخشی از محور ممکن است از قبل وجود داشته باشد. |
| `GasConfig.winning_bid_premium`, `percentile_gas_price` | در `LiveContext::new` ساخته میشوند | مبنای پیادهسازی متریک **same-block competition loss** (C-08). |
| GMX | `gmx_market_tokens` و `gmx_event_emitters` هنوز در `ArbTagPolicy`اند | **A-07 جدید:** حذف یا تگ صریح out-of-core (GMX دو تراکنشه است). |
| Pendle | `VenueRouters.pendle_router` در exec context هست | **A-08 جدید:** Pendle PT/YT = yield/rate → disposition خارج از core. |
| sAVAX | `refresh_savax_rate` + `SavaxRoute` فعال در live | **A-09 جدید:** وضعیت rate-arb مشخص شود (قرار core: Yield/Rate خارج از core). |
| رزیلینس موجود | تازهسازی tip، `auto_refetch_gaps`، `widen_span`، `MAX_CONSECUTIVE_FAILURES = 5`، `reconcile_sim_vs_exec`، `aggregate_fills` | دوباره پیشنهاد نشود؛ WP4 تنها لایه freshness/risk را اضافه میکند. |

---

## 2. رجیستری شکافها (Gap Register)

### 2.1 هشت تصحیح باقیمانده از گفتگوها

| ID | شکاف | وضعیت فعلی در کد | فایل(های) مقصد | P | تست پذیرش |
|---|---|---|---|---|---|
| **C-01** | Odos حذف شود → ParaSwap/Velora | `decode/aggregator.rs:12,95,245`, `types.rs:336`, `chain/events.rs:529`, `pool/discovery/*` هنوز Odos دارند | همانها + `decode/tests.rs:827` | P0 | `grep -ri odos core/src` فقط در کامنت «تاریخی/حذفشده» باقی بماند؛ تست `paraswap_velora_swap_decodes` اضافه شود |
| **C-02** | `BuilderTip` → `priority_fee`؛ `builder_bid` nullable | config درست است ولی اسناد/خروجیها شفاف نیستند | `docs/ARCHITECTURE.md`, `explorer/reject.rs` (کامنت), `types/strategy.rs`, `explorer/store/facts.rs:265` | P0 | مستندات فقط `priority_fee` بگویند؛ `builder_bid` در schema قابل `NULL` باشد |
| **C-03** | Split-Route از `route_shape` خارج → optimizer | ردپای `route_shape` در `core/src` یافت نشد؛ باید در لایه docs/optimizer تثبیت شود | `strategy_tags.rs`, `docs/mev_strategies.md`, `docs/ARCHITECTURE.md` | P0 | تگ `split_route` تحت محور `optimizer` تعریف شود، نه venue/route |
| **C-04** | Uniswap V2 رسمی در Avalanche نیست → forkها | decoderها عمومیاند ولی مستندات «Uniswap V2» میگویند | `docs/mev_strategies.md`, `pool/discovery/*`, `strategy_tags.rs` | P1 | Pangolin V2 / SushiSwap V2 به عنوان V2-compatible ذکر شوند |
| **C-05** | NetProfit بدون double-count SwapFees/Slippage | `expected_profit` قبل از gas است؛ «net» صریحی وجود ندارد | `explorer/profit.rs`, `paper/types.rs`, `jobs/live.rs:613` | P0 | تست: `gross=100, gas=10, flash=2, revert=3 ⇒ net=85` و feeها دوباره کسر نشوند |
| **C-06** | ۱۱ فیلد_missing Opportunity | `types/opportunity.rs:60` فقط تا `details` میرسد | `types/opportunity.rs`, `store/schema.rs:633`, `store/mod.rs:181`, `store/query.rs:76`, `results.rs:15` | P0 | serde round-trip + migration idempotency + ستونهای جدول |
| **C-07** | Pending-Tx Feed → Detect Large Swap → Trigger Re-quote | `DetectionPath::Pending` و `total_pending_txs` وجود دارند ولی **Re-quote trigger وجود ندارد** | `jobs/live.rs:1171,1002`, `fetch/*`, `mev/detectors/*` | P1 | تست: یک swap بزرگ pending → opportunity با `detection_path=pending` + `quote_expires_at` |
| **C-08** | متریکهای Quote-drift/expiry + same-block competition loss | فعلاً فقط reject ladder جداگانه است | `paper/store.rs`, `store/schema.rs`, `dashboard/reduce.rs`, `dashboard/csv.rs`, `live.rs:720` | P2 | دو متریک در خروجی روزانه/session و HTML نمایش داده شوند |

### 2.2 پیشنهادات بررسی من

| ID | شکاف | وضعیت فعلی | فایل(های) مقصد | P |
|---|---|---|---|---|
| **M-01** | `funding_source` به ازای **هر فرصت** (فعلاً only `ResultsFile.flash_loan_provider`) | `opportunity.rs:292` | `types/opportunity.rs`, `results.rs`, `store/schema.rs` | P0 |
| **M-02** | مهاجرت جدول `opportunities`/`rejected_candidates` برای فیلدهای جدید | جدول V1 تا `canonical_id` میرسد (`schema.rs:653`) | `store/schema.rs` (Migration Vn), `store/mod.rs`, `store/query.rs` | P0 |
| **M-03** | `GasQuote {basefee, priority, native_usd, sim_block, manipulation_risk}`؛ gas به AVAX با همان نرخ sim | `resolve_native_price` (live.rs:220) نرخ CoinGecko میدهد، نه نرخ simulation | `explorer/pricing.rs`, `jobs/live.rs:220`, `replay/replayer.rs:492` | P0 |
| **M-04** | دلایل رد: `QuoteExpired`, `QuoteDrift`, `FundingUnavailable`, `RepayFailed`, `AllowanceMissing`, `BalanceInsufficient`, `NonceConflict`, `SimFailed`, `RiskFlagBlocked` | فقط `StateStale`legacy وجود دارد (`reject.rs:27`) | `explorer/reject.rs`, `results.rs:114`, `dashboard/reduce.rs` | P1 |
| **M-05** | تگهای چندمحوره: family / mechanism / venue / **funding** / **execution** / **optimizer** | فقط `rate_provider_staleness` و تگهای پراکنده (`strategy_tags.rs:39`) | `explorer/strategy_tags.rs` | P1 |
| **M-06** | `underlying_pools_hash` + `router_version` + funding در canonical ID | `compute_canonical_id` فقط strategy+pools+tokens (`opportunity.rs:210`) | `types/opportunity.rs`, `explorer/canonical.rs` | P1 |
| **M-07** | Live quote-gate قبل از ledger؛ ثبت `sim_block/basefee/nonce/balance/allowance` | `process_pass` مستقیم به `update_ledger` میرود (`live.rs:1284-1286`) | `jobs/live.rs:566,1002,1171,613` | P1 |
| **M-08** | Live flagها: `--funding-provider`, `--max-quote-age-ms`, `--require-risk-clear`, `--pending-backrun` | `LiveOpts` (live.rs:34) فقط sim/exec/balance دارد | `jobs/live.rs:34`, `config/settings.rs:1027` | P1 |
| **M-09** (تصحیحشده در §1.3) | Ledger: تفکیک fills/attempts و نمایش funding fee + expired + drift (+ competition loss) | `revert_gas/loss/setup`، `recon`، `caps` و جدولها **از قبل در `render_ledger_summary` (live.rs:720) هستند** | `jobs/live.rs:720`, `paper/ledger.rs:545` | P1 |
| **M-10** | Explorer سمت realization: funding/router/pool-hash از لاگها + failed-arb attempts | `decode/flash_loan.rs`, `ingest.rs` funding را به MevEvent نمیتپد | `explorer/decode/*`, `explorer/classify/arb.rs`, `explorer/ingest.rs`, `explorer/dashboard/*` | P2 |

### 2.3 تصمیمات معماری که هنوز در مستندات تثبیت نشدهاند

| ID | مورد | محل تثبیت |
|---|---|---|
| **A-01** | Flash Loan = Funding Layer، خانواده آرب نیست | `docs/ARCHITECTURE.md` + `strategy_tags.rs` |
| **A-02** | Balancer Vault `0xBA122...`؛ fee **runtime** خوانده شود؛ ریسک reentrancy وقتی خود pool هم Balancer است | `docs/ARCHITECTURE.md`, `config/*`, `strategy_tags.rs` |
| **A-03** | Aave premium / Balancer fee / Uniswap V3 / LFJ in-kind fee → live state، نه hard-code | `exec/*`, `mev/detectors/*`, `profit.rs` |
| **A-04** | REVM باید **کل جریان** شامل Borrow/Callback/Swap/Repay/Validation + **revert path** را شبیهسازی کند و `L`های کاندید را امتحان کند | `paper/sim.rs`, `exec/revm_backend.rs`, `mev/detectors/*` |
| **A-05** | Uniswap V3 و V4 در Avalanche موجودند؛ Pharaoh هم CL هم DLMM | `docs/ARCHITECTURE.md`, `pool/discovery/*` |
| **A-06** | Liquidation Strategy جداگانه، executor قابلتوسعه — جزو core atomic arb نیست | `docs/mev_strategies.md`, `explorer/types.rs::is_forensic_action` |
| **A-07** | GMX (`gmx_market_tokens`, `gmx_event_emitters`) حذف یا out-of-core تگ شود | `jobs/live.rs::LiveContext::new`, `mev/detectors::ArbTagPolicy` |
| **A-08** | Pendle PT/YT (yield/rate) خارج از core | `exec::VenueRouters::pendle_router` |
| **A-09** | sAVAX rate-arb disposition (Yield/Rate خارج از core) | `jobs/live.rs::refresh_savax_rate`, `types/opportunity.rs::SavaxRoute` |

### 2.4 لیست ۱۳استراتژی مدل دیگری — نقطه شفافیت

خلاصهای از گفتگو که مبنای این پلن است، **نتیجه مقایسه** (۸ تصحیح + تصمیمات حذف/Expansion) را منتقل کرده است، ولی **خود لیست ۱۳گانه و جدول مقایسه فارسی را به متن کامل نداشته است**. بنابراین پلن حاضر نتایج آن مقایسه را پوشش میدهد، نه تک تک ردیفهای آن را. این را نمیتوانم بدون متن اصلی حدس بزنم.

برای بستن شکاف، لطفاً لیست ۱۳گانه را بده تا جدول زیر تکمیل و هر ردیف به WP مربوطه متصل شود:

| # | استراتژی (از مدل دیگری) | وضعیت: core / expansion / حذف | دلیل | WP | فایلهای مرتبط |
|---|---|---|---|---|---|
| ... | ... | ... | ... | ... | ... |

تا آن زمان، موارد قابل استنتاج از متن خلاصه همانهایاند که در §2.1 (C-01..C-08)، §1.1 (مرز دامنه) و §2.3 (A-01..A-09) آمدهاند.

---

## 3. مدل دامنه canonical — `MevOpportunity` نهایی

### 3.1 فیلدهای موجود (حفظ شوند)
`canonical_id`, `block_number`, `tx_index`, `strategy`, `pool_a`, `pool_b`, `token_in`, `token_out`, `input_amount`, `expected_profit`, `raw_profit`, `profit_slippage_{p1,m1,p2,m2}`, `gas_cost_wei`, `timestamp`, `path`, `tick_lower`, `tick_upper`, `liquidity_amount`, `victim_tx_index`, `backrun_tx_index`, `mempool_only`, `confidence`, `sender`, `tx_hash`, `detection_path`, `tags`, `route_tokens`, `savax_route`, `details`.

### 3.2 فیلدهای اضافهای (C-06, M-01)

| فیلد | نوع | دلیل | مصرفکننده |
|---|---|---|---|
| `quote_expires_at` | `Option<u64>` | جلوگیری از اجرا با quote منقضی | live gate, reject `QuoteExpired` |
| `underlying_pools_hash` | `Option<B256>` | تمایز مسیرهای aggregator-wrapped با pool set یکسان | canonical id, dedup |
| `router_version` | `Option<String>` | two routerها P&L متفاوت میدهند | canonical id, dashboard |
| `sim_block` | `Option<u64>` | تمام فیلدهای state نسبی به یک بلاک | sim, exec, pricing |
| `basefee_wei` | `Option<u128>` | gas دقیق | `GasQuote`, net profit |
| `nonce` | `Option<u64>` | جلوگیری از nonce conflict | exec, reject |
| `balance_wei` | `Option<U256>` | escrow/searcher balance @ sim_block | risk, ledger |
| `allowance_wei` | `Option<U256>` | allowance توکن (در non-flash path) | risk, reject |
| `risk_flags` | `Vec<String>` | جایگزین تک boolean | live gating, dashboard |
| `optimal_amount` | `Option<U256>` | `L*` خروجی amount optimizer | exec, net profit |
| `revert_gas_cost_wei` | `Option<U128>` | هزینه attempt ناموفق | net profit, ledger |
| `funding_source` | `Option<String>` | Balancer/Aave/UniV3/LFJ/… به ازای فرصت | funding layer, tags |
| `net_profit_wei` | `Option<U256>` | صریح، نه مشتق ضمنی | ranking, report |
| `priority_fee_wei` | `Option<u128>` | gas bidding واقعی | net profit, dashboard |
| `builder_bid` | `Option<Null>` | قابل توسعه، مقدار فعلی null | exec result |

> تمام فیلدها `#[serde(default, skip_serializing_if = "Option::is_none"/"Vec::is_empty")]` شوند تا فایلهای قدیمی results قابل خواندن بمانند.

### 3.3 محورهای تگ (M-05)

| محور | نمونه |
|---|---|
| `family` | `arb_atomic`, `backrun`, `rate_provider_staleness` |
| `mechanism` | `cross_venue`, `oracle_amm` (قابل Expansion)، `flash_arb` |
| `venue` | `pharaoh_dlmm`, `univ3`, `lfj_v2`, `pangolin_v2`, `sushiv2` |
| `funding` | `balancer_v2_vault`, `aave_v3`, `univ3_flash`, `lfj_lb_flash`, `v2_flash_swap` |
| `execution` | `single_tx`, `full_revert`, `priority_fee_bid`, `builder_bid_null` |
| `optimizer` | `split_route`, `amount_optimal_L`, `candidate_L_sweep` |
| `routing` | `aggregator_1inch`, `aggregator_kyber`, `aggregator_openocean`, `aggregator_paraswap_velora` |
| `scope` | `out_of_core` — GMX/sAVAX/Pendle: forensic حفظ، ولی خارج از sim/exec/ranking |

> **MevKind جدید اضافه نکن.** Backrun همان `ArbAtomic` + `backrun` tag باقی میماند.

### 3.4 حسابداری سود (C-05, M-03)

```
gross_profit_wei   = net token-flow residual after full simulated route   # شامل swap fee و slippage اجراشده
net_profit_wei     = gross_profit_wei
                     - (basefee_wei + priority_fee_wei) * gas_used
                     - flashloan_fee_wei          # Aave premium / Balancer fee / in-kind fee (runtime)
                     - revert_gas_cost_wei        # attemptهای ناموفق
```

**ممنوع:** کسر مجدد `SwapFees` یا `SlippageCost` از `gross` — داخل residual هستند.  
**Gas واحد:** AVAX؛ تبدیل USD با همان نرخی که REVM در `sim_block` استفاده کرد + `manipulation_risk` flag.

---

## 4. کاربستهها (Work Packages)

### WP0 — قراردادها و migration (P0) — پیشنیاز semua

**فایلها**
- `core/src/types/opportunity.rs` — اضافه کردن فیلدهای §3.2، `new()` را آپدیت کن.
- `core/src/explorer/store/schema.rs` — Migration Vn با `ensure_column` برای `opportunities` و `rejected_candidates`؛ `paper_sessions` و `paper_fills` هم ستونهای funding/revert/quote بگیرند.
- `core/src/explorer/store/mod.rs::OpportunityInput`, `core/src/explorer/store/query.rs::insert_opportunity_row` — mapping کامل.
- `core/src/explorer/results.rs::{PreparedOpportunity,persist_opportunities_to_explorer,persist_rejections_to_explorer}`.

**پذیرش:** migration دو بار اجرا شود خطا ندهد؛ ردیف قدیمی بدون فیلدهای جدید بخواند؛ ردیف جدید round-trip شود.

---

### WP1 — سود خالص و gas quote (P0) — وابسته به WP0

**فایلها:** `explorer/profit.rs`, `explorer/pricing.rs`, `paper/types.rs`, `paper/ledger.rs`, `jobs/live.rs::{update_ledger,resolve_native_price}`.

**تغییرات:** تابع `compute_net_profit()`، ساختار `GasQuote`، حذف double-count، ثبت `revert_gas_cost_wei` در ledger، استفاده از نرخ sim برای AVAX.

**پذیرش:** تست واحد فرمول بالا؛ تستی که confirms gas با AVAX نه USDC محاسبه شد.

---

### WP2 — تاکسونومی، funding و منابع (P0/P1)

**فایلها:** `explorer/decode/aggregator.rs`, `explorer/decode/tests.rs`, `explorer/types.rs`, `chain/events.rs`, `pool/discovery/{mod.rs,remote/mod.rs}`, `explorer/strategy_tags.rs`, `explorer/canonical.rs`, `types/opportunity.rs::compute_canonical_id`, `docs/{ARCHITECTURE.md,mev_strategies.md}`.

**تغییرات:** C-01..C-04 + A-01..A-06 + M-05/M-06. Odos → ParaSwap/Velora؛ V2-compatible forks؛ `split_route` به optimizer؛ Balancer runtime fee + reentrancy flag؛ canonical hash؛ مستندات PBS.

**پذیرش:** `grep -ri odos` پاک؛ تگ funding توسط decoder تولید شود؛ canonical دو router متفاوت را جدا نگه دارد.

---

### WP3 — کیفیت داده و reject ladder (P1) — وابسته به WP0

**فایلها:** `explorer/reject.rs`, `explorer/results.rs`, `explorer/ingest.rs`, `dashboard/reduce.rs`.

**تغییرات:** M-04 + جایگزینی معنای `StateStale` با `QuoteExpired/QuoteDrift`.

**پذیرش:** هر reject mới در `rejected_candidates` و dashboard reject ladder ظاهر شود.

---

### WP4 — Live: freshness / funding / pending / risk (P1) — وابسته به WP0,WP1,WP3

**فایلها:** `jobs/live.rs::{LiveOpts,run_once,process_pass,persist_results,update_ledger,render_ledger_summary,resolve_exec_context,resolve_sim_policy}`, `config/settings.rs`, `config/validation.rs`.

**تغییرات:** M-07/M-08/M-09 + C-07.

جریان پیشنهادی داخل `process_pass`:

```
fetch_blocks
  → run_blocks
  → [NEW] split_by_quote_freshness(opps, now_ms)
        ├─ stale → RejectReason::QuoteExpired / QuoteDrift
        └─ fresh → persist_results(opps)   # با sim_block/basefee/nonce/balance/allowance/funding
                   update_ledger(opps)     # net شامل funding + revert gas
  → render_ledger_summary(… funding / revert / expired / drift / competition …)
```

و مسیر expansion:

```
1b. Pending-Tx Feed → Detect Large Swap → Trigger Re-quote
      → Backrun(detection_path=pending, quote_expires_at, victim_tx_index)
```

**پذیرش:** تست: quote منقضی وارد ledger نشود و reject ثبت شود؛ flagها در CLI ظاهر شوند؛ summary **سه** خط جدید (funding fee / quote expiry-drift / competition loss) داشته باشد — خطوط revert/recon/caps/blocks از قبل موجوداند و نباید دوباره اضافه شوند.

---

### WP5 — Explorer سمت realization (P2) — وابسته به WP2

**فایلها:** `explorer/decode/{flash_loan.rs,aggregator.rs,attach.rs}`, `explorer/ingest.rs`, `explorer/classify/{arb.rs,causal.rs}`, `explorer/types.rs`, `explorer/dashboard/{enrich.rs,evidence.rs,reduce.rs,render.rs,csv.rs}`.

**تغییرات:** M-10 — استخراج funding provider/router_version/underlying_pools_hash از لاگهای realization و پر کردن `MevEvent.details`؛ ثبت failed arb attempts (tx status=0) به عنوان شاهد `revert_gas_cost`؛ پنل funding/expiry/drift/risk در HTML و CSV.

**پذیرش:** داشبورد realized funding را بر اساس Vault/Aave تفکیک کند؛ backrun causal موجود بماند و با pending مسیری که در WP4 اضافه شد قابل مقایسه باشد.

---

### WP6 — متریکهای backtest (P2) — وابسته به WP4,WP5

**فایلها:** `paper/store.rs`, `explorer/store/schema.rs` (`paper_sessions`), `dashboard/reduce.rs`, `dashboard/csv.rs`, `jobs/live.rs::render_ledger_summary`, `docs/CLI.md`.

**تغییرات:** C-08 — ذخیره و نمایش:

- `quote_drift_failure_rate`
- `quote_expiry_failure_rate`
- `same_block_competition_loss_rate`
- `funding_fee_share_of_gas`
- `revert_gas_share`

**پذیرش:** سه ران پشت سر هم در `paper_sessions` متریکها را پر کند؛ HTML/CSV mereka را نشان دهد.

---

## 5. ترتیب اجرا و gates

```
WP0 (schema/contracts)
  ├─→ WP1 (net profit + gas quote)
  ├─→ WP2 (taxonomy/funding/router/canonical)
  ├─→ WP3 (reject ladder)
  │     └─→ WP4 (live gate + pending + flags + summary)
  │            └─→ WP6 (metrics)
  └─→ WP5 (explorer realized coverage) ──────┘
```

**Gateها:**
1. بعد WP0: `cargo test -p mev-scout-core explorer::store` سبز.
2. بعد WP1: تست فرمول سود سبز； double-count رخ ندهد.
3. بعد WP2: `grep -ri odos core/src` پاک； مستندات آدرسها را داشته باشند.
4. بعد WP4: quote منقضی وارد ledger نشود.
5. بعد WP6: هر پنج متریک در خروجی ظاهر شوند.

---

## 6. آدرسها و حقایق مرجع (برای پیادهسازی)

| مورد | مقدار |
|---|---|
| Uniswap V3 Factory | `0x740b1c1de25031C31FF4fC9A62f554A55cdC1baD` |
| Uniswap V3 QuoterV2 | `0xbe0F5544EC67e9B3b2D979aaA43f18Fd87E6257F` |
| WAVAX | `0xB31f66AA3C1e785363F0875A1B74E27b85FD66c7` |
| USDC | `0xB97EF9Ef8734C71904D8002F8b6Bc66Dd9c48a6E` |
| Balancer V2 Vault | `0xBA12222222228d8Ba445958a75a0704d566BF2C8` |
| BalancerQueries | `0xC128468b7Ce63eA702C1f104D55A2566b13D3ABD` |
| LFJ V2.2 Factory | `0xb43120c4745967fa9b93E79C149E66B0f2D6Fe0c` |
| Pharaoh DLMMFactory | `0xEb480050b016f6c6d45203D2346B68bDDDa23D4D` |
| Pharaoh DLMMQuoter | `0xDdaE0aA4E93be4936c1BcC12d3001b35C75fEF40` |

**حقایق بحرانی:**
- Odos در `30 July 2026` برای همیشه متوقف شد → جایگزین ParaSwap/Velora.
- Balancer fee historically 0 ولی **باید runtime خوانده شود**.
- Pharaoh هم Concentrated Liquidity و هم DLMM دارد.
- Uniswap V2 رسمی در Avalanche نیست → Pangolin V2 / SushiSwap V2.
- Avalanche PBS ندارد → `priority_fee`، `builder_bid: null`.
- Gas به AVAX پرداخت میشود، تبدیل USD با نرخ sim_block + ریسک دستکاری.

---

## 7. منابع تحقیق (لینکهای جمعآوریشده)

- `https://support.avax.network/en/articles/5398705-what-is-arbitrage`
- `https://support.avax.network/en/articles/5398703-what-is-a-flash-loan`
- `https://support.avax.network/en/articles/8857127-usdc-cctp-faq`
- `https://coinmarketcap.com/academy/article/arbitrage-opportunities-in-defi`
- `https://docs.gmx.io/docs/trading/fees`
- `https://docs.chainstack.com/docs/avalanche-tutorial-aavev3-flash-loans-with-hardhat`
- `https://github.com/matejos/avax-arbitrage-bot`
- `https://bitquery.io/investigations/avalanche-dex-trading-two-bot-clusters`
- `https://bitquery.io/investigations/defi-liquidation-bots`
- `https://medium.com/@MEVzone/mev-infrastructure-for-avalanche-c-chain-6adb0e67d77d`
- `https://docs-v2.balancer.fi/reference/contracts/deployment-addresses/avalanche.html`
- `https://odos.xyz/` (برای سابقه تعطیلی)
- `https://docs.kyberswap.com/getting-started/supported-exchanges-and-networks`
- `https://developers.uniswap.org/docs/protocols/v3/deployments/v3-avalanche-deployments`

---

## 8. تصمیمات باز (قبل از شروع WP4/WP5 نیاز به تأیید تو)

1. **Backrun:** فعلاًtag محققشده (causal/logs-only) است. آیا در همین فاز مسیر pending re-quote هم اضافه شود یا به expansion عقب افتد؟ (پلن آن را در WP4 قرار داده است.)
2. **Oracle–AMM:** «قابل بررسی» مانده؛ تگ `oracle_amm` را تعریف میکنیم ولی detector اضافه نمیکنیم.
3. **Liquidation:** Strategy جداگانه؛ executor قابلتوسعه بماند، وارد core atomic arb نشود.
4. **Split-Route:** به عنوان optimizer mode پیاده شود، نه مسیر pre-defined؛ آیا در اولین release فعال باشد یا فقط flagدار؟

> در صورت تأیید، من **WP0 + WP1** را به عنوان اولین patch اعمال میکنم (اسکیمای opportunity + migration + فرمول net profit)، بدون دست زدن به منطق تشخیص موجود.
