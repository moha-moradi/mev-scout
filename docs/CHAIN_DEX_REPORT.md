# گزارش زنجیره‌ها، دکس‌ها و منابع نقدینگی اتمیک

- **تاریخ داده:** 2026-09-10
- **منابع:** DefiLlama API (`/v2/chains`، `/overview/dexs/{chain}`، `/v2/protocols`) + بررسی کدبیس mev-scout
- **دامنه:** ۹ زنجیره EVM منتخب — BSC، Robinhood Chain، Ethereum، Base، Polygon، Avalanche، Arbitrum، Monad، OP Mainnet
- **فیلتر دکس‌ها:** فقط دکس‌هایی که پروتکل مادر مشخصی دارند (فیلد `parentProtocol`)

راهنمای ستون کدبیس:
- ✅ پشتیبانی کامل (pool discovery + decoder + AMM math)
- 🟡 ناقص (بخشی از زنجیره پشتیبانی وصل است)
- ❌ بدون پشتیبانی (یا زنجیره در کدبیس تعریف نشده)

---

## ۱. خلاصه زنجیره‌ها (مرتب بر اساس حجم ۲۴ ساعته DEX)

| # | زنجیره | TVL | 24h DEX Vol | 7d DEX Vol | 24h Chg | زنجیره در کدبیس |
|---|--------|-----|-------------|------------|---------|:---:|
| 1 | BSC | $5.74B | $2.31B | $10.65B | +73.3% | ✅ |
| 2 | Robinhood Chain | $897.08M | $2.06B | $11.83B | +36.4% | ❌ |
| 3 | Ethereum | $49.71B | $1.36B | $8.89B | +4.7% | ✅ |
| 4 | Base | $5.67B | $996.96M | $5.97B | +10.2% | ✅ |
| 5 | Polygon | $814.22M | $202.48M | $1.24B | +27.6% | ✅ |
| 6 | Avalanche | $495.44M | $162.93M | $917.98M | -32.2% | ✅ |
| 7 | Arbitrum | $1.39B | $155.48M | $1.17B | +4.3% | ✅ |
| 8 | Monad | $1.02B | $130.64M | $1.01B | -74.5% | ❌ |
| 9 | OP Mainnet | $443.25M | $25.55M | $174.39M | +22.8% | ✅ |

کدبیس فقط ۷ زنجیره دارد: Polygon, Ethereum, BSC, Arbitrum, Base, Optimism (OP), Avalanche — `core/src/types/chain.rs:45-73` و `core/data/chains.toml`.

---

## ۲. دکس‌های برتر هر زنجیره × وضعیت کدبیس

### BSC — 24h: $2.31B

| DEX | پروتکل مادر | 24h Vol | 7d Vol | کدبیس |
|-----|--------------|---------|--------|:---:|
| PancakeSwap AMM V3 | PancakeSwap | $986.28M | $4.50B | ✅ |
| PancakeSwap AMM (V2) | PancakeSwap | $390.91M | $1.02B | ✅ |
| PancakeSwap Infinity | PancakeSwap | $369.93M | $2.29B | ✅ (فقط BSC) |
| Uniswap V4 | Uniswap | $173.33M | $862.72M | ✅ |
| Metric V2 | Metric | $72.77M | $398.47M | 🟡 |
| Lista DEX | Lista DAO | $50.91M | $328.06M | ✅ |
| Uniswap V3 | Uniswap | $38.25M | $376.20M | ✅ |
| Topaz CL | Topaz | $37.03M | $206.25M | ❌ |

### Robinhood Chain — 24h: $2.06B

کل زنجیره در کدبیس نیست؛ حتی دکس‌های Uniswap هم ❌.

| DEX | پروتکل مادر | 24h Vol | 7d Vol | کدبیس |
|-----|--------------|---------|--------|:---:|
| Uniswap V3 | Uniswap | $915.98M | $1.73B | ❌ |
| Uniswap V4 | Uniswap | $646.03M | $6.55B | ❌ |
| Pons V2 | Pons | $114.62M | $1.01B | ❌ |
| Ramses CL V2 | Ramses Exchange | $70.67M | $552.80M | ❌ |
| Uniswap V2 | Uniswap | $50.96M | $314.77M | ❌ |
| up v3 | Up | $39.39M | $461.11M | ❌ |
| GIGA V3 | Giga | $22.59M | $235.91M | ❌ |

### Ethereum — 24h: $1.36B

| DEX | پروتکل مادر | 24h Vol | 7d Vol | کدبیس |
|-----|--------------|---------|--------|:---:|
| Uniswap V4 | Uniswap | $508.83M | $2.60B | ✅ |
| Uniswap V3 | Uniswap | $316.11M | $2.00B | ✅ |
| Curve DEX | Curve Finance | $120.97M | $757.60M | ✅ |
| Fluid DEX | Fluid | $87.15M | $635.90M | 🟡 |
| 1inch Aqua | 1inch | $68.96M | $565.35M | ⛔ (DEFERRED — architecture mismatch) |
| Lista DEX | Lista DAO | $65.74M | $243.20M | ❌ (فقط BSC وصل است) |
| Metric V2 | Metric | $49.64M | $343.96M | 🟡 |
| Native Swap | Native | $39.47M | $142.33M | ❌ |
| Ekubo | Ekubo | $35.26M | $351.69M | ❌ (descope) |

### Base — 24h: $996.96M

| DEX | پروتکل مادر | 24h Vol | 7d Vol | کدبیس |
|-----|--------------|---------|--------|:---:|
| Aerodrome Slipstream | Aerodrome | $563.89M | $2.98B | ✅ |
| Uniswap V4 | Uniswap | $150.61M | $408.30M | ✅ |
| Uniswap V3 | Uniswap | $142.85M | $878.57M | ✅ |
| PancakeSwap AMM V3 | PancakeSwap | $102.17M | $638.26M | ✅ |
| Metric V2 | Metric | $77.71M | $415.57M | 🟡 |
| Aerodrome V1 | Aerodrome | $33.79M | $94.72M | ✅ (Solidly engine) |
| Metric V1 | Metric | $17.15M | $78.08M | 🟡 |

### Polygon — 24h: $202.48M

| DEX | پروتکل مادر | 24h Vol | 7d Vol | کدبیس |
|-----|--------------|---------|--------|:---:|
| Polymarket International | Polymarket | $70.38M | $478.61M | ❌ (descope) |
| Uniswap V4 | Uniswap | $31.66M | $204.01M | ✅ |
| Uniswap V3 | Uniswap | $26.00M | $146.49M | ✅ |
| Metric V2 | Metric | $17.83M | $100.51M | 🟡 |
| Ramses CL V2 | Ramses Exchange | $14.94M | $109.59M | ✅ (RamsesX) |
| Quickswap Dex | Quickswap | $9.24M | $72.37M | ✅ (UniV2 fork) |
| DODO AMM | DODO | $6.12M | $18.05M | ❌ (descope) |
| Quickswap V3 | Quickswap | $3.50M | $23.91M | ✅ (Algebra) |

### Avalanche — 24h: $162.93M

| DEX | پروتکل مادر | 24h Vol | 7d Vol | کدبیس |
|-----|--------------|---------|--------|:---:|
| Pharaoh DLMM | Pharaoh Exchange | $55.69M | $431.49M | ✅ (LB 2.1) |
| Pharaoh V3 | Pharaoh Exchange | $44.32M | $340.68M | ✅ (Algebra) |
| Metric V2 | Metric | $4.68M | $36.73M | 🟡 |
| Blackhole CLMM | Blackhole | $3.70M | $31.24M | ✅ |
| Uniswap V3 | Uniswap | $3.07M | $28.92M | ✅ |
| DODO AMM | DODO | $2.56M | $4.72M | ❌ (descope) |
| WOOFi Swap | WOOFi | $1.47M | $8.77M | ❌ (descope) |
| Pangolin V3 | Pangolin | $1.16M | $9.63M | ✅ |
| Joe V2.2 | Trader Joe | $715.4K | $7.03M | ✅ (LB 2.2) |
| PumpSpace V3 | PumpSpace | $547.4K | $5.26M | ❌ |

### Arbitrum — 24h: $155.48M

| DEX | پروتکل مادر | 24h Vol | 7d Vol | کدبیس |
|-----|--------------|---------|--------|:---:|
| Uniswap V3 | Uniswap | $94.34M | $712.38M | ✅ |
| Metric V2 | Metric | $23.78M | $93.45M | 🟡 |
| Uniswap V4 | Uniswap | $22.90M | $141.75M | ✅ |
| PancakeSwap AMM V3 | PancakeSwap | $7.47M | $58.36M | ✅ |
| Fluid DEX | Fluid | $7.27M | $45.38M | 🟡 |
| Camelot V3 | Camelot | $4.64M | $43.53M | ✅ |
| WOOFi Swap | WOOFi | $1.89M | $15.60M | ❌ (descope) |
| DODO AMM | DODO | $1.36M | $4.71M | ❌ (descope) |
| Curve DEX | Curve Finance | $1.26M | $13.25M | ✅ |
| GMX V2 AMM | GMX | $809.7K | $11.63M | ❌ (descope) |

### Monad — 24h: $130.64M

زنجیره در کدبیس نیست؛ همه ❌.

| DEX | پروتکل مادر | 24h Vol | 7d Vol | کدبیس |
|-----|--------------|---------|--------|:---:|
| Kuru CLOB | Kuru | $44.26M | $694.76M | ❌ |
| Uniswap V4 | Uniswap | $18.17M | $101.70M | ❌ |
| Pendle V2 | Pendle | $6.11M | $25.12M | ❌ |
| Balancer V3 | Balancer | $5.85M | $30.29M | ❌ |
| Metric V2 | Metric | $4.37M | $50.16M | ❌ |
| Uniswap V3 | Uniswap | $3.06M | $13.92M | ❌ |
| Curve DEX | Curve Finance | $2.28M | $8.98M | ❌ |
| PancakeSwap AMM V3 | PancakeSwap | $981.4K | $8.06M | ❌ |
| LFJ POE | Trader Joe | $743.7K | $8.17M | ❌ |

### OP Mainnet — 24h: $25.55M

| DEX | پروتکل مادر | 24h Vol | 7d Vol | کدبیس |
|-----|--------------|---------|--------|:---:|
| Velodrome V3 | Velodrome | $17.36M | $96.74M | ✅ (Slipstream) |
| EtherFi Cash Liquid | EtherFi | $4.36M | $31.82M | ❌ (descope) |
| Uniswap V3 | Uniswap | $2.84M | $19.20M | ✅ |
| WOOFi Swap | WOOFi | $1.34M | $11.50M | ❌ (descope) |
| Uniswap V4 | Uniswap | $770.2K | $6.07M | ✅ |
| DODO AMM | DODO | $351.2K | $561.4K | ❌ (descope) |
| Velodrome V2 | Velodrome | $276.0K | $3.16M | ✅ (Solidly engine) |
| Solidly V3 | Solidly Labs | $224.1K | $224.1K* | ❌ |
| Curve DEX | Curve Finance | $128.1K | $1.37M | ✅ |

\* مقدار 7d اصلی: $1.35M.

### جزئیات وضعیت‌های 🟡

| پروتکل | آنچه در کدبیس هست | آنچه کم است | مرجع |
|--------|--------------------|--------------|------|
| Metric (V1/V2) | swap topic، decoder، state-update | pool discovery / factory در `chains.toml` | `core/src/pool/decoders.rs:47-51`، `core/src/pool/state/apply.rs:194-197` |
| Fluid DEX | `FLUID_SWAP` topic، decoder، state-update | factory در `chains.toml` + discovery | `core/src/pool/decoders.rs:40-41`، `core/src/pool/discovery/mod.rs:381` |
| 1inch Aqua | `DexType::Aqua` enum placeholder only (intent-layer، بدون pool state) | DEFERRED — architecture mismatch (intent-based, pool-state detection incompatible) | `core/src/dex_type.rs:49-53` |

پروتکل‌های صراحتاً descoped در کدبیس (`docs/DEX_COVERAGE_PLAN.md:415-429`): Ekubo، Kuru، WOOFi، DODO، GMX، Hashflow، Maverick، 1inch aggregator routing، Polymarket، EtherFi Cash.

---

## ۳. منابع نقدینگی اتمیک (Flash Loan + Flash Swap)

«وام‌دهی» در این گزارش به معنی منابع نقدینگی اتمیک قابل استفاده در آربیتراژ است — یعنی استقراضی که در همان تراکنش باید برگردانده شود (flash loan) یا نقدینگی pool که با callback در همان tx تسویه می‌شود (flash swap).

### ۳.۱. Flash Loan Provider ها (وام خالص، بدون نیاز به collateral)

| Provider | مکانیزم | Fee | در کدبیس |
|----------|----------|-----|-----------|
| **Balancer V2 Vault** | `flashLoan()` روی Vault واحد `0xBA12...2C8` — همه توکن‌های Vault | 0% | ✅ (ارزان‌ترین — اولویت اول Auto) |
| **Aave V3 Pool** | `flashLoanSimple()` | 0.09% (مدل کدبیس) | ✅ |
| **Morpho Blue** | `flashLoan()` روی Morphotoken | 0% | ❌ |
| **Venus (BSC)** | flash loan از طریق Comptroller | جزئی | ❌ |
| **Fluid Liquidity** | flash loan از لایه Liquidity | جزئی | ❌ |
| **Uniswap V2-fork pairs** | flash swap (پرداخت در همان tx از طریق callback) | 0.3% swap fee | به‌صورت flash swap |

### ۳.۲. Flash Swap (نقدینگی pool با callback — تامین نقدینگی درون‌تراکنشی)

هر pool از خانواده UniV2 (`swap(amount0Out, amount1Out, to, data)`) و UniV3 (`swap` با callback) امکان flash swap دارد؛ یعنی می‌توان توکن را گرفت و در پایان همان tx معادل را برگرداند — برای آربیتراژ بدون سرمایه اولیه مناسب است.

| زنجیره | Flash Loan ها | Flash Swap ها |
|--------|----------------|----------------|
| **Ethereum** | Balancer V2 ✅، Aave V3 ✅، Morpho Blue، Fluid | UniV2/V3/V4، Curve، Sushi، Pancake V3، Fluid |
| **BSC** | Aave V3 ✅ (Balancer V2 روی BSC نیست)، Venus، Lista Lending | Pancake V2/V3/Infinity، UniV2/V3/V4، Lista |
| **Base** | Balancer V2 ✅، Aave V3 ✅، Morpho Blue، Fluid | Aerodrome (V1+Slipstream)، UniV2/V3/V4، Pancake V3 |
| **Polygon** | Balancer V2 ✅، Aave V3 ✅، Morpho Blue، Fluid | QuickSwap، UniV2/V3/V4، RamsesX |
| **Avalanche** | Balancer V2 ✅، Aave V3 ✅ | Trader Joe LB، Pharaoh، UniV2/V3/V4، Pangolin |
| **Arbitrum** | Balancer V2 ✅، Aave V3 ✅، Morpho Blue، Fluid | UniV2/V3/V4، Camelot، Pancake V3، Ramses |
| **OP Mainnet** | Balancer V2 ✅، Aave V3 ✅، Morpho Blue، Fluid | Velodrome (V2+V3)، UniV2/V3/V4 |
| **Robinhood Chain** | تعریف‌نشده در کدبیس (روی‌چین: Uniswap pools) | UniV2/V3/V4، Pons، Ramses (روی‌چین) |
| **Monad** | تعریف‌نشده در کدبیس (روی‌چین: Balancer V3 Vault) | UniV2/V3/V4، Kuru، LFJ، Pancake V3 (روی‌چین) |

علامت ✅ = وصل‌شده در `chains.toml`؛ بقیه فقط روی‌چین موجودند ولی در کدبیس wired نشده‌اند.

### ۳.۳. پشتیبانی کدبیس (FlashLoanProvider)

`core/src/types/strategy.rs:16` — enum: `Auto | Balancer | Aave | Uniswap`

| Provider | Fee مدل | Gas مدل | پیش‌نیاز config |
|----------|---------|---------|------------------|
| `Balancer` | 0 | 150k | `balancer_vault` (هر ۷ زنجیره دارد) |
| `Aave` | 0.09% | 250k | `aave_v3_pool` (هر ۷ زنجیره دارد) |
| `Uniswap` | 0.10% (متغیر per pool) | 200k | `uniswap_v3_factories` (هر ۷ زنجیره دارد) |
| `Auto` | ارزان‌ترین انتخاب | 150k | ترتیب: **Balancer V2 → Aave V3 → Uniswap Flash Swap** |

- اعتبارسنجی per-chain: `core/src/config/validation.rs:200-208`
- آدرس‌ها: `balancer_vault` و `aave_v3_pool` در تمام ۷ زنجیره `core/data/chains.toml` تنظیم شده‌اند
- Concurrency نکته مهم: پشتیبانی flash loan دقیقاً همان چیزی است که برای استراتژی‌های اتمیک (two-hop/multi-hop arb، JIT) لازم است و Balancer به‌دلیل fee صفر و gas کمتر، اولویت پیش‌فرض است.

### ۳.۴. Lending سنتی (پیوست — غیراتمیک، برای مرجع)

وام‌دهی کلاسیک (با collateral، غیراتمیک) — خودش برای آربیتراژ اتمیک مستقیماً به کار نمی‌آید ولی liquidation detector کدبیس به آن وابسته است:

| زنجیره | پروتکل‌های اصلی |
|--------|------------------|
| Ethereum | AAVE V3 ($29.7B)، SparkLend، Morpho Blue، Compound V3، Maple، Fluid Lending، Euler V2 |
| BSC | Venus Core Pool ($1.9B)، Lista Lending ($829M)، AAVE V3 ($266M) |
| Base | Morpho Blue ($1.6B)، AAVE V3 ($700M)، Moonwell، Compound V3، Fluid Lending |
| Polygon | AAVE V3 ($282M)، Morpho Blue، Fluid Lending، AAVE V2، Compound V3 |
| Avalanche | AAVE V3 ($777M)، Benqi ($372M)، Euler V2، Silo V2 |
| Arbitrum | AAVE V3 ($1.1B)، Compound V3 ($174M)، Fluid Lending ($116M)، Dolomite |
| OP Mainnet | AAVE V3 ($161M)، Compound V3 ($38M)، Moonwell، Exactly |
| Robinhood Chain | ثبت‌شده ندارد |
| Monad | ثبت‌شده ندارد |

پشتیبانی کدبیس از lending سنتی:
- **Aave V3** — کامل: flash-loan + liquidation detector (`core/src/mev/detectors/liquidation.rs:11-28`)، reserve prefetch، `core/src/chain/liquidations.rs`
- **Aave V2** — فقط flash-loan scan (`core/src/chain/events.rs:62-63`)
- **Compound V3** — فقط liquidation scan (topic `Absorb`) (`core/src/chain/liquidations.rs:17-19`)
- Morpho / Venus / Benqi / Spark / Dolomite / Moonwell — پشتیبانی نشده

---

## ۴. خلاصه معماری پشتیبانی در کدبیس mev-scout

- Rust workspace (`core` + `cli`) — alloy/revm/tokio/rusqlite — MEV scanner و backtester برای EVM
- ۱۳ موتور دکس در `DexType` (`core/src/dex_type.rs:9-53`): UniswapV2, UniswapV3, Curve, Balancer, Solidly, Camelot, UniswapV4, TraderJoeLB, Pendle, PancakeInfinity, Metric, Fluid, Aqua (DEFERRED — enum placeholder only)
- مکانیزم: بدون ABI فایل — topic hash های hardcoded در `chain/events.rs` و `pool/decoders.rs`؛ discovery از روی factory events؛ factory address ها در `core/data/chains.toml` + mirror در `ChainName::default_*_factories()` (`core/src/types/chain.rs:151-283`)
- اضافه‌کردن دکس جدید به دو روش: (۱) config-only اگر event family موجود باشد؛ (۲) `DexType` جدید با ~۱۵ نقطه اتصال — مستند در `docs/DEX_COVERAGE_PLAN.md:351-393` (آخرین نمونه کامل: PancakeSwap Infinity)
- Remote discovery هم موجود است: GeckoTerminal و DexScreener با skip-list و slug های curated (`core/src/pool/discovery/remote/`)
