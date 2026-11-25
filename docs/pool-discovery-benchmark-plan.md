# پلن: Indexing استخرهای ۳۰ روز اخیر بدون `eth_getLogs` + بنچمارک متقابل

## زمینه و تصمیم کلی

- هدف: لیست on-chain استخرهای ۳۰ روز اخیر، **بدون `eth_getLogs`**.
- وضع فعلی: نود لوکال **نداریم**. تنها peerها: `publicnode` (رسیت ۳۰روزه) و `Dwellir` (آرشیو؛ `eth_call` historic هنوز تست نشده).
- مسیر اصلی پیشنهادی: **گزینه ۲ = اسکن `eth_getBlockReceipts` + فیلتر محلی لاگها**.
- گزینه ۱ (factory state diff با `eth_call` آرشیو) **حذف نمیشود**؛ چون اصلاً کدی وجود ندارد که حذف شود. بهعنوان **fast-path اختیاری** نگه داشته میشود و تنها در صورتی وارد مسیر پیشفرض میشود که بنچمارک برنده شود.
- حالت فعلی `hybrid` دستنخورده باقی میماند (پیشفرض).

---

## فاز ۰ — Probe قابلیت RPCها (فقط خواندن)

نتیجهٔ این فاز تعیین میکند کدام methodها اساساً قابل اجرااند.

| تست | متد |peer | هدف |
|---|---|---|---|
| P1 | `eth_blockNumber` + `eth_getBlockReceipts(tip)` | publicnode, Dwellir | سلامت endpoint |
| P2 | `eth_getBlockReceipts(tip-30d)` | publicnode, Dwellir | retention رسیت ۳۰روزه |
| P3 | JSON-RPC batch شامل ۴ `eth_getBlockReceipts` | publicnode, Dwellir | پشتیبانی batch |
| P4 | `eth_call(allPoolsLength())` @ latest و @ tip-30d | Dwellir | قابلیت آرشیو → در دسترس بودن گزینه ۱ |
| P5 | `eth_getLogs` بدون address (@tip) | publicnode, Dwellir | تأیید مجدد محدودیت (`-32701`/403) |

نتیجه در جدولی بهنام `capability matrix` ثبت میشود و مبنای فاز بنچمارک است.

---

## فاز ۱ — Refactor دکدرها (بدون تغییر رفتار)

Closureهای decode داخل `scan_*_batch`ها به توابع قابل reuse تبدیل میشوند:

- `core/src/pool/discovery/v2.rs:16-59`، `v3.rs`، `solidly.rs`، `camelot.rs`، `curve.rs`، `balancer.rs`، `trader_joe.rs`، `pendle.rs`، `v4.rs`، `infinity.rs`، `metric.rs`، `fluid.rs`
- شکل هدف: `decode_*(&alloy::Log) -> Option<(Address, DiscoveredPool)>`
- مسیر فعلی `scan_factory_creation_events_pinned` (`discovery/mod.rs:742`) از همان توابع استفاده میکند → خروجی onchain فعلی نباید تغییر کند.
- تست واحد: لاگ sample فابریک هر DEX قبل/بعد refactor خروجی یکسان دهد.

---

## فاز ۲ — پیادهسازی `source = "receipts"`

1. `DiscoverOpts`/validation (`jobs/discover.rs:256`) مقدار `"receipts"` را بپذیرد (فقط TOML، مثل `source` فعلی).
2.-use مجدد `resolve_scan_window` و `lookback_scan_ranges` + watermark؛ `lookback_days = 30` دوباره معنادار میشود.
3. تنظیم جدید `receipt_batch_blocks` (پیشفرض ۴) واستفاده از `rpc_concurrency` موجود.
4. حلقهٔ اسکن:
   `block_windows(from,to,chunk)` → `rpc.get_blocks_and_receipts_batch()` (`rpc/client.rs:1217`)
   → تبدیل `LogData` (`data/types.rs:86-93`) به `alloy::Log` با `block_number`
   → `process_discovery_log` (`discovery/mod.rs:399`) + `decode_*`
   → `ScanBatchResult` → merge.
5. بقیه Pipeline بدون تغییر: `fetch_dex_metadata`، `resolve_token_symbols`، `build_discovery_output`، `apply_explorer_ranking`، health check، `persist_universe`.
6. `note_discovery_scan` فقط для رنج کاملاً موفق → شکافها در اجرای بعدی پر شوند.
7. `ProgressEvent{done,total}` همان stage `fetch` برای تخمین ETA.
8. باندلها با `SqliteStore::put_block_data_batch` (`cache/store/blocks.rs:46`) کش شوند تا `get_receipts(block)` در اجرای بعدی کار کند؛ هزینه دیسک لاگ شود و قابل خاموش کردن باشد.

---

## فاز ۳ — بنچمارک متقابل روشها ← **اضافهٔ جدید درخواستی**

یک هارنس کوچک (مثلاً `discover bench` یا binary جداگانه در `core`) همان `job_discover` را با `DiscoverOpts` مشخص صدا بزند و متریکها را JSON/CSV بریزد. **هیچکدام ازmethodها نباید با تنظیمات متفاوت مقایسه شوند** تا نتیجه معتبر باشد.

### Methodهای مورد مقایسه

| کد | Method | RPC | توضیح |
|---|---|---|---|
| M0 | `hybrid` (وضع موجود، `getLogs`) | publicnode / Dwellir | baseline; انتظار میرود باaddress-less logs شکست بخورد |
| M1 | `receipts` | publicnode | رسیت ۳۰روزه، ریسک rate-limit |
| M2 | `receipts` | Dwellir | آرشیو رسیت؛ batch support طبق P3 |
| M3 | `receipts` | mixed: publicnode(recent) + Dwellir(old) | ترکیب احتمالاً برنده |
| M4 | `factory_state` (گزینه ۱) | Dwellir (archive `eth_call`) | فقط اگر P4 موفق باشد |
| M5 | `remote` (کنترل غیر-on-chain) | HTTPS-only | baseline پوشش/TVL، `creation_block = 0` |

### پنجرهها

- **W1 smoke**: ۱٬۰۰۰ بلاک نزدیک tip — تمام methodها، ۳ بار تکرار، median.
- **W2 mid**: ۷ روز — تمام methodهای قابل اجرا.
- **W3 full**: ۳۰ روز — فقط دو برنده W2 (برای جلوگیری از ۶ بار هزینهٔ ساعتها).

### تنظیمات یکسان برای همه

```toml
[discover]
lookback_days = 30        # در W1/W2 متناسب با پنجره override میشود
min_tvl = 0.0             # ranking نباید پوشش را مخفی کند
max_pools = 100000        # cap نباید مقایسه را خراب کند
health_check = false      # ایزوله کردنDiscovery
resolve_remote_metadata = false
enrich = false
```

سپس یک run دوم با پیشفرضهای واقعی (`min_tvl=25000`, `max_pools=1000`, health/metadata Method) برای قابلیت استفادهٔ نهایی.

### متریکها

**پوشش / کیفیت**
- `pools_total`، تعداد آدرس یونیک
- شمارش بر `DexType` (مخصوصاً آیا V2 پوشش داده شده)
- `pools_with_creation_block > 0` و دقت `creation_block`
- کامل بودن metadata: `token0/token1 != 0`، `fee > 0`، `tick_spacing`، `dex_name`، `tvl_usd`
- overlap با M5 (remote) و با golden sample دستی W1
- `active_blocks`، تعداد pool پس از health check

**RPC / عملکرد**
- تعداد کل درخواستها و batchها، تعداد retry/skip
- خطاها: `-32701`, `403`, rate-limit, `null receipts`
- wall-clock time، throughput (block/s, pool/s)
- p50/p95 latency، حجم bytes منتقلشده
- RPC call به ازای هر pool کشفشده

### معیار برندگی

1. پوشش: `pools_with_creation_block>0` و تعداد DexTypeها بالاتر → بهتر.
2. صحت: overlap با golden sample W1 بالاتر → بهتر.
3. هزینه: در پوشش تقریباً برابر، زمان و تعداد RPC کمتر → بهتر.
4. M4 تنها زمانی به مسیر پیشفرض اضافه میشود که پوششASH حداقل ~80٪ M3 و زمانش < ۱/۱۰ M3 باشد؛ در غیراینصورت کد میماند ولی پیشفرض نمیشود.
5. حذف گزینه ۱ یعنی «غیرفعال نگه داشتن»، نه پاک کردن کد.

### خروجی بنچمارک

- `bench/pool-discovery-<method>-<window>.json` شامل متریکها + آدرسها (برای مقایسه دقیق)
- جدول خلاصه در انتها: `method | coverage | dex_families | time | rpc_calls | errors`
- نتیجهٔ نهایی: مشخص شود کدام ترکیب (M1/M2/M3/M4) بهعنوان پیشفرض `receipts` و کدام بهعنوان fast-path ثبت شود.

---

## فاز ۴ — Fast-path گزینه ۱ (مشروط به P4 + نتیجه بنچمارک)

- `source = "factory_state"` فقط برای فابریک enumerateشده: V3 `allPoolsLength/allPools`، Solidly `allPairsLength/allPairs`، Curve `pool_count/pool_list` (`core/src/rpc/client.rs:1569-1612`).
- تفاضل طول @tip و @tip-30d + جستجوی دوتایی برای `creation_block`، بدون `eth_getLogs`.
- V2 چون enumeration ندارد توسط M3 پوشش داده میشود.
- در صورتی که بنچمارک برنده نشود: کد باقی میماند، ولی وارد پیشفرض و مستندات کاربری نمیشود.

---

## فاز ۵ — تست و rollout

- تست `wiremock` برای batch receipts و تک*tک factory topicها.
- smoke ۱۰۰ بلاک قبل از هریک از W1/W2/W3.
- پیشفرض `hybrid` بدون تغییر میماند تا رفتار فعلی breaking نشود.
- لاگ ابتدایrun صریحاً هزینه تقریبی چاپ کند: ~۱٫۳M بلاک ≈ ۳۲۵k batch call ≈ ساعتها (با public peers ~۵–۶h).

---

## ترتیب اجرا

`Probe (۰)` → `Refactor (۱)` → `receipts source (۲)` → `Bench harness + runs (۳)` → `تصمیم fast-path (۴)` → `Tests/rollout (۵)`

پیادهسازی بعد از تایید این فایل انجام میشود.
