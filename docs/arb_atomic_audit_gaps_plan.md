#plan پیشنهادی بستن گپهای ممیزی `MevKind::ArbAtomic`

> **وضعیت:** لایهٔ کد WP-A (G-03، G-04) و موارد code-only P1/P2 (G-02، G-07–G-10) پیادهسازی شد و `cargo test --workspace --quiet` سبز است؛ بقیه منوط به I-1..I-3 و دسترسی RPC/archive است.  
> **تاریخ:** 2026-11-23  
> **دامنه:** فقط `core/src/explorer/` و فقط تراکنشهای `success=true` با `MevKind::ArbAtomic` محققشده  
> **ساختار:** دو لایه — `§0..§8` plan/gap registry و `§9..§13` audit snapshot @ `ef29f73`.  
> **قید مهم:** تغییرات فقط در `core/src/explorer/`، `core/data/`، `core/tests/` و docs مربوطهاند؛ نه `MevKind` جدیدی اضافه شد، نه `compute_net_profit`/basis `R`/thresholdهای causal تغییر کرد و نه event دومی ایجاد شد. هشهای real و ورودیهای I-1..I-3 هنوز تأمین نشدهاند.

---

## خلاصهٔ اجرا (2026-11-23)

| گپ | وضعیت | شواهد / یادداشت |
|---|---|---|
| G-01 (P0) | جزئی | کد `cross_dex_basis=protocol` و تست آن از قبل موجود است؛ pin کردن ردیف real مسدود است چون `pool_dex` کش ناقص است و enrichment در `jobs/index.rs` خارج از دامنه است. |
| G-02 (P1) | ✅ تمام | `arb_shape_basis="token_count"` + فیلد مستقل `pool_shape`؛ ماتریس ۴ حالته؛ مستندات همساز شد. |
| G-03 (P0) | ✅ تمام | `details.funding_providers[]` + یک تگ بهازای هر provider؛ `funding_source` همان first entry؛ تست double-flash با جمع صحیح fee و بدون کسر مضاعف. |
| G-04 (P0) | ✅ تمام | `explorer_tags.json` شامل `forwarded_profit` و ۸ تگ `funding:*`؛ تست `vocabulary_covers_every_emitted_arb_tag`. |
| G-05 (P1) | مسدود | ۷ ردیف Hunt نیاز به RPC/archive دارند. |
| G-06 (P1) | ✅ تست گیتد اضافه شد | ۴ هش §3 بهعنوان literal در `core/tests/explorer_corpus.rs` pin شدند (`pinned_real_arb_hashes_reproduce_route_meta_and_pnl`)؛ اجرا منوط به `MEV_SCOUT_E2E=1 RPC_URL=...` با endpoint آرشیو است و تاکنون اجرا نشدهاست. |
| G-07 (P2) | ✅ تمام | `details.profit_cause` صرفاً metadata؛ صفر تغییر در `net_profit_usd`. |
| G-08 (P2) | ✅ تمام | تست موجود `other_residuals_usd_is_audited_but_not_summed` سیاست را پوشش میدهد. |
| G-09 (P2) | ✅ تمام | negative fixture `unrelated_transfer_is_not_a_forwarded_payee`؛ TP موجود حفظ شد. |
| G-10 (P2) | ✅ تمام | `details.rejected_routes` + assert در تست دوچرخه، بدون double P&L. |
| G-11 (P3) | عمداً خارج | تا زمانی که مدل/اجرای end-to-end وجود ندارد detector اضافه نمیشود. |
| G-12 / G-13 / G-15 (P3) | ✅ تمام | WP-D: G-12 flag `underlying_pools_unknown` + ۲ تست (aggregator+DEX متمایز از DEX-only)؛ G-13 یادداشت محدودیت native inflow در `docs/ARCHITECTURE.md` + ۱ negative test در `profit.rs`؛ G-15 جدول تثبیت `explorer tag ↔ live scope tag ↔ disposition` در §3.3.1 با ثبت کامل `NA_TAGS`. |
| G-14 (P1) | مسدود | پنجرهٔ holdout مستقل + RPC لازم است. |

**ورودیهای مسدودکننده:** I-1 (۲۱ شناسهٔ AR)، I-2 (۷ شناسهٔ FUND)، I-3 (متن ۶ سؤال).  
**تأیید:** `cargo test --workspace --quiet` ← exit 0 (۶۹۳ تست passed، ۲ ignored در lib؛ تمام integration suites سبز).  
**کامیت:** این بهروزرسانی همراه با کد/تستها در یک کامیت ثبت شد.  
**بازتأیید ۲۰۲۶-۱۱-۲۳:** ادامهٔ اجرای plan از نقطهٔ توقف — `cargo test --workspace --quiet` ← exit 0 (۶۹۳ passed / ۲ ignored)؛ کد و تستهای G-02..G-10 و `explorer_tags.json` در درخت موجود تأیید شد. بررسی کاشهای محلی (`cache/*.sqlite`) نشان داد هیچ‌یک از ۴ هش pin (بلاک‌های ۱۰۰.۵M) و هیچ تگ Hunt در کاش نیست؛ بنابراین G-05، اجرای live G-06 و G-14 همچنان به RPC آرشیو (AVAX) محدود است و G-01 به enrichment `pool_dex` (فایل `jobs/index.rs` خارج از دامنهٔ فاز) محدود است.
**بازتأیید WP-D ۲۰۲۶-۱۱-۲۳:** G-12/G-13/G-15 پیاده و تست‌شده — `cargo test --workspace --quiet` ← exit 0؛ `underlying_pools_unknown` (G-12) در `classify/arb.rs`، یادداشت محدودیت + negative test (G-13) در `docs/ARCHITECTURE.md`/`explorer/profit.rs`، و جدول تثبیت scope/disposition + ثبت کامل `NA_TAGS` (G-15) در `docs/atomic_capital_free_arb_plan.md` §3.3.1.

---

## 0. ورودیهای لازم از شما (Blocking)

سه مورد هست که بدون آنها نمیتوان جدول نهایی را دقیق بست. بقیهٔ plan مستقل از اینهاست و قابل اجراست.

| # | ورودی | چرا لازم است | فرمت درخواستی |
|---|-------|--------------|----------------|
| I-1 | نگاشت ۲۱ شناسهٔ باقیماندهٔ AR | ۸ شناسه تأیید شد (AR-10, 11, 12, 13, 14, 18, 19, 20). بقیه در مخزن و در زمینهٔ فعلی وجود ندارند. `grep` برای `AR-` و `FUND-` نتیجهای نداد و خود `docs/atomic_capital_free_arb_plan.md:126-136` صریحاً میگوید لیست خارجی است. | `AR-01 = ...` تا `AR-29 = ...` |
| I-2 | نگاشت ۷ شناسهٔ FUND | کد ۷ مسیر funding دارد ولی شمارهٔ آنها در چکلیست مشخص نیست. | `FUND-01 = ...` تا `FUND-07 = ...` |
| I-3 | متن ۶ سؤال خلاصهٔ مدیریتی | پاسخ فعلی روی ۶ سؤال محتمل نوشته شده، نه متن اصلی. | `Q1 = ...` تا `Q6 = ...` |

**معیار بسته شدن I-1..I-3:** صرفاً جایگزینی سلولهای `ID` جدول و section خلاصه؛ نیازی به بازبینی شواهد نیست.

---

## 1. وضعیت فعلی

| شاخص | مقدار |
|---|---|
| ردیفهای ArbAtomic | 29 (7 Supported / 13 Partial / 9 Unsupported) |
| ردیفهای funding | 7 (6 Supported-on-code / 1 Partial) |
| تأییدشده روی tx.real موفق | 7 ردیف generic و رفتاری |
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

- [ ] I-1..I-3 دریافت و سلولهای ID نهایی شدند.
- [ ] جدول 29+7 با Evidence Level و Verdict کامل است.
- [ ] تمام ۷ ردیف Hunt یا Seeded شدند یا نتیجهٔ منفی صریح ثبت شد.
- [x] ۴ هش کامل section 3 در تست گیتد pin شدند (اجرای live منوط به RPC آرشیو باقی است).
- [ ] `explorer_tags.json` با تگهای emitشده همساز است.
- [ ] `funding_providers[]` و `profit_cause` بدون تغییر net اضافه شدند (در صورت تصویب).
- [ ] جلسهٔ تصمیم shape (توکنمحور یا استخرمحور) تشکیل و مستند شد.
- [ ] holdout مستقل از derivation window ثبت شد.
- [ ] `cargo test --workspace --quiet` سبز.

> **یادداشت 2026-11-23:** ضمایم A–E اضافه شدند. آیتمهای اجرایی بالا بدون تغییر باقی ماندند و تیک نخوردند.

- [x] ضمیمه A: ماتریس ۲۹+۷ با ID / Strategy / Decoder / Detection / Classification / Subtag / Route / P&L / Evidence / Code / Tests / Verdict همراه با Taxonomy Caveat ثبت شد.
- [x] ضمیمه B: ریسکهای False Positive / False Negative ثبت شدند.
- [x] ضمیمه C: جدول Missing Capabilities ثبت شد.
- [x] ضمیمه D: تستپلن ردیفبهردیف Partial / Unsupported ثبت شد.
- [x] ضمیمه E: خلاصه ۶ سؤاله مدیریتی ثبت شد.

---

## 9. ضمیمه A — ماتریس پوشش نهایی (Audit Snapshot)

Evidence scope: `MevKind::ArbAtomic`, successful realized transactions only, at commit `ef29f73`. No code was changed.

Evidence levels: **H** = code + targeted tests + full real successful tx hash in repo/docs cache; **M** = code + targeted tests, no full ArbAtomic real hash; **L** = code heuristic or decoder only; **N** = no implementation evidence / unverified mapping.

> **Taxonomy caveat:** the repository has no authoritative `AR-01..AR-29` or `FUND-01..FUND-07` registry. Strategy names below are the nearest code-evidenced mechanisms; rows marked `Unverified` cannot be considered contractually identified until the external taxonomy is supplied.

| ID | Strategy | Decoder | Detection | Classification | Subtag / Metadata | Route Accuracy | P&L Accuracy | Evidence Level | Code Location | Test Coverage | Verdict |
|---|---|---|---|---|---|---|---|---|---|---|---|
| AR-01 | Two-pool / spatial atomic arb | V2/V3/V4/Curve/Balancer/Solidly/LB/Pendle/Fluid/Metric swap topics + transfer attachment | Closed directed walk ≥2 edges, ≥2 pools, ≤8 edges, attributable flow, positive residual | `ArbAtomic`, `Exact`; `Inferred` only for forwarded profit | `arb_meta.arb_shape=two_pool` | Selected cycle only; unrelated receipt swaps excluded; pools sorted/deduped | Ledger residual `R`; flash fee netted once; other residuals excluded | H | `classify/arb.rs::classify_atomic_arbs`; `profit.rs::closed_cycle_walks`; `classify/legs.rs::resolve_arb_routes` | `scenarios::baseline_atomic_arb`, `explorer_golden`, `explorer_corpus` AVAX/Polygon | Supported |
| AR-02 | Triangular atomic arb | Same as AR-01 | Same cycle gate | Same | `arb_meta.arb_shape=triangular` | Same; shape is token/hop heuristic, not a formal 3-pool proof | Same | H | `arb.rs::arb_route_meta` L760-826; `profit.rs::closed_cycle_walks` | Cycle tests + real AVAX corpus notes | Supported |
| AR-03 | Multi-hop atomic arb | Same as AR-01 | Same cycle gate | Same | `arb_meta.arb_shape=multi_hop` | Same; bounded at 8 edges | Same | H | `profit.rs::MAX_CYCLE_EDGES`, `closed_cycle_walks`; `arb.rs::select_route` | `cycle_search_is_depth_bounded`, corpus multi-hop rows | Supported |
| AR-04 | Cross-DEX arb | Same as AR-01 | Same cycle gate | Same | `is_cross_dex`, `dex_count`, `venues`, `cross_dex_basis` | Protocol identity only when **every** leg has `pool_dex`; otherwise AMM-family fallback | Same residual; classification metadata does not alter P&L | M | `arb.rs::arb_route_meta` L789-824; `classify/mod.rs::BlockContext.pool_dex` L151-154 | `cross_dex_uses_protocol_identity_not_amm_family`, `cross_dex_metadata_declares_amm_family_fallback` | Partial — FP risk when `pool_dex` absent |
| AR-05 | Same-DEX / different-AMM arb | Same as AR-01 | Same cycle gate | Same | `amm_family_count>1`, `is_cross_dex=false` when protocol identity complete | Correct only with complete `pool_dex`; otherwise family fallback can mislead | Same | M | `arb.rs::arb_route_meta`; `classify/mod.rs::pool_dex` | `same_dex_different_amm_is_not_cross_dex` | Partial — metadata-only, not a distinct detector |
| AR-06 | Flash-loan-funded arb | Explicit flash topics: Aave V2/V3, Balancer, Uni V3, LFJ, Morpho | Fee/principal netting before residual; `flash_arb` if any flash fact | `ArbAtomic`, `Exact`/`Inferred` | `flash_arb`, `funding:<provider>`, `funding_source` | Funding does not alter route; provider recorded from `flashloans.first()` only | Principal not counted as profit; fee sum only complete for one token and no overflow | M | `decode/flash_loan.rs::decode_flash_loan`; `arb.rs` L73-149, L373-434 | `p0_4_flash_arb`, `split_flash_repay_is_detected`, `multiple_flash_fees_are_summed_when_complete` | Supported (code only; no real ArbAtomic seed) |
| AR-07 | V2 flash-swap-funded arb | Logless inference from out→callback→in transfer ordering for `Amm::V2\|Solidly` | Same arb gate + inferred flash fact | Same | `flash_arb`, `funding:v2_flash_swap` | Route unaffected; provider = pool address; no V4 flash accounting | Fee embedded in swap amounts; `fee=None`; double-subtract avoided by zero-token fact | M | `decode/flash_loan.rs::infer_v2_flash_swap` L198-263; `classify/mod.rs` L629-636 | `v2_flash_swap_is_inferred_as_funding`, `prefunded_v2_swap_is_not_inferred_as_flash_loan` | Supported (V2/Solidly only) |
| AR-08 | Long-tail token arb | Same as AR-01 | Same cycle gate | Same | `long_tail` when route touches non-blue-chip token | Uses selected route tokens only | Residual P&L unchanged; token-pricing risk remains | M | `arb.rs::is_long_tail_arb` L832+ | `p1_3_long_tail`; AVAX seed block 97783552 | Supported (behavioral tag only) |
| AR-09 | Backrun arb (as ArbAtomic tag) | Same as AR-01 + causal price/adjacency facts | Prior market-moving leg, opposite direction, better price within window | Same `ArbAtomic`; no separate kind | `backrun` tag + causal evidence | Anchors on emitted arb route; no route rewrite | Arb P&L owned by arb pass; causality remains `inferred` | H | `classify/causal.rs::classify_backruns`, `annotate_backrun`; `legs.rs` L22-48 | `baseline_backrun`, `core/tests/backrun.rs`, `causal_fp_corpus`, full ETH hashes | Supported |
| AR-10 | Curve imbalance arb | Curve `TokenExchange` topics | Curve leg + ≥2 distinct AMM families | `ArbAtomic` + mechanism tag | `curve_imbalance` | No reserve/imbalance measurement; only venue-mix gate | Same `R`; no mechanism-specific profit share | L | `strategy_tags.rs::arb_strategy_tags` L32-37 | `p3_5_curve_imbalance` | Partial — label only |
| AR-11 | Balancer rate-provider staleness | Balancer swap + `TokenRateCacheUpdated` | Balancer leg + cross-venue + no same-pool cache refresh | Same | `rate_provider_staleness` | Pool-scoped refresh check only; no rate divergence test | Same `R` | L | `strategy_tags.rs` L39-54; `decode/signals.rs` | `p3_2_balancer_staleness` | Partial — inferred label |
| AR-12 | Fee-on-transfer token arb | FoT registry lookup on route tokens | Route token has registered/measured FoT flag | Same | `fot_arb` | Route uses nominal amounts; transfer tax visible only in `route_delivery.net` amounts | Same `R`; tax not decomposed | L | `strategy_tags.rs` L56-74; `pool/state/pool_types.rs` | `p3_12_fot_rebase_arb` | Partial |
| AR-13 | Rebase token arb | Rebase registry lookup on route tokens | Route token has rebase registry flag | Same | `rebase_arb` | No balance-drift verification at classify time | Same `R` | L | `strategy_tags.rs` L56-77 | `p3_12_fot_rebase_arb` | Partial |
| AR-14 | sAVAX rate arb | sAVAX address + `getPooledAvaxByShares` rate fact | sAVAX↔wrapped-native route with ≥5 bps implied-rate divergence | Same | `savax_rate_arb`, `tag_modes.savax_rate_arb=exact` | Only direct sAVAX/WAVAX leg drives divergence | Same `R`; divergence amount not attributed | L | `strategy_tags.rs` L79-95, `savax_pool_diverges` L128-161; `arb.rs` L459-461 | `p3_16_savax_rate_arb` | Partial |
| AR-15 | Epoch-transition arb | `NotifyReward` facts + epoch-venue registry | ≥2 epoch-pool legs + reward token on route | Same | `epoch_transition` | Requires both legs on configured epoch venues | Same `R` | L | `strategy_tags.rs` L97-116; `decode/signals.rs::decode_epoch_reward` | `p3_15_epoch_transition` | Partial |
| AR-16 | GMX ADL-adjacent arb | GMX `EventEmitter` ADL/liquidation/impact + market-token registry | ADL in same/prior tx and route touches configured market token | Same | `gmx_adl_arb` | Route hit checked against configured GMX tokens | Same `R` | L | `arb.rs` L47-59, L201-205; `strategy_tags.rs` L118-121 | `p3_7_gmx_adl` | Partial |
| AR-17 | Claim-and-sell behavior inside ArbAtomic | Zero-from `Transfer` + same-tx sell swap | Existing ArbAtomic is retagged; no second P&L event | Still `ArbAtomic` when cycle exists | `claim_and_sell`, `airdrop`, `claimed_token` | Route unchanged; claim tag is same-tx only | No duplicate profit; if no arb, emitted as `Utility` outside scope | L | `arb.rs::classify_claim_and_sell` L628-688; `strategy_tags.rs::claim_and_sell_token` | `claim_and_sell_tags_the_existing_arb_instead_of_double_counting`, `p3_13_claim_and_sell` | Partial by scope exception |
| AR-18 | Aggregator/router-mediated arb | 1inch/ParaSwap/0x/legacy router topics | Aggregator edges excluded from cycle closure; dedup vs DEX edges | No dedicated detector; generic arb only if real DEX leg cycle exists | `router_version`; no underlying-pool reconstruction | `underlying_pools_hash` uses selected DEX pools only; router internals opaque | Same `R`; aggregator-only cycle intentionally rejected | L | `decode/aggregator.rs`; `legs.rs::is_cycle_edge` L104-109; `arb.rs` L435-458 | `aggregator_dup_edge_removed_when_dex_pair_covers`, `aggregator_only_cycle_produces_no_arb` | Unsupported — FN risk for genuine aggregator-mixed cycles |
| AR-19 | Split-route arb | No route-split decoder | No detection | No classification | `optimizer:split_route` exists as tag vocabulary only | `route_json` cannot express fractional/second route | No split-specific P&L | N | `types/tags.rs::OPTIMIZER_SPLIT_ROUTE`; `docs/arb_atomic_audit_gaps_plan.md` G-11 | None | Unsupported / Unverified |
| AR-20 | Stablecoin depeg arb | No depeg decoder or peg reference | No detector | No classification | No tag | No depeg route evidence mechanism | Would inherit generic `R` only if a cycle exists | N | Docs only: `docs/mev_strategies.md` §5.1 | None | Unsupported / Unverified |
| AR-21 | Wrapped-token peg arb | Wrapped-native mint/burn delta filtering | Generic cycle may include wrapped token | Generic `ArbAtomic` only | `wrapped_native_marker`; no mechanism tag | Wrap noise can be filtered; no peg-gap reconstruction | Same `R`; wrap residue policy documented | L | `profit.rs` wrap-noise tests; `types.rs::wrapped_native` | `wrap_noise_filtered`, `wrap_then_spend_without_native_flow_keeps_the_mint` | Partial — generic-only |
| AR-22 | Bridged-token arb | No bridge/peg decoder | No detector | No classification | No tag | No canonical/bridged asset mapping | No bridged-specific P&L | N | Docs only | None | Unsupported / Unverified |
| AR-23 | LST depeg arb | No LST fair-value decoder | No detector | No classification | No tag; sAVAX tag is separate and narrow | No LST peg/reference route attribution | No mechanism-specific attribution | N | Docs only §4.5 | None | Unsupported / Unverified |
| AR-24 | Pendle PT/YT yield arb | `Pendle` swap topic decoded into `Amm::Pendle` | Generic cycle may use Pendle leg | Generic `ArbAtomic` only | No Pendle mechanism tag | Tokens resolved via transfer proximity; PT/YT semantics not modeled | Same `R`; implied vs realized yield not computed | L | `decode/swap.rs::decode_pendle`; `types.rs::Amm::Pendle` | No dedicated decoder test in `decode/tests.rs` | Partial — venue decoded, strategy unsupported |
| AR-25 | Vault-token / share-price arb | Balancer Vault flash decoder only | No share-price detector | Generic `ArbAtomic` only | `funding:balancer_v2_vault` / vault funding metadata | Vault used as funding, not as priced share venue | Fee netting only; no NAV/share-price attribution | L | `types/tags.rs::canonical_funding_source`; `arb.rs` funding metadata | Dashboard funding panel test only | Partial — funding semantics only |
| AR-26 | Oracle/rate-provider AMM arb | Oracle update decoder exists for other passes | No atomic-arb detector | No classification | `ORACLE_AMM` vocabulary only; never emitted by core atomic arb | No oracle-vs-AMM route reconstruction | No oracle-specific attribution | N | `types/tags.rs::TAG_AXIS_REGISTRY` | None | Unsupported / Unverified |
| AR-27 | Init-price snipe arb | No pool-creation/first-swap classifier in Explorer arb pass | No detector | No classification | No tag | No initialization sequencing evidence | No mechanism P&L | N | Docs only §1.1 | None | Unsupported / Unverified |
| AR-28 | External-market arb: CEX–DEX / cross-chain / spot–perp | No CEX, bridge-finality, or perp venue decoder | No detector | No classification | No tag | Not representable in one-chain closed-cycle model | Not attributable | N | Out of scope by design | None | Unsupported / out of scope |
| AR-29 | Statistical / pairs / NFT-floor arb | No statistical or floor-price input | No detector | No classification | No tag | No fair-value reference in route | No mechanism P&L | N | Docs only | None | Unsupported / out of scope |

### Funding sources

The repository canonicalises seven sources in `types/tags.rs::canonical_funding_source`; the external `FUND-01..07` labels were not found, so rows use that canonical order.

| ID | Strategy | Decoder | Detection | Classification | Subtag / Metadata | Route Accuracy | P&L Accuracy | Evidence Level | Code Location | Test Coverage | Verdict |
|---|---|---|---|---|---|---|---|---|---|---|---|
| FUND-01 | Balancer V2 Vault | `decode_balancer_flash` via `decode_flash_loan` | `flashloans` fact → fee/principal netting | `funding_source=balancer_v2_vault` | `funding:balancer_v2_vault`, `flash_arb` | Route independent of provider | Fee netted only when transfer pattern permits; first provider only | M | `decode/flash_loan.rs` L95-110; `types/tags.rs` L96-121 | Canonical source test; no explorer decoder unit test | Supported (code only) |
| FUND-02 | Aave V3 | Explicit `FlashLoan` topic decoder | Same | `funding_source=aave_v3` | `funding:aave_v3` | Same | Premium decoded and netted | M | `decode/flash_loan.rs` L55-73 | `aave_flash_loans_decode`; real AVAX flash+liq hash is out-of-scope liquidation | Supported (code only) |
| FUND-03 | Aave V2 | Explicit topic decoder | Same | `funding_source=aave_v2` | `funding:aave_v2` | Same | Premium decoded | M | `decode/flash_loan.rs` L75-93 | `aave_flash_loans_decode` | Supported (code only) |
| FUND-04 | Uniswap V3 Flash | Explicit `Flash` topic decoder | Same; zero token requires transfer-based netting | `funding_source=univ3_flash` | `funding:univ3_flash` | Same | `token=ZERO` skips principal netting; fee may be recorded even when amount leg is zero | L | `decode/flash_loan.rs` L25-53 | Topic-family test; canonical mapping test | Supported (code only, weak) |
| FUND-05 | LFJ / Trader Joe LB Flash | 2.0 and 2.1/2.2 topic decoders | Same | `funding_source=lfj_lb_flash` | `funding:lfj_lb_flash` | Same | 2.1/2.2 chooses one packed leg on ties | M | `decode/flash_loan.rs` L132-184 | `lfj_lb_flash_loan_decodes`, `lfj_lb_v20_flash_loan_decodes` | Supported (code only) |
| FUND-06 | V2 flash swap | Transfer-order inference | Same | `funding_source=v2_flash_swap` | `funding:v2_flash_swap` | Provider = pool; no callback trace | Fee implicit in swap amounts; no explicit in-kind fee value | M | `decode/flash_loan.rs::infer_v2_flash_swap` | `v2_flash_swap_is_inferred_as_funding`, prefunded negative | Partial — inferred, V2/Solidly only |
| FUND-07 | Morpho Blue | Explicit `FlashLoan` topic decoder | Same | `funding_source=morpho_blue` | `funding:morpho_blue` | Same | Zero premium recorded | L | `decode/flash_loan.rs` L112-130 | No explorer decoder test found | Supported (code only, weak) |

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
| AR-04, AR-05 | Fixture matrix: complete `pool_dex`, partial `pool_dex`, empty `pool_dex`; assert `venues`, `dex_count`, `is_cross_dex`, `cross_dex_basis`; add real-hash golden asserting protocol basis where registry is populated | 4 full AVAX hashes above available |
| AR-06 | Two flash providers with distinct protocols; assert both in `funding_providers[]`, one event, correct fee sum, no double netting | No full real successful flash-funded ArbAtomic hash found in repo; `scenario_targets.rs` status is `Hunt` |
| AR-07 | Negative tests: prefunded V2 swap, V4 unlock/callback, Balancer/LB flash swap should not be mislabeled as `v2_flash_swap`; positive decoder golden retained | No full real ArbAtomic V2-flash-swap hash found |
| AR-10 | Curve + DEX fixture plus imbalance-magnitude negative (Curve leg without imbalance); assert tag only when evidence exists | No real Curve-imbalance seed; `P3.5=Hunt` |
| AR-11 | Same-pool refresh clears tag; other-pool refresh does not; add rate-divergence negative | No real seed; `P3.2=Hunt` |
| AR-12 | Measured/static FoT fixture plus non-FoT negative; assert net transfer tax visible in `route[].amount_*_net` | No real seed; `P3.12=Hunt` |
| AR-13 | Rebase token positive plus registry-absent negative; assert no tag when registry unknown | No real seed; `P3.12=Hunt` |
| AR-14 | Both sAVAX directions, below-threshold divergence, no-rate negative; assert `tag_modes` | No real seed; `P3.16=Hunt` |
| AR-15 | Reward token on/off route, one vs two epoch pools, timestamp-only negative | No real seed; `P3.15=Hunt` |
| AR-16 | ADL same/prior/later tx and market-token on/off route | No real seed; `P3.7=Hunt` |
| AR-17 | Same-tx claim-and-sell + closed arb must emit one ArbAtomic with `claim_and_sell`; must not emit second Utility | Real Utility hash exists above; no real ArbAtomic+claim combined hash found |
| AR-18 | Aggregator+DEX mixed route: assert when cycle is rejected, why; add metadata flag `underlying_pools_unknown`; assert selected route excludes router internals | Code comment cites KyberSwap false-arb regression but no full hash; no real sample found |
| AR-19 | Guard test: split-route opportunity input must not create `ArbAtomic` until route representation exists; assert docs/tag only | No real sample found |
| AR-20 | Stablecoin depeg hunt over known depeg window; negative assertion if no decoder/reference exists | No full real depeg ArbAtomic hash found |
| AR-21 | Wrapped-native mint/burn fixtures: wrap noise filtered; beneficiary unwrap not invented; assert no standalone mechanism tag | Generic real hashes may exercise wrapping, but no peg-specific real sample found |
| AR-22 | Bridged asset negative: no bridge identity → no bridged-arb claim | No real sample found |
| AR-23 | LST depeg negative: no fair-value reference → no LST-depeg claim | No real sample found |
| AR-24 | Pendle decoder golden with token direction and a negative asserting no yield-spread claim without PT/YT fair-value evidence | No full real Pendle ArbAtomic hash found |
| AR-25 | Vault-as-funding vs vault-as-venue separation; assert no share-price claim from funding metadata alone | No real sample found |
| AR-26 | `ORACLE_AMM` remains vocabulary-only; assert core detector never emits it | No real sample found |
| AR-27 | Init-price snipe guard: pool creation + first swap without closed cycle emits no ArbAtomic | No real sample found |
| AR-28 | Out-of-scope guard: CEX/cross-chain/spot-perp inputs cannot produce single-chain ArbAtomic | No real sample found |
| AR-29 | Out-of-scope guard: statistical/pairs/NFT-floor references cannot produce ArbAtomic without closed DEX cycle | No real sample found |
| FUND-01..07 | Per-provider decoder golden, funding tag, canonical `funding_source`, fee netting, multi-provider array, and first-provider backward compatibility | No real successful ArbAtomic hash found per provider; only out-of-scope Aave V3 liquidation context above |

---

## 13. ضمیمه E — خلاصه ۶ سؤاله مدیریتی

1. **Counts among the 29 arbitrage rows:** **7 Supported**, **13 Partial**, **9 Unsupported**. No row is fully verified as a mechanism-specific arbitrage type on a real successful transaction; support is strongest for generic route shapes and behavioral tags.

2. **Counts among the 7 funding sources:** **6 Supported on code**, **1 Partial** (`v2_flash_swap` inference). **0** have a pinned real successful `ArbAtomic` transaction in the repository; coverage is code/test-level only.

3. **Generic-only `ArbAtomic` cases:** **7 rows** — AR-01, AR-02, AR-03, AR-04, AR-05, AR-08, AR-09. They are confirmed only as generic route shapes or behavioral tags on real transactions, not as mechanism-verified subclasses.

4. **Five critical defects:**
   - **D1:** No authoritative AR/FUND taxonomy registry; 21 AR labels and all FUND labels are unverifiable from the repository.
   - **D2:** Cross-DEX detection falls back to AMM family when `pool_dex` is incomplete, creating protocol-identity false positives.
   - **D3:** Multi-provider flash funding is collapsed to `flashloans.first()`.
   - **D4:** Emitted tags (`forwarded_profit`, `funding:*`) are missing from `core/data/explorer_tags.json`.
   - **D5:** No mechanism-level P&L attribution or full real-hash golden set for the specialist tags; all rows reuse the same residual `R`.

5. **Minimum changes required:**
   - Freeze the AR/FUND taxonomy mapping in a versioned registry without adding new `MevKind`s.
   - Populate `pool_dex` for every route pool and fail closed to `unknown` instead of AMM-family cross-DEX.
   - Add `funding_providers[]` while keeping `funding_source` as the backward-compatible first entry.
   - Sync `explorer_tags.json` with every emitted tag and enforce it in tests.
   - Add `profit_cause` metadata and explicit route-shape fields (`pool_count`, `unique_tokens`, `hop_count`) without changing canonical net profit.
   - Pin the four full AVAX hashes and two ETH backrun hashes as gated golden tests; record explicit negative hunt results where no real seed exists.

6. **Repair order:**
   - **1 — Contract:** taxonomy mapping, tag vocabulary, and guardrails.
   - **2 — Identity:** complete `pool_dex`, fail-closed cross-DEX, route-shape disambiguation.
   - **3 — Funding:** multi-provider array and per-provider fee completeness.
   - **4 — Evidence:** real-hash golden tests and documented hunt results for all Partial/Hunt rows.
   - **5 — Attribution:** mechanism `profit_cause` metadata and multi-residual reporting, with canonical `net_profit_usd` unchanged.
   - **6 — Scope:** explicit no-emission guards for Unsupported/out-of-scope categories.

---

**پایان سند.** لایهٔ plan و ضمایم audit هر دو مستند شدند؛ هیچ کد، تست، کامیت یا PRی ایجاد نشده است. منتظر ورودیهای I-1..I-3 و تصمیم اجرای WPها هستیم.
