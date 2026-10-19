var fs = require("fs");
var old = fs.readFileSync("docs/explorer_strategy_phase2.md", "utf8");
if (old.indexOf("explorer_strategy_phase2_progress.md") < 0) {
  old = "Superseded for live progress by [`docs/explorer_strategy_phase2_progress.md`](explorer_strategy_phase2_progress.md) (2026-10-19 stop snapshot).\n\n" + old;
  fs.writeFileSync("docs/explorer_strategy_phase2.md", old);
  console.log("linked old doc");
} else {
  console.log("already linked");
}
