var fs = require("fs");
var path = require("path");
var cov = JSON.parse(fs.readFileSync(path.join("cache", "phase2_coverage.json"), "utf8"));
var a = cov.chains.avalanche || {};
var e = cov.chains.ethereum || {};
var p = cov.chains.polygon || {};
function kindLine(kinds) {
  return (kinds || []).map(function (k) {
    var profit = k.profit ? (" (~" + Math.round(k.profit) + " USD)") : "";
    return k.kind + " " + k.n + profit;
  }).join(", ");
}
function tagLine(tags) {
  return Object.keys(tags || {}).sort(function (x, y) { return tags[y].n - tags[x].n; }).slice(0, 12).map(function (t) {
    return t + " " + tags[t].n;
  }).join(", ") || "(none)";
}
var md = [
  "# Explorer strategy Phase-2 frequency / volume snapshot",
  "",
  "Last refresh: **" + (cov.generated_at || "n/a") + "** (interim while AVAX `--days 7` backfill runs).",
  "",
  "Source JSON: `cache/phase2_coverage.json`.",
  "",
  "## Avalanche (`cache/explorer-avalanche.sqlite`) ? IN PROGRESS",
  "",
  "- Historical window target: ~97562463..=97864863 (~302401 blocks, archive = dwellir + avalanche.drpc.org)",
  "- ops so far: **" + (a.ops || 0) + "** across blocks **" + ((a.block_range && a.block_range.min) || "?") + ".." + ((a.block_range && a.block_range.max) || "?") + "**",
  "- classified_blocks (DB total): **" + (a.classified_blocks || 0) + "**",
  "- kinds: " + kindLine(a.kinds),
  "- tags: " + tagLine(a.tags),
  "- flags: " + JSON.stringify(a.flags || {}),
  "",
  "## Polygon",
  "",
  a && p.missing ? "- DB missing" : ("- ops: **" + (p.ops || 0) + "**; kinds: " + kindLine(p.kinds) + "; tags: " + tagLine(p.tags)),
  "",
  "## Ethereum",
  "",
  e.missing ? "- DB missing" : ("- ops: **" + (e.ops || 0) + "** across **" + ((e.block_range && e.block_range.min) || "?") + ".." + ((e.block_range && e.block_range.max) || "?") + "**"),
  e.missing ? "" : ("- kinds: " + kindLine(e.kinds)),
  e.missing ? "" : ("- tags: " + tagLine(e.tags) + "; flags: " + JSON.stringify(e.flags || {})),
  "",
  "## Measure command",
  "",
  "```powershell",
  "mev-scout -f mev-scout.phase2-avax.toml explorer index --days 7",
  "node scripts/coverage_report_sqljs.js",
  "```",
  "",
].filter(function (x) { return x !== null; }).join("\n");
fs.writeFileSync("docs/explorer_strategy_phase2.md", md);
console.log("updated docs/explorer_strategy_phase2.md");
