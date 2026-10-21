const https = require("https");
const { URL } = require("url");
const RPC = process.env.BSC_RPC || "https://api-bsc-mainnet-full.n.dwellir.com/87efcc6b-d1fd-48f8-8ca3-4c53ae7b732b";
const AAVE = "0x6807dc923806fe8fd134338eabca509979a7e0cb";
const AAVE_LIQ = "0xe413a321e8681d831f4dbccbca790d2952b56f977908e45be37335533e005286";
const COMP_LIQ = "0x298637f684da70674f26509b10f07ec2fbc77a335ab1e7d6215a4b2484d8bb52";
const venus = new Set([
"0xeca88125a5adbe82614ffc12d0db554e2e2867c8","0xfd5840cd36d94d7229439859c0112a4185bc0255",
"0x95c78222b3d6e262426483d42cfa53685a67ab9d","0x2ff3d0f6990a40261c66e1ff2017acbc282eb6d0",
"0x151b1e2635a717bcdc836ecd6fbb62b674fe3e1d","0xa07c5b74c9b40447a954e1466938b865b6bbea36",
"0x882c173bc7ff3b7786ca16dfed3dfffb9ee7847b","0xf508fcd89b8bd15579dc79a6827cb4686a3592c8",
"0x57a5297f2cb2c0aac9d554660acd6d385ab50c6b","0xb248a295732e0225acd3337607cc01068e3b9c10",
"0x5f0388ebc2b94fa8e123f404b79ccf5f40b29176","0x1610bc33319e9398de5f57b33a5b184c806ad217",
"0x650b940a1033b8a1b1873f78730fcfc73ec11f1f","0x334b3ecb4dca3593bccc3c7ebd1a1c1d1780fbf1",
"0xf91d58b5ae142dacc749f58a49fcbac340cb0343","0x972207a639cc1b374b893cc33fa251b55ceb7c07",
"0xebd0070237a0713e8d94fef1b728d3d993d290ef","0x9a0af7fdb2065ce470d72664de73cae409da28ec",
"0xec3422ef92b2fb59e84c8b02ba73f1fe84ed8d71","0x5c9476fcd6a4f9a3654139721c949c2233bbbbc8",
"0x86ac3974e2bd0d60825230fa6f355ff11409df5c","0x26da28954763b92139ed49283625cecaf52c6f94",
"0x08ceb3f4a7ed3500ca0982bcd0fc7816688084c3","0x61edcfe8dd6ba3c891cb9bec2dc7657b3b422e93",
"0x78366446547d062f45b4c0f320cdaa6d710d87bb","0xb91a659e88b51474767cd97ef3196a3e7cedd2c8",
"0xc5d3466aa484b040ee977073fcf337f2c00071c1","0x6cfdec747f37daf3b87a35a1d9c8ad3063a1a8a0",
"0xbf762cd5991ca1dcddac9ae5c638f5b5dc3bee6e","0x27ff564707786720c71a2e5c1490a63266683612",
"0xc4ef4229fec74ccfe17b2bdef7715fac740ba0ba","0x4d41a36d04d97785bcea57b057c412b278e6edcc",
"0xf841cb62c19fcd4ff5cd0aab5939f3140baac3ea","0x86e06eafa6a1ea631eab51de500e3d474933739f",
"0xbf515ba4d1b52ffdceabf20d31d705ce789f2cec","0x689e0dab47ab16bcae87ec18491692bf621dc6ab",
"0x9e4e5fed5ac5b9f732d0d850a615206330bf1866","0x699658323d58ee25c69f1a29d476946ab011bd18",
"0x74ca6930108f775cc667894eea33843e691680d7","0x0c1da220d301155b87318b90692da8dc43b67340",
"0xd804de60afd05ee6b89aab5d152258fd461b07d5","0xcc1db43a06d97f736c7b045aedd03c6707c09bdf",
"0x6bca74586218db34cdb402295796b79663d816e9","0x6d0cdb3355c93a0cd20071abbb3622731a95c73e",
"0x89c910eb8c90df818b4649b508ba22130dc73adc","0x3d5e269787d562b74acc55f18bd26c5d09fa245e",
"0x6d3bd68e90b42615cb5abf4b8de92b154adc435e","0x92e6ea74a1a3047dabf4186405a21c7d63a0612a",
"0x97421799419eb782628e73e7220d8e0a207469a3","0xeb8ca841cbe1bc4832a10b15c7dab1081edad371",
"0xc36dfacc7a125859c106f29b9f2d874ccf29a55a","0x3e281461efb3d53ec20db207674373ed8ef3bba9",
"0xc0768948e668b7bacff8b4bd1babe0ed2b512d3c","0xb1ab0399766997c5d66a30b2f2055277b7fa5d6c",
"0x80a5694441810d2b871beed644b6d16d113ce06e",
].map(a => a.toLowerCase()));

function call(method, params) {
  const url = new URL(RPC);
  const body = JSON.stringify({ jsonrpc: "2.0", id: 1, method, params });
  return new Promise((resolve, reject) => {
    const req = https.request({ hostname: url.hostname, port: 443, path: url.pathname + url.search, method: "POST",
      headers: { "Content-Type": "application/json", "Content-Length": Buffer.byteLength(body) } },
      (res) => { let d=""; res.on("data",c=>d+=c); res.on("end",()=>{ try{ const j=JSON.parse(d); if(j.error) reject(new Error(JSON.stringify(j.error))); else resolve(j.result);}catch(e){reject(new Error(d.slice(0,200)));}});});
    req.setTimeout(60000, () => req.destroy(new Error("timeout")));
    req.on("error", reject); req.write(body); req.end();
  });
}

(async () => {
  const tip = parseInt(await call("eth_blockNumber", []), 16);
  console.log("tip", tip, "rpc", RPC);
  const lookback = parseInt(process.env.LOOKBACK || "30000", 10);
  const conc = parseInt(process.env.CONC || "8", 10);
  let aave = null, venusHit = null;
  let scanned = 0;
  async function scanBlock(bn) {
    const receipts = await call("eth_getBlockReceipts", ["0x" + bn.toString(16)]);
    if (!receipts) return;
    for (const rcpt of receipts) {
      for (const lg of rcpt.logs || []) {
        const t0 = (lg.topics && lg.topics[0] || "").toLowerCase();
        const addr = (lg.address || "").toLowerCase();
        if (!aave && t0 === AAVE_LIQ && addr === AAVE) {
          aave = { block: bn, tx: rcpt.transactionHash, searcher: rcpt.from, addr };
        }
        if (!venusHit && t0 === COMP_LIQ && venus.has(addr)) {
          venusHit = { block: bn, tx: rcpt.transactionHash, searcher: rcpt.from, vToken: addr };
        }
      }
    }
  }
  let bn = tip;
  const start = tip - lookback;
  while (bn >= start && (!aave || !venusHit)) {
    const batch = [];
    for (let i = 0; i < conc && bn >= start; i++, bn--) batch.push(bn);
    await Promise.all(batch.map((b) => scanBlock(b).catch((e) => {
      if (String(e.message).includes("timeout") || scanned % 500 === 0)
        console.error("err @"+b, String(e.message).slice(0,100));
    })));
    scanned += batch.length;
    if (scanned % 500 === 0 || aave || venusHit) {
      console.error("scanned", scanned, "down to", bn, "aave", !!aave, "venus", !!venusHit);
    }
    if (aave && venusHit) break;
  }
  console.log("DONE scanned", scanned);
  console.log("AAVE", JSON.stringify(aave));
  console.log("VENUS", JSON.stringify(venusHit));
})().catch((e) => { console.error(e); process.exit(1); });