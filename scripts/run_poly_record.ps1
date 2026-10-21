$ErrorActionPreference = 'Stop'
$env:RPC_URL = 'https://api-polygon-mainnet-full.n.dwellir.com/87efcc6b-d1fd-48f8-8ca3-4c53ae7b732b'
$env:MEV_SCOUT_E2E = '1'
$env:MEV_SCOUT_RECORD = '1'
$env:MEV_SCOUT_RECORD_CHAIN = 'polygon'
$env:MEV_SCOUT_RECORD_FROM = '94430973'
$env:MEV_SCOUT_RECORD_TO = '94431566'
Set-Location 'd:\gitlab.dte.repo\mev-scout'
cargo test -p mev-scout-core --test explorer_corpus corpus_assert -- --nocapture --exact