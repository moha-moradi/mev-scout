import { spawn } from "node:child_process";
console.log("restarting historical backfill days=7");
const c = spawn("cargo", ["run","-p","mev-scout-cli","--release","--","-f","mev-scout.phase2-avax.toml","explorer","index","--days","7"], { stdio: "inherit", shell: true });
c.on("exit", (code) => process.exit(code ?? 1));
