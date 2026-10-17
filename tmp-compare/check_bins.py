import os

home = os.path.expanduser("~")
ext = "." + "exe"
cargo = os.path.join(home, ".cargo", "bin", "cargo" + ext)
release = os.path.join(r"d:\gitlab.dte.repo\mev-scout\target\release", "mev-scout" + ext)
debug = os.path.join(r"d:\gitlab.dte.repo\mev-scout\target\debug", "mev-scout" + ext)
print("cargo", os.path.exists(cargo), cargo)
print("release", os.path.exists(release), release)
print("debug", os.path.exists(debug), debug)
