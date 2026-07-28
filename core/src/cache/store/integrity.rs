use std::collections::HashSet;

impl super::SqliteStore {
    pub fn find_uncached_blocks(&self, range: impl IntoIterator<Item = u64>) -> anyhow::Result<Vec<u64>> {
        let blocks: Vec<u64> = range.into_iter().collect();
        if blocks.is_empty() {
            return Ok(Vec::new());
        }
        let conn = self.conn();
        let placeholders: Vec<String> = blocks.iter().map(|_| "?".to_string()).collect();
        let sql = format!(
            "SELECT number FROM blocks
             INNER JOIN block_meta USING(number)
             WHERE number IN ({}) AND txs_fetched = 1",
            placeholders.join(","),
        );
        let mut stmt = conn.prepare(&sql)?;
        let params: Vec<rusqlite::types::Value> = blocks
            .iter()
            .map(|n| rusqlite::types::Value::Integer(*n as i64))
            .collect();
        let param_refs: Vec<&dyn rusqlite::types::ToSql> =
            params.iter().map(|v| v as &dyn rusqlite::types::ToSql).collect();
        let existing: HashSet<u64> = stmt
            .query_map(param_refs.as_slice(), |row| {
                row.get::<_, i64>(0).map(|v| v as u64)
            })?
            .filter_map(|r| r.ok())
            .collect();
        let missing: Vec<u64> = blocks
            .into_iter()
            .filter(|n| !existing.contains(n))
            .collect();
        Ok(missing)
    }

    pub fn check_integrity(&self, start: u64, end: u64) -> anyhow::Result<Vec<u64>> {
        self.find_uncached_blocks(start..=end)
    }

    pub fn check_integrity_range(&self, blocks: &[u64]) -> anyhow::Result<Vec<u64>> {
        self.find_uncached_blocks(blocks.iter().copied())
    }
}
