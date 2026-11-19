# پلن مینیمال‌سازی `explorer` (اجرایی برای مدل کم‌قدرت)

این سند پلن دقیق تغییرات CLI ماژول `explorer` است با هدف:
- اجرای **ایندکس/بک‌فیل برای یک بازه مشخص** (مثلاً ۱ روز / ۱ هفته / ۱ ماه) و **تولید خودکار داشبورد HTML در پایان**.
- حذف گزینه‌های غیرضروری و ساده‌سازی UX.
- نگه داشتن **فقط یک فلگ مربوط به خروجی فایل**: نام/مسیر فایل داشبورد (اختیاری؛ در غیر این صورت نام خودکار بر اساس بازه).

> وضعیت مبنا (کد فعلی): `cli/src/cli.rs`, `cli/src/commands/{mod.rs,explorer.rs,explorer/{backfill.rs,dashboard.rs,report.rs}}`, تست‌های `cli/tests/cli_args.rs`, `cli/tests/cli_dashboard.rs`, مستندات `README.md`, `docs/CLI.md`.

---

## 1) تصمیم‌های قطعی (طبق درخواست)

| مورد | تصمیم |
|---|---|
| `--confidence <LEVEL>` | **حذف کامل** (CLI، منطق، تست، داک). بدون فیلتر confidence. |
| `--format <FMT>` | **حذف کامل**. خروجی داشبورد فقط **HTML** (بدون `json`/`csv`). |
| `--repeat <DURATION>` | **حذف کامل**. اجرای یک‌باره و خروج. |
| `--dashboard` | **حذف فلگ** و تبدیل به رفتار **پیش‌فرض ON** (هم در bare explorer، هم بعد از `index`). |
| `--db <PATH>` | **حذف کامل**. منبع DB همیشه مسیر مؤثر از کانفیگ زنجیره فعال: `config.effective_explorer_db_path(&chain/config.chain)` (همان منطق فعلی `resolve_db` وقتی `--db` نیست). |
| خروجی فایل | **تنها فلگ خروجی:** `--out <PATH>` (اختیاری). اگر داده نشود، نام فایل **خودکار بر اساس بازه/نوع اجرا** ساخته می‌شود. |

### تصمیم‌های تکمیلی لازم برای سادگی/انسجام (پیشنهادیِ این پلن)
این‌ها برای رسیدن به «فقط نام فایل + آرگومان‌های بازه index» لازم/مفیدند و در این پلن اعمال می‌شوند:

1. **حذف `--kind`** (فیلتر گزارش متنی bare): چون حالت گزارش متنی حذف می‌شود، `--kind` هدفی ندارد.
2. **حذف حالت گزارش متنی bare (`explorer` بدون زیرفرمان)** و جایگزینی با تولید داشبورد all-time HTML. (مسیر فعلی: `cmd_explorer_report` + خروجی table/json/csv در stdout)
3. **`explorer index` همیشه در پایان داشبورد HTML می‌سازد** (حتی بدون فلگ قدیمی `--dashboard` روی index).
4. **پنجره داشبورد با بازه اجرا همخوان شود** (نه همیشه `all`): برای index از بازه واقعی بلاک‌های ایندکس‌شده (`from_block..to_block` برگشتی بک‌فیل) و برای bare از `all`.
5. **نمایش داشبورد در پایان:** پس از نوشتن فایل، مسیر خروجی چاپ شود و (best-effort) در مرورگر پیش‌فرض باز شود (ویندوز: `cmd /C start "" <file>`). در صورت خطای باز کردن، فقط هشدار لاگ/چاپ و خروج موفق حفظ شود.

> اگر هر کدام از موارد 1-5 مورد نظر نیست، قبل از پیاده‌سازی حذف/تغییرش دهید؛ اما پلن زیر با همین مفروضات نوشته شده است.

---

## 2) قرارداد CLI نهایی

### 2.1 `mev-scout explorer [--out <PATH>]`
- **بدون زیرفرمان**: داشبورد HTML **all-time** از DB اکسپلورر زنجیره فعال تولید می‌کند و خروج.
- `--out` اختیاری است (روی `ExplorerArgs` تعریف شود تا هم به شکل `explorer --out ...` و هم در ترکیب‌های رایج قابل قبول باشد).
- اگر `--out` داده نشود: نام خودکار (بخش 3).
- هیچ فلگ دیگری ندارد (`--dashboard/--db/--format/--repeat/--kind/--confidence` همگی حذف/نامعتبر).

### 2.2 `mev-scout explorer index [--window <WINDOW>] [--from-block N --to-block N] [--out <PATH>]`
- رفتار بک‌فیل/ایندکس همان منطق فعلی `cmd_backfill`:
  - پیش‌فرض بدون آرگومان بازه: **`7d`** trailing تا head امن.
  - `--window <Nh|Nd|Nw>` (معتبر، حداکثر 365 روز) ناسازگار با `--from-block/--to-block`.
  - `--from-block` و `--to-block` (شامل) باید با هم باشند و ناسازگار با `--window`.
- پس از **موفقیت** بک‌فیل: **همیشه** داشبورد HTML برای همان بازه ایندکس‌شده تولید می‌شود.
- `--out` اختیاری؛ در صورت نبود نام خودکار بر اساس بازه.
- فلگ `--dashboard`ِ قدیمیِ index حذف می‌شود.

### 2.3 Help/Errors مورد انتظار
- `explorer --help` فقط `index` و فلگ‌های `--out` (و آرگومان‌های index در help زیرفرمان) را نشان دهد.
- استفاده از فلگ‌های حذف‌شده ⇒ خطای clap (unknown/conflict) — نیازی به هندل دستی خاص نیست مگر پیام سفارشی مطلوب باشد.

---

## 3) نام‌گذاری خودکار فایل خروجی (`--out` omitted)

- مسیر پیش‌فرض: دایرکتوری `results/` (مثل فعلی). اگر وجود ندارد ساخته شود.
- نام فایل بر اساس **زنجیره فعال** (`config.chain` به صورت رشته/label فایل‌سیف، مثلاً `avalanche`, `bsc`) و **بازه اجرا**:

| حالت اجرا | ورودی بازه | نام فایل پیشنهادی |
|---|---|---|
| `explorer` (بدون زیرفرمان) | all-time | `results/explorer-<chain>-all.html` |
| `explorer index` (بدون آرگومان بازه) | پیش‌فرض `7d` | `results/explorer-<chain>-7d.html` |
| `explorer index --window <W>` | `<W>` | `results/explorer-<chain>-<w>.html` |
| `explorer index --from-block A --to-block B` | بلاک A..B | `results/explorer-<chain>-blocks-A-B.html` |

جزئیات پیاده‌سازی نام‌گذاری:
- `<chain>`: از `config.chain` (همان مقداری که در مسیر DB استفاده می‌شود؛ در نام فایل کاراکترهای غیرمجاز مسیر را با `-` جایگزین یا حذف کنید).
- `<w>`: مقدار window نرمال‌شده برای نام فایل (مثلاً trim و lowercase همان ورودی `24h/7d/2w`; کاراکترهای غیر `[a-zA-Z0-9._-]` حذف/جایگزین).
- در صورت وجود فایل: **overwrite** با نوشتن atomic (رفتار ساده و سازگار با هدف «اجرای یک‌باره»).
- اگر `--out` داده شد: همان مسیر استفاده شود (relative به cwd یا absolute)، بدون تغییر نام خودکار؛ دایرکتوری والد در صورت نیاز ساخته شود.

تابع کمکی پیشنهادی (مثلاً در `cli/src/commands/explorer.rs` یا ماژول کوچک داخلی):
```rust
fn default_dashboard_out(chain: &str, range_label: &str) -> PathBuf { /* results/explorer-<chain>-<label>.html */ }
```

---

## 4) همخوانی Window داشبورد با بازه

`core::explorer::dashboard::build` با `BuildOptions.window: WindowSpec` کار می‌کند. فعلاً CLI همیشه `WindowSpec::all()` می‌دهد — باید تغییر کند.

| حالت | `window label` | `since_ts/until_ts` |
|---|---|---|
| bare `explorer` | `all` | `WindowSpec::all()` |
| `index` با window (شامل پیش‌فرض 7d) | همان window (مثلاً `7d`) | **ترجیحاً بازه واقعی بلاک‌های ایندکس‌شده** `[from_block,to_block]` (پایین)؛ fallback: `WindowSpec::trailing(label, secs)` |
| `index` با بلاک | `blocks-<from>-<to>` | **بازه واقعی بلاک‌های ایندکس‌شده** `[from_block,to_block]` |

### محاسبه بازه واقعی برای index (پیشنهاد قطعی)
`cmd_backfill` الان `BackfillOutcome { from_block, to_block, ... }` را برمی‌گرداند و caller مقادیر ورودی (`window`/blocks) را دارد. پس بعد از بک‌فیل:
1. `range_label` را تعیین کن:
   - اگر آرگومان بلاکی بوده: `format!("blocks-{}-{}", outcome.from_block, outcome.to_block)`
   - اگر window-mode بوده: `args.window.clone().unwrap_or_else(|| "7d".to_string())` (همان پیش‌فرض)
2. `db_path = config.effective_explorer_db_path(&chain)` را باز کن **read-only** (مثلاً `ExplorerStore::open_read_only` یا `rusqlite::Connection::open_with_flags` read-only) و min/max ts بلاک‌های همان بازه را بگیر:
   ```sql
   SELECT MIN(ts), MAX(ts) FROM blocks
   WHERE block_number >= ?1 AND block_number <= ?2
   ```
3. ساخت `WindowSpec`:
   - اگر هر دو مقدار موجود بودند: `WindowSpec { label: range_label, since_ts: min_ts, until_ts: max_ts }`
   - اگر ردیفی نبود (بازه خالی/بدون بلاک ثبت‌شده): `WindowSpec::all()` با همان label یا fallback امن؛ مهم این است که `build` کرش نکند (می‌توان label را نگه داشت و since/until را `0..now+3600` گذاشت یا از `all()` استفاده کرد — در پلن: **fallback = `WindowSpec::all()` ولی `label` را اگر ممکن است `range_label` بگذار**).

> نکته: برای trailing می‌توانستیم فقط `WindowSpec::trailing` استفاده کنیم، اما استفاده از ts واقعی بلاک‌های `[from,to]` دقیق‌تر و همخوان با داده ایندکس‌شده است؛ این روش برای هر دو حالت index یکسان می‌شود.

---

## 5) تغییرات فایل‌به‌فایل (گام‌های پیاده‌سازی)

### گام A — `cli/src/cli.rs` (تعریف CLI)
1. **`ExplorerArgs`** را ساده کن:
   - نگه دار: `pub command: Option<ExplorerCommand>` (subcommand optional).
   - اضافه کن: `#[arg(long, value_name = "PATH", global = true)] pub out: Option<PathBuf>` (یا بدون `global` اما طوری که برای `explorer` و `explorer index` هر دو کار کند؛ تست کن. ساده‌ترین: `out` روی `ExplorerArgs` و همچنین در صورت نیاز propagation در dispatch).
   - **حذف کن:** فیلد `dashboard: DashboardArgs` (flatten)، `kind`, `confidence`.
   - **حذف کن متدها/منطق مرتبط:** `bare_flags_set`, `resolved_confidence` (اگر دیگر استفاده‌ای ندارند).
2. **`DashboardArgs` (CLI struct)** و هر چیزی که فقط برای `--dashboard/--db/--format/--repeat` بوده حذف کن (یا به حداقل برسان). اگر جایی لازم شد ساختار داخلی نگه دار، ولی از سطح CLI حذف شود.
3. **`IndexArgs`**:
   - نگه دار: `window`, `from_block`, `to_block` با همان constraints فعلی.
   - **حذف کن:** `pub dashboard: bool`.
4. مستندات `///` روی command/args را مطابق رفتار جدید به‌روز کن (مثلاً “bare explorer emits all-time HTML dashboard”, “index backfills range then emits HTML dashboard”).

### گام B — `cli/src/commands/explorer/dashboard.rs` (تولید HTML فقط، بدون repeat/format/db arg)
1. ساختار آرگومان داخلی را ساده کن (مثلاً):
   ```rust
   pub struct DashboardEmitArgs {
       pub db_path: PathBuf,      // همیشه از config
       pub out: PathBuf,          // resolve‌شده (خودکار یا --out)
       pub window: WindowSpec,    // all یا بازه index
   }
   ```
   یا امضای `cmd_dashboard(config, &DashboardEmitArgs)` / `emit_once` متناسب.
2. `emit_once`:
   - دیگر `resolve_db(config, db: Option<&Path>)` با ورودی کاربر را صدا نزن؛ `db_path` را از کانفیگ/آرگ ورودی بگیر.
   - `BuildOptions` را با `window` ورودی بساز (نه `WindowSpec::all()` ثابت) و مقادیر ثابت فعلی را حفظ کن: `max_ops: 2000`, `max_evidence: 500`, `budget_mb: 8`, `with_gaps: false`, `gap_block_limit: 10_000`.
   - فقط `render_html(&snap)?` را تولید و با `atomic_write` بنویس.
   - **حذف** شاخه‌های `json/csv` و match روی format.
3. `cmd_dashboard`:
   - **حذف** حلقه `--repeat`/`tokio::time::sleep` و منطق `repeat: Option<Duration>`.
   - یک‌بار emit کن، مسیر خروجی را لاگ/چاپ کن.
   - **نمایش فایل** (best-effort): اگر `cfg!(windows)` بود `Command::new("cmd").args(["/C", "start", "", &out_path_string]).spawn()` (یا status)؛ در غیر ویندوز می‌توان `xdg-open`/`open` را امتحان کرد یا فقط چاپ مسیر. شکست در باز کردن نباید باعث `Err` شود مگر تصمیم دیگر؛ پیشنهاد: ignore error + پیام.
4. `atomic_write` را نگه دار (مفید و ساده).

### گام C — `cli/src/commands/explorer.rs` (resolve DB/out، wiring)
1. `resolve_db`:
   - امضای جدید بدون آرگومان `db`: `fn resolved_db_path(config: &Config) -> PathBuf { PathBuf::from(config.effective_explorer_db_path(&config.chain)) }` (یا معادل).
   - تمام استفاده‌ها را آپدیت کن.
2. `run_dashboard` (یا تابع جایگزین مثل `emit_dashboard`) را برای API جدید بازنویسی کن:
   - ورودی‌ها: `config`, `out: Option<PathBuf>` (از CLI)، و context بازه (`WindowSpec` یا اطلاعات لازم برای ساختش + `range_label` برای نام فایل).
   - مسیر خروجی: اگر `out` هست همان، وگرنه `default_dashboard_out(chain_label, range_label)`.
   - فراخوانی `cmd_dashboard`/`emit_once` با `db_path` از کانفیگ.
3. `explorer_store(config, chain)` همچنان برای report قدیمی بود؛ اگر report حذف شد و استفاده دیگری ندارد، می‌توان نگه داشت یا حذف — بررسی مصرف.
4. تست واحد `resolve_db_uses_active_chain_and_explicit_override` باید آپدیت/حذف شود (explicit override دیگر وجود ندارد). تست جدید: مسیر از زنجیره فعال گرفته می‌شود.

### گام D — `cli/src/commands/explorer/backfill.rs`
- منطق بک‌فیل را حفظ کن. فقط در صورت نیاز کوچک:
  - `BackfillOutcome` از قبل `from_block/to_block` دارد — کافی است.
  - (اختیاری) اگر خواستی تمیزتر باشد، می‌توان helper برای parse duration secs عمومی/قابل‌استفاده برای label ساخت؛ اما الزامی نیست.
- اطمینان حاصل کن پیش‌فرض همچنان `7d` است.

### گام E — `cli/src/commands/mod.rs` (dispatch)
1. importها را تمیز کن: حذف `run_dashboard` قدیمی اگر امضا عوض شد، حذف `cmd_explorer_report` اگر report حذف می‌شود.
2. `impl CliCommand for ExplorerArgs`:
   - دیگر چک `bare_flags_set`/خطای “belong on bare explorer” به شکل فعلی لازم نیست (آن فلگ‌ها حذف شده‌اند).
   - `match &self.command`:
     - `None` => داشبورد all-time:
       - `range_label = "all"`، `window = WindowSpec::all()`، `out = self.out.clone()`
       - emit dashboard.
     - `Some(ExplorerCommand::Index(a))` => 
       - `outcome = cmd_backfill(config, a.window.as_deref(), a.from_block, a.to_block, progress).await?;`
       - `range_label` را از آرگومان‌ها تعیین کن: block-range => `blocks-{from}-{to}` (از outcome)، else `a.window.unwrap_or("7d")`.
       - `window` را از ts بلاک‌های `[outcome.from_block, outcome.to_block]` در DB بساز (گام 4).
       - emit dashboard با `out = self.out.clone()` (یا اگر خواستی index هم out جدا داشته باشد، فعلاً یک `--out` کلی کافی است).
   - دیگر `if a.dashboard { run_dashboard(default) }` وجود ندارد.
3. در صورت حذف report، exportهای `cmd_explorer_report` را از `commands/mod.rs` و `commands/explorer.rs` حذف کن.

### گام F — `cli/src/commands/explorer/report.rs` و ارجاعات آن
- اگر گزارش متنی bare حذف می‌شود: **فایل را حذف کن**، `mod report;` و `pub use report::cmd_explorer_report;` را از `explorer.rs` و `commands/mod.rs` بردار، و وابستگی‌های بلااستفاده (مثلاً استفاده‌های خاص) را تمیز کن.
- اگر نگه داشتنش در کد (بدون CLI) مدنظر است، حداقل از CLI/exports عمومی CLI جدا شود؛ ولی برای مینیمالیسم حذف کامل CLI-path ترجیح دارد. (هسته `store` report queries در core می‌ماند و آسیبی نمی‌بیند.)

### گام G — `cli/src/main.rs`
- `make_sink`: هنوز برای `Command::Explorer(a)` که `Index` است progress bar می‌خواهیم — `ExplorerCommand::Index(_)` بدون تغییر نام باقی می‌ماند، پس منطق فعلی درست است. فقط مطمئن شو match بعد از تغییرات cli compile می‌شود.

### گام H — جستجوی ارجاعات باقی‌مانده
قبل از اتمام، در workspace سرچ کن و پاکسازی کن:
- `--confidence`, `--format`, `--repeat`, `--kind` (در context explorer CLI)، `--dashboard` (به عنوان فلگ)، `--db` (در context explorer CLI dashboard).
- `cmd_explorer_report`, `DashboardArgs` قدیمی، `resolved_confidence`, `bare_flags_set`, `run_dashboard` با امضای قدیم.
- `WindowSpec::all()` در `cli/src/commands/explorer/dashboard.rs` نباید ثابت بماند (باید پارامتریک شود).

دستورهای مفید (PowerShell):
```powershell
Select-String -Path "cli\**\*.rs","docs\**\*","README.md" -Pattern "--confidence|--format \"html\"|--repeat|explorer --dashboard|--db" -SimpleMatch
```
(الگوها را دقیق‌تر بزن تا false positive کم شود.)

---

## 6) تغییرات تست‌ها

### 6.1 `cli/tests/cli_args.rs`
بخش‌های explorer باید بازنویسی شوند (تعداد زیادی تست مستقیماً فلگ‌های حذف‌شده را چک می‌کنند):
- **حذف/بازنویسی:** `explorer_help_lists_subcommands` (لیست فلگ‌های retained/removed را مطابق حالت جدید کن: `--out` باید باشد؛ `--dashboard/--db/--format/--repeat/--kind/--confidence` نباید باشند؛ subcommand فقط `index`).
- **حذف/بازنویسی:** `bare_explorer_is_all_time_report` → رفتار جدید: parse بدون خطا، بدون subcommand، دارای `out: Option`؛ (assertهای مربوط به dashboard flag/confidence/kind حذف).
- **حذف:** `explorer_confidence_flag_resolves_to_documented_values` (فلگ حذف شده).
- **آپدیت:** `explorer_index_defaults_to_backfill_shape` و تست‌های window/blocks/index: فیلد `dashboard` در `IndexArgs` دیگر نیست؛ assertions مربوط به آن حذف. `--out` در صورت نیاز تست شود.
- **حذف:** `explorer_index_rejects_bare_mode_flags` (سناریوهای `--kind index`, `--dashboard index` دیگر معنا ندارند؛ می‌توان تست جدید گذاشت که این فلگ‌ها unknown argument هستند — اختیاری ولی خوب).
- **حذف/بازنویسی:** `explorer_dashboard_flag_requirements` و هر تستی که `--db/--format/--repeat/--dashboard/--kind/--confidence` را روی explorer می‌سازد.
- **آپدیت:** جدول `removed_and_unknown_surface_fails` و کمک‌ها: `explorer dashboard` زیرفرمان همچنان unknown است (خوبه)، ولی مواردی مثل انتظار برای فلگ‌های قدیمی باید اصلاح شود.
- **اضافه (پیشنهادی):**
  - `explorer --out custom.html` parse می‌شود.
  - `explorer index --window 24h --out x.html` parse می‌شود.
  - فلگ‌های حذف‌شده (`--db`, `--format`, `--repeat`, `--confidence`, `--kind`, `--dashboard`) روی explorer/index خطای clap می‌دهند.

> نکته: تست‌های parse با `Cli::parse_from` به structهای جدید وابسته‌اند؛ بعد از تغییر `cli.rs` باید کامپایل شوند.

### 6.2 `cli/tests/cli_dashboard.rs`
این تست‌ها الان `explorer --dashboard --db ... --out ... --format ...` را اجرا می‌کنند و خروجی json/csv/html را بررسی می‌کنند. با حذف `--db/--format/--dashboard` باید بازطراحی شوند:
- چون `--db` حذف شده، CLI مسیر DB را از کانفیگ می‌خواند. برای تست offline باید یک **کانفیگ موقت** بسازید که `chain` و مسیر explorer DB (و حداقل تنظیمات لازم) را به DB تست‌شده اشاره کند، سپس CLI را با `-f <temp-config> explorer --out <out.html>` اجرا کنید. (الگوی ساخت کانفیگ/DB موقت را می‌توان از `cli_args.rs` یا helpers موجود الهام گرفت؛ `seed_db` فعلی ساختار DB legacy را می‌سازد که `build` ساپورت می‌کند.)
- تست HTML: بررسی وجود خروجی، HTML معتبر/دارای markerهای اصلی (بدون وابستگی زیاد به جزئیات داخلی)، و **عدم تغییر DB** (hash/bytes قبل و بعد) همچنان ارزشمند است.
- تست‌های `--format csv/json` و کمک‌متن قدیمی باید حذف یا با سناریوهای HTML-only جایگزین شوند (مثلاً بررسی نام پیش‌فرض تولیدشده وقتی `--out` نیست — در workspace temp با config کنترل‌شده).
- `dashboard_help_lists_retained_flags_only`: آپدیت به وضعیت جدید (`--out` هست؛ بقیه حذف).

### 6.3 تست‌های core (`core/tests/explorer_dashboard_*`)
- API کتابخانه (`build/render_html/render_json/render_csv`, `WindowSpec`, `--db` مفهومی CLI نیست) تغییر نمی‌کند مگر عمداً؛ این پلن CLI-focused است، پس این تست‌ها باید **بدون تغییر** بمانند. اگر کامپایل/رفتار core را دستکاری نکردید، دست نزنید.

### 6.4 تست‌های unit داخل ماژول‌ها
- `cli/src/commands/explorer.rs` تست `resolve_db_*` باید با امضای جدید تطبیق داده شود.
- هر تست دیگری که به struct/تابع‌های حذف‌شده ارجاع دارد آپدیت شود.

---

## 7) تغییرات مستندات

1. **`README.md`**
   - بخش/مثال‌های `explorer`: حذف `--kind/--confidence`، حذف `--dashboard` به عنوان فلگ و `--db/--format/--repeat`، توضیح اینکه bare explorer و `explorer index` هر دو در پایان داشبورد HTML تولید می‌کنند.
   - به‌روزرسانی مثال‌ها:
     - `mev-scout explorer` → داشبورد all-time (مثلاً `results/explorer-<chain>-all.html`)
     - `mev-scout explorer index --window 24h` → ایندکس 24h + داشبورد `results/explorer-<chain>-24h.html`
     - `mev-scout explorer index --from-block A --to-block B` → `results/explorer-<chain>-blocks-A-B.html`
     - `--out` برای نام/مسیر سفارشی.
2. **`docs/CLI.md`**
   - جدول command map و بخش `3.6 explorer`/توضیحات مشترک را مطابق رفتار جدید بازنویسی کن (حذف ارجاعات به فلگ‌ها و حالت‌های report/json/csv/repeat/db-override).
   - ذکر کن خروجی فقط HTML است و نام فایل بر اساس بازه خودکار است مگر `--out` داده شود.
3. سایر فایل‌های `docs/` (مثلاً auditها) معمولاً تاریخی‌اند؛ **ویرایش نکن** مگر ارجاع مستقیم به CLI فعلی داشته باشند که باعث سردرگمی کاربر شود (در این پلن: فقط README و docs/CLI.md).

---

## 8) معیار پذیرش (Definition of Done)

- [ ] `cargo test -p mev-scout-cli` (حداقل تست‌های CLI) پس از آپدیت‌ها پاس می‌شود.
- [ ] `cargo test`/build workspace بدون ارور کامپایل (با توجه به تغییر امضاها).
- [ ] `mev-scout explorer --help` و `mev-scout explorer index --help` فقط سرفیس جدید را نشان می‌دهند.
- [ ] `mev-scout explorer` (با کانفیگ معتبر) فایل HTML all-time با نام خودکار می‌سازد.
- [ ] `mev-scout explorer index --window 24h` (و حالت پیش‌فرض 7d، و حالت from/to) ایندکس می‌کند و فایل HTML با نام متناسب بازه می‌سازد.
- [ ] `--out` نام/مسیر را override می‌کند.
- [ ] فلگ‌های حذف‌شده توسط clap رد می‌شوند.
- [ ] DB اکسپلورر توسط مسیر کانفیگ خوانده می‌شود (بدون `--db`) و dashboard به صورت read-only ساخته می‌شود (رفتار build فعلی حفظ شود).

---

## 9) نکات پیاده‌سازی/ریسک‌های کم‌اهمیت

- **ساخت دایرکتوری خروجی:** قبل از `atomic_write`، `create_dir_all(parent)` انجام شود (الگوی فعلی در dashboard.rs وجود دارد).
- **label ایمن برای نام فایل:** windowهایی مثل `24h` مشکلی ندارند؛ اگر ورودی غیرمنتظره‌ای رسید، برای نام فایل sanitize کن اما منطق parse/بک‌فیل را تغییر نده.
- **باز کردن خودکار فایل:** در ویندوز از مسیر absolute استفاده کن (`fs::canonicalize` یا join با cwd) تا `start` مطمئن‌تر عمل کند؛ خطا را نادیده بگیر.
- **حذف کد مرده:** بعد از حذف report/فرمت‌ها/repeat، importها و توابع استفاده‌نشده را پاک کن تا warningهای clippy ایجاد نشود.
- **سازگاری core:** توابع `render_json/render_csv` و قابلیت‌های `WindowSpec::parse` در core می‌توانند باقی بمانند (بدون استفاده در CLI)؛ حذفشان در این تغییر لازم نیست. تمرکز روی CLI است.
- **`config.output`**: دیگر برای `explorer` استفاده نمی‌شود (report CLI حذف شده). تغییری در schema کانفیگ لازم نیست.
- **ترتیب args در dispatch:** `--out` را طوری تعریف کن که `explorer index --out x.html --window 1d` و `explorer --out x.html index --window 1d` هر دو (در حد امکان) پذیرفته شوند؛ اگر فقط یک حالت را ساپورت کردی، در help و تست مشخص کن.
