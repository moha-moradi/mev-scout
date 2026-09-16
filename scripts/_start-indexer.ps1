$body = '{"command":"explorer index","args":["--live"]}'
try {
  $r = Invoke-WebRequest -UseBasicParsing -Method POST -Uri http://127.0.0.1:7600/api/jobs -ContentType "application/json" -Body $body
  $r.Content
} catch {
  "job_err: $($_.Exception.Message)"
  if ($_.ErrorDetails) { $_.ErrorDetails.Message }
}
Start-Sleep -Seconds 2
(Invoke-WebRequest -UseBasicParsing http://127.0.0.1:7600/api/jobs).Content
(Invoke-WebRequest -UseBasicParsing "http://127.0.0.1:7600/api/explorer/feed?limit=5").Content
