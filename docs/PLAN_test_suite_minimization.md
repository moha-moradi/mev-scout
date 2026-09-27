# PLAN — کوچک‌سازی مجموعه تست (Test Suite Minimization)

> وضعیت: **اجرا شده (کامل).** تاریخ ثبت: ۲۰۲۶-۰۹-۲۷ · پایان اجرا: ۲۰۲۶-۰۹-۲۷
> دامنه: فقط حذف تکرار و اصلاح تست‌های گمراه‌کننده. **هیچ تست جدیدی اضافه نشده است.**
> کامیت‌های اجرا: `a44aa5b`، `4d06f23` (سطوح ۱ تا ۵ به‌جز ۱.۱۰ و ۵.۵) و بازبینی ۱.۱۰ + ۵.۵.

---

## ۱. وضعیت فعلی (خط پایه، پیش از اجرا — `51fde0a`)

| | |
|---|---|
| تست یونیت داخل `core/src` + `cli/src` | **۲۹۵** |
| تست integration در `core/tests` + `cli/tests` | **۱۰۴** |
| مجموع | **۳۹۹** تست در ۷۱ فایل |
| کد تست در برابر کد اصلی | ~۷۰۰۰ خط در برابر ~۴۲۰۰۰ خط (۰.۷٪) |
| فایل‌های source با صفر تست | ۱۰۷ فایل |
| خطوط source بی‌پوشش (≥۱۵۰ خط) | ۳۹ فایل / ۱۵٬۵۷۳ خط |
| CI | `cargo test --workspace` (`.github/workflows/ci.yml:31`) |

نکته: خط پایهٔ اولیهٔ سند ۳۸۹ بود؛ اندازه‌گیری مجدد روی `51fde0a` عدد **۳۹۹** را می‌دهد (اختلاف ۱۰ تست یونیت).

---

## ۲. آنچه نباید دست زده شود

الگوی درست از قبل در کدبیس وجود دارد و استاندارد بالاست:

| فایل | چرا خوب است |
|---|---|
| `core/src/rpc/multicall.rs:360-412` | assert چیدمان ABI بایت‌به‌بایت |
| `core/src/pool/math/core.rs:500` | تست تفاضلی در برابر brute force |
| `core/src/pool/selectors.rs:93` | تست table-driven با ۳۷ کیس — **مدل مرجع برای همهٔ ادغام‌ها** |
| `core/src/replay/whatif.rs` | اجرای واقعی EVM، مسیر temp یکتا |
| `core/tests/common/setup.rs` | fixture مشترک خوب |
| `core/tests/mev_corpus.rs` · `explorer_corpus.rs` | داده‌محور و آفلاین |
| گیتینگ `MEV_SCOUT_E2E` | تست‌های لایو در CI خودشان skip می‌شوند؛ CI سریع است |

---

## ۳. سطح ۱ — حذف خالص (۱۰ تست، ریسک صفر)

| # | اقدام | محل |
|---|---|---|
| 1.1 | تکرار بایت‌به‌بایت. هر دو یک `swap`/`transfer`/`block` می‌سازند، `arb_likely_parity = false`، و فقط `events.is_empty()` را assert می‌کنند. تنها تفاوت، کامنت است. | حذف `core/src/explorer/classify.rs:1761-1777` (`two_entity_cycle_does_not_emit`)، نگه‌داشتن `:1714-1730` |
| 1.2 | تاوتولوژی: `flash_loan_topics().len() == 4`. شمارش یک لیست هاردکد هیچ باگی را نمی‌گیرد. | حذف کل `#[cfg(test)] mod tests` در `core/src/chain/flashloans.rs:101-109` |
| 1.3 | تاوتولوژی: `liquidation_topics().len() == 2` | حذف کل mod در `core/src/chain/liquidations.rs:90-98` |
| 1.4 | تاوتولوژی: `trade_topics().len() == 13` | حذف کل mod در `core/src/chain/trades.rs:113-121` |
| 1.5 | تستِ یک ثابت در برابر خودش: `DEFAULT_BATCH_SIZE == 500`. اگر ثابت عوض شود این تست شکست می‌خورد — دقیقاً کاری که برایش ساخته نشده. | حذف `core/src/chain/scanner.rs:115-120`؛ نگه‌داشتن `:122-124` (`with_batch_size_clamps_to_minimum`) |
| 1.6 | `chain/transfers.rs:58-67` همان `0xddf252ad…3ef` را assert می‌کند که `chain/events.rs:827-833` assert می‌کند | حذف کل mod در `core/src/chain/transfers.rs:54-68` |
| 1.7 | هر دو `is_unsupported_dex` را تست می‌کنند که پیاده‌سازی‌اش در `pool/discovery/remote/mod.rs:191` است | یکی از `remote/dexscreener.rs:326` یا `remote/geckoterminal.rs:682` را نگه دارید |
| 1.8 | سه assert روی یک accessor که set برمی‌گرداند | حذف `core/tests/arbitrage.rs:166-180` (`test_pool_addresses_filter`) |
| 1.9 | عبارت مرده در تست | `core/src/explorer/pricing.rs:610` (`let _ = address!(...)`) |
| 1.10 | تست self-referential — fixtureهای داخل `golden.rs` را با خود `golden.rs` مقایسه می‌کند | **انجام شد:** `swap()`/`transfer()` هر دو `tx_index: 0` و `log_index: 0` هاردکد می‌کردند و `tx()` آن‌ها را اصلاح نمی‌کرد، پس هر ۶ کیسِ labeled ادعا می‌کردند همهٔ factها در tx 0 هستند. `tx()` حالا `tx_index` و ترتیب `log_index` را مهر می‌کند — همان کاری که `from_logs` روی factهای decode‌شده انجام می‌دهد (`classify.rs:1289-1321`). |

---

## ۴. سطح ۲ — ادغام در table (حدود ۳۵ تست، assertion ها قوی‌تر می‌شوند)

الگوی مرجع: `core/src/pool/selectors.rs:93`.

| # | گروه | خطوط | از → به | صرفه‌جویی |
|---|---|---|---|---|
| 2.1 | شش تست `is_err()` که **هیچ‌کدام نمی‌گویند کدام قانون نقض شده** | `config/validation.rs:457-497` | ۶ → ۱ table که پیام دقیق `ConfigError::Validation` را assert کند | **-۵** |
| 2.2 | ۴ تست liquidation، هر ۴ یک شکل با ۴ مقدار متفاوت | `classify.rs:2186,2209,2237,2261` | ۴ → ۱ table | **-۳** |
| 2.3 | ۵ تست backrun/frontrun با ۳ پارامتر | `classify.rs:1896-2105` | ۵ → ۱ table | **-۴** |
| 2.4 | جفت flashloan netting؛ فقط وجود/عدم وجود leg پرداخت فرق دارد | `classify.rs:1817,1850` | ۲ → ۱ table | **-۱** |
| 2.5 | سه جفت موازی | `decode.rs:1346`+`:1372` (Aave V2/V3) · `:1272`+`:1288` (truncated mint) · `:1697`+`:1724` (Paraswap) | ۶ → ۳ table | **-۳** |
| 2.6 | **بزرگ‌ترین فرصت ادغام:** ۱۵ تست که همگی `keccak(signature) == b256!(...)` را pin می‌کنند، پخش در ۴ فایل | `chain/events.rs:827,835,843,853,865,874` · `explorer/decode.rs:1507,1550,1859` · `pool/decoders.rs:454` · `pipeline/scanner.rs:241,256,269,290,300` | ۱۵ → یک manifest table + یک تست «همه متمایز‌اند» | **-۱۱** |
| 2.7 | pinهای محتوای chain/address | `types/chain.rs:348,396,420,437,458,469` (هر ۶) + `config/defaults.rs:145,152,162,176` | ۱۰ → ۱ registry test روی `core/data/chains.toml` | **-۷** |

### ۲.۶ — استثناهایی که باید باقی بمانند

همهٔ ۱۵ تست pin نیستند. این‌ها را جدا نگه دارید چون علاوه بر ثابت، «سیم‌کشی» را هم می‌سنجند:

- `pipeline/scanner.rs:241` `v4_swap_topic_differs_from_v3` و `:256` `infinity_cl_swap_topic_is_distinct_and_scanned` — دومی assert می‌کند subject واقعاً اسکن می‌شود
- `chain/events.rs:853` `solidly_swap_topic_is_verified` و `:865` — علاوه بر pin، `assert_ne!` برای تشخیص collision دارند

پس عدد واقعی ۲.۶ احتمالاً **-۹** است، نه -۱۱.

### ۲.۷ — استثناها

- `config/defaults.rs:198` `merge_defaults_fills_unset_fields_keeps_overrides` و `:236` `merge_default_chains_partial_polygon_keeps_start_block` **تست منطق واقعی merge هستند، نه pin. حتماً نگه دارید.** فقط چهار تست اول (`:145,152,162,176`) محتوای config را pin می‌کنند.

---

## ۵. سطح ۳ — استخراج fixture (تعداد تست ثابت، ~۳۰۰ خط پاک)

| # | محل | مشکل |
|---|---|---|
| 3.1 | `explorer/store.rs` | `BlockFactsInput` **۱۴ بار** (`:632,2436,2506,2599,2684,2759,2926,2951,2989,3007,3122,3147,3194,3248`) · `open_in_memory()` ۱۴ بار · `pricing::TokenUsd{}` ۱۲ بار → دو builder: `seed_block()` و `token_usd()` |
| 3.2 | `explorer/classify.rs` | `JitFact{}` **۹ بار** (`:2281,2294,2332,2378,2391,2628,2641,2744,2757`) · `LiquidationFact{}` ۴ بار · `FlashLoanFact{}` ۲ بار → builder |
| 3.3 | `pool/math/v3.rs:713-740` | literal ۲۴ فیلدی `PoolInfo` که با هر فیلد جدید می‌شکند → `..Default::default()` (الگوی موجود: `pool/state/manager.rs:866`) |
| 3.4 | `explorer/validate.rs:946` در برابر `:1061` | همان closure رویداد دوباره تعریف شده |

> توجه: builder **تعداد تست را کم نمی‌کند**، فقط خطوط را. این‌ها را با ۲.۱–۲.۷ اشتباه نگیرید.

---

## ۶. سطح ۴ — دو تستی که اعتماد کاذب می‌دهند (مهم‌ترین بخش)

> این دو از تمام تکرارهای سطح ۱ و ۲ با هم خطرناک‌ترند، چون **تست سبز است ولی کد غلط را تأیید می‌کند.**

### ۴.۱ `cli/src/commands/discover.rs:182,208` — تست، کپیِ کد تولید را می‌سنجد

```
cli/src/commands/discover.rs:182   fn cached_to_discovered(existing: PoolInfo) -> DiscoveredPool
cli/src/commands/discover.rs:208   fn persist_universe(cache: &SqliteStore, ...) -> usize
        ↑ این‌ها توابع تست هستند (در mod tests)
core/src/jobs/discover.rs:101      fn cached_to_discovered(...)      ← نسخه واقعی
core/src/jobs/discover.rs:127      fn persist_universe(...)          ← نسخه واقعی
```

دو تست `persist_universe_roundtrips_remote_pools_with_dex_type` و `persist_universe_second_sparser_run_never_clobbers_cached_row` هرگز کد واقعی را صدا نمی‌زنند. هر باگی در `persist_universe` واقعی از این‌ها رد می‌شود.

**گزینه‌ها:**
- توابع `core/src/jobs/discover.rs` را `pub` کنید و در تست صدا بزنید، یا
- هر دو تست را به `core/tests/` منتقل کنید تا از crate بیرون و در برابر کد واقعی اجرا شوند

### ۴.۲ `core/src/config/settings.rs:1119-1122` — data race ادعا‌شده به‌اشتباه «safe»

```rust
// SAFETY: single-threaded test binary execution for this module; the
// variable name is test-specific.
std::env::set_var("MS_CONFIG_TEST_RPC_KEY", "sekret123");
```

ادعا **غلط** است: `cargo test` نخ‌های موازی اجرا می‌کند و `std::env::set_var` در Rust ۲۰۲۴ یک data race است. هر ۴ تست این mod (`expands_set_env_vars_in_rpc_urls`، `leaves_unset_vars_verbatim`، `expands_plain_urls_and_env_reference`، `unterminated_placeholder_is_kept_verbatim`) دست به محیط می‌زنند.

**راه‌حل:** سریالایز کردن با mutex — الگوی آماده در خود کدبیس: `cli/tests/common/mod.rs:14` (`RPC_MUTEX` با بازیابی از poisoned lock). گزینهٔ بهتر: تزریق یک map از env به‌جای `std::env::set_var`.

---

## ۷. سطح ۵ — تقویف assertion بدون تغییر تعداد

| محل | فعلی | مشکل |
|---|---|---|
| `core/src/chain/labels.rs:66` | `assert!(!db.is_empty(), "should have at least some bundled labels")` | تقریباً بی‌ارزش — یک label مشخصِ bundled را resolve کنید |
| `core/src/explorer/ingest.rs:725` | `assert!(len >= 4)` | عدد دقیق را assert کنید |
| `core/src/explorer/canonical.rs:139` | `starts_with("Sandwich\|0x3000")` | کل رشته را assert کنید |
| `core/src/pool/math/pendle.rs:100-109` | بازهٔ `1000 < out < 3333` | کامنت همین تابع `≈1666` می‌گوید — بازه ۳ برابر گشاده است |
| `core/tests/config.rs` (۶ تست) | — | **پیش‌فرض سند غلط بود.** ۵ تست باقی‌مانده تکراری نیستند: `to_toml_string`، `merge_cli`، `ConfigBuilder` و serdeِ `ResultsFile` **هیچ تست یونیتی ندارند** و همین ۵ تست تنها پوشش آن‌هاست. تنها تکرار واقعی `test_config_builder_empty_is_default` بود که هر سه assertش را `test_config_builder` از قبل پوشش می‌داد — حذف شد (**-۱**). |

---

## ۸. عمداً در این پلن نیامده

### ۸.۱ ماتریس تلورانس ۳ کپی — حذف نمی‌شود

`jobs/trace.rs:482-575` · `mev/verdict.rs:128-200` · `paper/recon.rs:254-368`

کپی بودنش آگاهانه و مستند است — `mev/verdict.rs:120` می‌نویسد: *"Mirror of the trace gate's five-case matrix"*. امضای توابع فرق می‌کند (`Option<f64>` دلاری در برابر `Option<i128>` وی) و سومی (`paper_vs_executed`) شکل بازگشتی متفاوتی دارد. یکی‌کردنش generic/macro می‌خواهد که پیچیدگی‌اش از سودش بیشتر است.

### ۸.۲ دو تکرار در کد **تولید**، نه تست

این‌ها تست را کم نمی‌کنند ولی بدهی واقعی‌اند و جداگانه باید بررسی شوند:

1. **`llama_chain_prefix` دوبار تعریف شده** — `core/src/explorer/pricing.rs:38` و `core/src/cache/token_meta.rs:18`.
2. **سیاست RPC سه نسخهٔ متفاوت دارد** و مستندات با هیچ‌کدام نمی‌خواند:
   - `cli/tests/README.md:56-58` به fallback تابع `first_rpc_url` اشاره می‌کند که **در کل کدبیس وجود ندارد** (تنها تطابق، خودِ همین README است)
   - `core/tests/common/setup.rs:22-27` — فقط env، و عمداً از هر fallback پرهیز می‌کند
   - `cli/tests/common/mod.rs` (`rpc_url`) — فقط env
   - `core/tests/e2e.rs:190` — env، سپس fallback به `ChainName::Polygon.public_rpc_url()`

---

## ۹. شکاف پوشش (فقط برای آگاهی — خارج از دامنهٔ این پلن)

| ماژول | خط | تست |
|---|---|---|
| `core/src/pool/state/factory.rs` | ۲۲۱۵ | **۰** — بزرگ‌ترین فایل کدبیس |
| `core/src/mev/detectors/*` (۶ فایل) | ۲۹۹۰ | **۰ یونیت** (فقط `multi_hop` ۵ تست) — پوشش integration نازک: `tests/liquidation.rs` ۴ + `tests/sandwich.rs` ۴ |
| `core/src/rpc/client.rs` | ۱۷۰۶ | ۳ تست، همه فقط classifier رشته‌اند |
| `core/src/pipeline/runner.rs` | ۱۴۵۴ | ۵ تست، همه فقط `update_persistence` |
| `core/src/fetch/fetcher.rs` | ۶۹۱ | **۰** |
| `core/src/replay/replayer.rs` | ۶۹۴ | **۰** |
| `core/src/sigs/fallback_data.rs` | ۵۸۳ | **۰** |
| `core/src/jobs/live.rs` | ۵۲۴ | **۰** |
| `core/src/cache/store/mod.rs` | ۴۸۴ | **۰** |

پیشنهاد: بودجهٔ صرفه‌جویی‌شده از این پلن را اینجا خرج کنید — **ولی طبق تصمیم فعلی، این پلن هیچ تستی اضافه نمی‌کند.**

---

## ۱۰. خالص نتیجه (اعداد واقعی پس از اجرا)

| | قبل (`51fde0a`) | بعد | هدف پلن |
|---|---|---|---|
| تست یونیت | ۲۹۵ | **۲۵۳** | ~۲۵۰ |
| تست integration | ۱۰۴ | **۱۰۲** | ~۱۰۳ |
| **مجموع** | **۳۹۹** | **۳۵۵** | ~۳۴۴ |

**۴۴ تست کمتر (۱۱٪)، بدون افت پوشش.** عدد `cargo test --workspace` = **۳۵۵ passed / 0 failed**.

دو تستی که قبلاً بی‌اثر بودند حالا واقعاً کد را می‌سنجند:

- `persist_universe` / `cached_to_discovered` از کپیِ تست در `cli/src` به `core/src/jobs/discover.rs` منتقل شد و کد واقعی را صدا می‌زند.
- ۴ تست `settings.rs` دیگر `std::env::set_var` نمی‌زنند؛ map از env تزریق می‌شود.

سه انحراف از تخمین اولیه:

1. **خط پایهٔ سند ۱۰ تست کمتر از واقع بود** (۳۸۹ در برابر ۳۹۹ اندازه‌گیری‌شده).
2. **یونیت ۴۲- تست کمتر شد، نه ~۳۵** — ادغام‌های سطح ۲ گسترده‌تر از برآورد اولیه درآمدند.
3. **۵.۵ عملاً اجرا نشد** — چون پیش‌فرض سند (تکراری بودن ۶ تست) غلط بود. تنها ۱ تست کم شد، نه ۶.

---

## ۱۱. ترتیب اجرا (انجام‌شده)

1. **سطح ۴** (۲ تست) — خطرناک‌ترین: این دو تست الان کد غلط را تأیید می‌کنند ✅
2. **سطح ۱** (۱۰ تست) — ریسک صفر، سریع ✅
3. **سطح ۲** (حدود ۳۵ تست) — بیشترین صرفه‌جویی ✅
4. **سطح ۳** (~۳۰۰ خط) — استخراج fixture ✅
5. **سطح ۵** — تقویح assertion ✅ (به‌جز ۵.۵ که پیش‌فرضش غلط بود)

### بررسی پس از هر مرحله

```powershell
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

هر سه سبز در پایان اجرا. `clippy --all-targets` مهم است: حذف یک helper که فقط تست‌ها استفاده می‌کردند، warning `dead_code` می‌دهد و CI خطا می‌دهد.
