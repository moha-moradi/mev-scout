import json
under = json.load(open("venus-underlyings.json"))
markets = open("venus-markets.txt").read().split()
print("[bsc.liquidation_protocol_aliases]")
for m in markets:
    print(f\"\\\"{m}\\\" = \\\"venus\\\"\")
print()
print("[bsc.c_token_underlyings]")
for m, u in under:
    if u:
        print(f\"\\\"{m}\\\" = \\\"{u}\\\"\")
