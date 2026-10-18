# Explorer strategy Phase-2 frequency / volume snapshot

Generated from forensic DBs already on disk (2026-10-18). Full 7d tip-follow on
Avalanche publicnode failed mid-window (`missing trie node` on eth_call); use an
archive RPC for a clean 7d/30d recount.

Source JSON: `cache/phase2_coverage.json`.

## Avalanche (`cache/explorer-avalanche.sqlite`)

- ops: **83** across blocks **97505001..97857389** (slice grew during verify)
- kinds: unknown 54, arb_atomic 22 (~1637 USD), jit 5, liquidation 2
- tags: bundler/erc4337 51, claim_and_sell/airdrop 2, long_tail 1, flash_loan_liq 1, benqi protocol 1

## Polygon

- ops: **3** @ block 95391632 -- bundler x2 + arb_atomic x1

## Ethereum

- ops: **508** across blocks **26059586..26214788**
- kinds: arb_atomic 473 (~11641 USD), sandwich 8, jit 6, owner_salvage 12, liquidation 1, bundler/unknown 7
- tags: bundler, long_tail, bad_debt_liq; flags interest_accrued=1, bad_debt=1 @ 26214788

## vs docs/mev_strategies.md Opps/Mo bands

Indexed windows are narrow forensic slices, not month-normalized rates.

| surface | doc band | observed (this run) |
|---------|----------|---------------------|
| arb / long_tail | Continuous / 1K-10K/mo | dense on ETH; present on AVAX |
| sandwich | Continuous / 500-5K/mo | ETH yes; AVAX slice no |
| bundler | Continuous | dominant AVAX tip tag |
| flash_loan_liq | Daily / 100-500/mo | rare on AVAX (1 in set) |
| bad_debt_liq | Daily | 1 Morpho hit on ETH |
| keeper / GMX ADL / epoch | Continuous-Weekly | not observed (RPC limits) |

## Next measure step (archive RPC)

```powershell
mev-scout -f mev-scout.hunt-avax.toml explorer index --days 7
node --experimental-sqlite scripts/_coverage_report.mjs
```
