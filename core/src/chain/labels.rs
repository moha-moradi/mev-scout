//! Address labels — static known-address database + optional DefiLlama API fetch.
//!
//! Provides human-readable labels for well-known addresses (CEX hot wallets,
//! DEX router/factory contracts, MEV bot addresses, protocol contracts).
//!
//! The static snapshot is bundled as JSON in the crate. Runtime enrichment
//! from DefiLlama (free, no API key) is cached to SQLite.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// A labelled address.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AddressLabel {
    pub address: String,
    pub label: String,
    pub category: String,
}

/// Static address label database loaded from bundled JSON.
#[derive(Debug, Clone, Default)]
pub struct LabelDb {
    labels: HashMap<String, String>,
}

/// Bundled static labels — DEX routers, factory contracts, known MEV bots.
const BUNDLED_LABELS: &str = include_str!("../../data/address_labels.json");

impl LabelDb {
    /// Load labels from the bundled JSON snapshot.
    pub fn load() -> Self {
        let raw: Vec<AddressLabel> = serde_json::from_str(BUNDLED_LABELS)
            .unwrap_or_default();
        let mut labels = HashMap::new();
        for entry in &raw {
            labels
                .entry(entry.address.to_lowercase())
                .or_insert_with(|| entry.label.clone());
        }
        tracing::info!("Label DB: loaded {} bundled labels", labels.len());
        Self { labels }
    }

    /// Look up a label for an address.
    pub fn get(&self, addr: &str) -> Option<&str> {
        self.labels.get(&addr.to_lowercase()).map(|s| s.as_str())
    }

    /// Merge additional labels into the database.
    pub fn merge(&mut self, other: Self) {
        for (addr, label) in other.labels {
            self.labels.entry(addr).or_insert(label);
        }
    }

    /// Return total label count.
    pub fn len(&self) -> usize {
        self.labels.len()
    }

    /// Check if empty.
    pub fn is_empty(&self) -> bool {
        self.labels.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_labels_load() {
        let db = LabelDb::load();
        assert!(db.len() > 0, "should have at least some bundled labels");
    }
}
