# پلن بهبود تست‌های `cli/tests`

> نتیجه بازبینی ۵ فایل integration test + هارنِس `common/mod.rs`.
> هر آیتم با `- [ ]` علامت‌گذاری شده تا حین انجام تیک بخورد.
> ترتیب فازها اهمیت دارد: فاز ۰ باگ‌های شکستن‌دهنده مسیر موفق است و باید اول انجام شود.

---

## فاز ۰ — باگ‌های واقعی (Must fix) 🔴

### 0.1 — `cli_e2e.rs`: wipe شدن دایرکتوری export

**مشکل:** دو فراخوانی `temp_ws("cli_e2e")` (خطوط ۳۹ و ۴۳) مسیر یکسانی می‌سازند
(tag + pid یکسان). فراخوانی دوم داخل `temp_ws` اول `remove_dir_all` می‌زند و
دایرکتوری `export/` که قبلاً ساخته شده را پاک می‌کند. نتیجه: `db_path` به
`export/cache.db` اشاره می‌کند ولی پدرش وجود ندارد → `SqliteStore::open`
(`run.rs:28`) با `unable to open database file` شکست می‌خورد.

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

- [ ] یکسان‌سازی `ws` و ساخت export زیرمجموعه‌ی آن

### 0.2 — `cli_run_replay.rs`: پارس درصد Receipt verification همیشه panic می‌دهد

**مشکل:** فرمت خط در `replay.rs:141`:
```
  Receipt verification: 5/5 match (100.0%) — 1.23s
```
کد تست (`cli_run_replay.rs:104-108`):
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

- [ ] اصلاح پارس
- [ ] (اختیاری) تبدیل پارس به تابع در `common` + unit test داخل همان فایل

### 0.3 — تست تکراری `live_duration_without_loop_rejected_offline`

**مشکل:** در دو فایل وجود دارد:
- `cli/tests/cli_args.rs:112` (با assert `combined().contains`)
- `cli/tests/cli_live_mode.rs:159` (با assert روی stderr)

کامپایل نمی‌شکند (باینری‌های جدا هستند) ولی نگهداری دو نسخه است.

**اصلاح:** نسخه‌ی `cli_live_mode.rs` را نگه دار (کنار بقیه تست‌های live منطقی‌تر است)
و نسخه‌ی `cli_args.rs` را حذف کن؛ یا برعکس. فقط یکی بماند.

- [ ] حذف یکی از دو نسخه

### 0.4 — شرط مرده در `cli_run_replay.rs:45-50`

**مشکل:** `contains("even after refetch")` هرگز true نمی‌شود — این رشته فقط در
`core/tests/e2e.rs:304` چاپ می‌شود (خروجی تست core)، نه در خروجی CLI
(fetch فقط `Missing:` و `Refetched:` چاپ می‌کند، `fetch.rs:112,118`).
همچنین `A || B && C` بدون پرانتز مبهم است.

**اصلاح:**
```rust
if out.stdout.contains("Missing:")
    && !out.stdout.contains("Refetched:    5")
{
    eprintln!("WARN: provider left gaps; continuing with what was cached");
}
```

- [ ] حذف شرط مرده + پرانتزگذاری شرط باقی‌مانده

---

## فاز ۱ — استحکام هارنس `common/mod.rs` و سازگاری (Should fix) 🟡

### 1.1 — `cli_e2e.rs`: بدون timeout اجرا می‌شود

`cmd.output()` (`cli_e2e.rs:61`) تا ابد منتظر می‌ماند. بقیه تست‌ها `run_timed` دارند.
- [ ] جایگزینی با `run_timed(&mut cmd, HEAVY_TIMEOUT)` و رفتار tolerant روی timeout
      (SKIP/WARN، هم‌راستا با بقیه تست‌های شبکه‌ای)

### 1.2 — ترتیب قفل RPC_MUTEX ناسازگار است

- `cli_data_foundation.rs`: اول `lock()` بعد `ensure_gate_and_rpc` (پروب RPC داخل قفل)
- `cli_run_replay.rs` / `cli_live_mode.rs`: اول gate (پروب RPC **خارج از قفل**) بعد lock

پروب‌های خارج از قفل می‌توانند با ترافیک تست دیگر هم‌زمان شوند و به‌خاطر rate limit
بیهوده SKIP شوند.
- [ ] استانداردسازی: همیشه اول `RPC_MUTEX.lock()`، بعد `ensure_gate_and_rpc`
      (الگوی `cli_data_foundation.rs` مرجع شود)

### 1.3 — Poisoned mutex بقیه تست‌ها را می‌کُشد

`RPC_MUTEX.lock().unwrap()` — اگر تستی حین نگه‌داشتن قفل پنیک کند، همه‌ی تست‌های
بعدی همان باینری با PoisonError پنیک می‌شوند.
- [ ] در `common/mod.rs` یک helper اضافه شود:
```rust
pub fn rpc_lock() -> std::sync::MutexGuard<'static, ()> {
    RPC_MUTEX.lock().unwrap_or_else(|e| e.into_inner())
}
```
- [ ] جایگزینی همه‌ی `.lock().unwrap()` در ۳ فایل با helper

### 1.4 — `newest_*_json`: شکست metadata کل تابع را None می‌کند

در `newest_live_json` (`cli_live_mode.rs:28`) و `newest_json_matching`
(`cli_run_replay.rs:14`) عبارت `e.metadata().ok()?` باعث می‌شود شکست متادیتای
*یک* entry، کل تابع را `None` برگرداند (پیام گمراه‌کننده «file not found»).
- [ ] ادغام دو تابع به یکی در `common` با ورودی `(dir, prefix)`
- [ ] entry خراب skip شود، نه کل تابع:
```rust
let Ok(mtime) = e.metadata().and_then(|m| m.modified()) else { continue; };
```

### 1.5 — کدهای تکراری به `common` منتقل شوند

- [ ] `cfg()` (۳ فایل: cli_args, cli_data_foundation, cli_live_mode) → `common::repo_config_str()`
- [ ] `make_cfg` (۲ فایل) → `common::make_cfg(ws, extras) -> String`
- [ ] حذف `newest_live_json` و `newest_json_matching` به نفع نسخه‌ی مشترک (بند 1.4)

### 1.6 — assert های مرده / بی‌اثر

- [ ] `cli_data_foundation.rs:52-64`: بعد از `assert!(pools.is_array())` بلاک
      `if let Some(...)` مرده است → `pools.as_array().expect(...)` و حلقه روی آن.
      در بخش scan (خطوط ۱۳۸-۱۴۴) اصلاً assert is_array نیست → اگر خروجی object شود
      assertionهای فیلد بی‌سروصدا skip می‌شوند؛ همان الگوی expect را اعمال کن.
- [ ] `expect_fail` فقط exit code را چک می‌کند؛ برای تست‌های validation
      (`run_without_block_range_fails_offline` و همتاها) یک پیام کلیدی هم assert شود
      (مثلاً `contains("exactly one")` یا `contains("--days, --blocks, --block")`)
      تا fail به هر دلیل دیگری false positive نشود.

---

## فاز ۲ — تست‌های آفلاین جدید (ارزان و بدون شبکه) 🟢

همه در `cli_args.rs`، هر کدام چند خط، بدون `MEV_SCOUT_E2E`.

- [ ] `--from-block` بدون `--to-block` → fail با پیام «must be used together»
      (منطق: `validation.rs:90-94`)
- [ ] `--to-block` برابر یا کمتر از `--from-block` → fail با «must be greater than»
      (`validation.rs:96-103`)
- [ ] تداخل فلگ‌ها: `run --days 2 --blocks 5` → fail با «cannot be used together»
      (`validation.rs:82-88`)
- [ ] `--blocks 0` و `--block 0` برای `fetch` و `scan` (فعلاً فقط `run --block 0` هست)
- [ ] `-f nonexistent.toml` → رفتار `Config::load_or_default` (`main.rs:37`) را مستند کن:
      یا fail تمیز یا ادامه با default؛ تست باید رفتار واقعی را قفل کند
- [ ] TOML خراب (نوشتن فایل موقت با محتوای نامعتبر) → exit غیرصفر + پیام خطا
- [ ] `--version` → exit 0 و خروجی شامل نسخه
- [ ] اجرای بدون subcommand → exit غیرصفر (clap)

---

## فاز ۳ — پوشش شبکه‌ای گیت‌دار (وقتی RPC در دسترس است) 🌐

همه پشت `MEV_SCOUT_E2E=1` + `rpc_ready` + `RPC_MUTEX`.

- [ ] `scan` برای `--kind transfers` و `--kind flashloans` (فقط trades پوشش دارد؛
      liquidations/labels اختیاری)
- [ ] `tokens --symbol WETH` (فیلتر شامل حداقل یک نمونه شناخته‌شده)
      و `tokens --decimals 18`
- [ ] `replay --tx-index 0` روی بلاک شروعِ همان تست run_replay
- [ ] `discover --source hybrid --max-pools 20` (union دو مسیر + dedup)
- [ ] `discover --incremental` بعد از discover اولیه (ادامه از بالاترین بلاک کش)
- [ ] مسیر موفق `report --run-id <id>` با id واقعی از خروجی `run` (فعلاً فقط مسیرهای
      خطا و مسیر «latest» تست شده‌اند)
- [ ] `report --markdown-out <path>` → فایل ساخته شود و سرآیند جدول داشته باشد
- [ ] `run --batch-rpc` یک smoke یک‌بلاکی (مسیر JSON-RPC batching فعلاً تست ندارد)
- [ ] `fetch` دوباره روی همان رنج → `Cached: 5` (اثبات idempotency کش؛ ایده‌ی
      handoff داده که فعلاً در data_foundation واقعاً verify نمی‌شود)

---

## فاز ۴ — بهداشت repo (اختیاری ولی مهم) 🧹

- [ ] **کلیدهای API زنده در `mev-scout.toml`** (Alchemy/drpc/GetBlock) — خارج از
      scope تست‌ها ولی تست‌ها به همین فایل وابسته‌اند. پیشنهاد: انتقال به
      `mev-scout.example.toml` بدون کلید + خواندن RPC از env var در تست‌ها
      (`RPC_URL` قبلاً در cli_e2e پشتیبانی می‌شود؛ به `first_rpc_url` هم fallback
      env اضافه شود)
- [ ] تست `config_prints_resolved_toml_from_repo_file` به عدد `>= 9` provider
      وابسته است → اگر تعداد provider تغییر کرد تست می‌شکند؛ یا از فایل بخوان و
      تعداد را dynamic مقایسه کن، یا threshold را مستند کن
- [ ] (کوچک) در `cli_data_foundation.rs` تست «pipeline»، `tokens` و `scan` از
      `db_path` کش‌شده استفاده نمی‌کنند → handoff واقعی بین مراحل verify نمی‌شود؛
      حداقل یک runtime تمام مراحل روی یک `db_path` مشترک

---

## اجرا و راستی‌آزمایی

```powershell
# کامپایل تست‌ها (سریع‌ترین بازخورد)
cargo test -p mev-scout --test cli_args --no-run
cargo test -p mev-scout --test cli_run_replay --no-run
cargo test -p mev-scout --test cli_live_mode --no-run
cargo test -p mev-scout --test cli_e2e --no-run
cargo test -p mev-scout --test cli_data_foundation --no-run

# آفلاین‌ها (باید بدون شبکه کامل پاس شوند)
cargo test -p mev-scout --test cli_args

# شبکه‌ای (فقط وقتی RPCها در دسترس‌اند)
$env:MEV_SCOUT_E2E = "1"
cargo test -p mev-scout --test cli_e2e
cargo test -p mev-scout --test cli_run_replay
cargo test -p mev-scout --test cli_live_mode
cargo test -p mev-scout --test cli_data_foundation
```

## ترتیب پیشنهادی اجرا

| مرحله | محتوا | تلاش تقریبی |
|---|---|---|
| ۱ | فاز ۰ (چهار باگ) | ~۱ ساعت |
| ۲ | فاز ۱ (هارنس) | ~۲ ساعت |
| ۳ | فاز ۲ (آفلاین‌ها) | ~۱ ساعت |
| ۴ | فاز ۳ (شبکه‌ای‌ها) | نیم روز (وابسته به RPC) |
| ۵ | فاز ۴ (بهداشت) | جداگانه، بعد از تثبیت تست‌ها |
