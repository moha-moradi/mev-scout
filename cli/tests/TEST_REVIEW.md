# CLI Tests — Review & Gap Analysis

وضعیت تستهای `cli/tests` بهصورت کامل بررسی شد. این سند ارزیابی کیفیت، مشکلات تمیزی، و شکافهای پوشش را خلاصه میکند.

## کیفیت و تمیزی — نقاط قوت

- **هندسهی تست مشترک خوب است** (`common/mod.rs`):
  - `run_timed` با timeout و kill
  - `temp_ws` ایزوله
  - `temp_config` دستکاری TOML
  - `expect_ok` / `expect_fail` با پیامهای خطای مفید
  - `extract_json_array` آگاه از ANSI
  - `RPC_MUTEX` برای سریالسازی تستهای شبکهای
  - `ensure_gate_and_rpc` برای گیت کردن
- **جداسازی درست:** تستهای `cli_args.rs` همگی آفلایناند (کشف خطا یا چاپ config، بدون نیاز به شبکه)؛ بقیه با `MEV_SCOUT_E2E=1` گیت شدهاند.
- **تستهای شبکه مقاوم به ناپایداریاند** (warn-and-continue بهجای hard-fail روی RPCهای عمومی).
- **پایدار/تعیینپذیر:** بهجای وابستگی به محتوای بلاک، فیلدهای کلیدی ساختاری بررسی میشوند.

## مشکلات تمیزی (تکرار)

1. تست `live_duration_without_loop_rejected_offline` **دو بار یکسان** هست:
   - `cli_args.rs:112`
   - `cli_live_mode.rs:159`
2. هلپر `newest_live_json` (`cli_live_mode.rs:28`) و `newest_json_matching` (`cli_run_replay.rs:14`) تقریباً یکساناند و میتوانند به `common` منتقل شوند.
3. منطق `pick_factories` بین `discover.rs` و `validate_pools.rs` تکرار شده.

## شکافهای پوشش — چیزهایی که تست نشدهاند

### در سطح آفلاین (قابل تست بدون شبکه)

- اعتبارسنجی تعارض محدودهی بلاک:
  - ترکیب `--days` + `--blocks`
  - `--from-block` بدون `--to-block`
  - `to <= from`
  - `--days 0`
  - رد `replay --days`
  - اینها در `validation.rs` قبل از RPC رخ میدهند ولی فقط حالت «بدون محدوده» و «days>365» تست شدهاند.
- پرچمهای سراسری `--quiet` / `--verbose`.
- `run` / `fetch --batch-rpc`.
- `replay --tx-index`.
- `report --run-id` (انتخاب مثبت — فقط حالت منفی تست شده).
- `tokens --symbol` / `--decimals` / `--limit` و خروجی جدول پیشفرض.
- `config -f` با مسیر ناموجود.

### در سطح شبکه (E2E)

- `scan --kind` فقط `trades` تست شده؛ اینها خیر:
  - `transfers`
  - `flashloans`
  - `liquidations`
  - `labels`
  - `scan --address` / `--min-value`
  - خروجی CSV/جدول scan
- گزینههای `discover`:
  - `--source hybrid`
  - `--enrich`
  - `--min-tvl`
  - `--incremental`
  - `--resolve-remote-metadata`
  - `--health-check false`
  - `--solidly-fee-bps`
  - هشدار `--batch-size>5000`
- `validate-pools --source gecko` و `--markdown-out` (نوشتهشدن فایل markdown).

## نکتهی مهمتر

با `cargo test` معمولی فقط `cli_args.rs` واقعاً اجرا میشود؛ همهی تستهای pipeline ارزشمند بهصورت بیصدا skip میشوند مگر `MEV_SCOUT_E2E=1` ست شود. پس «چند تست واقعاً هر بار اجرا میشود» کم است.

## پیشنهاد برای کارهای بعدی

- [ ] پر کردن شکافهای آفلاین (تعارض محدودهی بلاک، `--quiet`/`--verbose`، `--batch-rpc`، فیلترهای `tokens` و …) — سریع و تعیینی.
- [ ] افزودن تستهای E2E برای scan kinds و گزینههای discover و `validate-pools --markdown-out`.
- [ ] حذف تکرارها: ادغام `newest_*` و حذف تست تکراری `live_duration_without_loop_rejected_offline`.
