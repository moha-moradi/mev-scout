# برنامهٔ اجرایی بهبود ماژول `core/src/explorer`

> این فایل برای اجرا توسط یک LLM دیگر (یا توسعه‌دهندهٔ کم‌تجربه‌تر) نوشته شده است.
> هر تسک مستقل است، اما ترتیب پیشنهادی را رعایت کنید. قبل از شروع هر تسک، بخش
> «دام‌ها»ی همان تسک را کامل بخوانید.

---

## ۰) قواعد کلی (قبل از هر کاری بخوانید)

1. **یک تسک = یک commit مستقل.** هیچ‌وقت چند تسک را در یک commit ترکیب نکنید.
2. قبل از تغییر، وضعیت پایه را ثبت کنید:
   ```powershell
   cd D:\Repositories\mev-scout
   cargo fmt --all -- --check
   cargo clippy --workspace --all-targets --message-format=short
   cargo test --workspace
   ```
   نکته: در PowerShell، خروجی stderr مربوط به `cargo` باعث می‌شود `$LASTEXITCODE`
   غیرصفر شود؛ به متن خطاها نگاه کنید، نه فقط exit code.
3. **Lintها سخت‌گیر هستند:** در `Cargo.toml` ریشه، `warnings = "deny"` و
   `clippy::unwrap_used` / `clippy::expect_used = "deny"` فعال است.
   - در کد production از `unwrap()` / `expect()` استفاده نکنید.
   - در تست‌ها (داخل `#[cfg(test)]` یا `tests/`) مجاز است.
   - کد مرده یا import بلااستفاده بگذارید، build fail می‌شود.
4. **Migrationهای SQLite را هرگز ویرایش نکنید.** فقط migration جدید append کنید
   (`store/schema.rs`، لیست `MIGRATIONS`). `SCHEMA_HEAD` خودکار برابر
   `MIGRATIONS.len()` می‌شود؛ تست‌های schema این را چک می‌کنند.
5. **رفتار عمومی (public API) را بی‌دلیل تغییر ندهید.** اگر امضای تابعی عوض شد،
   همهٔ callerها (شامل `cli/` و تست‌ها) را با `grep` پیدا و به‌روزرسانی کنید.
6. **خروجی golden / snapshot را الکی به‌روزرسانی نکنید.** اگر تست golden شکست،
   اول مطمئن شوید تغییر رفتار *خواسته‌شده* است، بعد snapshot را با دلیل در پیام
   commit توضیح دهید.
7. **شبکه در تست‌ها:** تست‌های شبکه‌ای معمولاً fixture/`#[ignore]` هستند. تست
   جدیدتان نباید به RPC/اینترنت واقعی وابسته باشد.
8. **خط‌شماره‌ها بعد از هر تغییر جابه‌جا می‌شوند.** در این سند شماره خط فعلی آمده
   است؛ برای پیدا کردن محل درست از نام تابع/سیمبل + `grep` استفاده کنید:
   ```powershell
   Select-String -Path core/src/explorer/**/*.rs -Pattern "find_closed_cycle"
   ```
   (اگر `rg` نصب نیست، از `Select-String` استفاده کنید.)
9. **هر تسک باید تست داشته باشد.** حداقل:
   - یک تست جدید که رفتار درست را قفل کند،
   - تست‌های موجود همان ماژول سبز بمانند.
10. **اگر تصمیم محصولی لازم است، حدس نزنید.** در این سند برای موارد حساس
    (`A4`، `A7/R2`، `P6`) دو گزینه با پیش‌فرض مشخص آمده است. پیش‌فرض امن را اجرا
    کنید و در commit message بنویسید که تصمیم نهایی با مالک محصول است.

### نقشهٔ فازها

| فاز | موضوع | ریسک | پیشنهاد |
|---|---|---|---|
| 0 | Baseline و ابزار | صفر | همین حالا |
| 1 | دقت (Accuracy) | متوسط تا بالا | اول `A1`, `A2`, `A5`, `A6` |
| 2 | کارایی (Performance) | کم تا متوسط | `P1`, `P2`, `P4`, `P7a`, `P7d` |
| 3 | مینیمالیسم | متوسط | `M1`, `M2`, `M4` |
| 4 | پایداری (Reliability) | متوسط | `R1`, `R3` |
| 5 | تصمیم‌های محصولی | بالا | `A3`, `A4`, `P6`, `R2` |

> `A3`، `A4`، `P6` و `R2` می‌توانند نتایج عددی گزارش‌ها را تغییر دهند. قبل از
> اجرای آن‌ها با مالک پروژه هماهنگ کنید. بقیهٔ تسک‌ها باید بدون تغییر معنادار
> اعداد، فقط کیفیت/سرعت/قرارداد را بهتر کنند.

### فهرست تسک‌ها

| ID | عنوان | فاز | تغییر رفتار؟ | وابستگی |
|---|---|---|---|---|
| T0.1 | ثبت baseline | 0 | خیر | — |
| A1 | PnL لیکوییدیشن هرگز Exact نباشد | 1 | بله (confidence کمتر) | — |
| A2 | یکسان‌سازی فیلتر cycle در arb/legs | 1 | بله (reject/تشخیص) | — |
| A3 | انتخاب cycle سودآور + bound | 5 | بله (ممکن است arb عوض شود) | A2 |
| A4 | قرارداد `profit_tokens` | 5 | پیش‌فرض: فقط doc | — |
| A5 | حذف fallback گمراه‌کننده | 1 | خیر/کم | — |
| A6 | حذف `Address::ZERO` از candidates | 1 | خیر | — |
| A7/R2 | وضعیت reorg و `unwind_from` | 1/5 | پیش‌فرض: فقط doc | — |
| P1 | پارس یک‌بارهٔ JSON در persist | 2 | خیر | — |
| P2 | پارس یک‌بارهٔ JSON در dashboard | 2 | خیر | — |
| P3 | `build_op_index` با struct تایپ‌دار | 2 | خیر (خروجی یکسان) | P2 |
| P4 | `log_family` با HashMap | 2 | خیر | — |
| P5 | chunk کردن price request | 2 | خیر | — |
| P6 | قیمت native تاریخی | 5 | بله (gas USD) | — |
| P7a | `EXISTS` در `block_classified` | 2 | خیر | — |
| P7b | `LEFT JOIN` داخل CTE در `unclassified_blocks` | 2 | خیر | اختیاری؛ جایگزینی با جدول `blocks` ممنوع |
| P7c | pool leaderboard از `swaps` | 2 | بله (اعداد leaderboard) | تصمیم محصولی |
| P7d | dedupe `pending_labels` | 2 | خیر | — |
| M1 | حذف cloneهای block-wide | 3 | خیر | — |
| M2 | حذف plumbing رشته‌ای details | 3 | خیر | P1 |
| M3 | یکسان‌سازی کامل cycle/profit | 3 | بله | A2 |
| M4 | مستندسازی kindهای legacy | 3 | خیر | — |
| R1 | skip خطای persist در pipelined | 4 | بله (backfill کامل‌تر) | — |
| R3 | رفتار hash تکراری در `blocks` | 4 | فقط doc/با A7 | A7 |

---

## فاز ۰ — Baseline و ابزار

### T0.1 — ثبت وضعیت پایه
- **کاری که باید بکنید:** خروجی `cargo test --workspace` و
  `cargo clippy --workspace --all-targets` را در توضیحات PR/commit اول ذخیره کنید.
- **چرا:** بعداً بفهمید کدام شکست از قبل وجود داشته است.
- **دام:** اگر تست‌های شبکه‌ای fail شدند، آن‌ها را با `-- --skip <name>` جدا کنید،
  نه اینکه کد را تغییر دهید.

---

## فاز ۱ — دقت (Accuracy)

### A1 — اصلاح قرارداد A19 برای PnL لیکوییدیشن (اولویت اول)

**فایل‌ها:** `core/src/explorer/store/pnl.rs`، `core/src/explorer/types.rs`،
`core/src/explorer/classify/mod.rs`، تست‌های مربوطه.

**وضعیت فعلی:**
- `liquidation_pnl` در `pnl.rs` (حدود خط ۳۳۷) برای لیکوییدیشن هم‌دارایی
  (`collateral_asset == debt_asset`) مقدار `"exact"` برمی‌گرداند (حدود خط ۳۷۹).
- `annotate_event_with_volume` (حدود خط ۹۴–۹۸) این مقدار را مستقیماً روی
  `confidence` می‌نویسد و `normalize_confidence` (در `classify/mod.rs` حدود خط
  ۳۱۳) را دور می‌زند.
- قرارداد `types.rs` (حدود خط ۴۳۸) می‌گوید فقط `PnlBasis::R` می‌تواند
  `Confidence::Exact` را پشتیبانی کند؛ لیکوییدیشن `O` است.

**تغییر موردنیاز:**
1. در `liquidation_pnl` هیچ‌وقت `"exact"` برنگردانید؛ حداکثر `"inferred"`.
2. دقت محاسبه را در `details` نگه دارید، مثلاً:
   ```json
   { "pnl_basis": "O", "liquidation_same_asset": true }
   ```
   و اگر لازم است کیفیت را منتقل کنید، از reason جداگانه استفاده کنید
   (`LIQ_SAME_ASSET`)، نه از `confidence`.
3. در `annotate_event_with_volume` مطمئن شوید persist هرگز `confidence` را
   *بالا* نمی‌برد؛ فقط می‌تواند پایین بیاورد (`Exact` → `Inferred`) یا همان را
   نگه دارد. یک تابع کوچک `min_confidence(a, b)` بسازید.
4. تست‌های موجود در `pnl.rs` که انتظار `"exact"` دارند (حدود خط ۹۳۷ و ۹۷۱)
   را به انتظار `"inferred"` + presence of `LIQ_SAME_ASSET` تغییر دهید.

**دام‌ها / چیزهایی که ممکن است از قلم بیفتد:**
- `normalize_confidence` فقط روی `Exact`ها اجرا می‌شود و قبل از persist است؛
  تغییر در persist نباید آن را حذف کند.
- اگر classifier از قبل `details.pnl_basis` را ست کرده است، آن را با مقدار
  اشتباه بازنویسی نکنید. فقط در صورت نبودِ key، `"O"` را ست کنید (یا از
  `entry(...).or_insert(...)` استفاده کنید).
- `ReportQuality` و dashboard ممکن است تعداد `exact`ها را نشان دهند؛ عدد
  گزارش بعد از این تغییر *کمتر* می‌شود و این درست است. آن را در commit توضیح
  دهید.
- تست‌های `tests/mev_corpus.rs` یا golden ممکن است به `confidence` وابسته
  باشند؛ با `grep -n "\"exact\""` همه را پیدا کنید.
- `LIQ_BONUS_APPROX` را برای هم‌دارایی حذف نکنید؛ bonus لیکوییدیشن حتی در
  هم‌دارایی هم وجود دارد. اگر مطمئن نیستید، همان reason را نگه دارید.

**معیار پذیرش:**
- هیچ مسیر لیکوییدیشنی `Confidence::Exact` تولید نکند.
- تست جدید: لیکوییدیشن هم‌دارایی با قیمت‌های موجود ⇒ `confidence == Inferred`
  و `details.pnl_basis == "O"`.
- `cargo test -p mev-scout-core explorer` سبز.

---

### A2 — یکسان‌سازی محاسبهٔ cycle بین `arb.rs` و `legs.rs`

**فایل‌ها:** `core/src/explorer/classify/arb.rs`،
`core/src/explorer/classify/legs.rs`، `core/src/explorer/classify/causal.rs`،
`core/src/explorer/profit.rs`، تست‌ها.

**وضعیت فعلی:**
- `arb.rs::classify_atomic_arbs` (حدود خط ۳۰۰–۴۳۰) مسیر کامل cycle + profit را
  دارد و edgeهای `Amm::Aggregator` را حذف می‌کند.
- `legs.rs::tx_cycle_profit` (حدود خط ۱۶۲–۲۰۸) فقط `has_closed_cycle` را چک
  می‌کند و **فیلتر aggregator را ندارد**. بنابراین می‌تواند تبدیل یک‌طرفهٔ
  aggregator را به‌عنوان cycle قبول کند.
- `causal.rs` از `legs.rs` استفاده می‌کند.

**تغییر موردنیاز:**
1. در `legs.rs` یک تابع مشترک بسازید (و `use crate::explorer::types::Amm;` را
   اضافه کنید):
   ```rust
   pub(super) fn resolved_cycle_edges(tx: &TxInput) -> Vec<(Address, Address, Address)> {
       tx.swaps
           .iter()
           .filter(|s| {
               s.amm != Amm::Aggregator
                   && !has_unresolved_tokens(s)
                   && s.token_in != decode::TOKEN0_SENTINEL
                   && s.token_in != decode::TOKEN1_SENTINEL
           })
           .map(|s| (s.token_in, s.token_out, s.pool))
           .collect()
   }
   ```
   `SwapFact` فیلد `amm: Amm` را دارد (`types.rs` حدود خط ۳۴۲). این دقیقاً
   همان فیلتر `arb.rs` حدود خط ۱۶۷–۱۷۷ است؛ آنجا را هم به این تابع منتقل
   کنید تا فقط یک نسخه بماند.
2. `tx_cycle_profit` و `classify_atomic_arbs` هر دو از این تابع استفاده کنند.
3. اگر arb از `Vec<(Address, Address, _)>` با نوع third متفاوت استفاده می‌کند،
   تابع را generic نکنید؛ یک type alias مشترک بسازید.
4. تست جدید: tx با edgeهای aggregator + یک swap معمولی که فقط با aggregator
   «cycle» می‌شود ⇒ `tx_cycle_profit` باید `CycleMiss::NotClosed` بدهد و
   `classify_atomic_arbs` هم event تولید نکند.

**دام‌ها / چیزهایی که ممکن است از قلم بیفتد:**
- `Amm::Aggregator` در `types.rs` (حدود خط ۲۸۵) تعریف شده؛ مطمئن شوید `SwapFact`
  این فیلد را دارد.
- `flow_attributable` و `searcher_candidates` در `legs.rs` ممکن است به ترتیب
  edgeها وابسته باشند؛ ترتیب `tx.swaps` را حفظ کنید.
- **یکسان‌سازی کامل فقط aggregator نیست.** `arb.rs` علاوه بر فیلتر edge، این
  کارها را هم می‌کند که `tx_cycle_profit` ندارد:
  - حذف `route_pools` از `profit_candidates` (حدود خط ۲۰۰–۲۱۰ arb.rs)،
  - منطق `forwarded_recipient` برای profit forwarding،
  - `initiator_blocks_cycle` / `has_material_deficit`.
  اگر می‌خواهید `tx_cycle_profit` واقعاً هم‌رفتار شود، این‌ها را هم منتقل
  کنید؛ ولی برای `A2` حداقل aggregator + sentinel کافی است. رفتار جدید را
  با تست مقایسه‌ای بین دو مسیر قفل کنید.
- `causal.rs` روی `CycleMiss::NotAttributable` / `InventorySpend` / `NoResidual`
  برچسب reject می‌زند؛ تغییر دسته‌بندی می‌تواند reject ladder را عوض کند.
  تست‌های `reject`/`dashboard` را بررسی کنید.
- `has_closed_cycle` در `profit.rs` همچنان توسط جاهای دیگر استفاده می‌شود؛
  امضایش را عوض نکنید، فقط ورودی را تمیزتر بدهید.

**معیار پذیرش:**
- یک تابع واحد برای ساخت edgeهای cycle وجود دارد و هر دو مسیر از آن استفاده
  می‌کنند.
- تست aggregator-only سبز و در هر دو مسیر رفتار یکسان است.
- `cargo test -p mev-scout-core explorer` سبز.

---

### A3 — انتخاب cycle سودآور به‌جای «اولین cycle» (تصمیم محصولی)

> **قبل از اجرا با مالک پروژه هماهنگ کنید.** این تسک می‌تواند نتایج تشخیص arb
> را تغییر دهد.

**فایل‌ها:** `core/src/explorer/profit.rs`، `core/src/explorer/classify/arb.rs`،
`core/src/explorer/classify/legs.rs`، تست‌ها.

**وضعیت فعلی:**
- `find_closed_cycle` (حدود خط ۲۱۹) اولین walk بسته‌شدهٔ DFS را برمی‌گرداند.
- `dfs_cycle` (حدود خط ۲۵۹) `walk_tokens` را از همهٔ edgeهای استفاده‌شده در آن
  walk می‌سازد. در گراف چندمسیره، این می‌تواند مجموعهٔ بزرگی از توکن‌ها بدهد و
  `has_material_deficit` / `initiator_blocks_cycle` را ضعیف کند.
- DFS بدون bound عمق/edge می‌تواند روی گراف متراکم هزینه‌بر شود.

**تغییر موردنیاز (پیش‌فرض محافظه‌کارانه):**
1. یک پارامتر `max_edges` برای cycle تعریف کنید (مثلاً `const MAX_CYCLE_EDGES: usize = 8;`).
2. DFS را به `depth < MAX_CYCLE_EDGES` محدود کنید.
3. `walk_tokens` را فقط از edgeهای همان cycle برگشتی بسازید (الان هم همین است،
   ولی مطمئن شوید edgeهای `used` بعد از backtrack پاک می‌شوند و تابع فقط
   edgeهای cycle نهایی را برمی‌گرداند).
4. برای تشخیص «وجود cycle» از BFS/کوتاه‌ترین cycle استفاده کنید اگر پیاده‌سازی
   ساده است؛ در غیر این صورت DFS فعلی + bound کافی است.
5. تست‌های زیر را اضافه کنید:
   - گرافی با دو cycle که یکی سودآور و یکی تزئینی است: حداقل assert کنید که
     انتخاب *deterministic* است (نه اینکه حتماً سودآور انتخاب شود).
   - گراف با ۱۰ edge و بدون cycle: زمان/عمق منفجر نشود.
   - `walk_tokens` فقط شامل توکن‌های cycle انتخابی باشد.

**دام‌ها / چیزهایی که ممکن است از قلم بیفتد:**
- شرط فعلی «حداقل ۲ pool متمایز» را حفظ کنید؛ حذف آن باعث تشخیص‌های غلط
  می‌شود.
- `has_closed_cycle` فقط boolean است؛ اگر پیاده‌سازی را عوض کردید، خروجی آن
  برای ورودی‌های موجود تغییر نکند (تست property-based یا جدول تست).
- `anchor` باید همان توکنی باشد که cycle به آن برمی‌گردد؛ در بازنویسی
  اشتباه نکنید.
- `walk_tokens` در `initiator_blocks_cycle` استفاده می‌شود؛ کوچک‌تر شدن آن
  می‌تواند rejectهای `OpenInventory` را کم کند (رفتار درست‌تر، ولی عدد
  گزارش عوض می‌شود).
- اگر انتخاب سودآورترین cycle را پیاده می‌کنید، باید `ledger` و `select_profit_token`
  را هم در دسترس داشته باشید؛ در `profit.rs` این‌ها وجود دارند ولی `find_closed_cycle`
  فعلاً ledger ندارد. یک تابع سطح بالاتر در `arb.rs` بسازید که cycleهای
  کاندید را بگیرد و با ledger انتخاب کند؛ `profit.rs` را فقط برای primitive
  نگه دارید.

**معیار پذیرش:**
- هیچ recursion بدون bound وجود ندارد.
- تست‌های cycle/arb سبز.
- در commit message صریحاً بنویسید که ممکن است تعداد arb تغییر کند.

---

### A4 — قرارداد `profit_tokens` (تصمیم محصولی)

> **قبل از اجرا با مالک پروژه هماهنگ کنید.** دو گزینه دارد و پیش‌فرض امن
> «مستندسازی» است.

**فایل‌ها:** `core/src/explorer/types.rs`، `core/src/explorer/classify/arb.rs`،
`core/src/explorer/store/pnl.rs`، `core/src/explorer/ingest.rs`، تست‌ها.

**وضعیت فعلی:**
- doc در `types.rs` (حدود خط ۱۶۷) می‌گوید `profit_tokens` = «همهٔ residuals
  مثبت post-netting» و persist روی همه USD-sum می‌زند (`pnl.rs` حدود خط ۹۹).
- اما `arb.rs` (حدود خط ۳۴۰–۳۵۴) فقط یک ورودی (anchor/priority) در
  `profit_tokens` می‌گذارد و بقیه را در `details.other_residuals` می‌فرستد.
- `ingest.rs::event_tokens` (حدود خط ۹۸۶) همهٔ `profit_tokens` را برای قیمت‌گذاری
  جمع می‌کند.

**گزینه‌ها:**
- **گزینهٔ امن (پیش‌فرض):** فقط doc را اصلاح کنید:
  ```
  profit_tokens = token(s) نمایشی/انتخابی که USD نهایی روی آن‌ها محاسبه می‌شود.
  residuals دیگر در details.other_residuals هستند و عمداً در USD جمع نمی‌شوند.
  ```
  و همان را در `pnl.rs` کنار منطق USD بنویسید.
- **گزینهٔ کامل:** همهٔ residuals مثبت را در `profit_tokens` بگذارید و
  `other_residuals` را حذف/نگه دارید. این کار USD arb را بالا می‌برد و باید
  تست‌های `classify/mod.rs` (حدود خط ۸۷۲ و ۹۱۷) و گزارش‌ها به‌روز شوند.

**تغییر موردنیاز (گزینهٔ امن):**
1. doc `types.rs` را دقیق کنید.
2. در `arb.rs` کامنت را به قرارداد جدید لینک دهید.
3. تست جدید: asserts کند `profit_tokens.len() == 1` برای arb چند residual، و
   `other_residuals` شامل بقیه است (تست موجود احتمالاً همین است؛ آن را
   صریح‌تر کنید).

**دام‌ها / چیزهایی که ممکن است از قلم بیفتد:**
- `pnl.rs` برای arb از `profit_tokens` USD-sum می‌کند، ولی `profit_token` /
  `profit_amount` را هم به‌عنوان نمایش اصلی نگه می‌دارد؛ نباید دوبار حساب شود.
- `details.profit_tokens` که در persist نوشته می‌شود با فیلد `MevEvent.profit_tokens`
  فرق دارد؛ doc را برای هر دو روشن کنید.
- `event_tokens` برای قیمت‌گذاری از `profit_tokens` استفاده می‌کند؛ اگر
  `other_residuals` قیمت‌گذاری نمی‌شوند، در persist باید `PARTIAL` علامت بخورد
  (اگر الان نمی‌خورد، این یک باگ مستقل است).
- `skim` چند token در `profit_tokens` می‌گذارد؛ doc جدید نباید آن را نقض کند.
  قرارداد را به‌صورت «هر میزانی که در `profit_tokens` است، USD-sum می‌شود»
  بنویسید، نه «همیشه یک token».

**معیار پذیرش:**
- doc و رفتار arb با هم هم‌خوان باشند.
- تست‌های arb/skim/pnl سبز.

---

### A5 — حذف fallback گمراه‌کننده در `liquidation_pnl`

**فایل:** `core/src/explorer/store/pnl.rs`.

**وضعیت فعلی:**
- در حالت نبود قیمت، `details["profit_usd_fallback"] = "collateral_amount"`
  ست می‌شود ولی مقدار برگشتی `None` است (حدود خط ۳۸۲–۳۸۷).

**تغییر موردنیاز:**
1. یا fallback را واقعاً پیاده کنید: `Some(collateral_usd)` با reason
   `LIQ_PARTIAL_COLLATERAL_ONLY`،
2. یا key را حذف/به `"none"` تغییر دهید و reason `MULTI_ASSET_PRICING` را
   نگه دارید.
3. پیش‌فرض امن: گزینهٔ ۲ (حذف ادعای دروغ)، چون ست کردن profit از collateral
   می‌تواند سود را بیش‌برآورد کند.

**دام‌ها:**
- dashboard/`evidence.rs` ممکن است `profit_usd_fallback` را بخواند؛ با
  `grep -n "profit_usd_fallback"` همهٔ مصرف‌کننده‌ها را پیدا کنید.
- تست‌های `pnl.rs` ممکن است وجود این key را assert کنند.

**معیار پذیرش:**
- اگر `profit_usd` برابر `None` است، details هیچ ادعای fallback اعمال‌شده
  نداشته باشد.
- تست جدید برای حالت missing price.

---

### A6 — حذف `Address::ZERO` از searcher candidates

**فایل:** `core/src/explorer/classify/legs.rs`.

**وضعیت فعلی:**
- `searcher_candidates` (حدود خط ۱۲۳) هر آدرسی با ≥۲ transfer را candidate
  می‌کند؛ `0x000…0` (mint/burn) هم می‌تواند وارد شود.

**تغییر موردنیاز:**
```rust
.filter(|(addr, count)| *count >= 2 && !addr.is_zero() && !candidates.contains(addr))
```
یا معادل آن قبل از `map`.

**دام‌ها:**
- `tx.from` می‌تواند به‌طور نظری zero باشد (tx سیستمی)؛ آیا می‌خواهید آن را هم
  حذف کنید؟ بله، `Address::ZERO` هرگز searcher واقعی نیست. اما تست‌های fixture
  ممکن است از zero استفاده کنند؛ آن‌ها را بررسی کنید.
- `NATIVE_MARKER` (`0xFF…FF`) نباید حذف شود؛ فقط zero.

**معیار پذیرش:**
- تست: transfer با `from/to = Address::ZERO` به candidates اضافه نشود.
- تست‌های موجود `searcher_candidates` سبز.

---

### A7 / R2 — وضعیت reorg و `unwind_from`

**فایل‌ها:** `core/src/explorer/mod.rs`، `core/src/explorer/ingest.rs`،
`core/src/explorer/store/sync.rs`، `core/src/explorer/store/facts.rs`.

**وضعیت فعلی:**
- doc در `mod.rs` (حدود خط ۱۰) و `ingest.rs` (حدود خط ۶) ادعای «reorg-aware via
  stored block hashes» دارد.
- اما `unwind_from` / `block_hash` فقط در تست‌ها صدا زده می‌شوند. تنها مسیر
  production، `cli/src/commands/explorer/backfill.rs` است که همهٔ بلاک‌ها را
  finalized فرض می‌کند.
- `insert_block_facts` از `INSERT OR IGNORE INTO blocks` استفاده می‌کند
  (`facts.rs` حدود خط ۲۱۱)، پس reorg با همان شماره، hash قدیمی را نگه می‌دارد.

**تغییر موردنیاز (پیش‌فرض امن):**
1. doc را اصلاح کنید: «hash ذخیره می‌شود، اما unwind در مسیر backfill اجرا
   نمی‌شود چون rangeها finalized فرض می‌شوند. برای live indexing باید
   `unwind_from` را وصل کرد.»
2. در `unwind_from`/`block_hash` doc بنویسید «API آماده است، مصرف‌کنندهٔ
   production ندارد».
3. **گزینهٔ کامل (نیاز به هماهنگی):** در مسیر live (اگر وجود دارد) قبل از
   index هر بلاک، `block_hash(number)` را با hash فعلی مقایسه کنید؛ اگر
   متفاوت بود `unwind_from(number)` را صدا بزنید. توجه: فقط explorer DB، نه
   scanner cache.

**دام‌ها:**
- `blocks_classified` گیت idempotency است؛ اگر hash عوض شد ولی
  `blocks_classified` پاک نشود، بلاک جدید هرگز index نمی‌شود.
- `unwind_from` جداول `opportunities` و paper را عمداً پاک نمی‌کند؛ دوباره
  ننویسید.
- `INSERT OR IGNORE` را به `ON CONFLICT(block_number) DO UPDATE` تغییر ندهید
  مگر اینکه مطمئن باشید بقیهٔ جداول هم به‌روز می‌شوند؛ در غیر این صورت
  ناسازگاری ایجاد می‌کنید.

**معیار پذیرش:**
- doc با واقعیت کد یکی باشد.
- اگر wiring انجام شد، تست: بلاک با hash متفاوت ⇒ `unwind_from` صدا زده شود
  و دوباره index شود.

---

## فاز ۲ — بهینه‌سازی

### P1 — پارس/سریالایز یک‌بارهٔ JSON در persist

**فایل:** `core/src/explorer/store/pnl.rs`، `core/src/explorer/store/mod.rs`
(`merge_details_json`)، تست‌ها.

**وضعیت فعلی:**
- `annotate_event_with_volume` حدود خط ۸۲: `ev.details.to_string()`
- حدود خط ۱۳۶ و ۲۰۴: `merge_details_json(...)`
- حدود خط ۲۷۸: `serde_json::from_str(&details_json)`
- `note_pricing_issue` حدود خط ۳۹۴: دوباره parse + stringify

**تغییر موردنیاز:**
1. در ابتدای تابع:
   ```rust
   let mut details: serde_json::Map<String, serde_json::Value> =
       match ev.details {
           serde_json::Value::Object(m) => m.clone(),
           _ => serde_json::Map::new(),
       };
   ```
   (یا `ev.details.as_object().cloned().unwrap_or_default()`.)
2. همهٔ annotationها را روی `details` انجام دهید (`insert`/`entry`).
3. `merge_details_json` را فقط در یک نقطه (یا کلاً حذف) استفاده کنید.
4. در انتها یک‌بار `serde_json::to_string(&details)` برای bind.
5. `note_pricing_issue` را به امضای `&mut serde_json::Map<...>` تغییر دهید یا
   یک تابع داخلی جدید بسازید؛ callerها را به‌روز کنید.

**دام‌ها / چیزهایی که ممکن است از قلم بیفتد:**
- ترتیب کلیدها در JSON برای تست‌هایی که substring چک می‌کنند مهم نیست، ولی
  golden snapshotها ممکن است byte-sensitive باشند. اگر snapshot دارید،
  خروجی را با دلیل به‌روز کنید.
- `unwrap_or_default()` فعلی در parse خطا، `{}` می‌دهد؛ رفتار جدید هم باید
  برای `details` غیر-object همان `{}` را بدهد. کرش نکنید.
- اعداد `U256` باید به‌صورت **string** بمانند؛ به `u64`/`f64` تبدیل نکنید.
- `route` key جدا serialize می‌شود (`route_json`)؛ آن را از map حذف نکنید.
- `details_json` در `PersistedOp` احتمالاً `Option<String>` است؛ بعد از
  تغییر، `Some(to_string)`. مطمئن شوید `None` در حالت خالی حفظ می‌شود اگر
  تستی به آن وابسته است.

**معیار پذیرش:**
- در کل مسیر persist حداکثر یک `to_string` و صفر `from_str` اضافی (به‌جز
  ورودی اولیه) وجود دارد.
- تست‌های `pnl.rs` سبز.
- یک benchmark ساده (اختیاری): تعداد allocation یا زمان persist یک بلاک قبل/بعد.

---

### P2 — پارس یک‌بارهٔ `details_json` در dashboard

**فایل:** `core/src/explorer/dashboard/reduce.rs` (و `evidence.rs` اگر لازم شد).

**وضعیت فعلی:**
- `details_has_token` (حدود خط ۶۳۷) و `details_tags` (حدود خط ۶۱۷) هر بار
  JSON را parse می‌کنند.
- call siteها: `op_flags` (حدود ۴۸۳)، `event_row` (۴۴۶/۴۵۳)، `build_op_index`
  (۵۴۹)، `evidence` (۱۰۹۹، ۱۱۵۵).
- `details_has_token` در خطای parse به `raw.contains(needle)` fallback می‌کند.

**تغییر موردنیاز:**
1. یک struct کوچک بسازید:
   ```rust
   struct ParsedDetails {
       tags: Vec<String>,
       reasons: HashSet<String>,
       flags: u32, // یا boolهای مشخص
   }
   ```
2. آن را یک‌بار per op در `reduce` بسازید و به `op_flags`/`event_row`/
   `build_op_index` پاس دهید.
3. `details_has_token` روی `ParsedDetails` کار کند، نه روی رشته.
4. fallback `raw.contains` را حذف کنید؛ اگر JSON خراب است، هیچ flag/tag نده
   (و در صورت امکان `debug!`/`warn!` بزنید).

**دام‌ها / چیزهایی که ممکن است از قلم بیفتد:**
- `reduce` امضای عمومی دارد (`pub fn reduce`)؛ اگر struct جدید را در امضا
  نمی‌آورید، آن را داخلی نگه دارید.
- `evidence.rs` ممکن است `OperationEconomics` بسازد و خودش JSON parse کند؛
  اگر از `ParsedDetails` استفاده نمی‌کند، حداقل duplicate parse را حذف کنید.
- `details_has_token` برای keyهای `tags`، `reasons`، `pricing_issues` چک
  می‌کند؛ این سه را در `ParsedDetails` نگه دارید.
- tagهای تکراری باید dedupe شوند (رفتار فعلی `details_tags`). همان را حفظ
  کنید.
- `op_flags` بیت‌های ثابت دارد (`FLAG_*` در حدود خط ۲۵۳)؛ ترتیب/مقدار را
  عوض نکنید چون HTML آفلاین به آن وابسته است.

**معیار پذیرش:**
- هر op در dashboard حداکثر یک‌بار parse شود.
- خروجی JSON dashboard برای یک DB نمونه byte-identical یا semantically
  identical باشد (تست snapshot یا diff).
- تست‌های dashboard سبز.

---

### P3 — حذف `serde_json::json!` تکراری در `build_op_index`

**فایل:** `core/src/explorer/dashboard/reduce.rs`.

**وضعیت فعلی:**
- `build_op_index` (حدود خط ۵۲۸–۶۰۸) برای هر op چند `json!` می‌سازد:
  `tag_ids`, `leg_vals`, `rows`.

**تغییر موردنیاز:**
1. **فرمت فعلی یک آرایه است، نه object.** `serde_json::json!([...])` یک
   آرایه تولید می‌کند. `#[derive(Serialize)]` روی struct با فیلدهای نام‌دار
   یک **object** تولید می‌کند و dashboard را می‌شکند. برای حفظ آرایه یکی از
   این دو را انتخاب کنید:
   - **tuple struct** (ساده‌ترین):
     ```rust
     #[derive(Serialize)]
     struct OpIndexRow(
         i64, u32, u32, u32, u32, u32, Option<u32>,
         u64, u32, Option<f64>, Option<f64>, Option<f64>,
         Option<f64>, Option<f64>, Vec<u32>, Vec<(u32, u32)>, i64,
     );
     ```
     ترتیب فیلدهای tuple = ترتیب عناصر آرایه.
   - یا `impl Serialize` با `serializer.serialize_seq(...)` اگر tuple
     خوانایی را کم می‌کند.
2. `tag_ids` را `Vec<u32>` و `leg_vals` را `Vec<(u32, u32)>` کنید.
3. `Option<f64>` را مستقیم استفاده کنید؛ serde آن را `null` می‌سازد.
   `skip_serializing_if` **نزنید**، چون در آرایه جای عنصر را جابه‌جا می‌کند.
4. `net` (که برای `excluded_from_net` می‌تواند Null باشد) را هم
   `Option<f64>` کنید.

**دام‌ها / چیزهایی که ممکن است از قلم بیفتد:**
- ساختار آرایه در HTML/JS مصرف می‌شود؛ **object شدن** بزرگ‌ترین خطر این
  تسک است. قبل و بعد را با یک نمونه serialize و diff کنید.
- `Option<f64>` در serde به‌صورت `null` سریالایز می‌شود؛ این با
  `serde_json::Value::Null` یکی است. `skip_serializing_if` را برای آرایه
  استفاده نکنید.
- `Intern` برای tagها `usize` برمی‌گرداند؛ به `u32` تبدیل کنید و از
  `try_from` استفاده کنید (lint `unwrap`!).
- طول tuple باید دقیقاً ۱۷ عنصر باشد (همان ۱۷ عنصر `json!` فعلی)؛ یکی
  کم/زیاد نشود.
- `conf_code` و `op_flags` احتمالاً `u32`/`i64` برمی‌گردانند؛ نوع‌ها را با
  tuple هماهنگ کنید.

**معیار پذیرش:**
- JSON خروجی `op_index` با قبل یکسان است (snapshot diff).
- تست‌های dashboard/HTML سبز.

---

### P4 — بهینه‌سازی `log_family`

**فایل:** `core/src/explorer/decode/mod.rs`.

**وضعیت فعلی:**
- `log_family` (حدود خط ۷۹–۱۶۱) یک زنجیرهٔ خطی از ~۴۰ مقایسهٔ `B256` است که
  برای هر log اجرا می‌شود.

**تغییر موردنیاز:**
1. یک `OnceLock<HashMap<B256, LogFamily>>` بسازید:
   ```rust
   static FAMILIES: OnceLock<HashMap<B256, LogFamily>> = OnceLock::new();
   fn families() -> &'static HashMap<B256, LogFamily> { ... }
   ```
2. `log_family` اول map را چک کند؛ اگر نبود `None`.
3. کلیدها همان topicهای static فعلی هستند.
4. اگر `LogFamily` قبلاً `Copy` نیست، `#[derive(Clone, Copy, ...)]` اضافه کنید.

**دام‌ها / چیزهایی که ممکن است از قلم بیفتد:**
- بعضی خانواده‌ها ممکن است به **بازه** یا چند topic نیاز داشته باشند
  (`is_swap_topic`, `is_liquidation_topic` و…). آن‌ها را در map نگذارید؛
  فقط تطابق دقیق `topic0` را map کنید و توابع بازه‌ای را دست‌نخورده نگه دارید.
- `B256` کلید `Hash` است؛ مشکلی ندارد.
- `OnceLock` در `std::sync` است؛ import کنید.
- lint `unwrap_used` روی `get_or_init` مشکلی ندارد.
- اگر در آینده topic جدیدی اضافه شد، باید در map هم اضافه شود؛ یک تست
  «همهٔ topicهای ثابت در map هستند» بنویسید تا drift نشود.

**معیار پذیرش:**
- `log_family` برای همهٔ topicهای موجود همان خروجی قبلی را بدهد (تست
  parametrized).
- کاهش محسوس زمان backfill در benchmark (اختیاری).

---

### P5 — Chunk کردن درخواست قیمت DefiLlama

**فایل:** `core/src/explorer/ingest.rs`، `core/src/explorer/pricing.rs`.

**وضعیت فعلی:**
- `warm_quotes_for_tokens` (حدود خط ۸۷۰) همهٔ `missing` را در یک
  `fetch_prices_llama` می‌فرستد. برای بلاک‌های پرترافیک/backfill طولانی، URL
  می‌تواند خیلی بلند شود.

**تغییر موردنیاز:**
1. `missing` را به chunkهای ~۵۰تایی تقسیم کنید.
2. هر chunk را با `FuturesUnordered` یا sequential (بسته به rate limit) fetch
   کنید.
3. خطای یک chunk کل عملیات را fail نکند؛ فقط آن chunk را `warn!` و رد کنید.
4. نتایج را merge کنید؛ `put_price` فقط برای quoteهای معتبر
   (`is_finite && > 0`) انجام شود.

**دام‌ها / چیزهایی که ممکن است از قلم بیفتد:**
- `fetch_prices_llama` ممکن است خودش URL را بسازد؛ اندازهٔ URL را در
  `pricing.rs` چک کنید، نه فقط اینجا.
- اگر chunkها هم‌زمان اجرا شوند، ممکن است rate-limit بخورید؛ یک
  concurrency محدود (مثلاً ۲–۴) انتخاب کنید.
- `known_token_cache` و `llama_decimals` بین chunkها باید merge شوند.
- قیمت‌های cache‌شده (`store.prices_at`) نباید دوباره fetch شوند.
- تست‌ها نباید شبکه واقعی بزنند؛ یک تابع chunking خالص بسازید و آن را تست
  کنید.

**معیار پذیرش:**
- تعداد توکن‌ها در هر request از یک حد (مثلاً ۵۰) بیشتر نشود.
- رفتار cache و source/hour حفظ شود.
- تست واحد برای chunking.

---

### P6 — قیمت native تاریخی (تصمیم محصولی)

**فایل:** `core/src/explorer/ingest.rs`، `core/src/explorer/pricing.rs`.

**وضعیت فعلی:**
- `native_price_cached(cfg, store, epoch_secs())` هر ۲۵۶ بلاک با زمان *الان*
  صدا زده می‌شود (حدود خط ۱۱۷۲ و ۱۲۶۷). برای backfill تاریخی، gas USD
  تقریبی است.

**تغییر موردنیاز (پیش‌فرض امن):**
1. در `details` یا `ReportQuality` یک flag `native_price_approximate` اضافه
   کنید (اگر چنین فیلدی دارید)، یا در doc بنویسید.
2. **گزینهٔ کامل:** `native_price_at(ts_block)` بسازید که از DefiLlama
   historical استفاده کند و per-hour cache شود. برای هر بلاک، timestamp بلاک
   را از `BlockData` بگیرید و قیمت همان ساعت را استفاده کنید.

**دام‌ها:**
- قیمت native برای gas و native residual استفاده می‌شود؛ تغییر آن می‌تواند
  `net_profit_usd` را عوض کند.
- `native_price_cached` ممکن است cache جدول `prices` را با `hour` کلید کند؛
  مطمئن شوید کلید درست است.
- اگر historical price نیست، به قیمت current fallback کنید و flag بزنید؛
  NULL کردن profit بهتر از عدد غلط است اما رفتار را عوض می‌کند.
- درخواست اضافی per block نزنید؛ per-hour cache الزامی است.

**معیار پذیرش:**
- برای یک بلاک تاریخی مشخص، gas USD با قیمت همان ساعت محاسبه شود (یا flag
  approximate ست شود).
- بدون افزایش محسوس تعداد RPC/HTTP call.

---

### P7 — بهبود کوئری‌های SQL

#### P7a — `block_classified` با `EXISTS`

**فایل:** `core/src/explorer/store/sync.rs`.

**وضعیت فعلی:** `SELECT COUNT(*) FROM blocks_classified WHERE block = ?1`
(حدود خط ۲۲).

**تغییر:**
```sql
SELECT EXISTS(SELECT 1 FROM blocks_classified WHERE block = ?1)
```
و `query_row` را به `bool`/`i64` تبدیل کنید.

**دام:** `query_row` نوع را دقیق بگیرید؛ `EXISTS` در SQLite `0/1` برمی‌گرداند.

**معیار پذیرش:** تست‌های `sync.rs` سبز؛ رفتار boolean یکسان.

#### P7b — `unclassified_blocks` با `LEFT JOIN`

**فایل:** `core/src/explorer/store/sync.rs` (حدود خط ۴۷–۷۶).

**وضعیت فعلی:** recursive CTE + `NOT IN` برای هر پنجرهٔ ۴۰۹۶.

**نتیجهٔ بررسی (مهم):** این تابع عمداً بلاک‌هایی را برمی‌گرداند که **هرگز
index نشده‌اند** (شکاف واقعی)، نه فقط بلاک‌های موجود در جدول `blocks`.
تست `unclassified_blocks_reports_gaps_across_windows` (حدود خط ۲۱۴ در
`sync.rs`) دقیقاً همین را assert می‌کند: `mark_block_classified(4100)` و
`unclassified_blocks(0, 5000)` باید ۵۰۰۰ عنصر بدهد. بنابراین:

**تغییر امن (اختیاری، برد کم):** فقط `NOT IN` را داخل همان CTE با `LEFT JOIN`
جایگزین کنید:
```sql
WITH RECURSIVE seq(b) AS (
  SELECT ?1 UNION ALL SELECT b+1 FROM seq WHERE b < ?2
)
SELECT s.b FROM seq s
LEFT JOIN blocks_classified c ON c.block = s.b
WHERE c.block IS NULL
```
**تغییر خطرناک (انجام ندهید):** `FROM blocks b LEFT JOIN blocks_classified ...`
زیرا بلاک‌های هرگز دیده‌شده را از دست می‌دهد و تست بالا fail می‌شود.

**پیش‌فرض:** اگر وقت کم است، این تسک را کلاً رد کنید؛ `P7a` برد بیشتری با
ریسک صفر دارد.

#### P7c — `top_pools` با جدول `swaps`

**فایل:** `core/src/explorer/store/query.rs` (حدود خط ۱۶۴–۱۹۳).

**وضعیت فعلی:** `GROUP_POOL` از `json_each(route_json)` استفاده می‌کند.

**تغییر پیشنهادی:** برای pool leaderboard از `swaps(pool, block_number)` +
join به `mev_ops` استفاده کنید، یا یک جدول واسط `op_pools` بسازید. اما:
- `swaps` فقط swapهای decode‌شده را دارد؛ ممکن است با `route_json` یکی نباشد.
- `COUNT(DISTINCT m.id)` و `SUM` semantics باید حفظ شوند.
- این تغییر می‌تواند اعداد leaderboard را عوض کند.

**پیش‌فرض:** اگر اعداد باید byte-identical بمانند، این تسک را انجام ندهید؛
فقط یک ایندکس روی `route_json` نمی‌تواند کمک کند. اگر انجام می‌دهید، تست
مقایسه‌ای روی DB نمونه اضافه کنید.

#### P7d — Dedupe کردن `pending_labels`

**فایل:** `core/src/explorer/store/facts.rs` (حدود خط ۲۰۲–۲۰۵ و ۳۹۱–۳۹۷).

**وضعیت فعلی:** `pending_labels` برای هر event یک `(searcher, contract, block)`
می‌سازد؛ اگر یک EOA چند op در یک بلاک داشته باشد، چند بار upsert می‌شود.

**تغییر:**
```rust
let pending_labels: PendingLabels = events
    .iter()
    .map(|ev| (ev.searcher, ev.contract, ev.block))
    .collect::<std::collections::BTreeSet<_>>()
    .into_iter()
    .collect();
```
(یا `HashSet` + sort برای deterministic بودن.)

**دام‌ها:**
- `PendingLabels` نوع alias است؛ ببینید `Vec` است یا نه.
- `label_competitor` ممکن است به ترتیب وابسته باشد؛ dedupe با `BTreeSet`
  deterministic است.
- خطاها best-effort هستند و بعد از commit نادیده گرفته می‌شوند؛ همان semantics
  را حفظ کنید.

**معیار پذیرش:** تعداد `label_competitor` برای یک بلاک با ops تکراری کاهش
یابد؛ تست موجود labels سبز.

---

## فاز ۳ — مینیمالیسم

### M1 — حذف cloneهای block-wide در `classify_block`

**فایل:** `core/src/explorer/classify/mod.rs` (حدود خط ۱۹۴–۲۱۷).

**وضعیت فعلی:**
- `block_oracle_owned`، `block_reserve_owned`، `block_rate_cache` با
  `flat_map(... .cloned())` ساخته می‌شوند.

**تغییر موردنیاز:**
1. اگر ساختار `BlockInput` اجازه می‌دهد، به‌جای `Vec<T>` یک `Vec<&T>` بسازید:
   ```rust
   let block_oracle: Vec<&OracleUpdateFact> =
       input.txs.iter().flat_map(|t| t.oracle_updates.iter()).collect();
   ```
2. `LiqBlockCtx` و `ArbTagOpts` را با `&[&T]` یا `&[T]` هماهنگ کنید.
3. اگر lifetimeها دردسر شد، `Cow<'_, [T]>` یا `Arc<[T]>` گزینهٔ دوم است؛
   اما `Vec<&T>` ساده‌ترین است.

**دام‌ها:**
- `LiqBlockCtx` ممکن است `&[OracleUpdateFact]` بخواهد؛ اگر `Vec<&T>` دادید،
  باید امضا را عوض کنید. همهٔ callerها را ببینید.
- `block_rate_cache` به `ArbTagOpts` می‌رود؛ اگر آنجا `&[RateCacheFact]` است،
  `Vec<&RateCacheFact>` را نمی‌توان مستقیم داد. یا امضا را به `&[&T]` عوض
  کنید یا از `Cow` استفاده کنید.
- `epoch_reward_tokens` از قبل `Vec<Address>` است (کپی سبک)؛ فقط tokens را
  نگه دارید.
- `epoch_venue_list` از `HashSet` کپی می‌شود؛ به `&HashSet` تبدیل کنید اگر
  ممکن است.

**معیار پذیرش:**
- هیچ `.cloned()` روی facts در `classify_block` باقی نماند (به‌جز موارد
  ضروری).
- تست‌های classify سبز.

---

### M2 — حذف plumbing رشته‌ای `details`

**فایل‌ها:** `core/src/explorer/store/pnl.rs`، `core/src/explorer/store/mod.rs`.

**وضعیت فعلی:** `details` به‌صورت `String` در چند لایه می‌چرخد
(`merge_details_json(&str) -> String`، `details_json: String`).

**تغییر:**
- همان تغییر `P1` است. این تسک را جدا انجام ندهید؛ `P1` کافی است.
- `merge_details_json` را حذف کنید اگر هیچ caller دیگری ندارد:
  ```powershell
  Select-String -Path core/src/**/*.rs -Pattern "merge_details_json"
  ```
- اگر caller دارد، آن را به یک تابع روی `Map` تبدیل کنید.

**دام:** حذف یک تابع `pub(super)` بدون به‌روزرسانی caller باعث build fail
می‌شود (که خوب است).

---

### M3 — یکسان‌سازی منطق cycle/profit

این تسک با `A2` هم‌پوشانی دارد. اگر `A2` انجام شد، اینجا فقط:
1. `searcher_candidates` + `resolved_cycle_edges` + `tx_cycle_profit` را به یک
   تابع `attribute_cycle` تبدیل کنید (اختیاری).
2. مطمئن شوید `arb.rs` و `legs.rs` هیچ منطق تکراری برای «انتخاب searcher»
   ندارند.

**دام:** `arb.rs` از `ledger` برای flash netting و forwarded profit استفاده
می‌کند؛ `legs.rs` این کارها را نمی‌کند. یکسان‌سازی کامل می‌تواند رفتار را
عوض کند. اگر مطمئن نیستید، فقط `A2` را انجام دهید و این تسک را رد کنید.

---

### M4 — مستندسازی kindهای legacy

**فایل:** `core/src/explorer/store/schema.rs` (حدود خط ۱۹۱، ۴۲۰، ۷۳۶، ۹۲۲).

**وضعیت:** CHECK constraints هنوز `sandwich`, `frontrun`, `backrun`, `jit_arb`
را دارند، در حالی که `MevKind` جدید این‌ها را ندارد.

**تغییر:**
1. کامنت بالای هر CHECK بنویسید: «legacy-only for old DBs; new code never
   writes these.»
2. یک تست در `schema.rs` یا `types.rs` اضافه کنید که مطمئن شود
   `MevKind::as_str()` هیچ‌وقت این مقادیر را تولید نمی‌کند.
3. migration جدید **نسازید**؛ فقط doc/test.

**دام:**
- DBهای قدیمی ممکن است این مقادیر را داشته باشند؛ dashboard باید بتواند
  بخواند. در `dashboard` mapping، این kindها را به یک برچسب
  «legacy/unknown» map کنید، نه panic.
- تست `map_kind` ممکن است روی این مقادیر assert کند؛ آن را نشکنید.

**معیار پذیرش:** تست جدید legacy kinds سبز؛ هیچ migration جدیدی اضافه نشده.

---

## فاز ۴ — پایداری

### R1 — یکسان‌سازی رفتار خطا در مسیر pipelined `run_range`

**فایل:** `core/src/explorer/ingest.rs` (حدود خط ۱۲۶۰–۱۳۳۲).

**وضعیت فعلی:**
- مسیر serial (`cap <= 1`) خطای `index_block` را `warn!` می‌کند، در
  `skipped` ثبت می‌کند و ادامه می‌دهد (حدود خط ۱۱۷۵–۱۲۰۲).
- مسیر pipelined از `index_block_fetched(...).await?` استفاده می‌کند (حدود
  خط ۱۲۷۱–۱۲۸۶)؛ یک خطای persist کل range را abort می‌کند.

**تغییر موردنیاز:**
1. در مسیر pipelined، خطای `index_block_fetched` را match کنید:
   ```rust
   let indexed = match index_block_fetched(...).await {
       Ok(v) => v,
       Err(e) => {
           warn!(block = need, error = %e, "backfill block failed; skipping");
           skipped.insert(need);
           next_persist += 1;
           let done = need - from_block + 1;
           emit_progress(done, outcome.ops, done == total);
           // مثل branch خطای fetch: خط لوله را دوباره پر کنید، وگرنه
           // اگر futures تمام شده باشند، حلقه با "pipeline stalled" bail می‌کند.
           while next_spawn < missing.len() && inflight.len() < posts {
               spawn_one(&mut next_spawn, &mut inflight)?;
           }
           continue;
       }
   };
   ```
2. مطمئن شوید `next_persist` همیشه advance می‌شود (وگرنه حلقهٔ بی‌نهایت).
3. `ready` را برای بلاک skip‌شده نگه ندارید؛ `bundle` قبلاً با
   `ready.remove(&need)` برداشته شده است. فقط مطمئن شوید دوباره insert نمی‌شود.
4. **رفیل خط لوله بعد از skip الزامی است.** بدون آن، اگر همهٔ futureهای
   inflight تمام شده باشند، دور بعد `inflight.next().await` مقدار `None`
   می‌دهد و کد به `bail!("pipeline stalled ...")` می‌رسد — یعنی همان abort
   که می‌خواستیم حذف کنیم.
5. سیاست: خطاهای **سطح بلاک** (decode/persist یک بلاک) skip شوند. خطاهای
   **سطح DB/infrastructure** (مثلاً `SQLITE_BUSY` مداوم، disk full) می‌توانند
   abort کنند. اگر تفکیک سخت است، فعلاً همه را skip کنید و در commit بنویسید.

**دام‌ها / چیزهایی که ممکن است از قلم بیفتد:**
- `skipped` در ابتدای حلقه چک می‌شود (خط ۱۲۶۲)؛ اگر بلاکی در `ready` است و
  بعداً fail می‌شود، باید `ready.remove` شود.
- `emit_progress` باید مثل مسیر serial پیش برود؛ `done = need - from_block + 1`.
- اگر `outcome.blocks_processed` را افزایش ندهید، گزارش پیشرفت درست است
  (بلاک انجام نشده)، ولی `skipped_blocks` باید در انتها ست شود (خط ۱۳۳۵).
- تست regression: یک بلاک را عمداً fail کنید (mock/store fault injection) و
  مطمئن شوید range ادامه می‌یابد. اگر fault injection سخت است، حداقل منطق
  را با یک تابع خالص (`handle_persist_result`) تست کنید.
- `run_range` قرارداد `total`/`ops` را دارد؛ تغییر ندهید.

**معیار پذیرش:**
- یک خطای persist در مسیر pipelined کل range را abort نکند.
- `skipped_blocks` شامل آن بلاک باشد.
- تست جدید/موجود سبز.

---

### R3 — رفتار `blocks` در برابر hash تکراری

**فایل:** `core/src/explorer/store/facts.rs` (حدود خط ۲۱۱).

**وضعیت:** `INSERT OR IGNORE INTO blocks` اگر بلاک با همان شماره وجود داشته
باشد، hash جدید را نادیده می‌گیرد. چون `unwind_from` در production صدا زده
نمی‌شود، این می‌تواند hash قدیمی را نگه دارد.

**تغییر موردنیاز (پیش‌فرض امن):**
- این تسک را با `A7/R2` ترکیب کنید. اگر reorg wiring انجام نشد، فقط یک
  کامنت در `facts.rs` بگذارید: «stale hash possible; see A7.»
- اگر wiring انجام شد، قبل از insert:
  ```rust
  if let Some(existing) = self.block_hash(block_number)? {
      if existing != format!("{block_hash:#x}") {
          self.unwind_from(block_number)?;
      }
  }
  ```
  **هشدار:** این کار داخل تراکنش `insert_block_facts` نباشد؛ `unwind_from`
  خودش تراکنش دارد. قبل از شروع تراکنش انجام دهید.

**معیار پذیرش:** مستندسازی یا تست reorg.

---

## فاز ۵ — تصمیم‌های محصولی (اجرا نکنید مگر تأیید شود)

- `A3` (انتخاب cycle سودآور)
- `A4` (گسترش `profit_tokens` — پیش‌فرض فقط doc)
- `P6` (قیمت native تاریخی)
- `R2` (wiring واقعی reorg)

برای این‌ها در commit message بنویسید: «behavior change, needs product sign-off».

---

## ضمیمهٔ الف — چیزهایی که به‌راحتی از قلم می‌افتند

1. **`normalize_confidence` قبل از persist اجرا می‌شود** و فقط `Exact`ها را
   پایین می‌آورد. persist نباید هیچ‌وقت `Exact` بسازد (به‌جز `PnlBasis::R`).
2. **`details.pnl_basis`** فقط یک key در JSON است؛ تغییر آن بدون تست،
   `ReportQuality` را بی‌صدا عوض می‌کند.
3. **`profit_usd` صفر**: در `pnl.rs` شرط `(total > 0.0).then_some(total)` است؛
   اگر `total == 0.0` مقدار `None` می‌شود. این رفتار را حفظ کنید.
4. **`NATIVE_MARKER`** (`0xFF…FF`) با `Address::ZERO` فرق دارد. فیلترهای
   قیمت‌گذاری هر دو را جدا مدیریت می‌کنند.
5. **`route_json`** جدا از `details_json` است؛ `top_pools` به آن وابسته است.
6. **`excluded_from_net`** همیشه 0 است (بقایای A10)؛ آن را دوباره فعال نکنید.
7. **`record_rejections`** یک flag config است؛ تغییر در rejectها می‌تواند
   حجم DB را عوض کند.
8. **`SCHEMA_HEAD`** از طول `MIGRATIONS` می‌آید؛ اگر migration اضافه کردید،
   تست‌های `user_version` خودکار به‌روز می‌شوند، ولی dashboard ممکن است
   schema قدیمی را رد کند. `dashboard` عمداً migrate نمی‌کند.
9. **SQLite JSON1** (`json_each`) در نسخهٔ bundled rusqlite موجود است؛ ولی
   اگر query را عوض کردید، `EXPLAIN QUERY PLAN` بگیرید.
10. **`serde_json::Value` و `U256`**: همیشه U256 را string نگه دارید؛
    `f64` برای amounts غلط است.
11. **`f64` NaN/Inf**: در `pnl.rs` چک‌های `is_finite` وجود دارد؛ حذف نکنید.
12. **`Intern` و `u32`**: تبدیل `usize` به `u32` با `try_from` و خطای
    `anyhow`، نه `as`.
13. **`OnceLock` و تست‌ها**: map استاتیک بین تست‌ها share می‌شود؛ اگر تستی
    topic اضافه می‌کند، map را دستکاری نکند.
14. **`cargo fmt`**: بعد از هر تسک اجرا کنید؛ diff را تا حد امکان محدود نگه
    دارید (فایل‌های unrelated را reformat نکنید).
15. **کامنت‌های «not-a-drop» / «intentional»** را حذف نکنید؛ آن‌ها تصمیم‌های
    قبلی را مستند می‌کنند. اگر تصمیم را عوض می‌کنید، کامنت را هم به‌روز کنید.
16. **تست‌های golden** (`core/src/explorer/golden.rs`،
    `tests/explorer_corpus.rs`) ممکن است به ترتیب/اعداد وابسته باشند. قبل از
    تغییر snapshot، دلیل رفتاری را ثابت کنید.
17. **بدون `unwrap` در production**: حتی در کد جدیدی که برای «سادگی» می‌نویسید.
    از `ok_or_else`/`anyhow!` استفاده کنید.
18. **`Address` sort/dedup**: `Address` ترتیب دارد؛ برای deterministic بودن
    از `sort_unstable` روی آدرس استفاده کنید (الگوی موجود در
    `searcher_candidates`).
19. **کامپایل Windows/PowerShell**: از `;` بین دستورات PowerShell استفاده
    نکنید اگر خروجی را کوتاه می‌خواهید؛ دستورات را جدا اجرا کنید.
20. **بعد از هر تسک**: `cargo test --workspace` و `cargo clippy --workspace
    --all-targets` را اجرا کنید. اگر lint جدیدی ظاهر شد، همان تسک را کامل
    نکنید.

---

## ضمیمهٔ ب — ترتیب پیشنهادی commitها

1. `docs(explorer): add refactor plan` (همین فایل)
2. `fix(explorer): liquidation PnL never claims Exact (A19)` — `A1`
3. `fix(explorer): unify cycle edge filtering across arb/causal` — `A2`
4. `fix(explorer): drop misleading liquidation usd fallback` — `A5`
5. `fix(explorer): exclude zero address from searcher candidates` — `A6`
6. `perf(explorer): parse details JSON once at persist time` — `P1`
7. `perf(explorer): parse details JSON once in dashboard reduce` — `P2`
8. `perf(explorer): typed op-index rows` — `P3`
9. `perf(explorer): O(1) log family lookup` — `P4`
10. `perf(explorer): chunk llama price requests` — `P5`
11. `perf(explorer): EXISTS / dedupe / query cleanups` — `P7a`, `P7d`
12. `refactor(explorer): avoid block-wide fact clones` — `M1`
13. `docs(explorer): document legacy schema kinds` — `M4`
14. `fix(explorer): skip failed blocks in pipelined backfill` — `R1`
15. `docs(explorer): correct reorg claims` — `A7/R2` (doc-only)
16. (اختیاری، با تأیید) `A3`, `A4`, `P6`, `R2` کامل.

---

## ضمیمهٔ ج — قالب گزارش پایان هر تسک

برای هر تسک این را در PR/commit بنویسید:

```markdown
## Task <ID>
- Files: ...
- Behavior change: yes/no (اگر yes: چه عدد/خروجی‌ای عوض می‌شود)
- Tests added: ...
- Tests run: cargo test --workspace
- Clippy: clean
- Risks / follow-ups: ...
```

اگر تستی را عمداً تغییر دادید، دلیلش را ذکر کنید. اگر تسکی را رد کردید،
دلیل رد را بنویسید (مثلاً `P7b` به‌خاطر invariant جدول `blocks`).
