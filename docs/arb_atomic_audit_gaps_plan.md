#plan پیشنهادی بستن گپهای ممیزی `MevKind::ArbAtomic`

> **وضعیت:** لایهٔ کد WP-A (G-03، G-04)، موارد code-only P1/P2 (G-02، G-07–G-10)، WP-D (G-12/G-13/G-15) و guard testهای ضمیمهٔ D پیادهسازی شدند و `cargo test --workspace --quiet` سبز است؛ G-05/G-06/G-14 با RPC آرشیو بسته شدند. I-1..I-3 با متن اصلی تسک (۲۰۲۶-۱۱-۲۴) بسته و جدولها بازنگاشت شدند. باقیماندهٔ plan منوط به enrichment `pool_dex` (G-01) است.  
> **تاریخ:** 2026-11-23 (بازتأیید، پاکسازی و بازنگاشت تاکسونومی: 2026-11-24)  
> **دامنه:** فقط `core/src/explorer/` و فقط تراکنشهای `success=true` با `MevKind::ArbAtomic` محققشده  
> **ساختار:** دو لایه — `§0..§8` plan/gap registry (وضعیت جاری) و `§9..§13` audit snapshot @ `ef29f73` (صرفاً historical؛ منعکسکنندهٔ وضعیت فعلی نیست).  
> **قید مهم:** تغییرات فقط در `core/src/explorer/`، `core/data/`، `core/tests/` و docs مربوطهاند؛ نه `MevKind` جدیدی اضافه شد، نه `compute_net_profit`/basis `R`/thresholdهای causal تغییر کرد و نه event دومی ایجاد شد. ورودیهای I-1..I-3 از متن اصلی تسک تأمین شدند (بازنگاشت docs-only، بدون ایجاد فایل جدید)؛ ۴ هش real مکانیزمی در G-05 pin شدند (۲۰۲۶-۱۱-۲۴).

---

## خلاصهٔ اجرا (2026-11-23)

| گپ | وضعیت | شواهد / یادداشت |
|---|---|---|
| G-01 (P0) | جزئی | کد `cross_dex_basis=protocol` و تست آن از قبل موجود است؛ pin کردن ردیف real مسدود است چون `pool_dex` کش ناقص است و enrichment در `jobs/index.rs` خارج از دامنه است. |
| G-02 (P1) | ✅ تمام | `arb_shape_basis="token_count"` + فیلد مستقل `pool_shape`؛ ماتریس ۴ حالته؛ مستندات همساز شد. |
| G-03 (P0) | ✅ تمام | `details.funding_providers[]` + یک تگ بهازای هر provider؛ `funding_source` همان first entry؛ تست double-flash با جمع صحیح fee و بدون کسر مضاعف. |
| G-04 (P0) | ✅ تمام | `explorer_tags.json` شامل `forwarded_profit` و ۸ تگ `funding:*`؛ تست `vocabulary_covers_every_emitted_arb_tag`. |
| G-05 (P1) | ✅ تمام (هانت live) | ۴ ردیف Seeded با هش 64-hex و ۳ ردیف با نتیجهٔ منفی صریح (۰ hit در پنجرهٔ 95681722..=95682322، ۱۱۱۷ arb اسکنشده). تست گیتد `g05_seeded_mechanism_hashes_carry_expected_tags` تگها را بازتأیید میکند. |
| G-06 (P1) | ✅ تمام (تست گیتد + اجرای live ثبتشده) | ۴ هش §3 بهعنوان literal در `core/tests/explorer_corpus.rs` pin شدند (`pinned_real_arb_hashes_reproduce_route_meta_and_pnl`)؛ اجرا منوط به `MEV_SCOUT_E2E=1 RPC_URL=...` با endpoint آرشیو است. طبق بهروزرسانی §G-06 با RPC آرشیو Dwellir اجرا و ۴ ردیف بازتولید شد؛ مقادیر hop بهروزرسانی‌شده (۳→۲ و ۴→۳) در جدول §3 و fixture منعکساند. |
| G-07 (P2) | ✅ تمام | `details.profit_cause` صرفاً metadata؛ صفر تغییر در `net_profit_usd`. |
| G-08 (P2) | ✅ تمام | تست موجود `other_residuals_usd_is_audited_but_not_summed` سیاست را پوشش میدهد. |
| G-09 (P2) | ✅ تمام (با ریسک باقیمانده) | negative fixture `unrelated_transfer_is_not_a_forwarded_payee`؛ TP موجود حفظ شد. gate اختیاری «payee must be contract or known router» اضافه نشد، چون مسیر logs-only قادر به تشخیص contract از EOA نیست؛ فقط transferهای funder-fed پذیرفته میشوند. |
| G-10 (P2) | ✅ تمام | `details.rejected_routes` + assert در تست دوچرخه، بدون double P&L. |
| G-11 (P3) | عمداً خارج | تا زمانی که مدل/اجرای end-to-end وجود ندارد detector اضافه نمیشود. |
| G-12 / G-13 / G-15 (P3) | ✅ تمام | WP-D: G-12 flag `underlying_pools_unknown` + ۲ تست (aggregator+DEX متمایز از DEX-only)؛ G-13 یادداشت محدودیت native inflow در `docs/ARCHITECTURE.md` + ۱ negative test در `profit.rs`؛ G-15 جدول تثبیت `explorer tag ↔ live scope tag ↔ disposition` در §3.3.1 با ثبت کامل `NA_TAGS`. |
| G-14 (P1) | ✅ تمام (holdout مستقل) | پنجرهٔ `101221000..=101221702` (۷۰۳ بلوک، ۲۰۲۶-۱۱-۲۴) ایندکس و با فید mevlive مقایسه شد: **۵۶/۵۹ (۹۴.۹٪)** recall، ۳ miss (همه بلاکها ایندکس و swapها دکد شدهاند)، خطای median ۰.۳۹٪. ردیف جدایی در `docs/benchmarks/mevlive_avax_arb_recall_2026-11-09.md` ثبت شد. |
| ضمیمه D ( guards ) | ✅ تمام | ۳ تست non-emission در `explorer/scenarios.rs`: `unsupported_axis_tags_are_never_emitted_by_arb_classifier` (تاکسونومی رسمی AR-08/AR-20 + G-15)، `docs_only_mechanisms_never_emit_a_tag` (AR-11/AR-14/AR-17 + سه دستهٔ خارج از تاکسونومی)، `venue_and_funding_only_facts_do_not_claim_mechanisms` (AR-21/AR-22). کامنتهای تست از شمارهگذاری قدیمیاند؛ نگاشت در §9 ثبت شد. |

**ورودیهای مسدودکننده:** فقط G-01 (enrichment `pool_dex` در `jobs/index.rs`)؛ I-1/I-2/I-3 بسته شدند (متن اصلی تسک، ۲۰۲۶-۱۱-۲۴).  
**تأیید:** `cargo test --workspace --quiet` ← exit 0 (۶۹۸ تست passed، ۲ ignored در lib؛ تمام integration suites سبز).  
**کامیتها:** `238ff35` (G-02..G-10)، `73fbd62` (G-06)، `cda3ea0` (WP-D)، `72499e5` (G-05/G-14) — پیادهسازیها در کامیتهای جداگانه ثبت شدند، نه یک کامیت.  
**بازتأیید ۲۰۲۶-۱۱-۲۳:** کد و تستهای G-02..G-10 و `explorer_tags.json` در درخت موجود تأیید شد. بررسی کاشهای محلی (`cache/*.sqlite`) نشان داد هیچ‌یک از ۴ هش pin (بلاک‌های ۱۰۰.۵M) و هیچ تگ Hunt در کاش نیست؛ بنابراین G-05 و G-14 به RPC/archive محدوداند (اجرای live G-06 طبق بهروزرسانی §G-06 با RPC آرشیو Dwellir ثبت شدهاست) و G-01 به enrichment `pool_dex` (فایل `jobs/index.rs` خارج از دامنهٔ فاز) محدود است.
**بازتأیید WP-D ۲۰۲۶-۱۱-۲۳:** G-12/G-13/G-15 پیاده و تست‌شده — `cargo test --workspace --quiet` ← exit 0؛ `underlying_pools_unknown` (G-12) در `classify/arb.rs`، یادداشت محدودیت + negative test (G-13) در `docs/ARCHITECTURE.md`/`explorer/profit.rs`، و جدول تثبیت scope/disposition + ثبت کامل `NA_TAGS` (G-15) در `docs/atomic_capital_free_arb_plan.md` §3.3.1.
**بازتأیید ۲۰۲۶-۱۱-۲۴ (پاکسازی):** کامنتهای TEMP در `explorer_corpus.rs` پاکسازی شدند؛ ۳ guard test ضمیمهٔ D در `explorer/scenarios.rs` اضافه شدند؛ شمارش تست به ۶۹۸ passed / ۲ ignored بهروزرسانی شد؛ تضادهای متنی G-06 و کامیت و DoD همساز شدند.
**بازتأیید G-05 ۲۰۲۶-۱۱-۲۴ (RPC آرشیو Dwellir):** هانت ۶۰۰بلوکه اجرا و ۴ seed.real پیدا شد؛ ۳ ردیف باقیمانده نتیجهٔ منفی صریح گرفتند (جزئیات §G-05). تست گیتد ۴/۴ تأیید شد.
**بازتأیید G-14 ۲۰۲۶-۱۱-۲۴ (RPC آرشیو Dwellir):** پنجرهٔ holdout مستقل ۷۰۳بلوکه ایندکس و با فید mevlive مقایسه شد؛ ۵۶/۵۹ recall (۹۴.۹٪) — ردیف جداگانه در benchmark اضافه شد (جزئیات §G-14).
**بسته شدن I-1..I-3 ۲۰۲۶-۱۱-۲۴ (docs-only):** متن اصلی تسک دریافت شد؛ ۲۹+۷ ردیف براساس تاکسونومی رسمی بازنگاشت و §13 براساس Q1..Q6 بازنویسی شد. هیچ فایل جدیدی و هیچ کدی تغییر نکرد.

---

## 0. ورودیهای لازم از شما (Blocking)

هر سه ورودی روی متن اصلی تسک (`Task: Complete ArbAtomic Coverage Audit — MEV-Scout Explorer`) تأمین و در ۲۰۲۶-۱۱-۲۴ اعمال شدند. متن مذکور در مخزن ذخیره نشد (قید «هیچ فایل جدیدی ایجاد نکن»)؛ مرجع آن همان متن ارسالشده توسط کاربر است.

| # | ورودی | وضعیت | محل اعمال |
|---|---|---|---|
| I-1 | نگاشت رسمی `AR-01..AR-29` | ✅ بسته — ۲۹ شناسه (Route A: ۱۰، Market B: ۱۳، Behavioral C: ۶) | §9 جدول Coverage Matrix (بازنگاشت ۲۹ ردیف) |
| I-2 | نگاشت رسمی `FUND-01..FUND-07` | ✅ بسته — Balancer V2 / Aave V3 / Uni V3 Flash / Uni V2 Flash Swap / SushiSwap-V2 fork / LFJ LB / Multiple providers | §9 جدول Funding sources (بازنگاشت ۷ ردیف) |
| I-3 | متن ۶ سؤال خلاصهٔ مدیریتی | ✅ بسته — متن §۶ تسک مبنای §13 قرار گرفت | §13 ضمیمه E (پاسخ Q1..Q6) |

**تأثیر بازنگاشت:** شمارهگذاری قبلی جدول با تاکسونومی رسمی مطابقت نداشت (مثلاً AR-10 قبلی = Curve Imbalance، اکنون AR-12). چهار ردیف قبلی (GMX ADL، init-price snipe، CEX/cross-chain/spot-perp، statistical/NFT-floor) و دو منبع funding اضافی (Aave V2، Morpho Blue) در لیست رسمی نیستند؛ بهصورت guard-row / canonical source اضافی در §9 حفظ شدند و در شمارش ۲۹+۷ محاسبه نمیشوند. سطوح شواهد و verdictها مجدداً براساس کد و هشهای real بازبینی شدند.

---

## 1. وضعیت فعلی

| شاخص | مقدار |
|---|---|
| ردیفهای ArbAtomic (تاکسونومی رسمی) | 29 (8 Supported / 14 Partial / 7 Unsupported / 0 Unverified — مطابق §13) |
| ردیفهای funding (تاکسونومی رسمی) | 7 (6 Supported / 1 Partial — مطابق §13) |
| تأییدشده روی tx.real موفق | ۹ ردیف H شامل shapeهای generic و تگهای رفتاری (G-05 چهار seed + دو backrun ETH + فلش/فاندینگ) |
| مکانیسمهای فاقد seed.real | 7 ردیف reject catalogue + تمام funding |
| کامیت پایه | `ef29f73` |
| درخت کاری | تمیز (پیش از ایجاد همین فایل) |

---

## 2. رجیستری گپها

### G-01 — DEX identity ناقص (P0)

- **گپ:** `arb_route_meta` در `classify/arb.rs:760-826` تنها وقتی از هویت پروتکل استفاده میکند که **همهٔ** لگها `pool_dex` داشته باشند؛ در جدیدترین کش محلی ۶۱ ردیف `arb_atomic` فاقد `venues[]`اند و بهطور کامل به `cross_dex_basis="amm_family"` رفتند.
- **پیشنهاد:** اطمینان از اینکه `pools.dex_name` توسط enrichment پر میشود و سپس pin کردن حداقل یک tx.real با `cross_dex_basis=protocol`.
- **فایلها:** `core/src/jobs/index.rs:45-48,148-155`، `core/src/explorer/ingest.rs:214-225,274`، `core/src/explorer/classify/arb.rs:792-824`.
- **معیار پذیرش:** حداقل ۱ ردیف real در golden؛ `venues` غیرتهی است و `dex_count` برابر تعداد DEX واقعی مسیر است.
- **ریسک عدم اجرا:** FP مداوم در ادعای cross-DEX میان forkهای یک پروتکل.

### G-02 — شکل مسیر heuristic (P1)

- **گپ:** `arb_shape` در `arb.rs:782-788` از `hop_count == 2 || unique_tokens == 2` و سپس `== 3` مشتق میشود. نمونهٔ real بلاک `100528634` دارای `hop_count=3, pool_count=2` است ولی `two_pool` نامیده میشود. در داک نیز «triangular 6-hop» دیده میشود.
- **پیشنهاد:** تصمیم صریح بگیرید که مرجع shape، تعداد توکنهاست یا تعداد استخرها. سپس یا منطق را اصلاح کنید یا فیلد `pool_count`-محور جداگانه اضافه کنید و مستندات را همساز کنید.
- **فایلها:** `arb.rs:758-826`، `docs/mev_strategies.md:2993-2997`، `docs/explorer_strategy_checklist.md:96-166`.
- **معیار پذیرش:** ماتریس fixture شامل `(hop=3,pool=2,token=2)`, `(hop=3,token=3)`, `(hop=6,token=3)`, `(hop=4,token=4)` با نتیجهٔ تعریفشده.
- **توصیه:** naming فعلی را «token-count shape» صدا بزنید تا با «hop» اشتباه نشود.

### G-03 — funding چندگانه (P0)

- **گپ:** feeهای تمام flashloanها جمع میشوند (`arb.rs:83-149`) ولی تگ و `funding_source` فقط از `tx.flashloans.first()` میآیند (`arb.rs:382-384,429-434`). آرایهٔ `flash_providers` فقط در liquidation وجود دارد.
- **پیشنهاد:** اضافه کاردن `details.funding_providers: string[]` و حفظ `funding_source` فعلی بهعنوان backward-compatible first entry.
- **فایلها:** `core/src/explorer/classify/arb.rs:373-434`، `core/src/explorer/decode/flash_loan.rs:275-279`، `core/src/types/tags.rs:96-122`.
- **معیار پذیرش:** تست سنتتیک دو flashloan با provider متفاوت → هر دو در آرایه و fee_sum درست، بدون double subtract.

### G-04 — vocabulary ناقص (P0)

- **گپ:** `forwarded_profit` و تمام `funding:*` در `core/data/explorer_tags.json:1-19` غایباند؛ `oracle_amm` عمداً vocabulary-only است و نباید اضافه شود مگر تصمیم جدی بر expansion.
- **پیشنهاد:** اضافه کردن تگهای emitشده به vocabulary و اطمینان از همساز بودن `explorer_checklist_sync` و `explorer_tag_coverage`.
- **فایلها:** `core/data/explorer_tags.json`، `core/tests/explorer_checklist_sync.rs`، `core/tests/explorer_tag_coverage.rs:23-28,106-126`.
- **معیار پذیرش:** `expected_tags` ⊂ تگهای emitشده واقعی در corpus گیتد.

### G-05 — seed.real برای ۷ ردیف (P1)

- **گپ:** `P0.4 flash_arb`، `P3.2 rate_provider_staleness`، `P3.5 curve_imbalance`، `P3.7 gmx_adl_arb`، `P3.12 fot_arb|rebase_arb`، `P3.15 epoch_transition`، `P3.16 savax_rate_arb` همگی `coverage_unknown`/`Hunt`اند (`scenario_targets.rs:117,191,201,211,221,241,251`).
- **پیشنهاد:** برای هر ردیف یک هش 64-hex موفق pin شود؛ در صورت عدم یافتن، همان نتیجهٔ منفی صریحاً ثبت شود، نه fixture سنتتیک بهعنوان واقعی.
- **فایلها:** `core/src/explorer/scenario_targets.rs`، `core/tests/explorer_corpus.rs`، `docs/explorer_strategy_checklist.md`.
- **معیار پذیرش:** هر ردیف یا `Seeded` با block/hash میشود یا یادداشت «hunt performed, 0 hits in window X» دارد.
- **اجرای 2026-11-24 (RPC آرشیو Dwellir):** هانت `g05_hunt_dump_avax` در پنجرهٔ `95681722..=95682322` (۶۰۰ بلوک، ۱۱۱۷ arb_atomic) اجرا شد. فراوانی تگها: `long_tail` ۹۷۳، `forwarded_profit` ۳۷۰، `flash_arb` ۴۵، `curve_imbalance` ۱۰، `rate_provider_staleness` ۲، `savax_rate_arb` ۱.
  - **Seeded (4):** `P0.4` ← blk 95681751 / `0x306e1885…` (`flash_arb`)؛ `P3.5` ← blk 95681823 / `0xc1532675…` (`curve_imbalance`)؛ `P3.2` ← blk 95682026 / `0x7e3d68f8…` (`rate_provider_staleness`)؛ `P3.16` ← blk 95682059 / `0xb77e1291…` (`savax_rate_arb`, Exact).
  - **منفی صریح (3):** `P3.7 gmx_adl_arb`، `P3.12 fot_arb|rebase_arb`، `P3.15 epoch_transition` — هر سه ۰ hit در همان پنجره.
  - شواهد در `scenario_targets.rs` (فیلد `hunt`)، `docs/explorer_strategy_checklist.md` و تست گیتد `g05_seeded_mechanism_hashes_carry_expected_tags` (`explorer_corpus.rs`) ثبت شدند. این تست گیتد با همان RPC آرشیو Dwellir اجرا و ۴/۴ تگ تأیید شد (blk 95681751/95681823/95682026/95682059).

### G-06 — golden.real قابل اجرا (P1)

- **گپ:** در `core/src/explorer` و `core/tests` **هیچ هش 64-hex واقعی** وجود ندارد؛ تنها placeholder all-`1` در `canonical.rs:188`. هشهای داک مخففاند و `live_tag_parity` آنها را از markdown استخراج میکند.
- **پیشنهاد:** pin کردن ۴ هش کامل کش محلی (جدول section 3) بهعنوان fixture گیتد همراه با assert بر `route`, `arb_meta.arb_shape`, `net_profit_usd` و `cross_dex_basis`.
- **معیار پذیرش:** تستی که با `MEV_SCOUT_E2E=1 RPC_URL=...` اجرا شود و ۴ ردیف را بازتولید کند.
- **نکته:** اینها git-ignored cacheاند؛ هشها را باید در فایل تست pin کرد، نه وابسته به مسیر cache.
- **بهروزرسانی 2026-11-23:** تست `pinned_real_arb_hashes_reproduce_route_meta_and_pnl` در `core/tests/explorer_corpus.rs` اضافه شد: چهار هش کامل را literal نگه میدارد، تکبلکها را با `run_range` بازتولید میکند و `route`، `arb_meta.arb_shape`/`hop_count`/`unique_tokens`، `cross_dex_basis` (سازگاری درونی protocol/amm_family) و `net_profit_usd` را assert میکند. مقادیر USD recorded با باند ±۵۰٪ (وابسته به price cache) بررسی میشوند.
- **بهروزرسانی 2026-11-23 (اجرای live):** با RPC آرشیو Dwellir (`api-avalanche-mainnet-archive.n.dwellir.com`) اجرا شد و ۴ ردیف بازتولید شد (100528634، 100529098، 100529236، 100529720). `hop_count` دو ردیف با extraction G-02 (رد کردن لبه‌های اضافی/غیرقابل‌بازگشت) نسبت به ارزش اولیهٔ pin تغییر کرد: 100528634: ۳→۲، 100529720: ۴→۳؛ این مقادیر در جدول §۳ و fixture گیتد به‌روزرسانی شدند. confidence ردیف 100529720 = `inferred` ثبت شد.

### G-07 — P&L مکانیسممحور (P2)

- **گپ:** تمام ۲۹ ردیف از همان residual `R` استفاده میکنند؛ حتی sAVAX فقط gate دارد و نمیتوان گفت «چقدر از سود ناشی از divergence بود».
- **پیشنهاد:** اضافه کردن `details.profit_cause` صرفاً بهعنوان metadata (مثلاً `{mechanism: "savax_rate_arb", divergence_bps: 7}`) بدون ایجاد event دوم و بدون تغییر `compute_net_profit`.
- **فایلها:** `classify/arb.rs:392-428`، `strategy_tags.rs`، `store/pnl.rs`.
- **معیار پذیرش:** صفر تغییر در `net_profit_usd` تستهای موجود.

### G-08 — multi-residual USD (P2)

- **گپ:** فقط `profit_tokens` در USD جمع میشود؛ سایر residuals مثبت در `details.other_residuals` آدیت میشوند ولی وارد net نمیشوند (`types.rs:204`, `pnl.rs:1191,1221`).
- **پیشنهاد:** تصمیم policy بگیرید: یا مستندسازی بهعنوان «عمداً کمشمار» (توصیه فعلی)، یا اضافه کردن `other_residuals_usd` به یک فیلد جداگانهٔ net (نه به `net_profit_usd` canonical).
- **معیار پذیرش:** تست `other_residuals_usd_is_audited_but_not_summed` یا جایگزین صریح آن.

### G-09 — forwarded_profit FP (P2)

- **گپ:** `forwarded_recipient` (`arb.rs:864-935`) فقط یک گیرندهٔ funder-fed میخواهد؛ شرط contract/payee/router بودن ندارد. دو payee → رد محافظهکارانه (`arb.rs:1609`).
- **پیشنهاد:** اضافه کردن یک negative fixture که transfer عادی به یک EOA غیرمرتبط را payee نپندارد؛ در صورت نیاز gate «payee must be contract or known router» اضافه شود.
- **معیار پذیرش:** TP موجود (`forwarded_profit_in_priority_token_is_inferred`) حفظ و FN دو payee یا کاهش یابد یا مستند شود.

### G-10 — undercount تک event (P2)

- **گپ:** دو چرخهٔ سودآور در یک tx → یک event (`arb.rs:167`, `select_route:523`, تست `:1300`).
- **پیشنهاد:** `candidate_route_count` و `other_residuals` موجوداند؛ پیشنهاد میشود فیلد `rejected_routes` metadata اضافه شود تا تحت شمارش قابل مشاهده باشد بدون double P&L.
- **معیار پذیرش:** تست دو چرخه علاوه بر تک event، آرایهٔ مسیرهای ردشده را assert کند.

### G-11 — split-route realised (P3 — خارج از core فعلی)

- **گپ:** فقط `optimizer:split_route` وجود دارد؛ مدل `route_json` نمیتواند تخصیص کسری/مسیر دوم را نمایش دهد.
- **پیشنهاد:** **فعلاً خارج نگه دارید.** تا زمانی که مدل و اجرای end-to-end وجود ندارد، نباید detector اضافه شود.
- **معیار پذیرش:** مستندسازی صریح در `mev_strategies.md` بهعنوان optimizer/execution، نه venue/route family.

### G-12 — aggregator داخلی (P3)

- **گپ:** dedup با تحمل 200/400 bps و ≤3-hop (`decode/aggregator.rs:164-235`)؛ poolset داخلی router بازسازی نمیشود.
- **پیشنهاد:** حداقل یک flag metadata مبنی بر «underlying pools unknown» برای route های aggregator-mixed.
- **معیار پذیرش:** تستی که aggregator+DEXرا متمایز از DEX-only نشان دهد.

### G-13 — logs-only native inflow (P3)

- **گپ:** `profit.rs:1` صریحاً میگوید unwrap-to-beneficiary و coinbase/searcher kickback قابل دیدن نیستند، فقط `tx.value`.
- **پیشنهاد:** مستندسازی بهعنوان محدودیت شناختهشده؛ در صورت نیاز به دقت بالاتر، ورودی trace/receipt اضافه شود (خارج از این فاز).
- **معیار پذیرش:** یادداشت در ARCHITECTURE.md و یک negative test.

### G-14 — بنچمارک holdout (P1)

- **گپ:** 47/47 در `mevlive_avax_arb_recall_2026-11-09.md:3,67` یک derivation window است، not independent holdout.
- **پیشنهاد:** freeze کردن پنجرهٔ مستقل دوم با همان oracle و ثبت recall جداگانه.
- **معیار پذیرش:** دو ردیف metric با label متفاوت در همان report.
- **اجرای 2026-11-24 (RPC آرشیو Dwellir):** پنجرهٔ مستقل `101221000..=101221702` (۷۰۳ بلوک، فید mevlive `2026-11-24T03:05:37Z..03:16:04Z`) ایندکس و مقایسه شد. نتایج: ۵۹ arb از mevlive، ۱۰۱ `arb_atomic` از ما، ۵۶ مطابق (**۹۴.۹٪**)، ۳ miss، ۴۵ our-only. خطای median PnL ۰.۳۹٪ و p90 ۰.۷۱٪؛ مجموع gross مطابقها `$2.6986` در برابر `$2.7063`. سه miss همگی در بلاکهای ایندکسشده با swap دکدشدهاند (گپ classifier، نه RPC). یک عدمتطابق توکن سود (`0xc0ae5e…`) با توافق USD ۰.۳٪. جزئیات و جدول dual-row در `docs/benchmarks/mevlive_avax_arb_recall_2026-11-09.md` §Fresh holdout؛ آرتیفاکتها `results/mevlive_avax_holdout_2026-11-24.json`، `results/compare_g14_holdout.json` و `results/index_g14_holdout.log` (محلی، git-ignored).

### G-15 — تگ forensic/scope (P3 — تثبیت تصمیم)

- **گپ:** GMX/sAVAX/Pendle با `out_of_core` در live حفظ میشوند ولی در Explorer برچسب متفاوتی دارند.
- **پیشنهاد:** جدول تبدیل را تثبیت کنید: `explorer tag` ←→ `live scope tag` ←→ disposition (forensic / core / expansion).
- **فایلها:** `tagging.rs:354-372`، `types/tags.rs`، `docs/ARCHITECTURE.md:111`، `docs/atomic_capital_free_arb_plan.md:178`.
- **معیار پذیرش:** `live_tag_parity` بدون NA-tag پنهان برای موارد قابل مقایسه.

---

## 3. هشهای real برای pin کردن (G-06)

| بلاک | هش کامل | shape | hop/uniq | profit_usd | net | conf |
|---|---|---|---|---|---|---|
| 100528634 | `0x37b413bbbd6af98b4919897223d39bfb3313f1e261232936c7b8cde54563bff9` | two_pool | 2 / 2 | 0.025902 | 0.010206 | exact |
| 100529098 | `0x7c709477088b85667b3b641d1b80546d833a6082b6570890244b975ecc106f1a` | multi_hop | 4 / 4 | 0.023446 | 0.003250 | inferred |
| 100529236 | `0x4d03e567171248c005a64da265667342bc1dedc6d9724e39643b0ba6268851e9` | multi_hop | 4 / 4 | — | — | inferred |
| 100529720 | `0x952f7e215908ea1d35fb59c43bc1c7dec109f935c18a615d62f0155025ea3d16` | triangular | 3 / 3 | — | — | inferred |

**Seedهای از پیش pinشده (status=1):**

| زنجیر | بلاک | هش | نقش |
|---|---|---|---|
| ETH | 26264539 | `0xdf4f0afbe7551cfe9805628fc2d2a3a2909e859f32bda202888c184a705abb03` | backrun |
| ETH | 26264858 | `0xf33bf44580da521ad53d0188bf9c5b3a507e50e61160a573ba28090b053e5c6a` | backrun دوم |
| AVAX | 97781738 | `0xf20793cadb1dfd7c48a24d302feb2d100505e90f7e846c9fb603a80d1c053fb4` | claim-and-sell (Utility) |
| AVAX | 97782322 | `0x9300191c7626be981444544234c2bcea719065c8142c1749f389fe431cc2d88b` | منفی کنترل: liquidation |

> هشهای داک (`0xfbf67ff0…7cc6e1` و …) **مخففاند** و نباید بهعنوان 64-hex معرفی شوند.

---

## 4. فازبندی اجرا

| فاز | تسکها | وابستگی | خروجی قابل تحویل |
|---|---|---|---|
| **WP-A — ورودی و صحت** | I-1, I-2, I-3, G-01, G-03, G-04 | — | جدول 29+7 نهایی + vocabulary کامل |
| **WP-B — شواهد real** | G-05, G-06, G-14 | WP-A | ۴ golden real + حداقل ۳ seed مکانیسمی یا نتیجهٔ منفی ثبتشده |
| **WP-C — دقت** | G-02, G-07, G-08, G-09, G-10 | WP-B | metadata شکل/P&L بدون تغییر net canonical |
| **WP-D — تثبیت مرز** | G-11, G-12, G-13, G-15 | WP-C | مستندات همساز + parity بدون NA-tag مخفی |

**ترتیب پیشنهادی:** WP-A أولاً، چون سه ورودی I را میبندد و بدون آن جدول نهایی معنایی ندارد.

---

## 5. اولویتهای پیشنهادی (خلاصه)

- **P0 (بلافاصله):** I-1..I-3, G-01, G-03, G-04
- **P1:** G-05, G-06, G-14
- **P2:** G-02, G-07, G-08, G-09, G-10
- **P3:** G-11, G-12, G-13, G-15

---

## 6. فرمانهای verification

```powershell
# Full workspace
cargo test --workspace --quiet

# Real-cache causal FP gate (اجرا شد: 15000 blk / 1890 dense / 0 claims)
cargo test -p mev-scout-core --test causal_fp_corpus -- --nocapture

# Real corpus (gated)
$env:MEV_SCOUT_E2E="1"; $env:RPC_URL="<archive-rpc>"
cargo test -p mev-scout-core --test explorer_corpus

# G-06: four pinned AVAX hashes (gated, ignored by default)
cargo test -p mev-scout-core --test explorer_corpus -- --ignored pinned_real_arb_hashes

# Tag vocabulary (ignored, live)
$env:MEV_SCOUT_COVERAGE="1"
cargo test -p mev-scout-core --test explorer_tag_coverage -- --ignored --nocapture

# Live parity (gated)
cargo test -p mev-scout-core --test live_tag_parity
```

**گیت سخت:** `cargo test --workspace --quiet` → exit 0 و `FAILED_SUMMARIES=0`.

---

## 7. چیزهایی که نباید تغییر کنند (Guardrails)

1. `MevKind` جدید اضافه نشود؛ backrun همان `ArbAtomic + tag` باقی بماند.
2. `compute_net_profit()` تنها مرجع net باقی بماند؛ costها نباید دوباره کسر شوند.
3. `pnl_basis="R"` و `mode="realized"` برای arb حفظ شوند.
4. `claim_and_sell` نباید event دوم بدهد.
5. `out_of_core` نباید وارد `core/data/explorer_tags.json` شود.
6. split-route تا زمان representable/executable بودن، detector نشود.
7. thresholdهای causal (`CAUSAL_WINDOW_TXS=8`, `CAUSAL_PRICE_MIN_PCT=0.5`) بدون کالیبراسیون روی seed.real تغییر نکنند.
8. fp_suspect یک observation label است، نه حکم قطعی FP.

---

## 8. Definition of done

- [x] I-1..I-3 دریافت و سلولهای ID نهایی شدند (متن اصلی تسک ۲۰۲۶-۱۱-۲۴؛ بازنگاشت docs-only).
- [x] جدول 29+7 با Evidence Level و Verdict کامل است (تاکسونومی رسمی §9).
- [x] تمام ۷ ردیف Hunt یا Seeded شدند یا نتیجهٔ منفی صریح ثبت شد (۴ seed + ۳ negative؛ هانت ۲۰۲۶-۱۱-۲۴).
- [x] ۴ هش کامل section 3 در تست گیتد pin شدند (اجرای live طبق بهروزرسانی §G-06 با RPC آرشیو ثبت شد).
- [x] `explorer_tags.json` با تگهای emitشده همساز است (`vocabulary_covers_every_emitted_arb_tag`).
- [x] `funding_providers[]` و `profit_cause` بدون تغییر net اضافه شدند.
- [x] تصمیم shape (توکنمحور + فیلد مستقل استخرمحور) در کد و `docs/mev_strategies.md` مستند شد.
- [x] holdout مستقل از derivation window ثبت شد (G-14 — پنجرهٔ ۲۰۲۶-۱۱-۲۴، ۵۶/۵۹ = ۹۴.۹٪).
- [x] `cargo test --workspace --quiet` سبز (۶۹۸ passed / ۲ ignored در lib).
- [x] guard testهای non-emission ضمیمهٔ D برای دستههای Unsupported اضافه شدند.

> **یادداشت 2026-11-23:** ضمایم A–E اضافه شدند. آیتمهای اجرایی بالا بدون تغییر باقی ماندند و تیک نخوردند.

- [x] ضمیمه A: ماتریس ۲۹+۷ با ID / Strategy / Decoder / Detection / Classification / Subtag / Route / P&L / Evidence / Code / Tests / Verdict ثبت و با تاکسونومی رسمی (I-1/I-2) بازنگاشت شد.
- [x] ضمیمه B: ریسکهای False Positive / False Negative ثبت شدند.
- [x] ضمیمه C: جدول Missing Capabilities ثبت شد.
- [x] ضمیمه D: تستپلن ردیفبهردیف Partial / Unsupported ثبت شد.
- [x] ضمیمه E: خلاصه ۶ سؤاله مدیریتی ثبت شد.

---

## 9. ضمیمه A — ماتریس پوشش نهایی (Audit Snapshot)

Evidence scope: `MevKind::ArbAtomic`, successful realized transactions only, at commit `ef29f73` and refreshed with G-05/G-06/G-14 evidence through 2026-11-24. No code changed for the taxonomy remap.

Evidence levels: **H** = code + targeted tests + full real successful tx hash; **M** = code + targeted tests (or a pinned real mechanism hash), no dedicated full golden; **L** = code heuristic or decoder only; **N** = no implementation evidence.

> **Taxonomy source (I-1, بستهشده ۲۰۲۶-۱۱-۲۴):** شناسهها از متن اصلی تسک گرفتهاند: A = Route-Based (AR-01..10)، B = Market-Based (AR-11..23)، C = Behavioral/Execution Subtags (AR-24..29). شمارهگذاری قدیمی سند معتبر نیست؛ نگاشت آن در یادداشت بعد از جدول funding آمده است.

| ID | Strategy | Decoder | Detection | Classification | Subtag / Metadata | Route Accuracy | P&L Accuracy | Evidence Level | Code Location | Test Coverage | Verdict |
|---|---|---|---|---|---|---|---|---|---|---|---|
| AR-01 | Two-Pool Same-DEX | V2/V3/V4/Curve/Balancer/Solidly/LB/Pendle/Fluid/Metric swap topics + transfer attachment | Closed directed walk ≥2 edges, ≥2 pools, ≤8 edges, positive residual | `ArbAtomic`; `two_pool` shape only — protocol identity is metadata, not a separate detector | `arb_meta.arb_shape=two_pool`, `pool_shape`, `venues`, `dex_count` | Selected cycle only; Same-DEX requires `pool_dex` on **every** leg (G-01) | Ledger residual `R`; flash fee netted once | H (shape) / M (identity) | `classify/arb.rs::classify_atomic_arbs`; `profit.rs::closed_cycle_walks`; `classify/legs.rs::resolve_arb_routes` | `baseline_atomic_arb`, `explorer_golden`, `explorer_corpus` | Partial — shape real, Same-DEX unproven without G-01 |
| AR-02 | Two-Pool Cross-DEX | Same as AR-01 | Same cycle gate | `is_cross_dex` from protocol identity; AMM-family fallback | `is_cross_dex`, `cross_dex_basis`, `venues` | Uni V2 + Sushi must be cross-DEX; Uni V2 + Uni V3 must not — only with complete `pool_dex` | `R` unchanged | M | `arb.rs::arb_route_meta`; `classify/mod.rs::pool_dex` | `cross_dex_uses_protocol_identity_not_amm_family`, fallback test | Partial — FP/FN when `pool_dex` absent |
| AR-03 | Triangular | Same as AR-01 | Closed walk with 3 unique tokens (token-count heuristic) | `arb_shape=triangular` | `arb_shape`, `pool_shape` | Token-count heuristic, not a formal 3-pool proof | `R` | H | `arb.rs::arb_route_meta` L760-826; `profit.rs::closed_cycle_walks` | Cycle tests + real AVAX corpus notes | Supported (shape-level) |
| AR-04 | Triangular Cross-DEX | Same as AR-01 | AR-03 gate + cross-DEX gate | No combined classifier; two metadata axes only | `arb_shape=triangular` + `is_cross_dex` | Both heuristics compose without joint validation | `R` | M | `arb.rs::arb_route_meta` L789-824 | Cross-DEX + cycle tests | Partial — metadata composition only |
| AR-05 | Multi-Hop | Same as AR-01 | Closed walk >2 edges, capped at 8 | `arb_shape=multi_hop` | `hop_count`, `pool_shape` | Bounded at 8 edges / 64 candidates; one route per tx | `R` | H | `profit.rs::MAX_CYCLE_EDGES`, `closed_cycle_walks`; `arb.rs::select_route` | `cycle_search_is_depth_bounded`, corpus multi-hop rows | Supported (shape-level) |
| AR-06 | Multi-Hop Cross-DEX | Same as AR-01 | AR-05 gate + cross-DEX gate | No dedicated class | Same as AR-04 + `hop_count` | Same protocol-identity caveat as AR-02 | `R` | M | `arb.rs::arb_route_meta`; `classify/mod.rs::pool_dex` | Cross-DEX tests on multi-hop fixtures | Partial — metadata composition only |
| AR-07 | Combined Multi-Hop | Multiple profitable cycles decoded | G-10 records rejected routes; one event emitted | No combined representation | `details.rejected_routes` | Only one route selected; extra cycles are not merged | `R` for selected cycle; other residuals audited, not summed | L | `arb.rs::select_route`; `profit.rs::closed_cycle_walks` | `other_residuals_usd_is_audited_but_not_summed` | Unsupported |
| AR-08 | Split-Route | No route-split decoder | No detection | No classification | `OPTIMIZER_SPLIT_ROUTE` vocabulary only | `route_json` cannot express parallel branches | No split-specific P&L | N | `types/tags.rs`; G-11 | Non-emission guard (old AR-19 test comment) | Unsupported / Unverified |
| AR-09 | Aggregator-Based | `decode/aggregator.rs`, router topics | Aggregator edges excluded from cycle closure; G-12 `underlying_pools_unknown` | No dedicated detector | `router_version`, `underlying_pools_unknown` | Underlying pools opaque; router-only cycles rejected | `R` for DEX legs only | L | `decode/aggregator.rs`; `legs.rs::is_cycle_edge`; `arb.rs` L435-458 | `aggregator_only_cycle_produces_no_arb`, G-12 tests | Unsupported — FN for genuine mixed cycles |
| AR-10 | Long-Tail | Same as AR-01 | Route touches a non-blue-chip token | Behavioral tag | `long_tail` | Selected route tokens only | `R`; token-pricing risk remains | H (G-05 seed `0x306e1885…` blk 95681751) | `arb.rs::is_long_tail_arb` | `p1_3_long_tail`, gated G-05 test | Supported (tag-level) |
| AR-11 | Stablecoin Depeg | No peg reference / depeg decoder | No detector | No classification | No tag | No depeg route evidence | Generic `R` only if a cycle exists | N | Docs only: `docs/mev_strategies.md` §5.1 | Non-emission guard (old AR-20 test comment) | Unsupported / Unverified |
| AR-12 | Curve Imbalance | Curve `TokenExchange` topics | Curve leg + ≥2 distinct AMM families | Mechanism tag only | `curve_imbalance` | No reserve/imbalance magnitude measurement | `R`; no mechanism-specific share | M (G-05 seed `0xc1532675…` blk 95681823) | `strategy_tags.rs::arb_strategy_tags` | `p3_5_curve_imbalance`, gated G-05 | Partial — label only |
| AR-13 | Balancer Rate Provider | Balancer swap + `TokenRateCacheUpdated` | Balancer leg + cross-venue + no same-pool cache refresh | Mechanism tag only | `rate_provider_staleness` | Pool-scoped refresh check; no rate divergence test | `R` | M (G-05 seed `0x7e3d68f8…` blk 95682026) | `strategy_tags.rs` L39-54; `decode/signals.rs` | `p3_2_balancer_staleness`, gated G-05 | Partial — inferred label |
| AR-14 | LST Arb | No LST fair-value decoder | No detector | No classification | No tag; sAVAX is separate and narrow | No LST peg/reference route | No mechanism attribution | N | Docs only §4.5 | Non-emission guard (old AR-23 test comment) | Unsupported / Unverified |
| AR-15 | sAVAX Rate Arb | sAVAX address + `getPooledAvaxByShares` | sAVAX↔wrapped-native with ≥5 bps implied divergence | Exact mechanism tag | `savax_rate_arb`, `tag_modes.savax_rate_arb=exact` | Only direct sAVAX/WAVAX leg drives divergence | `R`; divergence amount not attributed | M (G-05 seed `0xb77e1291…` blk 95682059) | `strategy_tags.rs` L79-95; `arb.rs` L459-461 | `p3_16_savax_rate_arb`, gated G-05 | Partial — exact tag, no profit split |
| AR-16 | Wrapped Token Arb | Wrapped-native mint/burn delta filtering | Generic cycle may include wrapped token | Generic `ArbAtomic` only | `wrapped_native_marker` | Wrap noise filtered; no peg-gap reconstruction | `R`; wrap residue policy documented | L | `profit.rs` wrap-noise tests; `types.rs::wrapped_native` | `wrap_noise_filtered`, `wrap_then_spend_without_native_flow_keeps_the_mint` | Partial — generic-only |
| AR-17 | Bridged Token Arb | No bridge/peg decoder | No detector | No classification | No tag | No canonical/bridged asset map | No bridged-specific P&L | N | Docs only | Non-emission guard (old AR-22 test comment) | Unsupported / Unverified |
| AR-18 | Fee-on-Transfer Arb | FoT registry lookup on route tokens | Route token carries FoT flag | Mechanism tag | `fot_arb` | Nominal swap amounts; tax visible in `route_delivery.net` | `R`; tax not decomposed | L + explicit 0-hit G-05 | `strategy_tags.rs` L56-74; `pool/state/pool_types.rs` | `p3_12_fot_rebase_arb` | Partial |
| AR-19 | Rebase Token Arb | Rebase registry lookup on route tokens | Route token carries rebase flag | Mechanism tag | `rebase_arb` | No balance-drift verification at classify time | `R` | L + explicit 0-hit G-05 | `strategy_tags.rs` L56-77 | `p3_12_fot_rebase_arb` | Partial |
| AR-20 | Oracle-AMM Arb | Oracle update decoder exists for other passes | No atomic-arb detector | No classification | `ORACLE_AMM` vocabulary only; never emitted | No oracle-vs-AMM route reconstruction | No oracle attribution | N | `types/tags.rs::TAG_AXIS_REGISTRY` | Non-emission guard (old AR-26 test comment) | Unsupported / Unverified |
| AR-21 | Pendle PT/YT Arb | `decode_pendle` → `Amm::Pendle` | Generic cycle may use a Pendle leg | Generic `ArbAtomic` only | No Pendle mechanism tag | PT/YT semantics not modeled | `R`; no yield-spread split | L | `decode/swap.rs::decode_pendle`; `types.rs::Amm::Pendle` | Non-emission guard (old AR-24 test comment) | Partial — venue decoded only |
| AR-22 | Vault Share Arb | Balancer Vault flash decoder | No share-price detector | Generic `ArbAtomic` only | `funding:balancer_v2_vault` | Vault is funding, not a priced share venue | Fee netting only; no NAV/share-price attribution | L | `types/tags.rs::canonical_funding_source`; `arb.rs` | Non-emission guard (old AR-25 test comment) | Partial — funding semantics only |
| AR-23 | Epoch Transition Arb | `NotifyReward` facts + epoch-venue registry | ≥2 epoch-pool legs + reward token on route | Mechanism tag | `epoch_transition` | Requires both legs on configured epoch venues | `R` | L + explicit 0-hit G-05 | `strategy_tags.rs` L97-116; `decode/signals.rs` | `p3_15_epoch_transition` | Partial — label only |
| AR-24 | Flash Loan Arb | Explicit flash topics: Aave V2/V3, Balancer, Uni V3, LFJ, Morpho | Fee/principal netting before residual; `flash_arb` | `ArbAtomic`, Exact/Inferred | `flash_arb`, `funding:*`, `funding_providers[]` | Funding does not alter route | Principal excluded; fee netted once | H (G-05 seed `0x306e1885…` blk 95681751) | `decode/flash_loan.rs::decode_flash_loan`; `arb.rs` L73-149, L373-452 | `p0_4_flash_arb`, `split_flash_repay_is_detected`, gated G-05 | Supported |
| AR-25 | Flash Swap Arb | Logless inference from out→callback→in transfer ordering (`Amm::V2\|Solidly`) | Inferred flash fact on same cycle | `ArbAtomic`, Inferred | `flash_arb`, `funding:v2_flash_swap` | Provider = pool address; no V4 flash accounting | Fee implicit in swap amounts; no double subtraction | H (same G-05 seed carries `funding:v2_flash_swap`) | `decode/flash_loan.rs::infer_v2_flash_swap`; `classify/mod.rs` L629-636 | `v2_flash_swap_is_inferred_as_funding`, prefunded negative | Supported (V2/Solidly only) |
| AR-26 | Backrun Arb | Same as AR-01 + causal price/adjacency facts | Prior market-moving leg, opposite direction, better price | Same `ArbAtomic`; tag only | `backrun` + causal evidence | Anchors on emitted route; no route rewrite | `R` owned by arb pass; causality `inferred` | H | `classify/causal.rs::classify_backruns`; `legs.rs` L22-48 | `baseline_backrun`, `core/tests/backrun.rs`, full ETH hashes | Supported |
| AR-27 | Forwarded Profit | Transfer attached to same tx | Non-swap third-party payee; funder-fed only | `ArbAtomic`, `Inferred` | `forwarded_profit` | Route unchanged; payee is not a route leg | Residual attributed as Inferred, not double-counted | M | `arb.rs` L230, L404 | `forwarded_profit_in_priority_token_is_inferred`, G-09 negative | Supported as subtag — attribution Inferred, not Exact |
| AR-28 | Claim & Sell + ArbAtomic | Zero-from `Transfer` + same-tx sell swap | Existing ArbAtomic is retagged | Still `ArbAtomic` when cycle exists | `claim_and_sell`, `airdrop`, `claimed_token` | Route unchanged; claim is same-tx only | No duplicate profit; standalone claim → Utility (outside scope) | L | `arb.rs::classify_claim_and_sell`; `strategy_tags.rs` | `claim_and_sell_tags_the_existing_arb_instead_of_double_counting` | Partial by scope exception |
| AR-29 | Multi-Funding Arb | All seven provider decoders | ≥2 flash facts in one tx | `ArbAtomic` + one tag per provider | `funding_providers[]`, `funding:*` per provider | Route independent of provider count | Fees summed once per provider; no double netting | M (code+test; no real hash) | `arb.rs` L378-452 | `multiple_flash_providers_are_all_persisted_and_tagged` | Supported (code/test-level) |

### Funding sources (I-2, بستهشده ۲۰۲۶-۱۱-۲۴)

منابع رسمی متن تسک `FUND-01..07`اند؛ ترتیب قبلی سند (Aave V2 و Morpho Blue را شامل میشد) معتبر نیست.

| ID | Funding Source | Decoder | Detection | Classification | Subtag / Metadata | Route Accuracy | P&L Accuracy | Evidence Level | Code Location | Test Coverage | Verdict |
|---|---|---|---|---|---|---|---|---|---|---|---|
| FUND-01 | Balancer V2 Flash Loan | `decode_balancer_flash` via `decode_flash_loan` | `flashloans` fact → fee/principal netting | `funding_source=balancer_v2_vault` | `funding:balancer_v2_vault`, `flash_arb` | Route independent | Fee netted only when transfer pattern permits | M | `decode/flash_loan.rs` L95-110; `types/tags.rs` L96-121 | Canonical source test | Supported (code only) |
| FUND-02 | Aave V3 Flash Loan | Explicit `FlashLoan` topic decoder | Same | `funding_source=aave_v3` | `funding:aave_v3` | Same | Premium decoded and netted | M | `decode/flash_loan.rs` L55-73 | `aave_flash_loans_decode` | Supported (code only) |
| FUND-03 | Uniswap V3 Flash | Explicit `Flash` topic decoder | Same; zero-token needs transfer-based netting | `funding_source=univ3_flash` | `funding:univ3_flash` | Same | `token=ZERO` skips principal netting; fee may be recorded with zero amount leg | L | `decode/flash_loan.rs` L25-53 | Topic-family + canonical mapping tests | Supported (code only, weak) |
| FUND-04 | Uniswap V2 Flash Swap | Transfer-order inference (`infer_v2_flash_swap`) | Inferred funding fact | `funding_source=v2_flash_swap` | `funding:v2_flash_swap`, `flash_arb` | Provider = pool; no callback trace | Fee embedded in swap amounts; no explicit in-kind fee | H (G-05 seed `0x306e1885…` blk 95681751) | `decode/flash_loan.rs::infer_v2_flash_swap` L198-263 | `v2_flash_swap_is_inferred_as_funding`, prefunded negative | Supported (real seed) |
| FUND-05 | SushiSwap / V2 Fork Flash Swap | Same V2 inference; protocol identity only as pool address / venue tag | No Sushi-specific funding key | Generic `funding_source=v2_flash_swap` | `funding:v2_flash_swap`, `venue:sushiv2` | Provider is the pool, not the fork protocol | Same generic V2 fee treatment | M | `types/strategy.rs::FlashLoanProvider::V2FlashSwap`; `mev/detectors/tagging.rs::venue_tag_for_dex_name` | Venue-tag tests; no funding-specific golden | Partial — generic V2 key, no fork-specific identity |
| FUND-06 | LFJ Liquidity Book Flash | 2.0 and 2.1/2.2 topic decoders | Same | `funding_source=lfj_lb_flash` | `funding:lfj_lb_flash` | Same | 2.1/2.2 picks one packed leg on ties | M | `decode/flash_loan.rs` L132-184 | `lfj_lb_flash_loan_decodes`, `lfj_lb_v20_flash_loan_decodes` | Supported (code only) |
| FUND-07 | Multiple Flash Loan Providers | All provider decoders + `funding_providers[]` | ≥2 providers in one tx | `funding_source` stays first (backward compatible) | One `funding:*` per provider | Route independent | Per-provider fee sum; no double netting | M (code+test; no real hash) | `arb.rs` L378-452 | `multiple_flash_providers_are_all_persisted_and_tagged` | Supported (code/test-level) |

> **خارج از لیست رسمی (شمارش نمیشوند):** Aave V2 و Morpho Blue در `canonical_funding_source` وجود دارند ولی در `FUND-01..07` نیامدهاند؛ GMX ADL (ردیف قدیمی AR-16)، init-price snipe (AR-27 قدیم)، CEX/cross-chain/spot-perp (AR-28 قدیم) و statistical/pairs/NFT-floor (AR-29 قدیم) guard-case باقی میمانند (G-05 سه مورد نخست را منفی صریح ثبت کرد).
>
> **نگاشت شمارهگذاری قدیمی → رسمی:** AR-02→03، AR-03→05، AR-04→02/06، AR-05→01 (same-DEX/different-AMM اکنون جنبهٔ AR-01/02 است)، AR-06→24، AR-07→25، AR-08→10، AR-09→26، AR-10→12، AR-11→13، AR-12→18، AR-13→19، AR-14→15، AR-15→23، AR-16→خارج، AR-17→28، AR-18→09، AR-19→08، AR-20→11، AR-21→16، AR-22→17، AR-23→14، AR-24→21، AR-25→22، AR-26→20، AR-27/28/29→خارج. کامنتهای سه guard test در `scenarios.rs` هنوز از شمارههای قدیمی استفاده میکنند (docs-only ماندن بازنگاشت).

---

## 10. ضمیمه B — ریسکهای False Positive / False Negative

1. **Cross-DEX false positives** — `arb_route_meta` falls back to `amm_family` unless every leg has a non-empty `pool_dex`. Uniswap V2 + SushiSwap V2 can be reported as cross-DEX with `cross_dex_basis="amm_family"`. Conversely, incomplete venue data can erase a true protocol-level cross-DEX claim.

2. **Route-shape ambiguity** — `arb_shape` uses `hop_count == 2 || unique_tokens == 2` then `== 3`. A 3-hop/2-token route is labeled `two_pool`; a 6-hop/3-token route is labeled `triangular`. These are token-count/hop heuristics, not pool-count facts.

3. **Aggregator false negatives / false positives** — `Amm::Aggregator` legs are excluded from cycle closure to prevent one-way router conversions from becoming arbs. This can miss genuine aggregator-wrapped cycles. Dedup tolerances of 200/400 bps over ≤3 hops can also drop legitimate edges or leave redundant ones.

4. **Token-direction uncertainty** — `attach_swap_tokens` falls back to ±24-log proximity. Balancer skips pairing entirely; 0x `Fill` leaves tokens unresolved until transfer pairing. Wrong direction changes cycle existence, route pools, and profit-token attribution.

5. **Cycle-search false negatives** — cycles are simple (no repeated intermediate token), require ≥2 distinct pools, are capped at 8 edges and 64 candidates, and use one selected route per transaction. Multiple profitable cycles in one tx produce one event.

6. **Funding-source under-attribution** — all fees are summed, but `funding_source`, funding tag, and canonical ID funding key come from `tx.flashloans.first()`. Multi-provider funding is not represented for `ArbAtomic`. V4 unlock/callback flash accounting is invisible.

7. **Forwarded-profit false positives / negatives** — `forwarded_recipient` infers a single funder-fed payee without requiring contract/router identity. Two payees are conservatively rejected; an unrelated EOA transfer could be misclassified as profit forwarding.

8. **P&L false positives** — residual accounting can still record dust-level fake profit; the AVAX `fp_suspect` row `0x2c388e3d…2c30cb` is an observed example. Deficit vetoes help but do not eliminate this class. The hash here is truncated and is **not** a full 64-hex real sample.

9. **P&L false negatives** — `other_residuals` are audited but excluded from `profit_tokens` USD sums; unwrap-to-beneficiary and coinbase/searcher kickbacks are not visible in a logs-only receipt path; tip-sink forwarding is dropped entirely.

10. **Mechanism-tag false positives** — `curve_imbalance` needs only Curve + another AMM family; `rate_provider_staleness` needs no actual rate divergence; FoT/rebase tags are registry-driven; epoch and GMX tags depend on configured registries. These are labels, not mechanism-verified classifications.

11. **Backrun heuristic risk** — `CAUSAL_WINDOW_TXS=8` and `CAUSAL_PRICE_MIN_PCT=0.5` remain uncalibrated against a broad real seed set. Real ETH seeds exist, but thresholds are still heuristics.

12. **Classifier precedence** — liquidation, skim, and one-kind-per-tx arbitration can suppress an `ArbAtomic` that shares a tx with another P&L-bearing kind. This is intentional for USD non-double-counting but is a recall loss for arb census purposes.

---

## 11. ضمیمه C — قابلیتهایmissing / محدودیتها

| Capability | Current state | Consequence | Relevant files |
|---|---|---|---|
| Complete protocol identity on every route pool | `pool_dex` optional; empty falls back to AMM family | Cross-DEX FP/FN | `classify/mod.rs::BlockContext.pool_dex`, `arb.rs::arb_route_meta` |
| Explicit route geometry model | One ambiguous `arb_shape` string | Shape claims are not reproducible from pool count | `arb.rs::arb_route_meta` |
| Aggregator underlying-pool reconstruction | Only router edge + `router_version`; selected DEX pools hashed | Split/aggregator routes not accurately represented | `decode/aggregator.rs`, `classify/legs.rs`, `types/opportunity.rs::underlying_pools_hash` |
| Multi-provider funding model | First flash loan only | Multi-source arb funding invisible | `arb.rs` L373-434, `decode/flash_loan.rs`, `types/tags.rs` |
| Mechanism-specific profit decomposition | Every row uses `pnl_basis="R"` residual | Cannot state how much profit came from Curve imbalance, sAVAX divergence, etc. | `arb.rs`, `strategy_tags.rs`, `store/pnl.rs` |
| Multi-residual USD policy | `other_residuals` audited but excluded | Under-counting is documented but not exposed as a separate metric | `types.rs::MevEvent`, `store/pnl.rs` |
| Emitted-tag vocabulary completeness | `forwarded_profit` and `funding:*` tags are emitted but absent from `core/data/explorer_tags.json` | Tag coverage tools and consumers drift from reality | `core/data/explorer_tags.json`, `explorer_tag_coverage.rs` |
| Full real-hash golden set | ETH backrun hashes exist; 4 AVAX cache hashes are documented but not pinned in code; most mechanism rows have no seed | Cannot make mechanism-level real-world claims | `tests/explorer_corpus.rs`, `docs/arb_atomic_audit_gaps_plan.md` §3 |
| Unsupported-mechanism guards | No tests asserting non-emission for docs-only categories | Scope creep risk if future decoders emit misleading generic arbs | docs + classifier tests |
| Independent holdout benchmark | 47/47 is derivation window, not holdout | Recall cannot be generalized | `docs/benchmarks/mevlive_avax_arb_recall_2026-11-09.md` |
| Token attachment certainty | ±24-log proximity fallback; Balancer skip; 0x tokens unresolved until transfer pairing | Direction/pool mistakes alter route and P&L owner | `decode/attach.rs`, `decode/aggregator.rs` |
| Cycle-search transparency | Simple cycles only; ≤8 edges; ≤64 candidates; one selected route per tx | Recall loss and unreported rejected routes | `profit.rs::closed_cycle_walks`, `arb.rs::select_route` |
| Backrun threshold calibration | `CAUSAL_WINDOW_TXS=8`, `CAUSAL_PRICE_MIN_PCT=0.5` | Causal claims remain inferred, not calibrated | `legs.rs`, `classify/causal.rs` |

---

## 12. ضمیمه D — تستپلن ردیفبهردیف Partial / Unsupported

### Real successful transactions available

Use these only as real evidence; synthetic fixtures must not be described as real samples.

| Chain | Block | Tx hash | Use |
|---|---:|---|---|
| Avalanche | 100528634 | `0x37b413bbbd6af98b4919897223d39bfb3313f1e261232936c7b8cde54563bff9` | Generic two-pool shape, route/meta/PnL golden |
| Avalanche | 100529098 | `0x7c709477088b85667b3b641d1b80546d833a6082b6570890244b975ecc106f1a` | Multi-hop, inferred/forwarded profit |
| Avalanche | 100529236 | `0x4d03e567171248c005a64da265667342bc1dedc6d9724e39643b0ba6268851e9` | Multi-hop metadata |
| Avalanche | 100529720 | `0x952f7e215908ea1d35fb59c43bc1c7dec109f935c18a615d62f0155025ea3d16` | Triangular metadata |
| Ethereum | 26264539 | `0xdf4f0afbe7551cfe9805628fc2d2a3a2909e859f32bda202888c184a705abb03` | Backrun tag on ArbAtomic |
| Ethereum | 26264858 | `0xf33bf44580da521ad53d0188bf9c5b3a507e50e61160a573ba28090b053e5c6a` | Second backrun tag |
| Avalanche | 97781738 | `0xf20793cadb1dfd7c48a24d302feb2d100505e90f7e846c9fb603a80d1c053fb4` | Claim-and-sell context; currently seeded as Utility, not ArbAtomic subtag |
| Avalanche | 97782322 | `0x9300191c7626be981444544234c2bcea719065c8142c1749f389fe431cc2d88b` | Aave V3 flash context; out-of-scope liquidation, not ArbAtomic |

All other hashes found in docs with `…` are truncated and must not be presented as full 64-hex real samples.

### Required tests for Partial / Unsupported rows

| IDs | Required test | Real-sample status |
|---|---|---|
| AR-01, AR-02 | Fixture matrix: complete `pool_dex`, partial `pool_dex`, empty `pool_dex`; assert `venues`, `dex_count`, `is_cross_dex`, `cross_dex_basis` | 4 full AVAX hashes above available; protocol-identity golden blocked by G-01 |
| AR-04, AR-06 | Triangular/multi-hop cross-DEX composition: assert `arb_shape` and `is_cross_dex` are independently derived | No dedicated real hash |
| AR-07 | Multi-cycle tx: assert one event, `rejected_routes` recorded, extra residuals audited but not summed | `other_residuals_usd_is_audited_but_not_summed` exists |
| AR-08 | Guard: split-route opportunity input must not create `ArbAtomic` until parallel-branch representation exists | No real sample |
| AR-09 | Aggregator+DEX mixed route: assert why a cycle is rejected; `underlying_pools_unknown`; router internals excluded from selected route | KyberSwap regression noted; no full hash |
| AR-11 | Stablecoin depeg hunt + negative assertion when no peg reference exists | No full real depeg ArbAtomic hash |
| AR-12 | Curve + DEX fixture plus imbalance-magnitude negative (Curve leg without imbalance) | G-05 real seed pinned `0xc1532675…` |
| AR-13 | Same-pool refresh clears tag; other-pool refresh does not; rate-divergence negative | G-05 real seed pinned `0x7e3d68f8…` |
| AR-14 | LST depeg negative: no fair-value reference → no LST-depeg claim | No real sample |
| AR-15 | Both sAVAX directions, below-threshold divergence, no-rate negative; assert `tag_modes` | G-05 real seed pinned `0xb77e1291…` |
| AR-16 | Wrapped-native mint/burn fixtures: wrap noise filtered; beneficiary unwrap not invented | Generic real hashes may exercise wrapping; no peg-specific sample |
| AR-17 | Bridged asset negative: no bridge identity → no bridged-arb claim | No real sample |
| AR-18, AR-19 | FoT/rebase positive + non-FoT/non-rebase negative; assert net transfer tax in `route[].amount_*_net` | G-05 explicit 0-hit negatives in 600-block hunt |
| AR-20 | `ORACLE_AMM` remains vocabulary-only; assert core detector never emits it | Guard test exists |
| AR-21 | Pendle decoder golden with token direction + negative asserting no yield-spread claim | No full real Pendle ArbAtomic hash |
| AR-22 | Vault-as-funding vs vault-as-venue separation; no NAV/share-price claim from funding alone | Guard test exists |
| AR-23 | Reward token on/off route, one vs two epoch pools, timestamp-only negative | G-05 explicit 0-hit negative |
| AR-24, AR-25 | Flash loan/swap: fee netting once, prefunded V2 negative, V4 unlock/callback negative | G-05 real seed `0x306e1885…` carries both `flash_arb` and `funding:v2_flash_swap` |
| AR-26 | Backrun causality positive + unrelated price-move negative | Full ETH hashes pinned in §3 |
| AR-27 | Forwarded-profit positive + funder-fed/unrelated-payee negatives | G-09 negative fixture exists; G-05 seed carries tag |
| AR-28 | Same-tx claim-and-sell + closed arb: one `ArbAtomic`, no second Utility | Real Utility hash exists; no combined real hash |
| AR-29 | Two providers with distinct protocols: both in `funding_providers[]`, one event, correct fee sum | `multiple_flash_providers_are_all_persisted_and_tagged` exists; no real hash |
| FUND-01..07 | Per-provider decoder golden, funding tag, canonical `funding_source`, fee netting, multi-provider array, first-provider backward compatibility | FUND-04 has a real G-05 seed; others code/test-level only |

**وضعیت اجرای تستپلن (۲۰۲۶-۱۱-۲۴، بازنگاشت I-1/I-2):** AR-24/AR-25 توسط تستهای flash و G-05، AR-29 توسط `multiple_flash_providers_are_all_persisted_and_tagged`، AR-09 توسط `realized_arb_carries_canonical_provenance_in_details` پوشش داده شدند. guard testهای غیاب emission برای AR-08، AR-11، AR-14، AR-17، AR-20، AR-21، AR-22 (و سه دستهٔ خارج از تاکسونومی) در `core/src/explorer/scenarios.rs` وجود دارند. سایر ردیفها منوط به seed.real/RPCاند.

---

## 13. ضمیمه E — خلاصه ۶ سؤاله مدیریتی (متن اصلی §۶ تسک)

**Q1 — from 29 arbitrage types, how many are Supported / Partial / Unsupported / Unverified?**
**Supported: 8** (AR-03, AR-05, AR-10, AR-24, AR-25, AR-26, AR-27, AR-29) · **Partial: 14** (AR-01, AR-02, AR-04, AR-06, AR-12, AR-13, AR-15, AR-16, AR-18, AR-19, AR-21, AR-22, AR-23, AR-28) · **Unsupported: 7** (AR-07, AR-08, AR-09, AR-11, AR-14, AR-17, AR-20) · **Unverified: 0** (ردیفهای فاقد پیادهسازی Unsupported ثبت شدند، نه Unverified). هیچ ردیف مکانیزمی (غیر از تگهای رفتاری دقیق sAVAX/flash/backrun) با اثبات اقتصادی کامل روی tx.real تأیید نشده است.

**Q2 — from 7 funding sources, how many are really identified?**
**6 Supported** (FUND-01 Balancer V2, FUND-02 Aave V3, FUND-03 Uni V3 Flash, FUND-04 Uni V2 Flash Swap, FUND-06 LFJ LB, FUND-07 Multiple providers) و **1 Partial** (FUND-05 Sushi/V2 fork — فقط کلید عمومی `v2_flash_swap`، بدون هویت funding اختصاصی). FUND-04 تنها منبعی است که هش real موفق G-05 دارد؛ بقیه code/test-levelاند. Aave V2 و Morpho Blue canonical sourceاند ولی در لیست رسمی ۷تایی نیستند.

**Q3 — which capabilities only record a generic `ArbAtomic` without identifying the real subtype?**
**9 rows:** AR-01, AR-02, AR-04, AR-06 (route/cross-DEX تنها metadata)، AR-03 و AR-05 (شکل hop-محور، نه اثبات تعداد pool)، AR-16 (wrapped)، AR-21 (Pendle فقط venue)، AR-22 (Vault فقط funding). AR-07/AR-08/AR-09 اساساً subtypeای emit نمیکنند؛ AR-12/13/15/18/19/23 تگ مکانیزمی میدهند ولی سهم P&L مکانیزم را جدا نمیکنند.

**Q4 — five critical defects with the largest impact on Explorer output correctness?**
1. **D1 — G-01 / `pool_dex`:** با کش ناقص، `cross_dex_basis="amm_family"` جای protocol identity مینشیند → Same-DEX/Cross-DEX FP و FN (Uni V2+Sushi در مقابل Uni V2+Uni V3).
2. **D2 — route representation:** `arb_shape` مبتنی بر token-count/hop است، نه pool-count؛ combined multi-hop، split-route و aggregatorِ دارای pool زیرین قابل بازسازی نیستند و یک tx فقط یک event میدهد.
3. **D3 — funding identity:** `funding_source` هنوز first provider است؛ `funding_providers[]` اضافه شد ولی Sushi/Fork و V4 flash accounting هویت جداگانه ندارند.
4. **D4 — mechanism evidence:** تگهای Curve/Balancer/FoT/Rebase/Epoch/Pendle/Vault بدون اندازهٔ عدمتعادل، divergence، balance-drift یا fair-value صادر میشوند و هیچ `profit_cause` مکانیزمی بر `net_profit_usd` اثر نمیکند.
5. **D5 — real-sample coverage:** چند ردیف (split, depeg, LST, bridged, aggregator-mixed, multi-funding, claim+arb) فاقد tx.real موفق pinشدهاند؛ سه negative صریح G-05 تنها جایگزین مستندشدهاند.

**Q5 — minimum changes required for reliable coverage?**
تثبیت نگاشت رسمی ۲۹+۷ (انجام شد، docs-only)؛ پر کردن `pool_dex` و fail-closed به `unknown` (G-01)؛ مدل route با `pool_count` و شاخههای موازی + مقایسهٔ cycleها بهجای first-cycle؛ نگهداری `funding_providers[]` با هویت provider اختصاصی؛ آستانهٔ شواهد مکانیزم قبل از emission تگ + `profit_cause` صرفاً metadata؛ golden test برای هشهای real باقیمانده و negativeهای مستند.

**Q6 — suggested repair order?**
**1 — Contract:** taxonomy/tag vocabulary و guardrails (تکمیل شد). **2 — Identity:** `pool_dex` کامل و cross-DEX fail-closed (G-01). **3 — Route:** shapeها، multi-cycle و parallel-branch/aggregator reconstruction. **4 — Evidence:** آستانههای مکانیزم و جداسازی تگ از اثبات اقتصادی. **5 — Attribution:** `profit_cause` و multi-residual، بدون تغییر `net_profit_usd`. **6 — Samples:** pin کردن هشهای real باقیمانده و ثبت negative huntها.

---

**پایان سند.** ضمایم A–E عکس وضعیت در کامیت `ef29f73`اند، ولی ماتریس A و خلاصهٔ E در ۲۰۲۶-۱۱-۲۴ با تاکسونومی رسمی متن تسک بازنگاشت شدند (I-1..I-3 بسته؛ هرگونه شمارهٔ AR/FUND قدیمی در کامنتها یا اسناد قدیمی معتبر نیست). لایهٔ کد WP-A/WP-C/WP-D و guard testهای ضمیمهٔ D در کامیتهای `238ff35`, `73fbd62`, `cda3ea0`, `72499e5` پیاده و تست شدند؛ G-05 و G-14 با RPC آرشیو Dwellir بسته شدند. `cargo test --workspace --quiet` سبز است (۶۹۸ passed / ۲ ignored در lib؛ تست گیتد G-05 جداگانه ۴/۴ اجرا شد). بازنگاشت تاکسونومی docs-only بود و کدی تغییر نکرد. باقیماندهٔ plan فقط enrichment `pool_dex` (G-01) است.

