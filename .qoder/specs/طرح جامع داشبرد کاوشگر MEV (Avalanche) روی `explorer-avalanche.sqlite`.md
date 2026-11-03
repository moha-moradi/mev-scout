# طرح جامع داشبرد کاوشگر MEV (Avalanche) روی `explorer-avalanche.sqlite`

## ۱. Context — چرا این تغییر لازم است

ماژول [core/src/explorer](file:///D:/gitlab.dte.repo/mev-scout/core/src/explorer) داده‌ی خام Avalanche C‑chain را از RPC می‌خواند (مسیر logs‑only، بدون trace)، دی‌کد و طبقه‌بندی می‌کند، سود را محاسبه و قیمت‌گذاری می‌کند و در یک SQLite تک‌زنجیره‌ای می‌نویسد. امروز **تنها** راه دیدن نتیجه، جداول stdout در CLI است (`table|csv|json`) و مستندات صریح می‌گویند «There is no web UI» (`docs/LOCAL_AVALANCHE.md:9`).

دو مشکل عملی که این طرح آن‌ها را حل می‌کند:

1. **عدم‌امکان کاوش اکتشافی.** تحلیل‌گر برای پرسیدن «چه کسی، از کدام استخر، با چه اطمینانی، و چقدر از این عدد قابل‌اتکا است؟» باید چندین بار CLI را با فلگ‌های مختلف اجرا کند. هیچ نمای زمانی، هیچ نمای استخر/مسیر، و هیچ نمای کیفیت قیمت‌گذاری وجود ندارد.
2. **یک خطر نوشتگی که همین حالا فعال است.** تنها سازنده‌ی فایل در کد، `ExplorerStore::open` است که **مهاجرت را اجرا می‌کند** ([core/src/explorer/store/schema.rs:52-67](file:///D:/gitlab.dte.repo/mev-scout/core/src/explorer/store/schema.rs#L52-L67))، و CLI از طریق آن باز می‌کند ([cli/src/commands/explorer.rs:12-14](file:///D:/gitlab.dte.repo/mev-scout/cli/src/commands/explorer.rs#L12-L14)). یعنی `mev-scout explorer report --db explorer-avalanche` فایلِ کهنه‌ی `cache/explorer-avalanche.sqlite` را بی‌سروصدا به v10 ارتقا می‌دهد (شامل دو rebuild کامل جدول در V5 و V8). یک داشبرد باید ساختاراً ناتوان از نوشتن باشد، نه «معمولاً» ناتوان.

**خروجی مورد انتظار:** یک زیر命令 جدید که یک فایل HTML خودبسنده تولید می‌کند؛ بدون سرور، بدون پورت، بدون وابستگی جدید، بدون امکان نوشتن روی دیتابیس، و با صداقت کامل درباره‌ی پوشش داده و کیفیت قیمت‌گذاری.

---

## ۲. فرض‌های صریح (از داده‌ی واقعی اندازه‌گیری‌شده)

این‌ها حدس نیستند؛ با باز کردن read‌only همان فایل اندازه گرفته‌ام. اگر کدام‌یک برای شما صادق نیست، بگویید تا طراحی عوض شود.

| # | فرض | شاهد |
|---|---|---|
| A1 | فایل هدف، `cache/explorer-avalanche.sqlite` است، روی **`PRAGMA user_version = 0`** — یعنی پیش از مهاجرت‌های V2..V10. جدول `mev_ops` فقط ۲۲ ستون دارد و `subkind`، `excluded_from_net` و ۵ پرچم کیفیت قیمت‌گذاری **وجود ندارند**؛ فقط ۴ ایندکس قدیمی روی `mev_ops` هست (no `mev_ops_natural`, `mev_ops_ts`, `mev_ops_tx_hash`). | اندازه‌گیری read‑only، در مقابل `cache/oracle-avalanche.sqlite` که `user_version=10` و ۲۹ ستون است |
| A2 | حجم داده **خیلی کوچک** است: ۱۳۹۳ بلاک، ۳۷۸۲ تراکنش، ۴۱۷۸ transfer، ۷۳۶ swap، و **۳۱ رویداد MEV**. بازه‌ی زمانی بلاک‌ها فقط **۲۴ دقیقه** است (۲۰۲۶‑۱۰‑۱۴ ۰۶:۵۸:۵۵ → ۰۷:۲۲:۵۵). | `blocks`/`mev_ops` |
| A3 | توزیع رویدادها: `unknown/exact=24`، `arb_atomic/inferred=5`، `jit/exact=2`. املا `unknown` همان پیش از V8 است که `MevKind::parse` امروز آن را به `Utility` نگاشت می‌کند ([core/src/explorer/types.rs:75](file:///D:/gitlab.dte.repo/mev-scout/core/src/explorer/types.rs#L75)). | `mev_ops` |
| A4 | داده‌ی USD **به‌شدت sparse** است: از ۳۱ ردیف، `gas_cost_usd` ۳۱، `profit_usd` و `net_profit_usd` ۲۶، **`volume_usd` فقط ۱**، و **`flashloan_fee_usd` صفر**. ⇒ هیچ KPI با «جمع» بدون کسر پوشش نمایش داده نمی‌شود. | `mev_ops` |
| A5 | فقط **۱۵ آدرس `eoa`** و **۸ «رقیب»** (`COALESCE(NULLIF(contract,''),eoa)`). ⇒ صفحه‌ی بازیگران بدون pagination و بدون «top‑N» واقعی ساخته می‌شود. | `mev_ops` |
| A6 | جدول `prices` ۲ ردیف دارد (AVAX و WAVAX، هر دو `$10.19`، `source=coingecko`) و `hour` سطلِ ساعتی است (`ts/3600`)، نه ثانیه. | `prices` |
| A7 | جداول `opportunities`، `rejected_candidates`، `paper_sessions`، `paper_fills` **صفر ردیف** هستند (و `record_rejections` پیش‌فرض خاموش است، [core/src/explorer/ingest.rs:132](file:///D:/gitlab.dte.repo/mev-scout/core/src/explorer/ingest.rs#L132)). ⇒ قیف رد (reject funnel) یک پنل «خالی به‌معنای خالی‑نبودِ ضبط» است، نه صفر رد. | شمارش ردیف‌ها |
| A8 | `sync_state` فقط یک ردیف دارد: `chain_id=43114`, `head=indexed_to=97509971`, `last_indexed_at=1791962580`. زنجیره فقط از **ساقه‌ی نام فایل** قابل استنتاج است؛ جدول‌های واقعی `chain` ندارند ([schema.rs:22-26](file:///D:/gitlab.dte.repo/mev-scout/core/src/explorer/store/schema.rs#L22-L26)). | `sync_state`, path rule در [core/src/config/settings.rs:614-620](file:///D:/gitlab.dte.repo/mev-scout/core/src/config/settings.rs#L614-L620) |
| A9 | ریسک هم‌زمانی: فایل WAL است و خواهرهای `-wal`/`-shm` کنارش وجود دارند. امروز `-wal` صفر بایت است، ولی اگر شاخص‌سازی زنده در میانه‌ی WAL‑recovery باشد، باز کردن read‑only می‌تواند خطا دهد. | `journal_mode=WAL` در [schema.rs:40-50](file:///D:/gitlab.dte.repo/mev-scout/core/src/explorer/store/schema.rs#L40-L50) |
| A10 | **انتخاب‌های قطعی شما:** خروجی HTML خودبسنده (بدون سرور) · اسنپشات دستی + `--repeat` · فقط همین فایل با تنزل ظریف · تک‌کاربره، لوپ‌تاپ + باکس SSH. | پاسخ به پرسش‌ها |
| A11 | مخاطب، مهندس/تحلیل‌گر forensic است نه مدیر ارشد: جدول‌های متراکم monospace، تم تیره، deep‑link قابل‌کپی در باگ‌ریپورت، و همیشه نمایش «چقدر از این عدد قابل اتکا است» در اولویت. | ماهیت پایپ‌لاین |
| A12 | ریپو Rust‑only است (هیچ `package.json`/`.tsx` وجود ندارد)، `rusqlite 0.31` با `bundled`، و lint gateها: `warnings=deny` + `unwrap_used/expect_used = deny` ([Cargo.toml:5-10,34](file:///D:/gitlab.dte.repo/mev-scout/Cargo.toml#L5-L10)). gateها: `cargo fmt --all --check`، `cargo clippy --workspace --all-targets -- -D warnings`، `cargo test --workspace` (`README.md:142-150`). | بررسی درخت |

---

## ۳. الگوی داده و منبع داده (Data model & sources)

**منبع داده فقط یکی است:** فایل SQLite. داشبرد هیچ RPC، هیچ API بیرونی و هیچ کلید API‌ای صدا نمی‌زند. این انتخاب عمداً انجام شده تا مسیر آفلاین روی باکس SSH و CI کار کند و `mev-scout.toml` (که کلیدهای زنده دارد) هرگز در مسیر ساخت artifact قرار نگیرد.

گروه‌های جدول که پنل‌ها از آن‌ها تغذیه می‌کنند:

- **Fact:** `mev_ops` (یک رویداد طبقه‌بندی‌شده به یک tx؛ کلید منطقی `(canonical_id, tx_hash)`).
- **Dimension/پوشش:** `blocks` (زمان زنجیره vs `indexed_at` زمان وال‌کلاک)، `blocks_classified` (دروازه‌ی idempotency و مبنای شکاف‌ها)، `sync_state`، `txs`، `labels` (۱۸ ردیف، همه `kind='searcher'`، `source='classifier'`).
- **Venue/جریان:** `swaps` (amm: v3 612، v2 56، aggregator 43، v4 25)، `transfers`، `jit_open_positions` (۲۲).
- **پشتیبان قیمت/اوراکل:** `prices`، `oracle_answers` (۶۹)، `reserve_rates` (۶).
- **JSON که ستون نیست:** `route_json[]` = `{pool, amm, token_in, token_out, token_source, amount_in, amount_out}` و `details_json` با کلیدهای مشاهده‌شده: `mode, tags, reasons, pnl_basis, pnl, route, arb_meta{hop_count,pool_count,dex_count,is_cross_dex,flashloan_funded,arb_shape,unique_tokens}, evidence, profit_tokens, protocol, pool, owner, tick_lower, tick_upper, liquidity, opened_block, burn_tx_index, held_blocks, amount0, amount1, bin_amm, fees_estimated, usd_approximate, paymaster, sender`.
- **خارج از دامنه در v1:** `opportunities`، `rejected_candidates`، `paper_*` (صفر ردیف) — فقط به‌صورت پنلِ «چرا خالی است» نمایش داده می‌شوند.

**قواعد معنایی که در لایه‌ی نمایش حکم پیمان را دارند (نه پانوشت):**

1. `NULL` یعنی «قابل قیمت‌گذاری نبود»، **هرگز صفر نه** (`store/pnl.rs:146-155,210-228`). فرمت‌کننده‌ی پول یک شاخه‌ی واحد برای `null → "—"` دارد و هیچ `SUM` سمت UI روی `null` صفر نمی‌کند.
2. ستون‌های USD از نوع f64 و تقریبی‌اند — «never read them as balances» (`schema.rs:333-336`). همیشه با پیشوند `≈` و با کسر پوشش.
3. **عدم‌تقارن `excluded_from_net`:** جمع `net` ردیف‌های دروازه‌شده را حذف می‌کند در حالی که `COUNT`/`gross`/`volume`/`gas`/`fee` آن‌ها را نگه می‌دارند (`query.rs:45-46`). روی فایل شما این ستون وجود ندارد ⇒ همه‌چیز «inclusive» و پنل صریحاً برچسب `schema v0: net-gating unavailable` می‌گیرد.
4. مبالغ رشته‌ی اعشاری U256 در **واحد خام** هستند؛ `0xff..ff` نشانه‌ی native (wei) است. humanize **فقط** وقتی که decimals از `core/data/known_tokens.json` بیاید؛ بقیه با برچسب `raw only (decimals unknown)`.
5. چهار ستون عملاً مرده در مسیر explorer: `blocks.producer`، `swaps.dex`، `swaps.sender`، `transfers.is_native` (`schema.rs:276-279`) ⇒ به‌جای سلول خالی، «not populated by the logs-only path».
6. `facets` (tag/reason) از **دیتا** ساخته می‌شود نه از literalهای کد: در این فایل `TRANSFER_CYCLE` وجود دارد، و در ریپو `forwarded_profit` تولید می‌شود ولی در `core/data/explorer_tags.json` نیست. فهرست hardcoded حتماً گم می‌کند.

---

## ۴. مدل بک‌اند

هسته در `core` زندگی می‌کند (به‌ همان دلیلی که کل منطق تجاری آنجاست) و CLI فقط argv و نوشتن فایل است.

### ۴.۱ باز کردن بدون مهاجرت — `ExplorerStore::open_read_only`

`configure_connection` نمی‌تواند استفاده شود چون `PRAGMA journal_mode=WAL` روی اتصال read‑only شکست می‌خورد ([schema.rs:44](file:///D:/gitlab.dte.repo/mev-scout/core/src/explorer/store/schema.rs#L44)). نسخه‌ی read‑only فقط `busy_timeout` می‌زند. الگوی موجود برای اقتباس: `SQLITE_OPEN_READ_ONLY` در [core/tests/explorer_kind_census.rs:291](file:///D:/gitlab.dte.repo/mev-scout/core/tests/explorer_kind_census.rs#L291) و در `core/src/sigs/resolver.rs`.

```rust
pub const SCHEMA_HEAD: i64 = MIGRATIONS.len() as i64;           // امروز ۱۰
pub fn open_read_only(path: impl AsRef<Path>) -> anyhowResult<Self>;
pub fn user_version(&self) -> i64;
```

### ۴.۲ Capability probe به‌جای فرض نسخه

```rust
pub struct StoreCaps { user_version, has_subkind, has_excluded_from_net,
                       has_pricing_flags, has_ops_ts_index, tables: HashSet<String> }
```
از `PRAGMA table_info(mev_ops)` + `sqlite_master`. خروجی در envelope ثبت می‌شود تا هر artifact خودش را توضیح دهد.

### ۴.۳ تصمیم کلیدی معماری: **تک‌reducer در Rust، SQL فقط برای پوشش**

این مهم‌ترین انحراف من از طرح اولیه است. سطح خواندن موجود **روی فایل شما کامل کار نمی‌کند**: `MEV_OP_COLUMNS` نام `subkind` را می‌برد ([query.rs:39-40](file:///D:/gitlab.dte.repo/mev-scout/core/src/explorer/store/query.rs#L39-L40))، `NET_USD_AGG` نام `excluded_from_net` را ([query.rs:45-46](file:///D:/gitlab.dte.repo/mev-scout/core/src/explorer/store/query.rs#L45-L46))، و `report_window_overview` پرچم‌های V10 را ([query.rs:473-478](file:///D:/gitlab.dte.repo/mev-scout/core/src/explorer/store/query.rs#L473-L478)). اگر پنل‌ها را به این متدها گره بزنیم، داشبردِ فایلِ درخواستیِ شما تقریباً خالی می‌شود.

بنابراین:
- **یک مسیر خواندن:** `ops_window(since, until)` با **projection تطبیقی** — ستون‌های موجود از `SELECT` می‌روند و ستون‌های غایب به `Option::None` نگاشت می‌شوند. یک `Vec<OpRecord>` کوچک.
- **یک مسیر تجمیع:** همه‌ی KPIها، سری زمانی با سطل خودکار، ماتریس `kind × subkind × confidence`، لیدربرد بازیگران، لیدربرد استخرها، فیسِت‌های tag/reason و شمارش‌های کیفیت **در Rust روی همان رکوردها** محاسبه می‌شوند. با ≤۱۰⁴ رویداد این بی‌اهمیت است، و یک‌بار semantics نوشته می‌شود.
- **SQL فقط برای چیزهایی که ردیف‌شان در حافظه جا نمی‌شود یا JSON لازم ندارند:** شمارش بلاک‌ها و پوشش (`blocks_in_window` semantics)، `sync_state`، شمارش جدول‌ها برای پنل سلامت، و شکاف‌ها (`unclassified_blocks` در [store/sync.rs:51](file:///D:/gitlab.dte.repo/mev-scout/core/src/explorer/store/sync.rs#L51) — حتماً پشت فلگ، چون CTE بازگشتی است).
- **مزیت جانبی:** `json_each(route_json)` که گران‌ترین عبارت این پایپ‌لاین است ([query.rs:163-170](file:///D:/gitlab.dte.repo/mev-scout/core/src/explorer/store/query.rs#L163-L170)) در مسیر داشبرد حذف می‌شود؛ مسیر را یک‌بار در Rust parse می‌کنیم و ایندکس `pool → [op_id]` را می‌سازیم.
- **ضامن ضد واگرایی:** یک تست reconcilation روی دیتابیس v10 (`cache/oracle-avalanche.sqlite`، با skip اگر نبود) که مجموع Rust‑computed را با `report_window_overview` و `report_by_kind` مقایسه می‌کند. یعنی semanticsِ `query.rs` منبع حقیقت می‌ماند و مسیر تازه نمی‌تواند بی‌صدا از آن جدا شود.

### ۴.۴ نقطه‌ی زمانی یکنواخت

کل build داخل یک `conn.unchecked_transaction()` (همان ابزار مسیر نوشتن، `query.rs:140`) تا اگر `explorer index --loop` میانه‌ی ساخت commit کرد، پنل‌ها از دو ارتفاع بلاک نیایند. بعد از commit هیچ چیزی نوشته نشده است.

### ۴.۵ envelope نسخه‌دار

```
api_version: 1 · generator.version · generated_at · db: {path, chain_label, chain_id,
size_bytes, user_version, schema_head, status} · caps · window {label, since_ts, until_ts, bucket_secs}
· panels{} · coverage · quality · truncation · semantics{null_usd_means, usd_columns_are, net_excludes, amounts_are}
```
`semantics` و `caps` عمداً **داخل payload** می‌روند تا هر مصرف‌کننده‌ی `--format json` همان هشدارها را بگیرد، نه فقط مرورگر.

### ۴.۶ پیکربندی و اسرار

- `--db <path>` برنده است و با **صفر config** کار می‌کند. بدون آن، از `Config::effective_explorer_db_path(&chain)` مثل [cli/src/commands/explorer.rs:12-14](file:///D:/gitlab.dte.repo/mev-scout/cli/src/commands/explorer.rs#L12-L14).
- **هرگز `validation::validate_live` صدا نمی‌شود** (کاری که `explorer report` امروز می‌کند، `cli/src/commands/explorer/report.rs:235`) — خواندن یک فایل به RPC‑resolution نیاز ندارد.
- خروجی RPC URL / `rpc_urls` در هیچ panel یا log مجاز نیست (placeholderهای `${ENV}` با کلید زنده expand می‌شوند).
- پیش‌فرض `--out` زیر `results/` که از قبل gitignore است.

---

## ۵. ساختار کلی داشبرد و بخش‌های اصلی

تک‌صفحه با rail چپ (ناوبری پنل‌ها) + نوار بالای چسبان. هر پنل دو چیز را همیشه نشان می‌دهد: **مبنای عدد** (`SQL/window/filters` یا `sample · n of N`) و **سلامت schema/پوشش** خود.

| پنل | محتوا | داده | نکته‌ی صداقت |
|---|---|---|---|
| **P0 سربرگ واقعیت داده** | نام فایل، `chain_label` از ساقه، `user_version` vs `SCHEMA_HEAD`، اندازه، **بازه‌ی پوشش: ۲۴ دقیقه**، `generated_at` | metadata | بزرگ‌ترین بدفهمی احتمالی اینجاست: پنجره‌ی ۲۴ دقیقه نباید مثل ۳۰ روز خوانده شود |
| **P1 سلامت و پوشش** | `head`/`indexed_to`/`last_indexed_at`، تعداد بلاک ایندکس‌شده در پنجره، تراکم رویداد (۳۱ در ۱۳۹۳ بلاک ≈ ۱ به ازای ۴۵ بلاک)، فاصله‌ی `blocks.ts` با `indexed_at` | `sync_state`,`blocks`,`blocks_classified` | پوشش **قبل از** هر عدد USD؛ شاخص‌سازی ۲۰ روز است |
| **P2 شکاف‌های ایندکس** (پشت `--with-gaps`) | تعداد و اولین/آخرین شکاف، نه کل بردار | `unclassified_blocks` | CTE گران؛ clamp به سقف بلاک |
| **P3 شبکه‌ی KPI** | ops، volume، gross، gas، flash‑fee، net، بالاترین تکی، distinct searchers/competitors | reducer | هر عدد با کسر پوشش روبرو: `volume_usd 1/31 priced`، `flash_fee 0/31` |
| **P4 نوار کیفیت قیمت‌گذاری** | priced/unpriced، exact/inferred، `usd_approximate`، multi‑asset، clamped، native‑unpriced، excluded‑from‑net | `details_json` + پرچم‌ها | **هرگز collapse نمی‌شود**؛ این پنلِ «چقدر اعتماد کنم» است |
| **P5 سری زمانی** | dual‑axis: شمارش ops و net USD، به‌همراه **نوار نیمه‌شفاف پوشش بلاک** در هر سطل | `mev_ops.ts` | سطل خودکار (۱/۵/۱۵ دقیقه، ساعت، روز) با درج سطل در عنوان محورها؛ `report_daily` اینجا بی‌فایده است چون ۳۱ رویداد در یک سطل روزانه می‌افتند؛ شکاف ایندکس ≠ شکاف MEV |
| **P6 ماتریس kind × subkind × confidence** | هر ۹ kind با صفرهای صریح (`MevKind::ALL`)، rollup `subkind`، و **نرمال‌سازی `unknown → utility` با شمار «۲۴ ردیف با املاى کهنه»** | `mev_ops` | اگر صفر را حذف کنیم، نبودِ sandwich معنی‌دار نمی‌شود |
| **P7 بازیگران** | برای هر رقیب: ops (inclusive)، gross (inclusive)، net (exclusive)، **excluded_ops** | `eoa`/`contract` + `labels` | بدون هر سه USD و ستون excluded، `net < gross` غیرقابل توضیح است |
| **P8 استخرها و مسیرها** | لیدربرد استخر،Mix AMM از `swaps`، صفحه‌ی هر استخر با لیست ops و **رنگ هر leg بر پایه‌ی `token_source`** | `route_json` در Rust | قانون: تنها `registry`/`transfer` می‌توانند نرخ USD بسازند؛ `proximity` هرگز ([core/src/explorer/pricing.rs:175-180](file:///D:/gitlab.dte.repo/mev-scout/core/src/explorer/pricing.rs#L175-L180)) |
| **P9 جدول رویدادها** | آرایه‌ی `MevOpRow` با مرتب‌سازی پایدار `block_number, tx_index, id` | `mev_ops` | NULL → `—`؛ فلگ‌های `subkind`/`confidence`/`pnl_basis`/`detector` دیده می‌شوند |
| **P10 دریل‌داون یک رویداد** | `details_json` بخش‌بندی‌شده (`arb_meta`, `pnl`, `profit_tokens`, `reasons`, `tags`, JIT/tick, `fees_estimated`) + legs مسیر + `victim_hashes` + ردیف `txs` برای gas + برچسب آدرس + برش اوراکل اطراف بلاک | per‑op | deep‑link `#/op/<tx_hash>`؛ مبالغ خام با برچسب decimals یا «unknown» |
| **P11 فیسِت‌های کیفیت/علت** | tag و reason با ops/gross/net در هر مقدار، + برچسب **out‑of‑catalogue** | از دیتا | فهرست از `DISTINCT` ساخته می‌شود؛ `forwarded_profit` مثال زنده‌ی خارج‑از‑کاتالوگ است |
| **P12 قیف رد** | نردبان ۲۲ دلیل `RejectReason` با صفرهای صریح + پیام «ضبط خاموش است» | `rejected_candidates` | خالی = نبودِ ضبط، نه نبودِ رد؛ `COALESCE(b.ts, r.created_at)` زمان mixed دارد |
| **P13 چرخه‌ی عمر JIT** | دو ناحیه‌ی جدا: **realized** (ops با tick range و `held_blocks`) و **detector state** (`jit_open_positions`، ۲۲ ردیف) | `mev_ops` + `jit_open_positions` | دوم هیچ‌وقت به‌عنوان واقعیت زنجیره ارائه نمی‌شود؛ `fees_estimated` تخمین است نه اندازه‌گیری |
| **P14 فیدهای اوراکل و نرخ‌ها** | سری `oracle_answers` (۶۹/۳۳ feed) و `reserve_rates` (۶)، `prices` با هشدار convention دو‑ساعته، و برش ±۳۰۰ بلاک برای `interest_accrued` | جداول feed | [core/src/explorer/interest_attr.rs:18](file:///D:/gitlab.dte.repo/mev-scout/core/src/explorer/interest_attr.rs#L18)؛ روی این فایل liquidation صفر است ⇒ پنل پیام «هیچ تصفیه‌ای در پنجره» می‌گیرد نه نمودار خالی |
| **P15 سلامت ذخیره‌سازی** | شمارش هر جدول، `rows/day` برای `transfers`، status پرچم‌های مرده، جدول‌های همیشه‌خالی | `PRAGMA`, `sqlite_master` | بدهی retention قابل اندازه‌گیری است: `prune_transfers` هیچ فراخوانی ندارد ([store/facts.rs:380-388](file:///D:/gitlab.dte.repo/mev-scout/core/src/explorer/store/facts.rs#L380-L388)) |

---

## ۶. جریان داده

```
نوشتن (دست‌نخورده):  explorer index --loop → ingest → decode → classify → profit → insert_block_facts
                     → cache/explorer-avalanche.sqlite  (WAL)

خواندن (تازه):  mev-scout explorer dashboard --db cache/explorer-avalanche.sqlite --window 24h --out results/dash.html
  1 resolve db path (بدون config/بدون validate_live)
  2 open_read_only            ← بدون migrate، بدون PRAGMA journal_mode
  3 StoreCaps::probe          ← user_version در برابر SCHEMA_HEAD
  4 BEGIN read txn            ← یک نقطه‌ی زمانی برای همه‌ی پنل‌ها
  5 SELECT تطبیقی ops_window + شمارش‌های پوشش/جداول + فیدها
  6 reducer: KPI, سری, ماتریس, لیدربردها, فیسِت‌ها, ایندکس pool→ops
  7 COMMIT                    ← هیچ نوشتگی؛ فقط بستن تراکنش read
  8 DashboardSnapshot → serde_json → escape → درون template
  9 atomic write (--out)      ← گزارش اندازه + نوتیف truncation
```
**رفرش:** دستی تکرار کامند؛ `--repeat 60s` با atomic rename همان فایل را بازتولید می‌کند تا reload مرورگر نزدیک‌به‌روز باشد. عمداً «live» نیست: artifact باید سن خود را اعلام کند (`generated_at` + `head/indexed_to` در بنر)، چون در بازبینی forensic یک عددِ کهنه‌ی بی‌برچسب بدتر از عددِ تازه نیست.
**دریل‌داون:** صفر round‑trip — evidence map درونlined است و با `tx_hash` کلید می‌شود.

---

## ۷. مدل فرانت‌اند

- **یک فایل، بدون build step.** template با `include_str!("../../../data/dashboard_template.html")` — همان الگوی `core/src/cache/token_cache.rs:126`. بدون npm، بدون `rust-embed`.
- **داده به‌صورت `<script id="snapshot" type="application/json">`.** چون payload inline است، نه fetch، پس **CORS معنا ندارد و `file://` کار می‌کند** — همین کل امکان‌پذیری گزینه‌ی بدون‌سرور است.
- **`</script>` یک باگ درست است نه سلیقه:** `serde_json` علامت `<` را escape نمی‌کند و هر `details_json` حاوی `</script>` بلوک را می‌شکند ⇒ `.replace("<", "\\u003c")` + تست.
- **نمودار: SVG دستی.** ECharts (~۲۵۰KB) و Chart.js (~۷۰KB) یا باید vendor شوند (blob باینری غیرقابل بازبینی) یا از CDN بیایند (که روی SSH آفلاین می‌شکند). سقف سری‌ها ~۳۶۰ نقطه و ردیف‌های لیدربرد ≤۱۰۰ است؛ ~۲۰۰ خط helper (`linePath`, `bars`, `stackedBars`, `scatter`, `heatCell`) کافی است، `viewBox`‑محور، تم CSS، tooltip با `<title>`. Canvas لازم نیست (<۵k primitive).
- **فیلترها:** `window, kind, subkind, confidence, tag, reason, competitor, pool, amm, min_net_usd, priced_only, q`. ماندگاری در **hash fragment** نه query string — چون `location.search` زیر `file://` نوشتنی نیست. سه لایه‌ی فیلتر با برچسب صریح روی هر پنل: aggregate (Rust/SQL، فقط `window/kind/confidence`) · cross‑tab (یک فیسِت در هر بار) · sample (ترکیب دلخواه روی ردیف‌های ارسالی). فیلتر چندفیسِتی روی artifact کوتاه‌شده از sample حساب می‌شود و خط‌کش روی جمع USD می‌گیرد.
- **بودجه‌ی بایت:** `--max-ops` (پیش‌فرض ۵۰۰۰)، `--evidence` (پیش‌فرض ۲۰۰)، `--budget-mb` (پیش‌فرض ۸) با اولویت ثابت: aggregate همیشه، بعد ردیف لاغر، بعد blob evidence؛ `--max-0` حالت aggregate‑only (~۵۰KB). `truncation.hit_budget` به نوار قرمز روی پنل‌های sample‑محور تبدیل می‌شود.
- **اکسپورت CSV** با هدر سازگار با `csv_lines` در `report.rs` تا خروجی داشبرد و CLI با هم reconcile شوند.

---

## ۸. انتخاب‌های تکنولوژی و دلیل (کارایی / مقیاس / مدیریت داده / UX)

| انتخاب | رد‌شده | دلیل |
|---|---|---|
| HTML استاتیک تولیدشده توسط CLI | axum+SPA | حجم ۳۱–۱۱۷ رویداد؛ بدون ~۱۰۰ crate جدید، بدون فعال‌کردن `tokio` net/http، بدون پورت روی باکسی که RPC‌اش `127.0.0.1` است؛ و «نمی‌تواند migrate کند» یک **ویژگی ساختاری** می‌شود نه انضباط |
| `rusqlite` موجود، read‑only | sqlx / pool | اتصال `Send` ولی نه `Sync` ([store/mod.rs:1-16](file:///D:/gitlab.dte.repo/mev-scout/core/src/explorer/store/mod.rs#L1-L16))؛ داشبرد تک‌رشته‌ای باقی می‌ماند و این محدودیت را دور نمی‌زند |
| reducer در Rust با projection تطبیقی | گره‌زدن به `query.rs` | سطح خواندن فعلی روی schema v0 خطای `no such column` می‌دهد؛ تنزل ظریف بدون نوشتن SQL موازیِ بی‌معنا |
| SVG دستی | ECharts/Chart.js | offline، قابل‌بازبینی، درشت‌نمایی مرورگر، بدون blob vendor‌شده |
| envelope با `api_version` و `semantics` در payload | schema ضمنی | artifactِ کهنه روی دیسک باید hard‑error بدهد نه بی‌صدا غلط نمایش دهد |
| `results/*.html` gitignored | commit کردن artifact | داده‌ی زنجیره در VCS؛ و جلوگیری از leakage |
| بدون Node toolchain | Vite/React | ریپو Rust‑only است و دوبار اسکریپت‌های خارج‌از‌درخت را حذف کرده (`3b72539`, `164921d`) — این یک ترجیح اعلام‌شده است |

**مقیاس‌پذیری (چون پرسیدید):** رشد واقعی در `mev_ops` نیست (~۲.۵KB/ردیف) بلکه در جدول‌های حقیقت است — `transfers` با ~۴۷۲k ردیف/روز و بدون prune فعال. پاسخ داشبرد: (i) بودجه‌ی بایت به‌عنوان یک **flag** نه crash، (ii) حالت aggregate‑only برای پنجره‌های بلند، (iii) P15 که بدهی retention را قابل مشاهده می‌کند. اصلاح ساختاری (فراخوانی prune با افق نگهداری، و ایندکسپوششی برای >۱۰⁵ رویداد) **خارج از این طرح** و به‌عنوان کار follow‑up ثبت می‌شود.

---

## ۹. فایل‌ها

**جدید**
- `core/src/explorer/dashboard/mod.rs` — `DashboardSnapshot`, `API_VERSION`, `build()`
- `core/src/explorer/dashboard/caps.rs` — `StoreCaps::probe`
- `core/src/explorer/dashboard/reduce.rs` — تک‌reducer (KPI، سری، ماتریس، لیدربرد، فیسِت، ایندکس استخر)
- `core/src/explorer/dashboard/evidence.rs` — parse typer‌شده‌ی `details_json`/`route_json` + resolve decimals از `core/data/known_tokens.json`
- `core/src/explorer/dashboard/render.rs` — snapshot → HTML (inline + escape)
- `core/data/dashboard_template.html` — shell + CSS + SVG helpers + hash router
- `cli/src/commands/explorer/dashboard.rs` — argv، `--format html|json`، نوشتن اتمی، `--repeat`
- `core/tests/explorer_dashboard_readonly.rs` — تست نگهبان کل پروژه
- `cli/tests/cli_dashboard.rs` — قرارداد CLI، آفلاین

**اصلاح (کوچک و append‑only)**
- [core/src/explorer/store/schema.rs](file:///D:/gitlab.dte.repo/mev-scout/core/src/explorer/store/schema.rs) — `SCHEMA_HEAD`، `open_read_only`، `user_version()`
- [core/src/explorer/store/mod.rs](file:///D:/gitlab.dte.repo/mev-scout/core/src/explorer/store/mod.rs) — re‑export
- [core/src/explorer/mod.rs](file:///D:/gitlab.dte.repo/mev-scout/core/src/explorer/mod.rs) — `pub mod dashboard;`
- [core/src/explorer/store/query.rs](file:///D:/gitlab.dte.repo/mev-scout/core/src/explorer/store/query.rs) — فقط افزودن متد (ویرایش متدهای فرود‌آمده ممنوع؛ `MEV_OP_COLUMNS`/`NET_USD_AGG` قفل‌lockstep هستند)
- [cli/src/cli.rs](file:///D:/gitlab.dte.repo/mev-scout/cli/src/cli.rs) + [cli/src/commands/explorer.rs](file:///D:/gitlab.dte.repo/mev-scout/cli/src/commands/explorer.rs) — subcommand و arm
- `docs/CLI.md` (کنار §3.6)، `docs/ARCHITECTURE.md` (جدول artifact)، و **اصلاح `docs/LOCAL_AVALANCHE.md:9`** به «no *served* UI; `explorer dashboard` emits a static file»

---

## ۱۰. مراحل ساخت (هر مرحله مستقل قابل‌حمل و تست)

1. **M0 — دسترسی بدون مهاجرت.** `open_read_only` + `user_version` + `SCHEMA_HEAD` + `StoreCaps`. تست‌ها: باز کردن فایل v0 و اثبات `user_version` هنوز ۰ و هنوز ۲۲ ستون؛ `INSERT` ⇒ `SQLITE_READONLY`؛ فایل نبود ⇒ خطای نام‌بر مسیر؛ و `core/tests/explorer_dashboard_readonly.rs`: sha256 قبل و بعد از یک sweep کامل read باید یکسان باشد و `-wal` رشد نکند. **این مرحله تنهایی خطر نوشتگی را برای همیشه می‌بندد.**
2. **M1 — envelope v1 + `--format json`.** snapshot و KPI/کیفیت/پوشش. باید **بدون `mev-scout.toml` و بدون `RPC_URL`** پاس شود؛ و با خروجی `explorer --output json` فعلی reconcile شود.
3. **M2 — reducer کامل.** سری با سطل خودکار، ماتریس `kind×subkind×confidence` با نرمال‌سازی `unknown`، لیدربردها با هر سه USD + `excluded_ops`، فیسِت از دیتا، ایندکس استخر بدون `json_each`. تست‌ها: reconcile با `report_window_overview` روی v10؛ bucket با `bucket=86400` جمع‌های `report_daily` را بازتولید کند؛ یک tag که در هیچ enum نیست (`forwarded_profit`) در فیسِت پیدا شود.
4. **M3 — artifact استاتیک.** template + SVG + chips + hash router + escape test. بررسی دستی از `file://` روی ویندوز و روی لینوکس هدلس: بدون console error، بدون network request.
5. **M4 — جدول رویداد + دریل‌داون.** `--evidence`، deep‑link، humanize فقط با decimals شناخته‌شده.
6. **M5 — استخر/JIT/فیدها + سلامت ذخیره‌سازی** (P8, P13, P14, P15) و قیف رد با پیام خالی‑بودن‑ضبط.
7. **M6 — `--window`/`--with-gaps`/`--repeat` و سفت‌کردن بودجه** + مستندات و اصلاح `LOCAL_AVALANCHE.md`.

---

## ۱۱. راستی‌آزمایی

```
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo test -p mev-scout-core explorer_dashboard
cargo test -p mev-scout-core --lib explorer::store
cargo test -p mev-scout-cli --test cli_dashboard

# اثبات بدون‑نوشتگی (مهم‌ترین چک، روی فایل خودش):
sha256sum cache/explorer-avalanche.sqlite
cargo run -p mev-scout-cli -- explorer dashboard --db cache/explorer-avalanche.sqlite --out results/dash.html
sha256sum cache/explorer-avalanche.sqlite   # باید یکسان باشد

# reconcile با CLI موجود:
cargo run -p mev-scout-cli -- explorer dashboard --db cache/explorer-avalanche.sqlite --format json
cargo run -p mev-scout-cli -- explorer --windows all --output json
#  gross / net / ops باید برای پنجره‌ی مشترک برابر باشد

# دود روی schema v10 (اثبات اینکه پنل‌های gated فعال می‌شوند):
cargo run -p mev-scout-cli -- explorer dashboard --db cache/oracle-avalanche.sqlite --out results/dash-v10.html
```
معیار پذیرش M0–M3: فایل شما — همان `explorer-avalanche.sqlite` روی schema v0 — یک artifact کامل تولید کند که در آن P0 صریحاً بنویسد schema v0 است و ۲۴ ردیف `unknown` نرمال‌سازی شده‌اند، و هیچ پنلی خطای SQL ندهد.

---

## ۱۲. ریسک‌ها و گاردریل‌ها

| ریسک | گاردریل |
|---|---|
| مهاجرت تصادفیِ فایل کهنه | ساختاراً: فقط `open_read_only` در مسیر داشبرد + تست sha256 (M0). ارتقا فقط به‌صورت زیر命令 صریح و opt‑in با backup `.pre-upgrade.bak` — **خارج از دامنه‌ی این طرح**، جدا تصمیم گرفته می‌شود |
| V7 در صورت duplicate رد می‌کند ([schema.rs:117-151](file:///D:/gitlab.dte.repo/mev-scout/core/src/explorer/store/schema.rs#L117-L151)) | امروز ۰ duplicate اندازه گرفته شد؛ به‌هرحال مسیر read هرگز به آن وابسته نیست |
| نمایش f64 USD به‌عنوان موجودی | همیش `≈` + کسر پوشش + `semantics` در payload |
| NULL که صفر به‌نظر برسد | یک فرمت‌کننده، یک شاخه‌ی `null → "—"`؛ تست روی ردیف `profit_usd = None` |
| عدم‌تقارن `excluded_from_net` روی v0 | نبودِ ستون یک **capability** است؛ پنل net‑gating برچسب unavailable و توضیح می‌گیرد نه عدد غلط |
| `-wal`/`-shm` در میانه‌ی recovery | یک retry بعد از ۵۰۰ms و پیام تشخیصی؛ **هرگز** `immutable=1` (در حضور writer زنده غلط است) |
| هزینه‌ی `json_each` و `unclassified_blocks` | حذف اولی از مسیر داشبرد؛ دومی پشت فلگ با clamp |
| leakage از config زنده | بدون `validate_live`، بدون چاپ RPC URL، `--db` مسیر مستقیم |
| lint gateها (`unwrap_used` deny) | همه‌ی مسیرها `Result` + `?`؛ unwrap فقط در `#[cfg(test)]` (کلاپس `clippy.toml`) |
| ambiguity تک‌زنجیره | `chain_id` از `sync_state` + `chain_label` از نام فایل، در P0؛ بدون ادعای cross‑chain |

---

## ۱۳. خارج از دامنه (عمداً)

سرور HTTP/SPA، هر نوع auth، multi‑DB compare، نوشتن روی دیتابیس، فعال‌کردن `record_rejections`، فراخوانی `prune_transfers`، مهاجرت خودکار، و هر پنلی که داده‌ی موجود در این فایل پاسخ آن را نمی‌دهد (sandwich/frontrun/backrun/liquidation/skim در این پنجره صفراند — پس فقط به‌صورت صفرهای صریح در P6 ظاهر می‌شوند).