use super::RunManifest;

impl super::SqliteStore {
    pub fn put_manifest(&self, manifest: &RunManifest) -> anyhow::Result<()> {
        let conn = self.conn();
        conn.execute(
            "INSERT OR REPLACE INTO run_manifests (run_id, chain, start_block, end_block, resolved_at, range_mode, strategies, flash_loan_provider)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            rusqlite::params![
                manifest.run_id,
                manifest.chain,
                manifest.start_block as i64,
                manifest.end_block as i64,
                manifest.resolved_at as i64,
                manifest.range_mode,
                manifest.strategies.join(","),
                manifest.flash_loan_provider,
            ],
        )?;
        Ok(())
    }

    pub fn get_manifest(&self, run_id: &str) -> anyhow::Result<Option<RunManifest>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT run_id, chain, start_block, end_block, resolved_at, range_mode, strategies, flash_loan_provider
             FROM run_manifests WHERE run_id = ?1",
        )?;
        let mut rows = stmt.query(rusqlite::params![run_id])?;
        match rows.next()? {
            Some(row) => Ok(Some(super::row_to_manifest(row)?)),
            None => Ok(None),
        }
    }

    /// Most recent run by resolution time (ties broken by insert order).
    /// Powers the `report` default "latest run" selection from SQLite.
    pub fn latest_manifest(&self) -> anyhow::Result<Option<RunManifest>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT run_id, chain, start_block, end_block, resolved_at, range_mode, strategies, flash_loan_provider
             FROM run_manifests
             ORDER BY resolved_at DESC, rowid DESC
             LIMIT 1",
        )?;
        let mut rows = stmt.query([])?;
        match rows.next()? {
            Some(row) => Ok(Some(super::row_to_manifest(row)?)),
            None => Ok(None),
        }
    }

    /// All manifests, newest first (API `/api/runs` + `/api/results`).
    pub fn list_manifests(&self) -> anyhow::Result<Vec<RunManifest>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT run_id, chain, start_block, end_block, resolved_at, range_mode, strategies, flash_loan_provider
             FROM run_manifests
             ORDER BY resolved_at DESC, rowid DESC",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)? as u64,
                r.get::<_, i64>(3)? as u64,
                r.get::<_, i64>(4)? as u64,
                r.get::<_, String>(5)?,
                r.get::<_, String>(6)?,
                r.get::<_, String>(7)?,
            ))
        })?;
        let mut out = Vec::new();
        for r in rows {
            let (run_id, chain, start_block, end_block, resolved_at, range_mode, strategies, provider) =
                r?;
            out.push(RunManifest {
                run_id,
                chain,
                start_block,
                end_block,
                resolved_at,
                range_mode,
                strategies: strategies.split(',').map(str::to_string).collect(),
                flash_loan_provider: provider,
            });
        }
        Ok(out)
    }
}
