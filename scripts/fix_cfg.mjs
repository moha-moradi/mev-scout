import { readFileSync, writeFileSync } from "node:fs";
let t = readFileSync("mev-scout.phase2-avax.toml", "utf8");
t = t.replace(/\n\[explorer\]\nconfirmations = 4\npoll_interval_ms = 2000\n(?=\n\[explorer\])/, "\n");
writeFileSync("mev-scout.phase2-avax.toml", t);
console.log("fixed", (t.match(/\[explorer\]/g) || []).length);
