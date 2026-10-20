import re
for name in ("mevlive","847","931"):
    data = open(f"tmp-compare/{name}.js", encoding="utf-8").read()
    print("\n########", name)
    for pat in (r".{120}sort.{120}", r".{80}page.{80}", r".{100}WebSocket.{100}", r".{80}arbitrages.{80}", r".{60}limit.{60}"):
        ms = list(re.finditer(pat, data))
        print("--", pat, "count", len(ms))
        for m in ms[:6]:
            print(m.group(0).replace("\n"," ")[:300])
            print("---")
