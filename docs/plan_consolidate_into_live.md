# طرح: یکپارچه‌سازی `run` و `paper` در `live`

> **وضعیت:** فاز ۱ تا ۵ اجرا و تأیید شد (fmt/clippy/test سبز). فقط بخش شبکه‌ایِ (۶) باقی مانده.
> **تاریخ:** ۱۴۰۴/۰۷/۰۱
> **دامنه:** کاهش سطح فرمان CLI از ۸ به ۶، با انتقال کامل قابلیت‌ها به `live`

---

## ۱) انگیزه و تصمیم پایه

**محدودیت:** برای زنجیرهٔ هدف (biter) نود آرشیو رایگان در دسترس نیست و راه‌اندازی آن
عملی نیست. بنابراین قابلیت بک‌تست تاریخی عملاً بی‌استفاده است و `live` برای کاربرد
هدف کافی است.

**تصمیم‌های تثبیت‌شده:**

| مورد | تصمیم | دلیل |
|---|---|---|
| `run` | فقط از CLI حذف؛ `core/src/jobs/run.rs` **می‌ماند** | `mev_corpus.rs` (۸۵۰ خط) و `paper_corpus.rs` (۴۲۳ خط) به `job_run` نیاز دارند |
| `paper` | فرمان حذف؛ قابلیتش داخل `live` ادغام می‌شود | `paper_corpus.rs` فقط کتابخانهٔ `core/src/paper/` را می‌خواهد، نه `job_paper_*` |
| `live --blocks N` | رد شد | `use_latest()` ذخیره‌ها را در `tip-1` می‌خواند؛ اسکن بازهٔ قدیم‌تر مسیر ذخیره‌ها را خراب می‌کند |
| ledger در `live` | **همیشه فعال** | محاسبهٔ خالص روی opps موجود — صفر RPC اضافه |
| پرگم موجودی | `--initial-balance <wei>` + `--initial-balance-usd <$>` | native = primitive دقیق؛ USD = لایهٔ راحتی |
| `paper sim` / `paper stats` | هر دو حذف | هر دو با `live` + `report` پوشش داده می‌شوند |
| `core/src/paper/` | کامل می‌ماند | `ledger`/`recon`/`types` را live و corpus test مصرف می‌کنند |

---

## ۲) سطح فرمان پس از تغییر

```
report · config · discover · tokens · live · explorer        (۸ → ۶)
```

### امضای جدید `live`

```bash
mev-scout live
mev-scout live --loop [--duration 30m] [--max-blocks 100]
mev-scout live --initial-balance 10000000000000000000   # native، دقیق و آفلاین
mev-scout live --initial-balance-usd 10                 # لایهٔ راحتی، نیازمند قیمت
mev-scout live --reserve 500000000000000000             # native idle
mev-scout live --max-fills-per-block 4
mev-scout live --native-usd 1.23                        # اختیاری: قیمت را دستی بده
```

---

## ۳) مبنای فنی تصمیم

### ۳.۱ چرا بک‌تست تاریخی بدون آرشیو ناممکن است

ذخیره‌های استخر یک‌بار در `block_num` خوانده می‌شوند
(`BacktestRunner::init_pools` → `PoolManager::init_from_rpc`،
`core/src/pipeline/runner.rs:363`) و سپس فقط با `update_from_logs` **به‌جلو** می‌روند
(`core/src/pipeline/runner.rs:700-708` و `783-800`).

> هر بازهٔ تاریخی ذاتاً به state در `start - 1` نیاز دارد. بدون archive، هیچ مسیری برای
> بازتولید درست بازهٔ قدیم وجود ندارد — نه در `run`، نه در hybrid.

استثنا: `run --blocks N` (بازهٔ نزدیک tip) روی فول‌نود کار می‌کند، چون `tip - N` هنوز در
retention است. `live --loop --max-blocks N` **همین کار را می‌کند** — ولی ضعیف‌تر و تکه‌تکه.

### ۳.۲ چرا ledger تمیز داخل `live` جا می‌شود

`LedgerPolicy::apply(&[MevOpportunity])` (`core/src/paper/ledger.rs:96`) یک تابع
**pure** است که خودش بر اساس `block_number` گروه‌بندی می‌کند
(`ledger.rs:111-118`). بنابراین در حلقهٔ live:

```rust
opps.extend(new_opps);              // انباشت
let ledger = policy.apply(&opps);   // همیشه درست، بدون state-threading
```

بدون نیاز به هیچ مدیریت state اضافه.

### ۳.۳ زیرساخت قیمت native از قبل موجود است

دستور پخت دقیقاً در `core/src/jobs/trace.rs:374-390` پیاده‌سازی شده:

```
store.price_at(Address::ZERO, hour)              // native کلیدشده با ZERO (ingest.rs:524)
  → store.price_at(wrapped_native, hour)
    → fetch_native_price_llama(chain, wrapped)   // DefiLlama
      → fetch_native_price_coingecko(chain)     // CoinGecko
```

تبدیل: `pricing::wei_to_usd(wei, native_usd)` (`core/src/explorer/pricing.rs:219`).

⚠️ **مانع برای زنجیرهٔ تازه:** `native_asset_id` (`pricing.rs:25`) و
`llama_chain_prefix` (`pricing.rs:38`) هر دو `match` بسته روی `ChainName` هستند و
بازوی زنجیرهٔ biter را ندارند ⇒ `--initial-balance-usd` روی آن زنجیره در دسترس نیست.
دلیل اصلی تصمیم «native = primitive».

---

## ۴) مقایسهٔ فعلی `run` در برابر `live` (مبنای فاز ۱)

| بُعد | `run` | `live` وضعیت فعلی |
|---|---|---|
| ورودی بازه | `--days/--blocks/--block/--from+--to` | ندارد (tip خودکار) |
| اعتبارسنجی | `validate_and_resolve` | `validate_live` — ۴ بررسی را رد می‌کند |
| init استخر | `prev_block` با بلوک عددی ⇒ نیازمند archive | `use_latest()` ⇒ بدون archive |
| موتور بلوک | `run_range` (همیشه full replay) | `run_range_hybrid` + `detect_state_horizon` |
| `capture_pending` | دارد | **اعمال نمی‌شود** |
| `max_candidates_per_tx` | دارد | **اعمال نمی‌شود** |
| `batch_rpc` | دارد | **عمداً نیست** — `tokio::join!` در `fetcher.rs:321` دو round-trip را موازی می‌فرستد |
| `block_concurrency` | `effective_block_concurrency` | **عمداً نیست** — روی بازهٔ ۱ بلاکیِ tip بی‌اثر است |
| `auto_refetch_gaps` | دارد | **ندارد** ⇒ گپ در live **دائمی** است |
| `print_startup_plan` | دارد | ندارد |
| `render_block_summary_table` | دارد | ندارد؛ در حلقه دور ریخته می‌شود |
| `run_id` | یکی برای کل اجرا | **جدید برای هر پاس** ⇒ ۲۴۰۰ manifest در ۲ ساعت |
| `total_txs_scanned` | درست | **باگ `+= 0`** |
| retry | ندارد | `MAX_CONSECUTIVE_FAILURES = 5` |
| one-shot tip | از flag | از `ctx.tip` قدیمی |

---

### چرا `batch_rpc` و `block_concurrency` به `live` اضافه نمی‌شوند

این دو **عمداً** منتقل نمی‌شوند (برخلاف بقیهٔ سطرهای جدول که نقص live هستند):

- **`batch_rpc`** — در `client.rs:1110` هر دو متد `eth_getBlockByNumber` و
  `eth_getBlockReceipts` را در **یک** HTTP POST می‌فرستد؛ یعنی per-block است نه per-range،
  پس در پاس تک‌بلاکی live هم ۲ round-trip را به ۱ کم می‌کند. اما مسیر غیر-batch در
  `fetcher.rs:321` از `tokio::join!` استفاده می‌کند و دو درخواست را **همزمان** می‌فرستد،
  پس batch چیزی جز **سهمیهٔ rate-limit** نمی‌خرد — نه latency. در ازایش کل JSON بلاک و
  receipts را در یک buffer جمع می‌کند و لاگ می‌کند (`client.rs:1221-1230`) و خطای یکی
  هر دو را می‌کُشد. در برابر نود محلی، هزینهٔ حافظه بدون هیچ سودی است.
- **`block_concurrency`** — تنها در `fetcher.rs:284` به‌کار می‌رود:
  `cap = block_concurrency.min(total_blocks)` و اگر `cap <= 1` مسیر ترتیبی (`:286`) اجرا
  می‌شود. یعنی فقط concurrency **داخل یک بازهٔ پیوسته**. در live بازهٔ هر پاس
  `last_block+1 ..= current_tip` است؛ تا وقتی live به tip چسبیده بازه ۱ بلاک ⇒ `cap = 1` ⇒
  **no-op**. concurrency بین shardها از قبل `parallelism` است (`fetcher.rs:165`) که live
  آن را با `with_parallelism(provider_configs.len())` می‌سازد، پس چیزی از دست نمی‌رود.

هر سه قابلیت در `run` **دست‌نخورده** می‌مانند (batch روشن، `block_concurrency` از تنظیمات،
refetch فعال)، پس `mev_corpus.rs` / `paper_corpus.rs` / `replay.rs` روی Avalanche بدون افت
سرعت کار می‌کنند. در `live`، `Fetcher` با پیش‌فرض‌های `batch_rpc: false` و
`block_concurrency: 1` ساخته می‌شود (`fetcher.rs:97,100`) و کد مرده وارد `live` نمی‌شود.

#### چرا `auto_refetch_gaps` **باید** بماند

این تنها موردی است که مسئلهٔ throughput نیست، مسئلهٔ **درستی** است. جریان کار:
`check_integrity_range` (`fetcher.rs:522`) گپ‌ها را می‌دهد و `fetcher.rs:534` **یک‌بار**
دوباره تلاش می‌کند. تعریف گپ در `integrity.rs:14-17`: بلاک فقط وقتی موجود است که در
`blocks` JOIN `block_meta` باشد **و `txs_fetched = 1`** — یعنی هم بلاک غایب و هم بلاک
نیمه‌نوشته گپ‌اند.

چرا برای live **مهم‌تر** از run است:

1. **مسابقهٔ tip ذاتاً در live وجود دارد.** `run` روی تاریخ تثبیت‌شده اسکن می‌کند و بلاک
   همیشه موجود است. live تازه‌ترین بلاک را از نود می‌خواهد و خود کد این حالت را
   مستند کرده: `client.rs:1214-1218` — «eth_getBlockReceipts returned null — the node may
   not have indexed this block yet» (و `:1090-1092` در مسیر غیر-batch).
2. **گپ در live دائمی است.** حلقه `last_block = current_tip` (`live.rs:507`) و
   `advance_to(current_tip)` (`:515`) را بی‌بازگشت جلو می‌برد؛ بلاکی که یک‌بار fail شد
   هرگز بازبینی نمی‌شود ⇒ **حفرهٔ دائمی در opportunity record**. بدتر: در خطا
   `live.rs:513`، `pool_manager.reset_to_tip(current_tip)` می‌زند، یعنی state کلاً واگرا
   می‌شود و بی‌صدا ادامه می‌دهیم.
3. شرط `txs_fetched = 1` یعنی بلاک نیمه‌نوشته هم گپ است — دقیقاً همان چیزی که رقابت
   write-buffer در tip می‌تواند تولید کند.

هزینه‌اش وقتی گپی نیست صفر است: `fetcher.rs:535-537` همان ابتدا `gaps.is_empty()` و
`Ok(0)`. یعنی روی گپ واقعی فقط یک retry انجام می‌شود، که بیمهٔ ارزانی است و «حفرهٔ دائمی +
واگرایی بی‌صدا» را به «یک تلاش دیگر» تبدیل می‌کند.

---

## ۵) فازهای اجرا

### فاز ۱ — انتقال قابلیت‌های `run` به `live`

#### ۱.۱ تنظیم runner

`core/src/jobs/live.rs`

| # | تغییر | محل |
|---|---|---|
| 1 | `with_capture_pending(config.backtest.capture_pending)` | بعد از `live.rs:126` |
| 2 | `with_max_candidates_per_tx(config.backtest.max_candidates_per_tx)` | همان‌جا |
| 3 | `auto_refetch_gaps` روی `missing_after_fetch` (الان با `.map(\|_\| ())` دور ریخته می‌شود) | `live.rs:158,160` |

#### ۱.۲ درستی

| # | تغییر | محل |
|---|---|---|
| 4 | one-shot دوباره tip را بخواند به‌جای مصرف `ctx.tip` قدیمی | `run_once`، `live.rs:271` |
| 5 | رفع باگ `total_txs_scanned += 0` | `live.rs:510` |
| 6 | انباشت `block_stats` در حلقه و بازگرداندن در `LiveLoopOutcome` | `live.rs:461` |
| 7 | refactor `validate_live` به `validate_common()` مشترک | `config/validation.rs:414` |

**جزئیات ۷:** `validate_live` یک کپی تقریباً-یکسان از `validate_and_resolve_for` است که
این ۴ بررسی را بی‌سروصدا رد می‌کند:
- `gas_limit` در بازهٔ `21_000..=30_000_000`
- `rpc.rps_limit <= 10_000`
- `backtest.proximity_window <= 100`
- هشدار نبودِ قرارداد flashloan اجباری

هدف: یک `validate_common()` که هر دو صدا بزنند؛ `validate_live` فقط
`check_range_conflicts` را رد می‌کند.

#### ۱.۳ یک `run_id` برای کل سشن

الان هر پاس یک `live_{epoch}` تازه می‌سازد (`live.rs:409`) و manifest + رکورد explorer
می‌نویسد. با یک `run_id` ثابت:

- `report` بدون `--run-id` کل سشن را نشان می‌دهد، نه فقط آخرین بلوک
- `explorer validate --run-id` قابل استفاده می‌شود
- `PaperOutcome.linked_run_id` دیگر نیازمند join با کاما نیست

**ایمنی دیتابیس:**
- `opportunities` با `INSERT` خالص پر می‌شود (`explorer/store.rs:975`) و opps هر پاس روی
  بلوک‌های مجزا است ⇒ بدون تکرار
- `put_manifest` با `INSERT OR REPLACE` روی کلید اصلی `run_id` است
  (`cache/store/manifests.rs:7`) ⇒ بازنویسی با بازهٔ رشد‌کننده بی‌خطر

---

### فاز ۲ — ادغام ledger در `live`

#### ۲.۱ پرچم‌ها

`cli/src/cli.rs` — افزودن به `LiveArgs`:

| پرگم | نوع | پیش‌فرض | اثر |
|---|---|---|---|
| `--initial-balance` | `u128` (wei) | `config.paper.starting_gas_wei` | `LedgerPolicy.starting_gas_wei` |
| `--initial-balance-usd` | `f64` | — | resolve به wei، نیازمند قیمت |
| `--reserve` | `u128` (wei) | `config.paper.reserve_wei` | `LedgerPolicy.reserve_wei` |
| `--max-fills-per-block` | `usize` | `config.paper.max_fills_per_block` | clamp به `HARD_MAX_FILLS_PER_BLOCK = 32` |
| `--native-usd` | `f64` | fetch | نمایش P&L به USD بدون تماس شبکه |

**قواعد:**
- اولویت با `--initial-balance` است. دادن هر دو ⇒ خطا (ابهام نیست کدام مقدار واقعی است).
- شکست resolve قیمت ⇒ **hard error** با پیامی که `--initial-balance` را پیشنهاد دهد.
  بازگشت خاموش به موجودی متفاوت برای یک عدد مالی گمراه‌کننده است.
- قیمت **یک‌بار** در شروع سشن fetch و نگه داشته می‌شود، نه در هر پاس.

#### ۲.۲ وضعیت `LiveContext`

افزودن به `LiveContext` (`core/src/jobs/live.rs:51`):

- `policy: LedgerPolicy`
- `session_opps: Vec<MevOpportunity>` (انباشته)
- `session_run_id: String` (ثابت)
- `native_usd: Option<f64>`

#### ۲.۳ چرخهٔ ledger

پس از هر پاس (پس از `run_blocks`):

```rust
ctx.session_opps.extend(opps);
let ledger = ctx.policy.apply(&ctx.session_opps);
progress.log(&render_ledger_summary(&ledger, ctx.native_usd));
```

- هزینه `O(n)` در هر پاس. تا ~۱۰⁴ فرصت بی‌اشکال؛ برای سشن‌های طولانی‌تر `apply` را هر
  K پاس اجرا کن.
- `paper_sessions` **یک‌بار در پایان سشن** نوشته می‌شود.

**دلیل یک‌بار نوشتن:** `insert_paper_session` (`paper/store.rs:69`) یک `INSERT` خالص با
`session_id TEXT PRIMARY KEY` است و `paper_fills` کلید `AUTOINCREMENT` دارد ⇒ نوشتن دوبارهٔ
همان `session_id` نقض قید می‌دهد.

#### ۲.۴ نکتهٔ نمایشی

`is_native_eligible` (`ledger.rs:41`) فقط
`TwoHopArb | MultiHopArb | Jit | JitArb | Sandwich` را می‌پذیرد و
liquidation را همیشه رد می‌کند. چون ledger همیشه فعال است، شمارندهٔ `not_native_unit`
همیشه بزرگ‌تر از صفر خواهد بود. در قالب خروجی صریح برچسب بخورد تا هشدار کاذب به نظر نرسد.

با فعال‌شدن `capture_pending` (فاز ۱، آیتم ۱) فرصت‌های `mempool_only` هم ظاهر می‌شوند؛
ledger آن‌ها را می‌پذیرد ولی پس از tx-anchored رتبه‌بندی می‌کند
(`ledger.rs:135-140`، `ledger.rs:361-381`).

#### ۲.۵ آنچه در اجرا اضافه شد (خارج از طرح اولیه)

- **`ResolvedLedger`** — `LiveContext::new` با ۸ آرگومان از سقف clippy عبور نمی‌کرد؛
  `policy` و `native_usd` در یک ساختار جمع شدند.
- **`pricing::usd_to_wei` و `pricing::signed_wei_to_usd`** — تبدیل USD↔wei قبلاً فقط در
  یک جهت (`wei_to_usd`) وجود داشت. `usd_to_wei` به‌جای تبدیل درون‌خطی، ورودی‌های
  غیرقابل‌نمایش (قیمت صفر، NaN، سرریز `u128`) را با `None` رد می‌کند تا یک موجودی
  کوچکِ ساختگی تولید نشود.
- **`paper_session_for_run`** (`paper/store.rs`) — چون `run_id` سشن ثابت شد، `report`
  دیگر به join با کاما نیاز ندارد و می‌تواند P&L را مستقیم با `linked_run_id` پیدا کند.
  `ReportOutcome.ledger_session` اضافه شد و `job_report` آن را چاپ می‌کند.
  این همان «پیشنهاد اختیاری» بخش فاز ۴ است که در فاز ۲ انجام شد.
- **۱۴ تست واحد** در `core/src/jobs/live.rs` (اولویت پرچم‌ها، خطاهای قیمت، انباشت
  ledger، برچسب‌گذاری `not_native_unit`) و ۲ تست CLI (`cli_args.rs`) برای کشف پرچم‌ها و
  رد همزمان دو فرم موجودی.
- **`LiveOpts::persist_session`** — `jobs::paper` هر دو مسیر `job_live` را صدا می‌زد و بعد
  خودش دوباره ledger را apply و ذخیره می‌کرد؛ با فعال‌شدن خودکار ledger در `live`، هر
  سشن `paper` **دو ردیف** در `paper_sessions` می‌نوشت و `report` رکورد اشتباه را برمی‌داشت.
  این پرچم (`true` برای `live`، `false` برای هر دو فراخوانی `paper`) مالکیت ردیف را صریح
  می‌کند و رفتار `paper` را بدون تغییر نگه می‌دارد.
- **رفع بلعیدن خطای one-shot** — `persist_ledger_session(...).ok()` نتیجه را دور می‌ریخت؛
  حالا خطا با `tracing::warn` گزارش می‌شود، چون نتایج و manifest از قبل نوشته شده‌اند و
  از دست رفتن فقط ردیف P&L نباید یک اسکن کامل‌شده را بی‌نتیجه کند.
- **`ResolvedLedger`** جایگزین آرگومان‌های جداگانه در `LiveContext::new` شد (۸ آرگومان از
  سقف clippy `too_many_arguments` عبور نمی‌کرد).

---

### فاز ۳ — حذف `run` از CLI — ✅ انجام شد

| فایل | تغییر |
|---|---|
| `cli/src/commands/run.rs` | حذف کامل |
| `cli/src/cli.rs` | حذف `Run(RunArgs)` و `struct RunArgs` |
| `cli/src/commands/mod.rs` | حذف import `RunArgs`، `mod run`، `pub use run::cmd_run`، `impl CliCommand for RunArgs`، بازوی `Run(a)` در `execute` |
| `cli/src/overrides.rs` | حذف بازوی `Command::Run` |
| `cli/src/main.rs` | حذف `Command::Run(_)` از `matches!` |

**حفظ:** `core/src/jobs/run.rs`، `BlockRangeArgs` (تنها مصرف‌کنندهٔ باقی‌ماندهٔ `discover`)،
`config/mod.rs:11`.

**پیام‌های متنی اصلاح‌شده:**
- `core/src/jobs/report.rs:38` — اکنون `execute 'mev-scout live' first`
- `core/src/jobs/paper.rs:260` — اکنون `run 'mev-scout live' first`
- `core/src/pool/discovery/mod.rs:1442` — کامنت دیگر به `mev-scout run` اشاره نمی‌کند

#### ۳.۱ انحراف از طرح: تست‌های بازه به `discover` منتقل نشدند

طرح گفته بود شش تست اعتبارسنجی بازه از `run` به `discover` منتقل شوند. این کار نشد، چون
`job_discover` **قبل** از حل بازه، `init_rpc` را صدا می‌زند (`jobs/discover.rs:218-223`)؛
یعنی آن تست‌ها که ذاتاً offline هستند به شبکه وابسته می‌شدند و کند/شکننده می‌شدند.

تفکیک انجام‌شده به‌جای آن:
- **سطح clap** (`--days 400`، `--blocks 0`، `--block 0`) → روی `discover` ماند، چون
  `value_parser` پیش از هر کار شبکه‌ای رد می‌کند. یک تست `--blocks 0` هم اضافه شد که
  اصلاً وجود نداشت.
- **سطح runtime** (جفت‌بودن `--from-block`/`--to-block`، ترتیب بازه، تعارض دو فرم، نبود
  بازه، `block = 0` در سطح کتابخانه، و `serde(skip)` بودن فیلدهای بازه در TOML) → به
  ۷ تست واحد در `core/src/config/validation.rs` منتقل شد که روی `RangeSpec::from_flags`
  مستقیم کار می‌کنند و کاملاً hermetic هستند.

#### ۳.۲ انحراف از طرح: `cli_run_replay.rs` حذف نشد، مهاجرت کرد

طرح گفت کل باینری را حذف کن چون پوشش `report` از قبل موجود است. بررسی نشان داد
`cli_live_mode.rs` فقط payload خام JSON را پوشش می‌دهد و هیچ‌جا **شکل رندرشدهٔ**
table و CSV را قفل نمی‌کند (هدر CSV، خطوط `Run ID:`/`Chain:`). حذف باینری پوشش را کم
می‌کرد، پس با `git mv` به `cli_live_report.rs` تغییر نام یافت و `run --blocks 5` آن به
`live` one-shot تبدیل شد. تنها assertion بازه (`end_block - start_block <= 5`) حذف شد،
چون `live` بازه‌ای نمی‌گیرد و در tip کار می‌کند.

`cli_e2e.rs` و `cli_network_coverage.rs` طبق طرح به `live` تغییر کردند.

---

### فاز ۴ — حذف فرمان `paper` — ✅ انجام شد

| فایل | تغییر |
|---|---|
| `cli/src/commands/paper.rs` | حذف کامل |
| `core/src/jobs/paper.rs` | حذف کامل |
| `core/src/jobs/mod.rs` | حذف `mod paper` و exportهای `job_paper_*` / `Paper*Opts` / `PaperOutcome` / `PaperStatsOutcome` |
| `cli/src/cli.rs` | حذف `Paper(PaperArgs)`، `PaperArgs`، `PaperCommand`، `PaperRunArgs`، `PaperLiveArgs`، `PaperSimArgs`، `PaperStatsArgs` |
| `cli/src/commands/mod.rs` | حذف `mod paper`، `pub use paper::cmd_paper`، import `PaperArgs`، `impl CliCommand for PaperArgs`، بازوی `Paper(a)` |
| `cli/src/overrides.rs` | حذف بازوی `Command::Paper` |
| `cli/src/main.rs` | حذف `Command::Paper(_)` از `matches!` |

**حفظ:** کل `core/src/paper/` — `ledger.rs`، `recon.rs`، `types.rs`، `store.rs` را `live`
و `paper_corpus.rs` مصرف می‌کنند. بخش `[paper]` در TOML هم **حفظ شد** چون `live` از
`config.paper.ledger_policy()` برای مقادیر پیش‌فرض ledger استفاده می‌کند.

#### ۴.۱ آنچه طبق پلن انجام نشد

- **`PaperMode::Run` و `::Sim` حذف نشدند.** پلن این را «نظافت اختیاری» می‌دانست؛ نگه‌داشتن
  هزینهٔ صفر دارد و داده‌های قدیمی در DB به‌صورت رشته باقی می‌مانند. `::Sim` فقط در یک
  تست `paper/store.rs` ساخته می‌شود.
- **پیشنهاد اختیاری `report`** در فاز ۲ انجام شده بود (`ReportOutcome.ledger_session` +
  `paper_session_for_run`)، پس اینجا تکراری نبود.

#### ۴.۲ قابلیت‌هایی که با حذف `paper` از بین رفت (بدون جایگزین)

این‌ها طبق پلن حذف شدند، ولی ثبتشان لازم است چون داده‌هایشان هنوز در DB نوشته می‌شود:

- **`paper sim`** — replay آفلاین ledger روی فرصت‌های یک run ذخیره‌شده با
  `--wallet-multiplier`. هیچ معادلی ندارد.
- **`paper stats`** — مرور تاریخچهٔ همهٔ سشن‌ها (`--session` / `--since 1d|7d|30d|all`).
  `report` فقط P&L سشنِ متصل به **یک run مشخص** را نشان می‌دهد، نه تاریخچهٔ بین‌سشنی.

`core::paper::store::{list_paper_sessions, paper_session, paper_fills}` و
`core::pipeline::aggregate_fills` برای همین هنوز `pub` هستند ولی دیگر هیچ فرمان CLI
آن‌ها را صدا نمی‌زند (فقط تست‌ها). اگر بخواهد مرور تاریخچه حفظ شود، کم‌هزینه‌ترین راه
افزودن `--sessions` به `report` است، چون همهٔ plumbing از قبل در `core::paper` هست.

---

### فاز ۵ — تست‌ها و مستندات — ✅ انجام شد

`run` در ۴ فایل تست به‌عنوان «وسیلهٔ تست» استفاده شده بود:

| فایل | وضعیت |
|---|---|
| `cli/tests/cli_args.rs:33` | ✅ `help_lists_kept_commands` به ۶ فرمان به‌روزرسانی شد؛ `run` و `paper` به فهرست حذف‌شده‌ها اضافه شدند |
| `cli/tests/cli_args.rs:68-79` | ✅ `paper_help_lists_subcommands` → `removed_paper_subcommand_fails` |
| `cli/tests/cli_args.rs` (۸ تست بازه) | ✅ طبق جزئیات بخش ۳.۱ تفکیک شد: سطح clap روی `discover`، سطح runtime در تست‌های واحد `core/src/config/validation.rs` |
| `cli/tests/cli_run_replay.rs` | ✅ طبق بخش ۳.۲ به `cli_live_report.rs` مهاجرت کرد (به‌جای حذف، برای حفظ پوشش table/CSV) |
| `cli/tests/cli_e2e.rs:21-101` | ✅ `cli_real_live_smoke`: `live` one-shot سپس `report` |
| `cli/tests/cli_network_coverage.rs:131-147` | ✅ `live_smoke`: `live` سپس `report` |
| `cli/tests/README.md` | ✅ فهرست باینری‌ها به‌روزرسانی شد |

**تست‌های جدید:** ۲ تست CLI برای کشف پرچم‌های ledger و رد همزمان دو فرم موجودی (پیشنهاد
بخش ۵ پلن)؛ ۷ تست واحد `RangeSpec::from_flags`؛ ۱۴ تست واحد `jobs::live`؛ ۳ تست
`pricing`. مجموع تست‌های واحد core از ۲۵۶ به ۲۸۰ رسید.

#### مستندات — ✅
- `README.md`: جدول فرمان‌ها، نمونه‌های CLI، ساختار `cli/`، و یادداشت مهاجرت.
- `docs/ARCHITECTURE.md`: نمودار ماژول‌ها (۲ mermaid)، جدول فرمان‌ها، جدول storeها،
  بخش‌های ۴.۴/۴.۵/۴.۶/۴.۸، بخش Block range، دیاگرام pipeline.
  **ریسک ۵ پلن مستند شد:** `report` حالا یک run برای کل سشن است، نه هر پاس — manifest
  با `INSERT OR REPLACE` بازه را گسترش می‌دهد و همهٔ پاس‌ها به همان run اضافه می‌شوند.
  رکوردهای قدیمیِ per-pass همچنان جداگانه خواندنی‌اند.
- `mev-scout.example.toml`: بخش `[paper]` با توضیح اینکه ledger همیشه فعال است.
- `docs/roadmap_to_100pct.md`: ارجاع‌های `paper stats`/`sim`/`run`/`live` به فرمان‌های
  موجود تغییر کرد.
- کامنت‌های stale در `pipeline/aggregate.rs`، `jobs/report.rs`، `config/settings.rs`،
  `jobs/live.rs` و `tests/paper_corpus.rs`.

#### تصمیم: `report --sessions` ساخته نشد
پیشنهاد `report --sessions` (مرور تاریخچهٔ `paper_sessions`) **رد شد**: مرور تاریخچهٔ
بین‌سشنی نیاز واقعی نبود و صرفاً «راه دیدن دادهٔ از قبل ذخیره‌شده» بود. مسیر مورد نیاز
(گزارش پایان سشن) از قبل با `render_ledger_summary` پوشش داده می‌شود.
`list_paper_sessions`/`paper_session`/`paper_fills`/`aggregate_fills` به‌عنوان API
کتابخانه `pub` باقی ماندند.

#### تأیید نهایی
```powershell
cargo fmt --all -- --check        # ✓
cargo clippy --workspace --all-targets -- -D warnings   # ✓
cargo test --workspace           # ✓ (core 286 واحد، ۲۷ تست CLI، بدون خطا)
```
بخش اختیاریِ شبکه‌ای (۶) هنوز اجرا نشده — نیازمند `MEV_SCOUT_E2E=1` و `RPC_URL` است.

---

### افزودنی پس از فاز ۵ — تفکیک per-strategy در گزارش پایان سشن

خلاصهٔ پایان سشن فقط جمع کل را نشان می‌داد؛ درخواست اصلی کاربر «سهم هر استراتژی»
بود. `render_by_strategy` (`core/src/jobs/live.rs`) اضافه شد که
`aggregate_fills` را صدا می‌زند — همان rollupی که `report` استفاده می‌کند، پس اعداد
نمی‌توانند از آن جدا بیفتند.

تصمیم‌ها:
- **USD فقط وقتی قیمت معلوم است**؛ در غیر این صورت فقط wei، بدون حدس.
- **ردیف‌های skip وارد جدول نمی‌شوند** — چون fill نیستند و سودشان محقق نشده. فقط
  شمارش `not_native_unit` در `note` می‌آید.
- **ترتیب بر اساس `abs(net)` نزولی** تا استراتژی غالب اول بیاید، نه ترتیب HashMap.
- **تست سازگاری** که جمع ردیف‌ها با `net_profit_wei` تیتر برابر باشد.

نکتهٔ کشف‌شده در حین تست: `LedgerPolicy::apply` هر fill با `net <= 0` را به‌عنوان
`NonPositiveNet` رد می‌کند، پس **همهٔ ردیف‌های جدول ذاتاً مثبت‌اند**. فرمت علامت‌دار
به‌عنوان محافظ باقی ماند ولی مسیر منفی در عمل غیرقابل‌دستیابی است.

---

## ۶) تأیید (Verification)

```powershell
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

```powershell
# اختیاری — نیازمند شبکه
$env:MEV_SCOUT_E2E = "1"
$env:RPC_URL = "…"
$env:RUST_MIN_STACK = "134217728"

cargo test --release -p mev-scout-core --test mev_corpus
cargo test --release -p mev-scout-core --test paper_corpus

cargo test -p mev-scout-cli --test cli_live_mode    -- --test-threads=1
cargo test -p mev-scout-cli --test cli_e2e          -- --test-threads=1
cargo test -p mev-scout-cli --test cli_network_coverage -- --test-threads=1
```

---

## ۷) ریسک‌ها

1. **`--initial-balance-usd` روی زنجیرهٔ biter کار نخواهد کرد** — `native_asset_id` و
   `llama_chain_prefix` بازوی این زنجیره را ندارند. باید صریح و loud باشد، نه خاموش.
2. **اگر live نتواند به tip بچسبد** — هر پاس چند بلاک می‌شود و گلوگاه `run_blocks`
   (replay ذاتاً ترتیبی) باقی می‌ماند، چون عمداً `block_concurrency` اضافه نشده. باید روی
   زنجیرهٔ واقعی اندازه‌گیری شود؛ اگر لازم شد بعداً به‌صورت یک خط اضافه گردد.
3. **فعال‌شدن `capture_pending`** در live یعنی mempool capture در هر پاس ⇒ RPC بیشتر.
   باید بررسی شود که روی زنجیره‌ای که mempool ندارد هزینهٔ بیهوده ایجاد نکند.
4. **افت پوشش تست CLI** — با حذف `cli_run_replay.rs` مسیر `report` از مسیر واقعی شبکه
   کمتر تست می‌شود. جبران: تغییر `cli_e2e` و `cli_network_coverage` به `live`.
5. **تغییر معنای `report`** — قبلاً هر پاس یک run جدا بود، الان کل سشن یک run است. باید در
   `docs/ARCHITECTURE.md` مستند شود.
6. **کار مستقل و موازی:** اضافه‌کردن زنجیرهٔ biter به `ChainName` (`core/src/types/chain.rs:46`)
   + `core/data/chains.toml` + بازوی `native_asset_id`/`llama_chain_prefix` + آدرس
   factoryها / wrapped-native. بدون این، `--initial-balance-usd` و `discover` هم کار نمی‌کنند.

---

## ۸) ترتیب اجرا

```
فاز ۱  ──▶  فاز ۲  ──▶  [تأیید روی زنجیرهٔ واقعی]  ──▶  فاز ۳  ──▶  فاز ۴  ──▶  فاز ۵
تنظیم‌ها     ledger
```

فاز ۱ و ۲ باید پیش از هر حذفی کامل و روی زنجیرهٔ واقعی تأیید شوند.

کار مستقل: افزودن زنجیرهٔ biter (بند ۷.۶) می‌تواند موازی انجام شود.

---

## ۹) وضعیت نهایی

همهٔ فازها اجرا شده‌اند. سطح CLI از ۸ فرمان به ۶ رسید:

```
report · config · discover · tokens · live · explorer
```

آنچه `live` حالا به‌تنهایی انجام می‌دهد: تشخیص در tip، ذخیرهٔ فرصت‌ها، و گزارش P&L
ledger در پایان سشن. `report` همان سشن را آفلاین بازخوانی می‌کند.

**آنچه هنوز انجام نشده:** بخش ۶ اختیاریِ شبکه‌ای (E2E و corpus با RPC واقعی).

---

## پیوست الف — نقشهٔ وابستگی‌ها

### آنچه `job_run` را نگه می‌دارد (باید بماند)
- `core/tests/mev_corpus.rs:586,691` — corpus دتکتورها
- `core/tests/paper_corpus.rs:274` — corpus paper↔executed
- `core/tests/replay.rs:330` — `BacktestRunner::run_range` (غیر hybrid)
- `core/src/jobs/mod.rs:32` — export

### آنچه `core/src/paper/` را نگه می‌دارد (باید بماند)
- `core/tests/paper_corpus.rs:57` — `LedgerPolicy`, `paper_vs_executed`, `LedgerResult`,
  `PaperFill`, `ReconReport`
- `live` (فاز ۲) — `LedgerPolicy`, `PaperMode::Live`, `insert_paper_session`
- `paper/store.rs:240-272` — تست‌های خودش

### مصرف‌کنندگان `BlockRangeArgs` پس از حذف
- فقط `discover` (`cli/src/cli.rs:247`)

### مصرف‌کنندگان `range_mode` / manifest پس از تغییر
- `report` (`core/src/jobs/report.rs:20`) — فقط خواندن از SQLite
- `explorer validate` (`core/src/jobs/explorer_validate.rs:55`) — فقط SQL روی
  `ExplorerStore`؛ با `run_id` از live کار می‌کند
- `discover` (`core/src/jobs/discover.rs:422`) — `config.range_spec()` با fallback

---

## پیوست ب — مصرف‌کنندگان فعلی `ExplorerStore` مربوط به paper

| متد | محل | سرنوشت |
|---|---|---|
| `ensure_paper_tables` | `paper/store.rs:9` | می‌ماند (از `initialize` صدا زده می‌شود) |
| `insert_paper_session` | `paper/store.rs:57` | می‌ماند — live در پایان سشن صدا می‌زند |
| `insert_paper_fill` | `paper/store.rs:98` | می‌ماند (private) |
| `paper_session` | `paper/store.rs:130` | اختیاری — برای خلاصهٔ `report` |
| `paper_fills` | `paper/store.rs:173` | اختیاری |
| `list_paper_sessions` | `paper/store.rs:147` | اختیاری |
| DDL جداول | `explorer/store.rs:437-474` | نگه داشته شود (بی‌خطر و دادهٔ موجود را حفظ می‌کند) |
