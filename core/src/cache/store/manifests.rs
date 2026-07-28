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
            Some(row) => Ok(Some(super::row_to_manifest(&row)?)),
            None => Ok(None),
        }
    }

    pub fn list_manifests(&self) -> anyhow::Result<Vec<(String, RunManifest)>> {
        let conn = self.conn();
        let mut stmt = conn.prepare(
            "SELECT run_id, chain, start_block, end_block, resolved_at, range_mode, strategies, flash_loan_provider
             FROM run_manifests ORDER BY resolved_at DESC",
        )?;
        let mut rows = stmt.query([])?;
        let mut results = Vec::new();
        while let Some(row) = rows.next()? {
            let run_id: String = row.get(0)?;
            let manifest = super::row_to_manifest(&row)?;
            results.push((run_id, manifest));
        }
        Ok(results)
    }
}
