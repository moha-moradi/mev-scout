import re, urllib.request
base = "https://explorer.mev.zone/_next/static/chunks/"
pages = [
 "pages/mevlive-d08a8773a6e33927.js",
 "539-d1bd5d4c26ead873.js",
 "931-b0241aaa765c5db2.js",
 "847-3091e4e7278f93f3.js",
 "514-6b6eec96d6dc4a34.js",
 "pages/index-648280b7e15a0647.js",
]
# pull build manifest for all chunk names
man = urllib.request.urlopen(urllib.request.Request(
 "https://explorer.mev.zone/_next/static/nFgsW7LGbYxn3ioNVr3qK/_buildManifest.js",
 headers={"User-Agent":"Mozilla/5.0"}), timeout=30).read().decode("utf-8","replace")
chunks = sorted(set(re.findall(r"static/chunks/([^\"']+\.js)", man)))
print("manifest chunks", len(chunks))
# also the ones we know
extra = pages
seen=set()
needles = ("arbitrages", "liquidations", "/live", "apiUrl", "usdProfit", "blockNumber")
for name in chunks + extra:
    if name in seen: continue
    seen.add(name)
    url = base + name if not name.startswith("http") else name
    try:
        data = urllib.request.urlopen(urllib.request.Request(url, headers={"User-Agent":"Mozilla/5.0"}), timeout=30).read().decode("utf-8","replace")
    except Exception as e:
        print("FAIL", name, e)
        continue
    if not any(n in data for n in needles):
        continue
    print("\n==", name, "len", len(data))
    # print nearby context for path-like strings
    for m in re.finditer(r'.{0,80}(arbitrages|liquidations|mevlive|/ws).{0,80}', data):
        s = m.group(0).replace("\n"," ")
        print(s[:220])
