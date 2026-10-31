//! Kind-collision census (Task A15) — measure which `MevKind` pairs actually
//! co-occur on the same `(block_number, tx_index)` in an already-indexed
//! explorer DB, and what the lower-precedence member of each pair is worth in
//! USD, before Task B8 hard-codes `kind_precedence` into the tx_index claims
//! registry.
//!
//! Three tests:
//! 1. `census_sql_prepares_against_real_schema` (always on) — builds the real
//!    store schema in memory via the real migration code and prepares AND
//!    executes every census query against seeded collision rows. A schema
//!    rename (or a `MevKind`/CHECK drift) fails this static gate, not the
//!    ignored one.
//! 2. `kind_collision_census` (ignored) — runs the two census queries for real
//!    against the configured DB(s) and prints the result table recorded in
//!    `docs/explorer_strategy_checklist.md` § "Kind collision census".
//!    Contract: one row per *unordered* kind pair co-occurring on the same
//!    `(block_number, tx_index)`, with counts, plus the gross `profit_usd`
//!    (the plan's `gross_profit_usd`) of the lower-precedence member of each
//!    pair, so the cost of every precedence choice is in USD.
//! 3. `no_tx_index_carries_two_pnl_kinds` (ignored) — the permanent guard and
//!    B8's fitness function: in a *post-B8* database zero
//!    `(block_number, tx_index)` rows may carry two P&L-bearing kinds. Every
//!    kind carries P&L, so the guard asserts zero multi-kind rows, period.
//!
//! HONEST RED: on today's *pre-B8* databases the guard is expected to FAIL
//! wherever a real collision exists (e.g. the Liquidation+ArbAtomic co-label
//! the A15 brief describes). That red is the fitness function B8 satisfies —
//! it is not breakage, and the test's doc says so. When the census is empty
//! (today's corpus: zero collisions) the guard passes; it goes red again the
//! moment a collision lands before B8 does.
//!
//! Gates (A0 convention — never a silent green):
//! - census: set `MEV_SCOUT_CENSUS=1` and select explicitly:
//!   `cargo test -p mev-scout-core --test explorer_kind_census -- --ignored --nocapture`
//! - guard: set `MEV_SCOUT_E2E=1` (same selection).
//!   Both tests `bail!` when their gate is set but no DB resolves, and skip
//!   with a printed note when the gate is unset (plain `cargo test` reports
//!   them as `ignored`, never `ok`).
//!
//! DB resolution (both ignored tests):
//! - `MEV_SCOUT_CENSUS_DB` — explicit DB files, OS path-list separated
//!   (`;` on Windows), relative entries resolve against the repo root; or
//! - `MEV_SCOUT_CENSUS_CONFIG` (default: repo-root `mev-scout.toml`) →
//!   `Config::effective_explorer_db_path` for the resolved chain.
//!
//! The chain label comes from the DB file stem, never from a column:
//! `mev_ops` has no chain column (explorer DBs are single-chain files; see
//! `Config::validate_explorer_db_isolation`). Files without a `mev_ops` table
//! are skipped with a printed note — and if nothing censusable remains, the
//! test bails.
#![allow(clippy::unwrap_used, clippy::expect_used)]
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context};
use mev_scout_core::config::Config;
use mev_scout_core::explorer::kind_precedence;
use mev_scout_core::explorer::store::ExplorerStore;
use mev_scout_core::explorer::types::MevKind;
use rusqlite::Connection;

/// Every `MevKind`, in precedence order. The static schema test cross-checks
/// this list against the `mev_ops.kind` CHECK constraint, so a new variant or
/// a spelling drift fails the static gate instead of silently dropping kinds
/// from the census join.
const ALL_KINDS: &[MevKind] = &[
    MevKind::Sandwich,
    MevKind::Frontrun,
    MevKind::Backrun,
    MevKind::ArbAtomic,
    MevKind::JitArb,
    MevKind::Jit,
    MevKind::Liquidation,
    MevKind::Skim,
    MevKind::Utility,
];

/// `(kind, precedence)` VALUES rows injected straight from `kind_precedence`,
/// so the census SQL can never drift from the Rust order (the single source).
fn precedence_values_sql() -> String {
    ALL_KINDS
        .iter()
        .map(|k| format!("('{}', {})", k.as_str(), kind_precedence(*k)))
        .collect::<Vec<_>>()
        .join(", ")
}

/// CTEs shared by both census queries:
/// - `prec`: precedence per kind, injected from `kind_precedence`;
/// - `colliding`: `(block_number, tx_index)` groups carrying 2+ distinct kinds
///   (`tx_index IS NOT NULL` — a NULL index anchors no transaction);
/// - `tx_kinds`: one row per (colliding tx, kind) with its precedence number;
/// - `pairs`: exactly one row per unordered kind pair per colliding tx
///   (`kind_a < kind_b` canonicalizes the pair, `COUNT` never double-counts
///   duplicate same-kind rows);
/// - `pair_gross`: per (colliding tx, kind) gross profit — `mev_ops.profit_usd`
///   is the pre-gas gross the plan's `gross_profit_usd` refers to.
fn census_ctes() -> String {
    format!(
        "
WITH prec(kind, p) AS (VALUES {}),
colliding AS (
  SELECT block_number, tx_index
  FROM mev_ops
  WHERE tx_index IS NOT NULL
  GROUP BY block_number, tx_index
  HAVING COUNT(DISTINCT kind) >= 2
),
tx_kinds AS (
  SELECT DISTINCT m.block_number, m.tx_index, m.kind, pp.p AS precedence
  FROM mev_ops m
  JOIN colliding c
    ON c.block_number = m.block_number AND c.tx_index = m.tx_index
  JOIN prec pp ON pp.kind = m.kind
),
pairs AS (
  SELECT k1.block_number, k1.tx_index, k1.kind AS kind_a, k2.kind AS kind_b
  FROM tx_kinds k1
  JOIN tx_kinds k2
    ON k2.block_number = k1.block_number
   AND k2.tx_index = k1.tx_index
   AND k1.kind < k2.kind
),
pair_gross AS (
  SELECT m.block_number, m.tx_index, m.kind, SUM(m.profit_usd) AS gross_usd
  FROM mev_ops m
  JOIN colliding c
    ON c.block_number = m.block_number AND c.tx_index = m.tx_index
  GROUP BY m.block_number, m.tx_index, m.kind
)",
        precedence_values_sql()
    )
}

/// Census query 1: one row per unordered kind pair co-occurring on the same
/// `(block_number, tx_index)`, with the number of txs carrying that pair.
fn census_pairs_sql() -> String {
    format!(
        "{}
SELECT kind_a, kind_b, COUNT(*) AS co_occurrences
FROM pairs
GROUP BY kind_a, kind_b
ORDER BY co_occurrences DESC, kind_a, kind_b",
        census_ctes()
    )
}

/// Census query 2: per unordered pair, the summed gross `profit_usd` of the
/// *lower-precedence* member (higher precedence number = loses the tx under
/// B8's exclusivity) — the USD cost of each precedence choice. A NULL gross
/// (unpriced op) contributes 0; the census header prints the unpriced count
/// so the sum is never mistaken for complete.
fn census_usd_sql() -> String {
    format!(
        "{}
SELECT pf.kind_a, pf.kind_b,
       COUNT(*) AS co_occurrences,
       SUM(CASE WHEN pa.precedence > pb.precedence
                THEN COALESCE(ga.gross_usd, 0.0)
                ELSE COALESCE(gb.gross_usd, 0.0) END) AS lower_precedence_gross_usd
FROM pairs pf
JOIN tx_kinds pa
  ON pa.block_number = pf.block_number AND pa.tx_index = pf.tx_index AND pa.kind = pf.kind_a
JOIN tx_kinds pb
  ON pb.block_number = pf.block_number AND pb.tx_index = pf.tx_index AND pb.kind = pf.kind_b
LEFT JOIN pair_gross ga
  ON ga.block_number = pf.block_number AND ga.tx_index = pf.tx_index AND ga.kind = pf.kind_a
LEFT JOIN pair_gross gb
  ON gb.block_number = pf.block_number AND gb.tx_index = pf.tx_index AND gb.kind = pf.kind_b
GROUP BY pf.kind_a, pf.kind_b
ORDER BY lower_precedence_gross_usd DESC, co_occurrences DESC, pf.kind_a, pf.kind_b",
        census_ctes()
    )
}

/// The post-B8 invariant: zero `(block_number, tx_index)` rows carry two
/// P&L-bearing kinds. Every kind is P&L-bearing, so this is just the
/// multi-kind group listing (empty output = invariant holds).
fn guard_multi_kind_sql() -> String {
    "SELECT block_number, tx_index, COUNT(DISTINCT kind) AS kinds,
            GROUP_CONCAT(DISTINCT kind) AS kinds_list
     FROM mev_ops
     WHERE tx_index IS NOT NULL
     GROUP BY block_number, tx_index
     HAVING COUNT(DISTINCT kind) >= 2
     ORDER BY block_number, tx_index"
        .to_string()
}

/// Preflight: any `kind` outside the census kind list would be silently
/// dropped by the `prec` join — bail instead of undercounting.
fn unknown_kind_sql() -> String {
    let kinds: Vec<String> = ALL_KINDS
        .iter()
        .map(|k| format!("'{}'", k.as_str()))
        .collect();
    format!(
        "SELECT COUNT(*) FROM mev_ops WHERE kind NOT IN ({})",
        kinds.join(", ")
    )
}

fn db_stats_sql() -> &'static str {
    "SELECT COUNT(*),
            COUNT(DISTINCT kind),
            MIN(block_number),
            MAX(block_number),
            COUNT(DISTINCT block_number),
            COALESCE(SUM(tx_index IS NULL), 0),
            COALESCE(SUM(profit_usd IS NULL), 0)
     FROM mev_ops"
}

fn gate_on(var: &str) -> bool {
    std::env::var(var).ok().as_deref() == Some("1")
}

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_default()
}

fn file_label(path: &Path) -> String {
    path.file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

/// Resolve the DB files to census: `MEV_SCOUT_CENSUS_DB` wins (OS path list),
/// else the configured explorer DB (`MEV_SCOUT_CENSUS_CONFIG`, default the
/// repo-root `mev-scout.toml`). Bails when a gate named a resource that does
/// not resolve — the A0 "I asked for it and you silently skipped" case.
fn resolve_census_dbs() -> anyhow::Result<Vec<PathBuf>> {
    if let Some(list) = std::env::var_os("MEV_SCOUT_CENSUS_DB") {
        let entries: Vec<PathBuf> = std::env::split_paths(&list)
            .filter(|p| !p.as_os_str().is_empty())
            .collect();
        anyhow::ensure!(
            !entries.is_empty(),
            "MEV_SCOUT_CENSUS_DB is set but resolves to no paths"
        );
        let mut out = Vec::with_capacity(entries.len());
        for entry in entries {
            let resolved = if entry.exists() {
                Some(entry.clone())
            } else {
                let via_root = workspace_root().join(&entry);
                via_root.exists().then_some(via_root)
            };
            match resolved {
                Some(p) => out.push(p),
                None => bail!(
                    "MEV_SCOUT_CENSUS_DB entry does not resolve to an existing file: {}",
                    entry.display()
                ),
            }
        }
        return Ok(out);
    }

    // Config fallback (mirrors explorer_tag_coverage.rs).
    let cfg_path = std::env::var("MEV_SCOUT_CENSUS_CONFIG").unwrap_or_else(|_| {
        let root = workspace_root().join("mev-scout.toml");
        if root.exists() {
            root.to_string_lossy().into_owned()
        } else {
            "mev-scout.toml".into()
        }
    });
    let config = Config::load(&cfg_path).map_err(|e| anyhow::anyhow!("config {cfg_path}: {e}"))?;
    let (chain, _) = mev_scout_core::config::validation::resolve_chain(&config)
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    let db_path = PathBuf::from(config.effective_explorer_db_path(&chain));
    let resolved = if db_path.is_absolute() {
        db_path
    } else {
        workspace_root().join(db_path)
    };
    anyhow::ensure!(
        resolved.exists(),
        "no explorer DB resolves (config {cfg_path}, chain {chain}, expected {}); \
         set MEV_SCOUT_CENSUS_DB to an indexed explorer DB",
        resolved.display()
    );
    Ok(vec![resolved])
}

fn open_readonly(path: &Path) -> anyhow::Result<Connection> {
    Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
        .with_context(|| format!("open read-only {}", path.display()))
}

/// True when the file is an explorer DB (has a `mev_ops` table). Scanner
/// cache DBs are skipped with a printed note by the callers.
fn has_mev_ops(conn: &Connection) -> bool {
    conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='mev_ops'",
        [],
        |r| r.get::<_, i64>(0),
    )
    .map(|n| n > 0)
    .unwrap_or(false)
}

/// Census for one DB: header stats + both queries, printed for the checklist.
fn census_one_db(conn: &Connection, path: &Path) -> anyhow::Result<()> {
    let label = file_label(path);
    let (ops, distinct_kinds, lo, hi, blocks, null_tx, unpriced): (
        i64,
        i64,
        Option<i64>,
        Option<i64>,
        i64,
        i64,
        i64,
    ) = conn.query_row(db_stats_sql(), [], |r| {
        Ok((
            r.get(0)?,
            r.get(1)?,
            r.get(2)?,
            r.get(3)?,
            r.get(4)?,
            r.get(5)?,
            r.get(6)?,
        ))
    })?;
    let outside: i64 = conn.query_row(&unknown_kind_sql(), [], |r| r.get(0))?;
    anyhow::ensure!(
        outside == 0,
        "{label}: {outside} mev_ops rows carry a kind outside the census kind list — \
         the census would silently undercount; extend ALL_KINDS first"
    );
    eprintln!(
        "== DB {} (chain label from file stem: {label})",
        path.display()
    );
    eprintln!(
        "   ops={ops} distinct_kinds={distinct_kinds} window={lo:?}..={hi:?} \
         blocks_with_ops={blocks} null_tx_index={null_tx} unpriced_profit_usd={unpriced}"
    );

    let mut stmt = conn.prepare(&census_pairs_sql())?;
    let mut rows = stmt.query([])?;
    let mut any = false;
    while let Some(r) = rows.next()? {
        if !any {
            eprintln!("   kind_a | kind_b | co_occurrences");
            any = true;
        }
        let ka: String = r.get(0)?;
        let kb: String = r.get(1)?;
        let n: i64 = r.get(2)?;
        eprintln!("   {ka} | {kb} | {n}");
    }
    if !any {
        eprintln!(
            "   kind-pair collisions: NONE — no (block_number, tx_index) carries two kinds (census empty)"
        );
        return Ok(());
    }

    let mut stmt = conn.prepare(&census_usd_sql())?;
    let mut rows = stmt.query([])?;
    eprintln!(
        "   USD at stake — gross profit_usd of the lower-precedence member per pair \
         (NULL gross counts as 0; see unpriced count above):"
    );
    eprintln!("   kind_a | kind_b | txs | lower_precedence_gross_usd");
    while let Some(r) = rows.next()? {
        let ka: String = r.get(0)?;
        let kb: String = r.get(1)?;
        let n: i64 = r.get(2)?;
        let usd: f64 = r.get(3)?;
        eprintln!("   {ka} | {kb} | {n} | {usd:.4}");
    }
    Ok(())
}

/// Step 1–3: run the two census queries for real against the configured DB(s)
/// and print the result table for the checklist. `#[ignore]`d so plain
/// `cargo test` skips it; `bail!` when the gate is set but nothing resolves.
#[test]
#[ignore = "needs an indexed explorer DB — set MEV_SCOUT_CENSUS=1"]
fn kind_collision_census() -> anyhow::Result<()> {
    if !gate_on("MEV_SCOUT_CENSUS") {
        eprintln!("skip: set MEV_SCOUT_CENSUS=1 to run (see module docs)");
        return Ok(());
    }
    let dbs = resolve_census_dbs()?;
    let mut censusable = 0usize;
    for path in &dbs {
        let conn = open_readonly(path)?;
        if !has_mev_ops(&conn) {
            eprintln!(
                "census DB {}: not an explorer DB (no mev_ops table) — skipped",
                path.display()
            );
            continue;
        }
        censusable += 1;
        census_one_db(&conn, path)?;
    }
    anyhow::ensure!(
        censusable > 0,
        "census gate is set but none of the {} resolved DB(s) contains a mev_ops table",
        dbs.len()
    );
    Ok(())
}

/// Step 4: the permanent guard — B8's fitness function. In a *post-B8*
/// database zero `(block_number, tx_index)` rows may carry two P&L-bearing
/// kinds (every kind is P&L-bearing, so: zero multi-kind rows, period).
///
/// EXPECTED RED on pre-B8 databases wherever a real collision exists (the A15
/// brief documents Liquidation+ArbAtomic co-labeling as unrestrained today) —
/// that red is the fitness function B8 satisfies, not test breakage. On
/// today's corpus (A15 census: empty) the guard passes; it goes red the moment
/// a collision lands before B8's claims registry does.
#[test]
#[ignore = "needs an indexed explorer DB — set MEV_SCOUT_E2E=1"]
fn no_tx_index_carries_two_pnl_kinds() -> anyhow::Result<()> {
    if !gate_on("MEV_SCOUT_E2E") {
        eprintln!("skip: set MEV_SCOUT_E2E=1 to run (see module docs)");
        return Ok(());
    }
    let dbs = resolve_census_dbs()?;
    let mut violations: Vec<String> = Vec::new();
    let mut checked = 0usize;
    for path in &dbs {
        let conn = open_readonly(path)?;
        if !has_mev_ops(&conn) {
            eprintln!(
                "guard DB {}: not an explorer DB (no mev_ops table) — skipped",
                path.display()
            );
            continue;
        }
        checked += 1;
        let label = file_label(path);
        let mut stmt = conn.prepare(&guard_multi_kind_sql())?;
        let mut rows = stmt.query([])?;
        while let Some(r) = rows.next()? {
            let block: i64 = r.get(0)?;
            let tx_index: i64 = r.get(1)?;
            let kinds: String = r.get(3)?;
            violations.push(format!(
                "{label}: block {block} tx_index {tx_index} kinds [{kinds}]"
            ));
        }
    }
    anyhow::ensure!(
        checked > 0,
        "guard gate is set but none of the {} resolved DB(s) contains a mev_ops table",
        dbs.len()
    );
    if !violations.is_empty() {
        bail!(
            "post-B8 invariant violated: {} (block_number, tx_index) row(s) carry two \
             P&L-bearing kinds:\n  {}\nExpected red while B8 is unlanded — B8's claims \
             registry must resolve these, not this test (docs/explorer_strategy_checklist.md \
             § Kind collision census).",
            violations.len(),
            violations.join("\n  ")
        );
    }
    eprintln!(
        "guard ok: 0 multi-kind rows across {checked} explorer DB(s) — the B8 invariant holds on this data"
    );
    Ok(())
}

/// Always-on static gate: the census SQL must prepare AND execute correctly
/// against the real store schema (built by the real migration code, copied
/// into an in-memory DB) seeded with synthetic collision rows. A schema rename
/// (e.g. `mev_ops.profit_usd`), a lost CHECK constraint, or a `MevKind`
/// spelling drift fails here under plain `cargo test` — not behind the
/// ignored gate.
#[test]
fn census_sql_prepares_against_real_schema() -> anyhow::Result<()> {
    let mem = schema_probe_memory_conn()?;

    // 1. The kind CHECK constraint, ALL_KINDS and MevKind::parse agree.
    let ddl: String = mem
        .query_row(
            "SELECT sql FROM sqlite_master WHERE type='table' AND name='mev_ops'",
            [],
            |r| r.get(0),
        )
        .context("real schema has no mev_ops table")?;
    let check_kinds = check_constraint_kinds(&ddl)?;
    let expected: BTreeSet<String> = ALL_KINDS.iter().map(|k| k.as_str().to_string()).collect();
    assert_eq!(
        check_kinds, expected,
        "mev_ops.kind CHECK constraint != ALL_KINDS — extend ALL_KINDS (and the census) \
         for the new kind"
    );
    for kind in &check_kinds {
        MevKind::parse(kind)
            .with_context(|| format!("schema kind '{kind}' does not parse as a MevKind"))?;
    }

    // 2. Every census statement prepares against the real schema.
    for sql in [
        census_pairs_sql(),
        census_usd_sql(),
        guard_multi_kind_sql(),
        unknown_kind_sql(),
    ] {
        mem.prepare(&sql).with_context(|| {
            format!("census SQL failed to prepare against the real schema:\n{sql}")
        })?;
    }

    // 3. Behavioral check on seeded collisions (incl. NULL gross + NULL tx_index).
    seed_collisions(&mem)?;

    let pairs: Vec<(String, String, i64)> = query_tuples(&mem, &census_pairs_sql())?;
    assert_eq!(
        pairs,
        vec![
            ("arb_atomic".into(), "backrun".into(), 1),
            ("arb_atomic".into(), "jit".into(), 1),
            ("arb_atomic".into(), "liquidation".into(), 1),
            ("arb_atomic".into(), "sandwich".into(), 1),
            ("backrun".into(), "sandwich".into(), 1),
            ("skim".into(), "utility".into(), 1),
        ],
        "pair enumeration / counts wrong (dup same-kind rows must not double-count)"
    );

    let usd: Vec<(String, String, i64, f64)> = query_tuples(&mem, &census_usd_sql())?;
    let expect_usd: Vec<(&str, &str, i64, f64)> = vec![
        // Ordered by the loser's gross. After B8, ArbAtomic (precedence 7)
        // loses to Liquidation and Jit, so those costs are the arb row.
        ("arb_atomic", "backrun", 1, 9.0),
        ("arb_atomic", "sandwich", 1, 9.0),
        ("arb_atomic", "jit", 1, 5.0),
        ("backrun", "sandwich", 1, 4.0),
        ("arb_atomic", "liquidation", 1, 3.0),
        ("skim", "utility", 1, 0.0), // NULL gross (unpriced) contributes 0
    ];
    assert_eq!(usd.len(), expect_usd.len(), "usd row count");
    for (got, want) in usd.iter().zip(&expect_usd) {
        assert_eq!(
            (got.0.as_str(), got.1.as_str(), got.2),
            (want.0, want.1, want.2)
        );
        assert!(
            (got.3 - want.3).abs() < 1e-9,
            "usd for {}/{}: got {} want {}",
            got.0,
            got.1,
            got.3,
            want.3
        );
    }

    let guard: Vec<(i64, i64, i64, String)> = query_tuples(&mem, &guard_multi_kind_sql())?;
    let got_guard: BTreeSet<(i64, i64)> = guard.iter().map(|(b, t, _, _)| (*b, *t)).collect();
    assert_eq!(
        got_guard,
        BTreeSet::from([(100, 0), (100, 1), (200, 0), (300, 0)]),
        "guard must list exactly the seeded multi-kind txs — and exclude the \
         NULL-tx_index block 400 (a NULL index anchors no tx)"
    );
    assert!(guard.iter().all(|(_, _, n, _)| *n >= 2));

    let outside: i64 = mem.query_row(&unknown_kind_sql(), [], |r| r.get(0))?;
    assert_eq!(outside, 0, "preflight must accept every census-listed kind");
    Ok(())
}

/// Build the REAL schema in an in-memory DB: create a throwaway store file via
/// `ExplorerStore::open` (the real migration code), copy its committed DDL out
/// of `sqlite_master` (tables first, then indexes), execute it into
/// `Connection::open_in_memory`, and delete the probe file.
fn schema_probe_memory_conn() -> anyhow::Result<Connection> {
    let probe = std::env::temp_dir().join(format!(
        "mev-scout-a15-schema-probe-{}.sqlite",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&probe); // stale from a crashed run
    {
        let _store = ExplorerStore::open(&probe)?;
    } // schema committed, connection closed (WAL checkpointed on close)

    let file = Connection::open(&probe)?;
    let mut ddl_stmt = file.prepare(
        "SELECT sql FROM sqlite_master
         WHERE sql IS NOT NULL AND name NOT LIKE 'sqlite_%'
         ORDER BY CASE type WHEN 'table' THEN 0 ELSE 1 END, rowid",
    )?;
    let mut rows = ddl_stmt.query([])?;
    let mem = Connection::open_in_memory()?;
    while let Some(row) = rows.next()? {
        let sql: String = row.get(0)?;
        mem.execute_batch(&sql)?;
    }
    drop(rows);
    drop(ddl_stmt);
    drop(file);
    let _ = std::fs::remove_file(&probe);
    let _ = std::fs::remove_file(probe.with_extension("sqlite-wal"));
    let _ = std::fs::remove_file(probe.with_extension("sqlite-shm"));
    Ok(mem)
}

/// Extract the `kind IN (...)` literal list from the mev_ops DDL.
fn check_constraint_kinds(ddl: &str) -> anyhow::Result<BTreeSet<String>> {
    let start = ddl
        .find("kind IN")
        .context("mev_ops DDL lost its kind CHECK constraint")?;
    let rest = &ddl[start..];
    let end = rest
        .find(')')
        .context("unterminated kind CHECK constraint")?;
    let list = &rest[..end];
    let mut out = BTreeSet::new();
    let mut parts = list.split('\'');
    while let Some(_outside) = parts.next() {
        if let Some(quoted) = parts.next() {
            out.insert(quoted.to_string());
        }
    }
    anyhow::ensure!(!out.is_empty(), "parsed no kinds from CHECK: {list}");
    Ok(out)
}

/// Seed synthetic collision rows (direct INSERT — the census is query-only on
/// real DBs; this is the in-memory probe).
fn seed_collisions(conn: &Connection) -> anyhow::Result<()> {
    conn.execute_batch(
        "
        INSERT INTO mev_ops (block_number, tx_index, tx_hash, ts, kind, eoa, confidence,
                             detector, created_at, profit_usd) VALUES
          -- block 100 tx 0: liquidation + arb (the A15 brief's dangerous pair;
          -- today's precedence deletes the $10 liquidation label)
          (100, 0, '0xa1', 1, 'liquidation', '0xe1', 'exact', 'census-test', 1, 10.0),
          (100, 0, '0xa2', 1, 'arb_atomic',  '0xe1', 'exact', 'census-test', 1, 3.0),
          -- block 100 tx 1: jit + arb
          (100, 1, '0xa3', 1, 'jit',         '0xe2', 'exact', 'census-test', 1, 2.0),
          (100, 1, '0xa4', 1, 'arb_atomic',  '0xe2', 'exact', 'census-test', 1, 5.0),
          -- block 100 tx 2: single kind → no collision
          (100, 2, '0xa5', 1, 'arb_atomic',  '0xe3', 'exact', 'census-test', 1, 1.0),
          -- block 200 tx 0: three kinds → three pairs
          (200, 0, '0xa6', 2, 'sandwich',    '0xe4', 'exact', 'census-test', 1, 1.0),
          (200, 0, '0xa7', 2, 'backrun',     '0xe4', 'exact', 'census-test', 1, 4.0),
          (200, 0, '0xa8', 2, 'arb_atomic',  '0xe4', 'exact', 'census-test', 1, 9.0),
          -- block 300 tx 0: loser kind wholly unpriced (NULL gross → 0)
          (300, 0, '0xa9', 3, 'skim',        '0xe5', 'exact', 'census-test', 1, 1.5),
          (300, 0, '0xaa', 3, 'utility',     '0xe5', 'exact', 'census-test', 1, NULL),
          -- block 400: two kinds but NULL tx_index → must not collide
          (400, NULL, '0xab', 4, 'liquidation', '0xe6', 'exact', 'census-test', 1, 7.0),
          (400, NULL, '0xac', 4, 'arb_atomic',  '0xe6', 'exact', 'census-test', 1, 8.0);
        ",
    )?;
    Ok(())
}

/// Run a census statement returning N columns and collect them as a tuple.
fn query_tuples<T>(conn: &Connection, sql: &str) -> anyhow::Result<Vec<T>>
where
    T: FromRow,
{
    let mut stmt = conn.prepare(sql)?;
    let mapped = stmt.query_map([], T::from_row)?;
    let mut out = Vec::new();
    for row in mapped {
        out.push(row?);
    }
    Ok(out)
}

/// Minimal local row-mapping trait so `query_tuples` stays generic without a
/// dependency on rusqlite's `Row` in a public signature.
trait FromRow {
    fn from_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Self>
    where
        Self: Sized;
}
impl FromRow for (String, String, i64) {
    fn from_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok((r.get(0)?, r.get(1)?, r.get(2)?))
    }
}
impl FromRow for (String, String, i64, f64) {
    fn from_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
    }
}
impl FromRow for (i64, i64, i64, String) {
    fn from_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
    }
}
