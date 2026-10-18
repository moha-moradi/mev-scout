var fs = require("fs");
var md = fs.readFileSync("docs/explorer_strategy_phase2.md", "utf8");
md = md.replace("interim while AVAX `--days 7` backfill runs).", "paused; resume with `node scripts/run_backfill.mjs`).");
md = md.replace("? IN PROGRESS", "- PAUSED (~21% of 7d window classified)");
md = md.replace(/- classified_blocks \(DB total\): \*\*\d+\*\*/, "- classified_blocks (DB total): **63583** (~62k inside 7d window)");
fs.writeFileSync("docs/explorer_strategy_phase2.md", md);
console.log("ok");
