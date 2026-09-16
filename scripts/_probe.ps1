try { "health=" + (Invoke-WebRequest -UseBasicParsing http://127.0.0.1:7600/api/health -TimeoutSec 5).Content } catch { "health_fail $_" }
try { "feed=" + (Invoke-WebRequest -UseBasicParsing "http://127.0.0.1:7600/api/explorer/feed?limit=3" -TimeoutSec 5).Content.Substring(0,[Math]::Min(500,(Invoke-WebRequest -UseBasicParsing "http://127.0.0.1:7600/api/explorer/feed?limit=1").Content.Length)) } catch { "feed_fail $_" }
try {
  $f = (Invoke-WebRequest -UseBasicParsing "http://127.0.0.1:7600/api/explorer/feed?limit=5" -TimeoutSec 5).Content | ConvertFrom-Json
  "feed_count=$($f.Count)"
  $f | ForEach-Object { "b=$($_.block_number) kind=$($_.kind) token=$($_.profit_token) profit=$($_.net_profit_usd) ts=$($_.ts)" }
} catch { "feed_parse_fail $_" }
