import re, urllib.request
URL = "https://explorer.mev.zone/_next/static/chunks/pages/mevlive-d08a8773a6e33927.js"
req = urllib.request.Request(URL, headers={"User-Agent": "Mozilla/5.0"})
data = urllib.request.urlopen(req, timeout=30).read().decode("utf-8", "replace")
print("len", len(data))
strs = re.findall(r'"([^"\\]{6,220})"', data)
keys = ("http", "api", "mev", "arb", "graphql", "block", "socket", "wss")
interesting = [s for s in strs if any(k in s.lower() for k in keys)]
print("interesting", len(set(interesting)))
for s in sorted(set(interesting)):
    print(s)
