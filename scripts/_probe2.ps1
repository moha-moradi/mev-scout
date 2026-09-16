$log = Get-ChildItem api_data\logs\job_*.log | Sort-Object LastWriteTime -Descending | Select-Object -First 1
"LOG=$($log.FullName)"
Get-Content $log.FullName -Tail 40
""
"--- feed ---"
$feed = (Invoke-WebRequest -UseBasicParsing "http://127.0.0.1:7600/api/explorer/feed?limit=8").Content | ConvertFrom-Json
$feed | ForEach-Object {
  $route = if ($_.route_json) { ($_.route_json | ConvertFrom-Json).Count } else { 0 }
  "b=$($_.block_number) kind=$($_.kind) hops=$route native=$($_.native_price_usd) usd=$($_.profit_usd) token=$($_.profit_token.Substring(0,[Math]::Min(12,$_.profit_token.Length))) amt=$($_.profit_amount)"
}
"--- sync ---"
(Invoke-WebRequest -UseBasicParsing http://127.0.0.1:7600/api/sync).Content
"--- health ---"
(Invoke-WebRequest -UseBasicParsing http://127.0.0.1:7600/api/health).Content
