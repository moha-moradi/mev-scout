import re, urllib.request
urls = {
 "mevlive": "https://explorer.mev.zone/_next/static/chunks/pages/mevlive-d08a8773a6e33927.js",
 "847": "https://explorer.mev.zone/_next/static/chunks/847-3091e4e7278f93f3.js",
 "931": "https://explorer.mev.zone/_next/static/chunks/931-b0241aaa765c5db2.js",
}
for name, url in urls.items():
    data = urllib.request.urlopen(urllib.request.Request(url, headers={"User-Agent":"Mozilla/5.0"}), timeout=30).read().decode("utf-8","replace")
    open(f"tmp-compare/{name}.js","w",encoding="utf-8").write(data)
    print(name, len(data))
    for key in ("sort", "page", "limit", "block", "live", "ws", "type"):
        print(" ", key, data.lower().count(key))
