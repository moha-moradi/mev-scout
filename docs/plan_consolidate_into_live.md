# طرح: یکپارچه‌سازی `run` و `paper` در `live`

> **وضعیت:** طرح — تأییدنشده
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
| `batch_rpc` | دارد | **اعمال نمی‌شود** |
| `block_concurrency` | `effective_block_concurrency` | **پیش‌فرض ۱** ⇒ کند |
| `auto_refetch_gaps` | دارد | **ندارد** |
| `print_startup_plan` | دارد | ندارد |
| `render_block_summary_table` | دارد | ندارد؛ در حلقه دور ریخته می‌شود |
| `run_id` | یکی برای کل اجرا | **جدید برای هر پاس** ⇒ ۲۴۰۰ manifest در ۲ ساعت |
| `total_txs_scanned` | درست | **باگ `+= 0`** |
| retry | ندارد | `MAX_CONSECUTIVE_FAILURES = 5` |
| one-shot tip | از flag | از `ctx.tip` قدیمی |

---

## ۵) فازهای اجرا

### فاز ۱ — انتقال قابلیت‌های `run` به `live`

#### ۱.۱ تنظیم runner

`core/src/jobs/live.rs`

| # | تغییر | محل |
|---|---|---|
| 1 | `with_capture_pending(config.backtest.capture_pending)` | بعد از `live.rs:126` |
| 2 | `with_max_candidates_per_tx(config.backtest.max_candidates_per_tx)` | همان‌جا |
| 3 | `with_batch_rpc(config.backtest.batch_rpc)` | `fetch_blocks`، `live.rs:152` |
| 4 | `with_block_concurrency(config.effective_block_concurrency(...))` | `live.rs:153` |
| 5 | `auto_refetch_gaps` روی `missing_after_fetch` (الان با `.map(\|_\| ())` دور ریخته می‌شود) | `live.rs:158,160` |

#### ۱.۲ درستی

| # | تغییر | محل |
|---|---|---|
| 6 | one-shot دوباره tip را بخواند به‌جای مصرف `ctx.tip` قدیمی | `run_once`، `live.rs:271` |
| 7 | رفع باگ `total_txs_scanned += 0` | `live.rs:510` |
| 8 | انباشت `block_stats` در حلقه و بازگرداندن در `LiveLoopOutcome` | `live.rs:461` |
| 9 | refactor `validate_live` به `validate_common()` مشترک | `config/validation.rs:414` |

**جزئیات ۹:** `validate_live` یک کپی تقریباً-یکسان از `validate_and_resolve_for` است که
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

---

### فاز ۳ — حذف `run` از CLI

| فایل | تغییر |
|---|---|
| `cli/src/commands/run.rs` | حذف کامل |
| `cli/src/cli.rs` | حذف `Run(RunArgs)` (`:28`) و `struct RunArgs` (`:232-235`) |
| `cli/src/commands/mod.rs` | حذف import `RunArgs` (`:2`)، `pub use run::cmd_run` (`:25`)، `impl CliCommand for RunArgs` (`:38-41`) |
| `cli/src/overrides.rs` | حذف بازوی `Command::Run` (`:15-17`) |
| `cli/src/main.rs` | حذف `Command::Run(_)` از `matches!` خط ۲۸ |

**حفظ:** `core/src/jobs/run.rs`، `BlockRangeArgs` (تنها مصرف‌کنندهٔ باقی‌مانده `discover`)،
`config/mod.rs:11`.

**پیام‌های متنی نیازمند اصلاح:**
- `core/src/jobs/report.rs:34` — `"execute 'mev-scout run' first"`
- (در صورت حذف `job_paper_sim`، `core/src/jobs/paper.rs:252` هم حذف می‌شود)

---

### فاز ۴ — حذف فرمان `paper`

| فایل | تغییر |
|---|---|
| `cli/src/commands/paper.rs` | حذف کامل |
| `core/src/jobs/paper.rs` | حذف کامل (۳۱۳ خط) |
| `core/src/jobs/mod.rs` | حذف exportهای `paper` (`:27-28`) |
| `cli/src/cli.rs` | حذف `Paper(PaperArgs)` (`:57`)، `PaperArgs` (`:305-308`)، `PaperCommand` (`:311-320`)، `PaperRunArgs` (`:323-326`)، `PaperLiveArgs` (`:329-341`)، `PaperSimArgs` (`:344-350`)، `PaperStatsArgs` (`:353-362`) |
| `cli/src/commands/mod.rs` | حذف import `PaperArgs` (`:2`) + `impl CliCommand for PaperArgs` (`:100`) |
| `cli/src/overrides.rs` | حذف بازوی `Command::Paper` (`:26-31`) |
| `cli/src/main.rs` | حذف `Command::Paper(_)` از خط ۲۸ |

**حفظ:** کل `core/src/paper/` — `ledger.rs` و `recon.rs` و `types.rs` را live و
`paper_corpus.rs` مصرف می‌کنند؛ `store.rs` را live برای نوشتن سشن به کار می‌برد.

**نظافت اختیاری:** `PaperMode::Run` و `::Sim` دیگر ساخته نمی‌شوند. ان enum را می‌توان نگه
داشت (هزینهٔ صفر؛ داده‌های قدیمی در DB به‌صورت رشته باقی می‌مانند و `mod.rs:16` آن را
re-export می‌کند) یا حذفشان کرد که فقط یک تست را در `paper/store.rs:264` دست می‌زند.

**پیشنهاد اختیاری:** `report` یک بخش خلاصهٔ ledger نشان دهد وقتی برای آن `run_id` سشن
`paper_sessions` وجود دارد — وگرنه داده ذخیره می‌شود ولی هیچ‌کس نمی‌خواندش.

---

### فاز ۵ — تست‌ها و مستندات

`run` در ۴ فایل تست به‌عنوان «وسیلهٔ تست» استفاده شده است:

| فایل | مشکل | راه‌حل |
|---|---|---|
| `cli/tests/cli_args.rs:33` | `help_lists_kept_commands` فهرست دقیق ۸ فرمان را assert می‌کند | به‌روزرسانی به ۶ فرمان |
| `cli/tests/cli_args.rs:68-79` | `paper_help_lists_subcommands` | حذف |
| `cli/tests/cli_args.rs` `:95,123,134,223,235,247,259,292` | ۸ تست اعتبارسنجی بازه از `run` استفاده می‌کنند | انتقال به `discover` (همان `BlockRangeArgs` را دارد و محدودیت‌های clap یکسان است). فقط `:95` و `:280` معنای متفاوت دارند چون `discover` به‌جای خطا fallback دارد ⇒ نیازمند بازنویسی |
| `cli/tests/cli_run_replay.rs` | کل باینری (۱ تست `run_report_chain`) | حذف باینری. پوشش `report` از قبل موجود است: `cli_args.rs:524` و `cli_live_mode.rs:49,113` |
| `cli/tests/cli_e2e.rs:21-101` | `cli_real_run_smoke`: `run` سپس `report` | تغییر به `live` one-shot سپس `report` |
| `cli/tests/cli_network_coverage.rs:131-147` | `run_smoke`: `run --blocks 2` سپس `report` | تغییر به `live` |
| `cli/tests/README.md` | فهرست باینری‌ها شامل `cli_run_replay` | به‌روزرسانی |

**تست جدید پیشنهادی:** چون ledger حالا در `live` است، یک تست CLI برای `--initial-balance`
و برای اولویت آن بر `--initial-balance-usd` اضافه شود. منطق خالص ledger از قبل ۵ تست
واحد دارد (`ledger.rs:285-409`).

**بررسی پس از ۱.۳:** `cli_live_mode.rs:117` مقدار `range_mode == "live"` را assert
می‌کند — با `run_id` واحد سالم می‌ماند.

#### مستندات
- `README.md:60` (`mev-scout run --blocks 100`)
- `README.md:70` (`mev-scout paper run --blocks 100`)
- `docs/ARCHITECTURE.md`: خطوط ۱۳۱، ۱۶۴-۱۶۷، ۱۹۰، ۳۱۱-۳۱۴، ۵۳۵، ۵۵۰، ۶۵۶
- بررسی `mev-scout.example.toml` برای ارجاع به `run`/`paper` در کامنت‌ها

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
2. **افزایش چند برابری سرعت live** با فعال‌شدن `block_concurrency` — ممکن است روی RPC
   عمومی باعث rate-limit شود. `config.rpc.rps_limit` محدودکننده است ولی باید روی زنجیرهٔ
   واقعی تنظیم و تست شود.
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
