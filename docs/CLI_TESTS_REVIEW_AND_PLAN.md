# CLI Tests — مرور، تحلیل شکاف‌ها و پلن بهبود

> این سند، ادغامِ `docs/TEST_REVIEW.md` (مرور و تحلیل شکاف‌ها) و
> `docs/TEST_IMPROVEMENT_PLAN.md` (پلن اقدام) است؛ همه‌ی مطالب هر دو در همین یک سند آمده.
>
> - بخش ۱: وضعیت فعلی، نقاط قوت، مشکلات تمیزی، شکاف‌های پوشش
> - بخش ۲: پلن بهبود در ۵ فاز (هر آیتم با `- [ ]`، حین انجام تیک بخورد)
> - بخش ۳: اجرا و راستی‌آزمایی + ترتیب پیشنهادی
>
> ترتیب فازها اهمیت دارد: فاز ۰ باگ‌های شکستن‌دهنده‌ی مسیر موفق است و باید اول انجام شود.

---

# بخش ۱ — مرور و تحلیل شکاف‌ها (Review & Gap Analysis)

وضعیت تست‌های `cli/tests` به‌صورت کامل بررسی شد. این بخش ارزیابی کیفیت، مشکلات تمیزی،
و شکاف‌های پوشش را خلاصه می‌کند. نتیجه‌ی بازبینی: ۵ فایل integration test + هارنِس `common/mod.rs`.

## ۱.۱ کیفیت و تمیزی — نقاط قوت

- **هندسه‌ی تست مشترک خوب است** (`cli/tests/common/mod.rs`):
  - `run_timed` با timeout و kill
  - `temp_ws` ایزوله
  - `temp_config` دستکاری TOML
  - `expect_ok` / `expect_fail` با پیام‌های خطای مفید
  - `extract_json_array` آگاه از ANSI
  - `RPC_MUTEX` برای سریال‌سازی تست‌های شبکه‌ای
  - `ensure_gate_and_rpc` برای گیت کردن
- **جداسازی درست:** تست‌های `cli_args.rs` همگی آفلاین‌اند (کشف خطا یا چاپ config، بدون نیاز
  به شبکه)؛ بقیه با `MEV_SCOUT_E2E=1` گیت شده‌اند.
- **تست‌های شبکه مقاوم به ناپایداری‌اند** (warn-and-continue به‌جای hard-fail روی RPCهای عمومی).
- **پایدار/تعیین‌پذیر:** به‌جای وابستگی به محتوای بلاک، فیلدهای کلیدی ساختاری بررسی می‌شوند.

## ۱.۲ مشکلات تمیزی (تکرار)

1. تست `live_duration_without_loop_rejected_offline` **دو بار یکسان** هست:
   - `cli/tests/cli_args.rs:112` (assert با `combined().contains`)
   - `cli/tests/cli_live_mode.rs:159` (assert روی stderr)
   → رفع: فاز ۰، بند 0.3.
2. هلپر `newest_live_json` (`cli_live_mode.rs:28`) و `newest_json_matching`
   (`cli_run_replay.rs:14`) تقریباً یکسان‌اند و می‌توانند به `common` منتقل شوند
   → رفع: فاز ۱، بندهای 1.4 و 1.5.
3. منطق `pick_factories` بین `discover.rs` و `validate_pools.rs` تکرار شده
   (در `validate_pools.rs:51` هلپر مستقل + تست واحد دارد؛ `discover.rs` همان انتخاب
   کارخانه‌ها را inline انجام می‌دهد) → رفع: فاز ۱، بند 1.5 (اختیاری).

## ۱.۳ شکاف‌های پوشش — چیزهایی که تست نشده‌اند

### در سطح آفلاین (قابل تست بدون شبکه)

- اعتبارسنجی تعارض محدوده‌ی بلاک:
  - ترکیب `--days` + `--blocks`
  - `--from-block` بدون `--to-block`
  - `to <= from`
  - `--days 0`
  - رد `replay --days`
  - این‌ها در `core/src/config/validation.rs` قبل از RPC رخ می‌دهند ولی فقط حالت
    «بدون محدوده» و «days>365» تست شده‌اند.
  → رفع: فاز ۲ (چند بند؛ شامل مسیر فایل کانفیگ — ببین یادداشت‌های فاز ۲).
- پرچم‌های سراسری `--quiet` / `--verbose` → رفع: فاز ۲.
- `run` / `fetch --batch-rpc` → رفع: فاز ۳.
- `replay --tx-index` → رفع: فاز ۳.
- `report --run-id` (انتخاب مثبت — فقط حالت منفی تست شده) → رفع: فاز ۲ (آفلاین با فایل دست‌ساز) + فاز ۳ (انتگراسیون).
- `tokens --symbol` / `--decimals` / `--limit` و خروجی جدول پیش‌فرض → رفع: فاز ۲
  (🔵 `tokens` کاملاً آفلاین است).
- `config -f` با مسیر ناموجود → رفع: فاز ۲ (با رفتار واقعیِ تأییدشده: fallback بی‌صدا به default).

### در سطح شبکه (E2E)

- `scan --kind` فقط `trades` تست شده؛ این‌ها خیر:
  - `transfers`
  - `flashloans`
  - `liquidations`
  - `labels` (کم‌هزینه‌ترین — فقط اتصال RPC لازم دارد)
  - `scan --address` / `--min-value`
  - خروجی CSV/جدول scan
  → رفع: فاز ٣.
- گزینه‌های `discover`:
  - `--source hybrid`
  - `--enrich`
  - `--min-tvl`
  - `--incremental`
  - `--resolve-remote-metadata`
  - `--health-check false`
  - `--solidly-fee-bps`
  - هشدار `--batch-size>5000`
  → رفع: فاز ٣ (`--enrich` در `data_foundation` فقط به‌صورت tolerant لمس شده).
- `validate-pools --source gecko` و `--markdown-out` (نوشته‌شدن فایل markdown) → رفع: فاز ٣.

## ۱.۴ نکته‌ی مهم‌تر — چند تست واقعاً هر بار اجرا می‌شود

با `cargo test` معمولی، همه‌ی تست‌های `cli_args.rs` و تست آفلاین `cli_live_mode.rs`
(`live_duration_without_loop_rejected_offline`) واقعاً اجرا می‌شوند؛ بقیه‌ی تست‌های
ارزشمند (pipelineها) بی‌صدا skip می‌شوند مگر `MEV_SCOUT_E2E=1` ست شود. پس
«چند تست واقعاً هر بار اجرا می‌شود» کم است و «test result: ok» برای باینری‌های
گیت‌دار گمراه‌کننده است (اسکیپ خاموش، بدون نام‌بردن از تست).

---

# بخش ۲ — پلن بهبود

## فاز ۰ — باگ‌های واقعی (Must fix) 🔴

### 0.1 — `cli_e2e.rs`: wipe شدن دایرکتوری export

**مشکل:** دو فراخوانی `temp_ws("cli_e2e")` (خطوط ۳۹ و ۴۳) مسیر یکسانی می‌سازند
(tag + pid یکسان). فراخوانی دوم داخل `temp_ws` اول `remove_dir_all` می‌زند و
دایرکتوری `export/` که قبلاً ساخته شده را پاک می‌کند. نتیجه: `db_path` به
`export/cache.db` اشاره می‌کند ولی پدرش وجود ندارد → `SqliteStore::open`
(`cli/src/commands/run.rs:28`؛`Connection::open` پوشه‌ی والد نمی‌سازد) با
`unable to open database file` شکست می‌خورد.

**فایل:** `cli/tests/cli_e2e.rs:39-53`

**اصلاح:**
```rust
let ws = temp_ws("cli_e2e");
let export = ws.join("export");
std::fs::create_dir_all(&export).unwrap();
// سپس temp_config با ("export_path", export.to_str().unwrap())
```

**پذیرش:** اجرای تست با `MEV_SCOUT_E2E=1` باید به مرحله `assert!(out.status.success())` برسد
و `run_*.json` واقعاً در export پیدا شود.

> **✅ انجام شد + کشف مهم:** علاوه بر wipe شدن export، یک باگ عمیق‌تر در هارنس کشف شد:
> `temp_config` وقتی `rpc_urls` چندخطی را با آرایه‌ی تک‌خطی جایگزین می‌کرد، شرط drain
> خطوط ادامه (`!lines[i].contains(']')`) بعد از جایگزینی false می‌شد و خطوط URL باقی‌مانده
> TOML را خراب می‌کردند → fallback بی‌صدا به default config → همیشه `./cache/polygon-...sqlite`.
> یعنی cli_e2e از ابتدا حتی بدون باگ wipe هم fail می‌شد. هر دو رفع شدند و تست با
> `MEV_SCOUT_E2E=1` پاس می‌شود. همچنین تست با `run_timed` timeout دارد (فاز ۱.۱).

- [x] یکسان‌سازی `ws` و ساخت export زیرمجموعه‌ی آن

### 0.2 — `cli_run_replay.rs`: پارس درصد Receipt verification همیشه panic می‌دهد

**مشکل:** فرمت خط در `cli/src/commands/replay.rs:141`:
```
  Receipt verification: 5/5 match (100.0%) — 1.23s
```
کد تست (`cli/tests/cli_run_replay.rs:104-108`):
```rust
l.split('(').nth(1)                                  // "100.0%) — 1.23s"
    .and_then(|s| s.trim_end_matches(['%', ')', ' ']).trim().parse().ok())
```
`trim_end_matches` از **انتهای** رشته حذف می‌کند؛ انتهای رشته `1.23s` است پس هیچ‌چیز
حذف نمی‌شود → `parse::<f64>` شکست می‌خورد → `.expect` → **panic**.
هر بار که replay موفق شود و این خط چاپ شود تست fail می‌شود.

**فایل:** `cli/tests/cli_run_replay.rs:104-108`

**اصلاح:**
```rust
let pct: f64 = l
    .split('(')
    .nth(1)
    .and_then(|s| s.split(')').next())              // فقط داخل پرانتز
    .and_then(|s| s.trim_end_matches('%').trim().parse().ok())
    .expect("parseable match percentage");
```

**پذیرش:** واحد تست کوچک روی این پارس با رشته‌ی نمونه (`"100.0%) — 1.23s"`) پاس شود؛
تست اصلی با `MEV_SCOUT_E2E=1` بدون panic از بخش replay عبور کند.

- [x] اصلاح پارس
- [x] (اختیاری) تبدیل پارس به تابع در `common` + unit test داخل همان فایل
      (پیاده‌سازی: `common::parse_receipt_match_pct` + unit test در `cli_run_replay.rs`)

### 0.3 — تست تکراری `live_duration_without_loop_rejected_offline`

**مشکل:** در دو فایل وجود دارد:
- `cli/tests/cli_args.rs:112` (با assert `combined().contains`)
- `cli/tests/cli_live_mode.rs:159` (با assert روی stderr)

کامپایل نمی‌شکند (باینری‌های جدا هستند) ولی نگهداری دو نسخه است.

**اصلاح:** نسخه‌ی `cli_live_mode.rs` را نگه دار (کنار بقیه تست‌های live منطقی‌تر است)
و نسخه‌ی `cli_args.rs` را حذف کن؛ یا برعکس. فقط یکی بماند.

- [x] حذف یکی از دو نسخه (نسخه‌ی `cli_args.rs` حذف شد)

### 0.4 — شرط مرده در `cli_run_replay.rs:45-50`

**مشکل:** `contains("even after refetch")` هرگز true نمی‌شود — این رشته فقط در
`core/tests/e2e.rs:304` چاپ می‌شود (خروجی تست core)، نه در خروجی CLI
(fetch فقط `Missing:` و `Refetched:` چاپ می‌کند، `cli/src/commands/fetch.rs:112,118`).
همچنین `A || B && C` بدون پرانتز مبهم است.

**اصلاح:**
```rust
if out.stdout.contains("Missing:")
    && !out.stdout.contains("Refetched:    5")
{
    eprintln!("WARN: provider left gaps; continuing with what was cached");
}
```

- [x] حذف شرط مرده + پرانتزگذاری شرط باقی‌مانده

### 0.5 — `cli_live_mode.rs`: فلاگ‌های `--export-path` / `--db-path` وجود ندارند (تست‌ها حتمی fail)

**مشکل:** `live_one_shot_smoke` (خطوط ۵۹-۶۱) و `live_loop_duration_graceful_exit`
(خطوط ۱۰۸-۱۱۰) آرگومان‌های `--export-path` و `--db-path` را به باینری پاس می‌دهند،
اما این فلاگ‌ها در `cli.rs::LiveArgs` تعریف نشده‌اند (فقط `--loop/--duration/--poll-interval`).
clap آن‌ها را رد می‌کند:

```
error: unexpected argument '--export-path' found
```

بنابراین هر دو تست live حتی با `MEV_SCOUT_E2E=1` و RPC سالم **حتماً با exit 2 fail می‌شوند**
(فعلاً فقط چون گیت‌دارند دیده نمی‌شوند؛ تأیید تجربی با اجرای مستقیم باینری).

**فایل:** `cli/tests/cli_live_mode.rs:59-61, 108-110` و `cli/src/cli.rs:307-320`

**اصلاح:** به‌جای آرگومان CLI، `export_path` و `db_path` را مثل بقیه تست‌ها در `temp_config`
بریزید (الگوی `cli_run_replay.rs:39`). تغییر تولیدی (افزودن فلاگ به `LiveArgs`) فقط اگر واقعاً
به محصول نیاز است انجام شود.

**پذیرش:** `MEV_SCOUT_E2E=1; cargo test -p mev-scout-cli --test cli_live_mode` باید به مرحله
assert روی خروجی برسد، نه خطای clap.

- [x] جایگزینی `--export-path/--db-path` با `temp_config` extras

---

## فاز ۱ — استحکام هارنس `common/mod.rs` و سازگاری (Should fix) 🟡

### 1.1 — `cli_e2e.rs`: بدون timeout اجرا می‌شود

`cmd.output()` (`cli_e2e.rs:61`) تا ابد منتظر می‌ماند. بقیه تست‌ها `run_timed` دارند.
- [x] جایگزینی با `run_timed(&mut cmd, HEAVY_TIMEOUT)` و رفتار tolerant روی timeout
      (SKIP/WARN، هم‌راستا با بقیه تست‌های شبکه‌ای)

### 1.2 — ترتیب قفل RPC_MUTEX ناسازگار است

- `cli_data_foundation.rs`: اول `lock()` بعد `ensure_gate_and_rpc` (پروب RPC داخل قفل)
- `cli_run_replay.rs` / `cli_live_mode.rs`: اول gate (پروب RPC **خارج از قفل**) بعد lock

پروب‌های خارج از قفل می‌توانند با ترافیک تست دیگر هم‌زمان شوند و به‌خاطر rate limit
بیهوده SKIP شوند.
- [x] استانداردسازی: همیشه اول `RPC_MUTEX.lock()`، بعد `ensure_gate_and_rpc`
      (الگوی `cli_data_foundation.rs` مرجع شود؛ همه‌ی فایل‌ها اکنون `rpc_lock()` اول)

### 1.3 — Poisoned mutex بقیه تست‌ها را می‌کُشد

`RPC_MUTEX.lock().unwrap()` — اگر تستی حین نگه‌داشتن قفل پنیک کند، همه‌ی تست‌های
بعدی همان باینری با PoisonError پنیک می‌شوند.
- [x] در `common/mod.rs` یک helper اضافه شود:
```rust
pub fn rpc_lock() -> std::sync::MutexGuard<'static, ()> {
    RPC_MUTEX.lock().unwrap_or_else(|e| e.into_inner())
}
```
- [x] جایگزینی همه‌ی `.lock().unwrap()` در ۳ فایل با helper

### 1.4 — `newest_*_json`: شکست metadata کل تابع را None می‌کند

در `newest_live_json` (`cli_live_mode.rs:28`) و `newest_json_matching`
(`cli_run_replay.rs:14`) عبارت `e.metadata().ok()?` باعث می‌شود شکست متادیتای
*یک* entry، کل تابع را `None` برگرداند (پیام گمراه‌کننده «file not found»).
- [x] ادغام دو تابع به یکی در `common` با ورودی `(dir, prefix)`
      (پیاده‌سازی: `common::newest_json_file`)
- [x] entry خراب skip شود، نه کل تابع:
```rust
let Ok(mtime) = e.metadata().and_then(|m| m.modified()) else { continue; };
```

### 1.5 — کدهای تکراری به `common` منتقل شوند

- [x] `cfg()` (۳ فایل: cli_args, cli_data_foundation, cli_live_mode) → `common::repo_config_str()`
- [x] `make_cfg` (۲ فایل) → `common::make_cfg(ws, extras) -> String`
- [x] حذف `newest_live_json` و `newest_json_matching` به نفع نسخه‌ی مشترک (بند 1.4)
- [x] (اختیاری) تکرارِ انتخاب کارخانه بین `discover.rs` و `validate_pools.rs` —
      به `core::pool::discovery::pick_factories` منتقل شد + ۳ unit test
      (`factory_selection_tests` در `core/src/pool/discovery/mod.rs`)؛
      `discover.rs` از inline if/else به همان helper تغییر کرد

### 1.6 — assert های مرده / بی‌اثر

- [x] `cli_data_foundation.rs:52-64`: بعد از `assert!(pools.is_array())` بلاک
      `if let Some(...)` مرده است → `pools.as_array().expect(...)` و حلقه روی آن.
      در بخش scan (خطوط ۱۳۸-۱۴۴) اصلاً assert is_array نیست → اگر خروجی object شود
      assertionهای فیلد بی‌سروصدا skip می‌شوند؛ همان الگوی expect را اعمال کن.
- [x] `expect_fail` فقط exit code را چک می‌کند؛ برای تست‌های validation
      (`run_without_block_range_fails_offline` و همتاها) یک پیام کلیدی هم assert شود
      (مثلاً `contains("exactly one")` یا `contains("--days, --blocks, --block")`)
      تا fail به هر دلیل دیگری false positive نشود.

### 1.7 — `RPC_MUTEX` فقط درون یک باینری تست سریال می‌کند

`RPC_MUTEX` یک static در هر باینری تست است؛ `cargo test` باینری‌ها را به‌صورت
**هم‌زمان** می‌راند، پس تست‌های شبکه‌ای `cli_run_replay` / `cli_live_mode` /
`cli_data_foundation` در پردازه‌های جدا می‌توانند هم‌زمان روی RPCهای عمومی بزنند —
و `cli_e2e` اصلاً در قفل شرکت نمی‌کند (هیچ `RPC_MUTEX`ای ندارد).
- [x] اگر E2Eها باید سریال باشند، در بخش اجرا با `--test-threads=1` اجرا شوند یا به‌جای
      mutex درون‌فرایندی از قفل فایل (مثل crossbeam) استفاده شود؛ حداقل در README مستند شود.
      (پیاده‌سازی: `cli/tests/README.md` مستند شد؛ قفل فایل به‌عنوان بهبود آینده مانده)

---

## فاز ۲ — تست‌های آفلاین جدید (ارزان و بدون شبکه) 🟢

همه در `cli_args.rs`، هر کدام چند خط، بدون `MEV_SCOUT_E2E`. این فاز شکاف‌های آفلاین
بخش ۱.۳ را می‌بندد.

> **⚠️ کشف حین اجرا — premise اصلی این فاز غلط بود:** فیلدهای رنج در `Config`
> (`days/blocks/block/from_block/to_block`) همه `#[serde(skip)]` هستند
> (`core/src/config/settings.rs:189-199`) — یعنی **هیچ‌کدام از فایل TOML قابل ست‌کردن
> نیستند**. ترکیب «کلپ رد می‌کند + کانفیگ نمی‌تواند ست کند» یعنی چک‌های داخلی
> `validation.rs` برای این مقادیر از هیچ مسیر CLI قابل رسیدن نیستند (فقط مستقیم از
> کتابخانه‌ی core). تست‌های پیاده‌شده رفتار واقعی را قفل می‌کنند (بی‌اعتنایی بی‌صدا +
> خطای عمومی no block range) و این یافته به‌عنوان تصمیم محصول ثبت شد.

- [x] `--from-block` بدون `--to-block` → fail با پیام «must be used together»
      (منطق: `validation.rs:90-94`)
- [x] `--to-block` برابر یا کمتر از `--from-block` → fail با «must be greater than»
      (`validation.rs:96-103`)
- [x] تداخل فلگ‌ها: `run --days 2 --blocks 5` → fail با «cannot be used together»
      (`validation.rs:82-88`)
- [x] `replay --days 1` → ⚠️ رفتار واقعی (تأیید تجربی): `ReplayArgs` اصلاً فلگ رنج
      ندارد، پس clap با «unexpected argument '--days' found» (exit 2) رد می‌کند و
      شاخه‌ی `validation.rs:155-158` («not supported by the replay subcommand») از CLI
      غیرقابل‌رسیدن است. تست رفتار واقعی (clap rejection) قفل شد.
- [x] `--blocks 0` و `--block 0` برای `fetch` و `scan` (فعلاً فقط `run --block 0` هست)
      ⚠️ این مقادیر از لایه‌ی clap رد می‌شوند (`range(1..)`)؛ یعنی چک‌های داخل
      `validation.rs` (`--blocks must be >= 1` / `--block must be > 0`) از طریق CLI
      **هرگز** قابل رسیدن نیستند و فقط از مسیر فایل کانفیگ قابل تست‌اند → بند بعدی.
- [x] اعتبارسنجی از مسیر فایل کانفیگ → ⚠️ **رفتار واقعی (تأیید تجربی):** چون فیلدها
      `serde(skip)` هستند، مقادیر نوشته‌شده در TOML **بی‌صدا نادیده** می‌شوند و `run`
      با خطای عمومی «no block range specified» fail می‌شود؛ تستِ
      `config_file_block_range_fields_are_ignored_cli_only` همین را قفل می‌کند.
- [x] `-f nonexistent.toml` → واقعیتِ بررسی‌شده (تأیید تجربی): `Config::load_or_default`
      (`cli/src/main.rs:37` ← `core/src/config/settings.rs:254`، `unwrap_or_default()`)
      **بی‌صدا** به default برمی‌گردد و exit 0 می‌شود؛ تست رفتار واقعی را قفل کند +
      به‌عنوان باگ محصول تصمیم گرفته شود (آیا `-f` ناموجود باید fail کند؟ — خطرناک:
      `run -f bogus.toml` بی‌صدا با config پیش‌فرض و RPCهای عمومی اجرا می‌شود)
      ✅ تست `missing_config_file_falls_back_to_default` رفتار واقعی را قفل کرد؛
      تصمیم محصول هنوز باز است.
- [x] TOML خراب → ✅ تست `broken_toml_config_file_falls_back_to_default` رفتار واقعی
      (exit 0 + default) را قفل کرد؛ تصمیم محصول با بند قبلی یکی است.
- [x] `--version` → exit 0 و خروجی شامل نسخه
- [x] اجرای بدون subcommand → exit غیرصفر (clap)
- [x] `tokens` 🔵 **کاملاً آفلاین است** (`cmd_tokens` هیچ RPC باز نمی‌کند؛ فقط
      SqliteStore + لیست توکن‌های bundled): `tokens --symbol`، `tokens --decimals`،
      `tokens --limit` و خروجی جدول پیش‌فرض — همه در
      `tokens_filters_work_offline` (فیلترها با خروجی JSON پارس و assert می‌شوند؛
      USDC (6 اعشار) به‌جای WETH چون در لیست bundled پلیگون guaranteed است)
- [x] `report` 🔵 **کاملاً آفلاین است**: یک `run_<id>.json` مینیمال (`ResultsFile` با
      `opportunities` خالی) در export نوشته می‌شود؛ مسیر مثبت `report --run-id` +
      انتخاب «latest» (با ۵۰ms فاصله‌ی mtime) + roundtrip جدول/CSV/JSON در
      `report_selects_explicit_run_id_offline` تست شد
- [x] `--quiet` / `--verbose`: پارس `--quiet config` / `--verbose config` (exit 0) +
      اثر `--quiet` در `cli_e2e` (نبودِ INFO/DEBUG/WARN در خروجی) assert شد
- [x] `scan --min-value <غیرعددی>` و `--address <نامعتبر>`: تست رفتار فعلیِ
      «بی‌صدا نادیده گرفته می‌شود» در `cli_network_coverage.rs`
      (`scan_address_filter_and_silent_invalid_filter`) قفل شد؛ تصمیم «reject با
      خطا» هنوز باز است

---

## فاز ۳ — پوشش شبکه‌ای گیت‌دار (وقتی RPC در دسترس است) 🌐

همه پشت `MEV_SCOUT_E2E=1` + `rpc_ready` + `RPC_MUTEX`. این فاز شکاف‌های شبکه‌ای
بخش ۱.۳ را می‌بندد. — **✅ پیاده‌سازی شد در `cli/tests/cli_network_coverage.rs`
(۷ تست، همه با rpc_lock + tolerant SKIP/WARN).**

> **🐛 کشف مهم حین اجرا — باگ واقعی محصول:** اولین اجرای `scan --kind transfers`
> روی Polygon یک **panic** در `core/src/chain/events.rs:208` کشف کرد
> (`range end index 32 out of range for slice of length 0`): decoderهای
> `decode_transfer` / `decode_uniswap_v3_swap` / `decode_uniswap_v2_swap` بدون چکِ
> طول data، اسلایس `[0..32]` می‌گرفتند و یک log مال‌فورم از RPC زنده کرش می‌داد.
> هر سه با گارد طول رفع شدند + unit test regression
> (`short_data_payloads_are_skipped_not_panicking`) اضافه شد.

> **⚠️ کشف حین اجرا — `--health-check false` از CLI غیرقابل‌رسیدن است:** clap فلگ
> bool با `default_value` را در هر دو فرم (`--health-check false` و `--health-check=false`)
> رد می‌کند (exit 2، تأیید تجربی). اگر غیرفعال‌کردن health check محصولاً لازم است،
> باید در `cli.rs` اصلاح شود؛ فعلاً تست مسیر پیش‌فرض (فعال) را پوشش می‌دهد.

- [x] `scan` برای `--kind transfers` و `--kind flashloans` (فقط trades پوشش داشت؛
      liquidations عمداً حذف شد — نادر بودن رویداد و هزینه‌ی RPC)،
      `--kind labels` 🔵 (خروجی bundled labels)
- [x] `scan --address <آدرس pool کشف‌شده>` (فیلتر روی subset با assert عدم افینتی) +
      خروجی CSV (هدر `block,tx_hash,token,from,to,value`)
- [x] `replay --tx-index 0` روی بلاک fetchشده (بلاک از `run_*.json` استخراج می‌شود)
- [x] `discover --source hybrid --max-pools 20` (union + assert dedup آدرس‌ها) +
      هشدار `--batch-size > 5000` (تأیید: در discover.rs:162 هست و assert شد)
- [x] `discover --incremental` بعد از discover اولیه
- [x] گزینه‌های تکمیلی discover: `--enrich` (assert tvl/volume وقتی سرویس پاس دهد) +
      `--min-tvl` (assert رعایت فیلور وقتی tvl موجود است) + `--resolve-remote-metadata`
      (smoke) — در `discover_remote_option_flags_enrich_min_tvl_resolve_metadata`؛
      همه tolerant به خطای سرویس مرجع. `--health-check false` unreachable است
      (یادداشت بالا) و از رنج تست حذف شد.
- [x] `validate-pools --source gecko --markdown-out <path>` → فایل ساخته می‌شود و
      جدول markdown دارد (tolerant به خطای سرویس مرجع)
- [x] `run --batch-rpc` و `fetch --batch-rpc` smoke دوبلاکی
- [x] `fetch` دوباره روی همان رنج → `Cached:` (tolerant — اگر خط Cached نبود فقط WARN)

---

## فاز ۴ — بهداشت repo (اختیاری ولی مهم) 🧹

- [x] **کلیدهای API زنده در `mev-scout.toml`** → سه‌لایه حل شد:
      1. پشتیبانی محصولی `${ENV_VAR}` در `rpc_url` / `rpc_urls` /
         `coingecko_api_key` (گسترش هنگام load؛ متغیر unset به‌صورت literal می‌ماند
         تا fail بلند باشد — `Config::expand_env_secrets` در settings.rs + ۴ unit test)
      2. `mev-scout.example.toml` بدون کلید واقعی (الگوی env placeholder)
      3. تست‌ها: `common::first_rpc_url()` اکنون اول `RPC_URL` env را می‌خواند
         (کلون تازه بدون mev-scout.toml هم کار می‌کند) و `rpc_ready` URL را
         داخل کانفیگ موقت تزریق می‌کند؛ CLI test
         `config_env_var_placeholders_expand_from_environment` رفتار را end-to-end
         قفل می‌کند.
      ⚠️ خود `mev-scout.toml` همچنان کلیدهای زنده دارد — مهاجرت نهایی فایل به
      الگوی env و چرخش (revoke) کلیدهای کامیت‌شده تصمیم صاحب repo است.
- [x] تست `config_prints_resolved_toml_from_repo_file` به عدد `>= 9` provider
      وابسته است → اکنون تعداد را **دینامیک** از خود repo TOML می‌خواند
      (`common::repo_config_text()`) و با آن مقایسه می‌کند
- [x] (کوچک) در `cli_data_foundation.rs` تست «pipeline»، `tokens` و `scan` از
      `db_path` کش‌شده استفاده نمی‌کنند → `tokens` اکنون روی همان `db_path`
      مشترک pipeline اجرا می‌شود (`scan` ذاتاً db نمی‌خواند — مستند شد)

---

# بخش ۳ — اجرا و راستی‌آزمایی

## دستورات

```powershell
# کامپایل تست‌ها (سریع‌ترین بازخورد) — ⚠️ نام پکیج mev-scout-cli است نه mev-scout
cargo test -p mev-scout-cli --test cli_args --no-run
cargo test -p mev-scout-cli --test cli_run_replay --no-run
cargo test -p mev-scout-cli --test cli_live_mode --no-run
cargo test -p mev-scout-cli --test cli_e2e --no-run
cargo test -p mev-scout-cli --test cli_data_foundation --no-run
cargo test -p mev-scout-cli --test cli_network_coverage --no-run

# آفلاین‌ها (باید بدون شبکه کامل پاس شوند)
cargo test -p mev-scout-cli --test cli_args
cargo test -p mev-scout-cli --test cli_live_mode --test cli_run_replay --test cli_e2e --test cli_data_foundation

# شبکه‌ای (فقط وقتی RPCها در دسترس‌اند؛ برای سریال‌سازی بین باینری‌ها --test-threads=1 ببین 1.7)
$env:MEV_SCOUT_E2E = "1"
cargo test -p mev-scout-cli --test cli_e2e -- --test-threads=1
cargo test -p mev-scout-cli --test cli_run_replay -- --test-threads=1
cargo test -p mev-scout-cli --test cli_live_mode -- --test-threads=1
cargo test -p mev-scout-cli --test cli_data_foundation -- --test-threads=1
cargo test -p mev-scout-cli --test cli_network_coverage -- --test-threads=1
```

## وضعیت نهایی اجرا (۶ سپتامبر ۲۰۲۶)

هر شش باینری تست با `MEV_SCOUT_E2E=1` و `--test-threads=1` اجرا و پاس شد.
پس از تکمیل سه آیتم باقی‌مانده (1.5 اختیاری، گزینه‌های discover، 4.1)، شمارش
نهایی آفلاین: ۴۲ تست CLI + ۱۰۱ unit core (به‌جز ۵ تست mock شبکه‌ای
geckoterminal/dexscreener که **از قبل** fail بودند و به این سند مربوط نیستند)
+ تست‌های گیت‌دار شبکه‌ای همه ok.

| باینری / واحد | تست‌ها | نتیجه |
|---|---|---|
| cli (unit) | 6 | ok |
| cli_args | 25 | ok |
| cli_data_foundation | 3 | ok |
| cli_e2e | 1 | ok |
| cli_live_mode | 3 | ok |
| cli_network_coverage | 8 | ok |
| cli_run_replay | 2 | ok |
| core unit (events/config/discovery و ...) | 96 | ok (۴ تست env-expansion جدید شامل) |

سه باگ/یافته‌ی واقعی محصول در حین اجرا کشف شد: panic در decoderهای log
(`core/src/chain/events.rs` — transfer/v2/v3، رفع شد)، `--health-check false`
غیرقابل‌استفاده از CLI (مستند شد؛ تصمیم محصول باز است)، و خرابی TOML در
`temp_config` هارنس روی آرایه‌ی چندخطی `rpc_urls` (رفع شد).

## ترتیب پیشنهادی اجرا

| مرحله | محتوا | تلاش تقریبی |
|---|---|---|
| ۱ | فاز ۰ (پنج باگ) | ~۱ ساعت |
| ۲ | فاز ۱ (هارنس) | ~۲ ساعت |
| ۳ | فاز ۲ (آفلاین‌ها) | ~۱ ساعت |
| ۴ | فاز ۳ (شبکه‌ای‌ها) | نیم روز (وابسته به RPC) |
| ۵ | فاز ۴ (بهداشت) | جداگانه، بعد از تثبیت تست‌ها |