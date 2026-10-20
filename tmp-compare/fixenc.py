p = "tmp-compare/cmp.py"
b = open(p, "rb").read()
if b.count(b"\x00") > 5:
    t = b.decode("utf-16")
    open(p, "w", encoding="utf-8", newline="\n").write(t)
    print("rewritten", len(t))
else:
    print("already utf8", len(b))

