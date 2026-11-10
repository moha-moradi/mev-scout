# اصلاح داشبورد و حسابداری واقعی JIT

## Context
هدف، پاسخ روشن و قابل‌اتکا به شش درخواست است: توزیع تگ‌ها، پوشش بلاک/زمان زیر Overview، سود واقعی JIT، مقایسه بازده هزینهٔ استراتژی‌ها، جزئیات تصویری فرصت و توضیح قوانین تشخیص با hover.

تصمیم‌های کاربر:
- زمان زیر Overview، **بازهٔ زمانی بلاک‌ها روی زنجیره** است، نه مدت اجرای ایندکس.
- **محاسبه واقعی JIT در همین مرحله** انجام شود؛ نه صرفاً تغییر ظاهر.

دامنهٔ پیشنهادی حسابداری واقعی: موقعیت‌های V3 استاندارد با افتتاح، مالکیت و تسویهٔ کاملِ قابل‌اثبات. حسابداری LB، موقعیت‌های مشترک مدیر NFT و موقعیت‌های دارای موجودی قبلی/تسویهٔ ناقص بدون شواهد کافی، صریحاً «نامعلوم» می‌مانند. این محدودیت پوشش در UI نمایش داده می‌شود؛ محاسبهٔ تقریبی سهم LP جای محاسبهٔ واقعی جا زده نمی‌شود.

## اصول و شواهد فعلی
- داشبورد، HTML آفلاین و تک‌فایلی است؛ تولید آن نباید RPC بزند یا DB را migrate کند. قرارداد فعلی در [docs/CLI.md:445](file:///D:/gitlab.dte.repo/mev-scout/docs/CLI.md#L445) حفظ می‌شود.
- نمودارها از کل پنجره داده ساخته می‌شوند، نه events/evidence محدودشده با budget یا نتایج جست‌وجو. NULL با صفر فرق دارد.
- `reduce_facets` از قبل تگ‌ها را تجمیع می‌کند؛ نیاز به ساخت دوبارهٔ این مسیر نیست.
- تخمین فعلی [classify/jit.rs:185](file:///D:/gitlab.dte.repo/mev-scout/core/src/explorer/classify/jit.rs#L185) کارمزد کل سواپ‌ها را بدون سهم LP و بدون محدودیت دقیق عمر موقعیت جمع می‌کند؛ اکنون وارد سود دلاری نشده است.
- محاسبهٔ تقریبی fee-growth در موتور replay جایگزین حسابداری واقعی نیست. این کار، بازنویسی عمومی replay یا شبیه‌ساز استخرها را شامل نمی‌شود.
- تغییرات ثبت‌نشدهٔ کاربر در [classify/arb.rs](file:///D:/gitlab.dte.repo/mev-scout/core/src/explorer/classify/arb.rs) و اسناد benchmark حفظ می‌شوند؛ ادغام JitArb در لایهٔ classify/mod انجام می‌شود، نه با بازنویسی classifier آربیتراژ.

## ۱. ثبت شواهد و حسابداری واقعی JIT

### داده و ذخیره‌سازی
- در [decode/jit.rs:28](file:///D:/gitlab.dte.repo/mev-scout/core/src/explorer/decode/jit.rs#L28) دریافت Mint/Burn حفظ و Collect استاندارد V3 اضافه شود؛ sender/recipient، owner، محدوده ticks و مقدار خام هر دو توکن نگه داشته شوند.
- ترتیب قطعی `(block, tx_index, receipt_log_index)`، هش بلاک/تراکنش و شناسهٔ افتتاح موقعیت حفظ شود. receipt-local log index با log index سراسری اشتباه نشود.
- migration بعدی به [store/schema.rs:27](file:///D:/gitlab.dte.repo/mev-scout/core/src/explorer/store/schema.rs#L27) افزوده شود؛ اکنون V13 است، بنابراین V14، مگر اینکه HEAD تغییر کرده باشد. migrationهای قبلی دست‌نخورده بمانند.
- شواهد مرتب LP، baseline موقعیت در مرز بلاک و وضعیت چرخه ذخیره شوند. amounts/liquidity/gas با اعداد صحیح بدون افت دقت ذخیره شوند؛ قیمت گس واقعی از receipt گرفته شود، نه تبدیل رفت‌وبرگشتی f64.
- مدل فعلی overwrite-on-mint/delete-on-burn در [store/jit_positions.rs:193](file:///D:/gitlab.dte.repo/mev-scout/core/src/explorer/store/jit_positions.rs#L193) به پردازش مرتب تغییر کند. افزودن نقدینگی، partial burn و reopen باید قابل‌شناسایی باشند، حتی اگر برای محاسبهٔ سود واجد شرایط نباشند.
- ذخیره facts، وضعیت موقعیت و checkpoint در یک transaction انجام شود؛ فاصلهٔ فعلی بین ذخیره بلاک و `persist_jit_positions` در [ingest.rs:597](file:///D:/gitlab.dte.repo/mev-scout/core/src/explorer/ingest.rs#L597) حذف شود. خواندن وضعیت از بلاک‌های آینده در backfill ممنوع باشد.

### اثبات و فرمول
یک ماژول محدود و اختصاصی [jit_accounting.rs — جدید](file:///D:/gitlab.dte.repo/mev-scout/core/src/explorer/jit_accounting.rs) مسئول reconciliation و نتیجهٔ typed باشد؛ کد valuation اولیه و repricing از همان منطق استفاده کنند.

محاسبهٔ واقعی تنها زمانی معتبر است که:
1. پروتکل/توکن‌ها استاندارد و قابل‌پشتیبانی و مالک اقتصادی/پرداخت‌کننده/گیرنده قابل‌انتساب باشند؛ pool owner مشترک به‌عنوان searcher فرض نشود.
2. baseline قبل از افتتاح، liquidity و tokensOwed صفر را ثابت کند؛ همه رویدادهای قبل از Mint در همان بلاک نیز بررسی شوند.
3. یک افتتاح بدون افزودن یا اختلاط، برداشت کامل و Collect کامل در تراکنش بستن، داخل پنجره JIT ثبت شده باشد؛ حالت same-transaction و cross-block با ترتیب logs کنترل شود.
4. receipts و بازهٔ لازم کامل باشند و وضعیت پایان بلاک، صفر شدن liquidity و tokensOwed را تأیید کند؛ فعالیت بعدی همان position key نتیجه را مخدوش نکند.
5. انتقال‌های ERC-20 و انتساب هزینه‌های تراکنش با این چرخه تطبیق کنند. هزینهٔ مشترک میان چند موقعیت بدون روش قابل‌اثبات تقسیم نشود.

برای هر توکن، با baseline صفر و تسویهٔ کامل:

`paid_lp_fees = sum(Collect) - sum(Burn principal)`

`position_cashflow = sum(Collect) - sum(Mint deposit)`

Burn به‌تنهایی دریافت پول نیست؛ افزایش طلب موقعیت است. مقدار منفیِ غیرممکن در reconciliation کارمزد خطا/نامعلوم است، نه صفرسازی. کارمزد واقعیِ تسویه‌شده از قبل سهم دیگر LPها، تغییر نقدینگی فعال و سهم پروتکل را لحاظ کرده است؛ مقدار/fee tier پیش‌فرض یا نقدینگی انتهای سواپ وارد این فرمول نمی‌شود. این روش ادعای انتساب دقیق کارمزد به تک‌تک سواپ‌ها ندارد.

- برای baseline و settlement، متد hash-pinned تاریخی بر پایهٔ مسیر موجود [rpc/client.rs:1569](file:///D:/gitlab.dte.repo/mev-scout/core/src/rpc/client.rs#L1569) اضافه شود. پاسخ با هش بلاک معتبر شود؛ cache برحسب هش/position، تعداد درخواست محدود و بدون fallback به latest یا صفر باشد.
- نبود archive state، ABI معتبر، انتساب مالکیت یا شواهد کامل به status/reason روشن منجر شود؛ ایندکس عمومی را صرفاً به علت غیرقابل‌قیمت‌گذاری بودن JIT متوقف نکند.
- اعتبارسنجی سواپ به عمر موقعیت محدود شود؛ ورود/خروج در همان تراکنش با log order بررسی شود. semantics بازه V3 نیمه‌باز است. endpoint tick به‌تنهایی اثبات کامل عبور از محدوده نیست؛ در نبود شواهد کافی، قطعیت تشخیص افزایش نیابد.
- تخمین قدیمی `fees_estimated` دیگر مبنای سود نیست؛ داده‌های قدیمی در raw evidence حفظ، ولی در UI به‌عنوان تخمین تخصیص‌نیافته و غیرقابل‌استفاده در ROI معرفی شوند.

### ارزش‌گذاری، هزینه و JitArb
- کارمزد LP با سود کل موقعیت یکی نیست. سود ناخالص، ارزش دریافتی‌ها در زمان دریافت منهای ارزش سپرده‌ها در زمان پرداخت است؛ تغییر ترکیب/قیمت اصل سرمایه نیز اثر دارد. کارمزد دریافتی جدا نمایش داده شود.
- قیمت هر جریان در timestamp/hour خودش و قیمت native هر receipt گس استفاده شود. مبلغ خام، timestamp و منبع quote ذخیره شوند؛ USD همچنان تقریبی است. نبود قیمت هر جزء لازم، نتیجهٔ مربوطه را NULL کند، نه جمع ناقص به‌عنوان سود کامل.
- signed cashflow و زیان حفظ شوند؛ استفاده از `profit_tokens` مثبت‌محور یا `DeltaLedger::net()` برای حذف زیان ممنوع است. از `net_signed()` در [profit.rs:51](file:///D:/gitlab.dte.repo/mev-scout/core/src/explorer/profit.rs#L51) برای تطبیق انتساب استفاده شود.
- enrichment پیش از price warming/persistence در [ingest.rs:563](file:///D:/gitlab.dte.repo/mev-scout/core/src/explorer/ingest.rs#L563) قرار گیرد؛ token/quote discovery باید سپرده، دریافتی و هزینه‌های همه تراکنش‌های مرتبط را پوشش دهد.
- شاخهٔ valuation JIT به [store/pnl.rs:84](file:///D:/gitlab.dte.repo/mev-scout/core/src/explorer/store/pnl.rs#L84) و [store/reprice.rs:23](file:///D:/gitlab.dte.repo/mev-scout/core/src/explorer/store/reprice.rs#L23) با evaluator مشترک اضافه شود. repricing با قیمت ناقص نباید سود معتبرِ ظاهری و قدیمی باقی بگذارد.
- وضعیت reconciliation، completeness هزینه، pricing و confidence از یکدیگر تفکیک شوند؛ صرفاً قیمت داشتن به معنای Exact نیست.
- گس هر تراکنش متعلق به عملیات، شامل افتتاح و بستن/Collect، فقط یک بار حساب شود. هزینه‌های تأمین مالی نیز با رعایت `flashloan_fee_netted_in_ledger` یک بار منظور شوند.
- در [classify/mod.rs:267](file:///D:/gitlab.dte.repo/mev-scout/core/src/explorer/classify/mod.rs#L267)، JitArb شواهد arb را حفظ کند؛ سود arb و LP فقط با اثبات عدم هم‌پوشانی یا یک ledger متحد جمع شوند. در غیر این صورت، اجزا دیده شوند ولی سود ترکیبی نامعلوم بماند. event مستقلِ تکراری تولید نشود.
- stamp هش JitArb و canonical identity موقعیت اصلاح شود؛ owner و شناسه افتتاح در [canonical.rs:57](file:///D:/gitlab.dte.repo/mev-scout/core/src/explorer/canonical.rs#L57) لحاظ شوند. تغییرات لازم برای claim کردن تراکنش‌های متعلق به عملیات و جلوگیری از دوباره‌شماری هزینه/سود تست شوند.

### بازپردازش و reorg
- در [store/sync.rs:81](file:///D:/gitlab.dte.repo/mev-scout/core/src/explorer/store/sync.rs#L81)، unwind وضعیت چرخه را از facts/baselineهای معتبر بازسازی و هر حسابداری وابسته به بلاک حذف‌شده را invalid کند؛ حتی اگر افتتاح قبل از نقطه rollback بوده باشد.
- DB قدیمی با migrate صرف، شواهد گم‌شده پیدا نمی‌کند و ایندکس عادی نیز checkpointهای قبلی را رد می‌کند. اعتبارسنجی و تولید JIT واقعی روی DB و scanner cache جدید و مجزا با پنجرهٔ پیوسته انجام شود؛ پاک‌کردن یا reindex دیتابیس کاربر خودکار نباشد.

## ۲. پوشش بلاک و زمان زیر Overview
- [dashboard/mod.rs:170](file:///D:/gitlab.dte.repo/mev-scout/core/src/explorer/dashboard/mod.rs#L170) و loader فعلی timestamps توسعه یابند: حداقل/حداکثر شماره بلاک و timestamp متناظر از جدول `blocks` در همان پنجره انتخاب‌شده خوانده شوند، نه فقط بلاک‌های دارای MEV.
- زیر عنوان نمایش داده شود: زنجیره، `Block start → end`، `UTC start → end`، مدت پوشش زنجیره و تعداد بلاک‌های ثبت‌شده.
- برای ایندکس انجام‌شده با `--days`/`--hours` نیز همین رنج واقعی بلاک قابل‌نمایش است؛ نیازی به حدس‌زدن فرمان اصلی نیست.
- پنجره درخواستی، پوشش موجود و checkpoint سراسری مفاهیم جدا هستند. min/max به معنای نبود gap نیست؛ تعداد بلاک و وضعیت gaps موجود حفظ شوند. DB خالی و مقادیر unavailable با `—` نمایش داده شوند.

## ۳. نمودار تگ‌ها و بازده هزینه در Analytics

### تگ‌ها
- استفاده از `tag_facets` در [dashboard/reduce.rs:110](file:///D:/gitlab.dte.repo/mev-scout/core/src/explorer/dashboard/reduce.rs#L110).
- میله‌های افقی مرتب‌شده با تعداد عملیات و درصد از **کل عملیات پنجره**؛ تگ‌های بیشتر با scroll/data zoom قابل‌دیدن باشند.
- membership هر تگ در هر عملیات یک‌بار شمرده شود. توضیح ثابت: «یک عملیات می‌تواند چند تگ داشته باشد؛ جمع درصدها الزاماً ۱۰۰٪ نیست». تگ ناشناخته یا بدون تگ پنهان نشود؛ نمودار دایره‌ای استفاده نشود.

### بازده
- reducer مجزا برای `strategy_economics` پیش از truncation در [dashboard/reduce.rs:239](file:///D:/gitlab.dte.repo/mev-scout/core/src/explorer/dashboard/reduce.rs#L239) اضافه شود؛ numerator و denominator دقیقاً از همان cohort باشند.
- هزینه در این نمودار **هزینهٔ اجرای ثبت‌شده: گس + کارمزد تأمین مالی معلوم** است؛ سرمایه سپرده‌شده/وام‌گرفته‌شده یا حجم سواپ نیست. هزینه‌های خارج از این دامنه، مثل پرداخت داخلی نامشهود به builder، صریحاً در محدودیت نمودار ذکر شوند.
- شرط ورود: درآمد قابل‌انتساب، اجزای لازم هزینه و quotes معلوم و finite؛ JIT تأییدنشده، bundler با `gas_cost_as_profit` و قیمت‌گذاری ناقص/مخدوش وارد نسبت نشوند. صرف Inferred بودن به‌تنهایی علت حذف نیست؛ پوشش Exact/Inferred مشخص شود.
- اگر profit ذخیره‌شده از قبل flash fee را کم کرده، فقط با مقدار معلوم fee، gross قبل از این هزینه بازسازی شود؛ fee هم در denominator هست و هم دوباره از net کم نمی‌شود. هزینهٔ سواپِ embedded دوباره کسر نشود.

`ROI_cost = 100 × Σ net / Σ recorded_execution_cost`

`net_per_$10 = 10 × Σ net / Σ recorded_execution_cost`

- این نسبت مجموع‌ها است، نه میانگین درصد هر عملیات. هزینه صفر یا cohort خالی → N/A، نه Infinity یا صفر.
- نمودار افقی با محور مثبت/منفی، درصد و مثال روشن: «هر ۱۰ دلار هزینه → ۲۵ دلار سود ناخالص، ۱۵ دلار سود خالص؛ ۱۵۰٪». تعبیر تاریخی است، نه پیش‌بینی سود یا بازده سرمایه.
- زیان‌های معلوم، از جمله sandwichهای `excluded_from_net`، در این مقایسه اقتصادی حفظ شوند؛ رفتار KPIهای فعلی عوض نشود و تفاوت cohort در caption مشخص باشد. تعداد واجد شرایط/کل و علت موارد خارج‌شده کنار نمودار بیاید.

## ۴. جزئیات تصویری عملیات و توضیحات hover
فایل اصلی UI، [core/data/dashboard_template.html:1072](file:///D:/gitlab.dte.repo/mev-scout/core/data/dashboard_template.html#L1072)، با الهام از [تصویر مرجع](file:///D:/gitlab.dte.repo/mev-scout/vibe_images/mev-scout-operation-concept_1794117818915_e5ef5da7.png) بازآرایی شود، بدون بازطراحی ناوبری نامرتبط.

- نمای جزئیات عریض‌تر: breadcrumb و actions، کارت‌های Net/Gross/Gas/Volume، سپس آبشار و facts در دو ستون، مسیر توکن‌ها و timeline، و شواهد در پایین. ROI و کارمزد LP در بخش economics تکمیلی بیایند.
- آبشار دقیقاً با net تطبیق کند؛ flash fee ازقبل-netted دوباره کسر نشود، زیان با رنگ/جهت درست و هزینهٔ نامعلوم با وضعیت incomplete نمایش داده شود، نه میله صفر ساختگی.
- از `parse_route`، `tokenLabel`، `ammIcon`، `confidenceBadge` و `chart` استفاده شود. ورودی اولین و خروجی آخرین leg نیز نمایش یابد؛ نام DEX یا اتصال مسیر حدس زده نشود. JIT نمای سپرده/برداشت و توکن‌ها داشته باشد، نه مسیر سواپ ساختگی.
- در [dashboard/evidence.rs:29](file:///D:/gitlab.dte.repo/mev-scout/core/src/explorer/dashboard/evidence.rs#L29)، id/tx_index/ts و شواهد حسابداری منتقل شوند. timeline فقط ترتیب/زمان رویدادهای واقعی mint، burn، collect یا legs موجود را نشان بدهد؛ زمان مراحل داخلی ثبت‌نشده، TWAP فرضی و confidence عددی ۹۸٪ ساخته نشوند.
- تگ‌ها، دلایل تشخیص، محدودیت قیمت‌گذاری، طرف‌های مرتبط و raw JSON تفکیک شوند. نبود قربانی صرفاً «شاهدی ثبت نشده» باشد، نه ادعای قطعیت عدم وجود قربانی.
- selection/evidence maps/deep links از شناسه عملیات استفاده کنند تا چند موقعیت در یک tx قاطی نشوند. لینک hash قدیمی در صورت یکتا بودن resolve شود و در حالت چندتایی انتخاب عملیات ارائه شود. copy-link و بازگشت مرورگر تست شوند.
- یک منبع توضیحات کنار `sliceLabel` برای تعریف، قواعد واقعی تشخیص و محدودیت‌ها ایجاد شود؛ در نمودارها، legend/نام استراتژی و badgeها با hover و keyboard focus، و در موبایل با لمس قابل‌دسترسی باشد.
- توضیحات شامل همه kindها و utility subkindها باشد و «قاعده تشخیص» را از «شرط معتبر بودن سود» جدا کند. thresholds فقط با برچسب default یا از تنظیمات واقعاً ثبت‌شده نمایش یابند؛ برای داده قدیمی ادعای استفاده از تنظیمات فعلی نشود. متن tooltip و داده‌های ناشناخته با `esc()` escape شوند.
- فراخوانی‌های undefined به `passesSlice/passesUtility/passesConfidence` در Analytics حذف شوند؛ این بقایای فیلترهای حذف‌شده مانع render هستند. فیلتر سراسری جدید اضافه نشود.
- layout واکنش‌گرا، route طولانی، tooltip خروج از کادر، evidence حذف‌شده با budget و operation بدون route پوشش داده شوند.

## ترتیب اجرا و فایل‌های تکمیلی
1. قرارداد شواهد/تست‌ها → decoder و migration → lifecycle و reconciliation → valuation/repricing.
2. پوشش بلاک و reducerهای Analytics.
3. UI، detail identity، hover و اصلاح waterfall.
4. آزمون end-to-end و مقایسه خروجی‌های قبل/بعد.

علاوه بر فایل‌های بالا، تغییرات یکپارچه‌سازی در [types.rs:607](file:///D:/gitlab.dte.repo/mev-scout/core/src/explorer/types.rs#L607)، [decode/mod.rs:59](file:///D:/gitlab.dte.repo/mev-scout/core/src/explorer/decode/mod.rs#L59)، [store/mod.rs:131](file:///D:/gitlab.dte.repo/mev-scout/core/src/explorer/store/mod.rs#L131)، [store/facts.rs:209](file:///D:/gitlab.dte.repo/mev-scout/core/src/explorer/store/facts.rs#L209)، [jobs/index.rs:11](file:///D:/gitlab.dte.repo/mev-scout/core/src/jobs/index.rs#L11) و [dashboard/caps.rs:9](file:///D:/gitlab.dte.repo/mev-scout/core/src/explorer/dashboard/caps.rs#L9) محدود به همین مسیر باشد. هیچ تغییر permission/CI، حذف داده یا commit در این درخواست نیست.

## Verification
- تست decoder: ABI واقعی Mint/Burn/Collect، داده ناقص و overflow، ترتیب logs، recipient و owner درست.
- تست حسابداری: baseline صفر؛ LP رقیب با سهم کارمزد مستقل؛ Collect بزرگ شامل اصل سرمایه ولی fee کوچک؛ پروتکل fee؛ زیان دو‌توکنی؛ دو ساعت با قیمت متفاوت؛ گس چند تراکنش بدون دوباره‌شماری؛ missing archive/quote؛ same-tx و cross-block؛ سوآپ قبل/بعد از عمر موقعیت و عبور از محدوده.
- نمونهٔ واقعیِ ذخیره‌شده یا fixture قرارداد معتبر ثابت کند fee فقط مبلغ تسویه‌شدهٔ آن موقعیت است؛ صرف mock کردن خروجی مطلوب کافی نیست. موارد partial burn، preexisting owed، manager مشترک، reopen، deferred Collect و LB با reason مناسب غیرقابل‌محاسبه بمانند.
- تست store: migration و بازکردن مجدد، idempotent retry، rollback با failure injection، محدودیت خواندن آینده، بازسازی چرخه بعد از reorg و برابری initial valuation/repricing.
- تست reducer: numerator/denominator یکسان، زیان‌های مشاهده‌شده، سود ساختگی bundler، flash-netting، null/zero cost، تگ تکراری/هم‌پوشان، DB خالی و رنج با بلاک‌های بدون MEV. تغییر `max_ops/evidence/budget` آمار جدید را تغییر ندهد.
- توسعه تست‌های موجود [explorer_golden.rs](file:///D:/gitlab.dte.repo/mev-scout/core/tests/explorer_golden.rs)، [explorer_dashboard_filters.rs](file:///D:/gitlab.dte.repo/mev-scout/core/tests/explorer_dashboard_filters.rs)، [explorer_dashboard_readonly.rs](file:///D:/gitlab.dte.repo/mev-scout/core/tests/explorer_dashboard_readonly.rs)، [render.rs](file:///D:/gitlab.dte.repo/mev-scout/core/src/explorer/dashboard/render.rs) و [cli_dashboard.rs](file:///D:/gitlab.dte.repo/mev-scout/cli/tests/cli_dashboard.rs)؛ readonly generation باید نسخه و محتوای DB را تغییر ندهد.
- فرمان‌های هدف: `cargo test -p mev-scout-core --lib explorer`، integrationهای بالا با `cargo test -p mev-scout-core --test ...`، `cargo test -p mev-scout-cli --test cli_dashboard`، و fmt/clippy برای بخش‌های تغییرکرده.
- پس از پیاده‌سازی، artifactهای fixture و یک پنجره محدود واقعی در **مسیرهای خروجی جدید و cacheهای مجزا** تولید شوند. هیچ wipe/reindex روی DB اصلی بدون تأیید مستقل انجام نشود؛ archive RPC ناموجود صریحاً به‌عنوان محدودیت validation گزارش شود.
- HTML با سرور استاتیک loopback در مرورگر تست شود و سپس عملکرد آفلاین آن نیز بررسی شود؛ محصول همچنان به سرور وابسته نیست. Overview، Analytics، tags، ROI، همه hoverها، جزئیات JIT/arb/زیان/نامعلوم، عملیات هم‌هش، جست‌وجو، link/history، تب‌های دیگر و عرض موبایل تست شوند؛ console/network برای regression بررسی شود.