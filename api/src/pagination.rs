//! Offset pagination wrapper shared by list endpoints.

use serde::Serialize;

/// Generic paginated response. MVP slices the full `Vec` in the API layer;
/// `limit` values are capped per-endpoint before reaching here.
#[derive(Debug, Clone, Serialize)]
pub struct Paginated<T> {
    pub items: Vec<T>,
    pub total: u64,
    pub offset: u64,
    pub limit: u64,
}

pub fn paginate<T>(all: Vec<T>, offset: u64, limit: u64) -> Paginated<T> {
    let total = all.len() as u64;
    let items = all
        .into_iter()
        .skip(offset as usize)
        .take(limit as usize)
        .collect();
    Paginated {
        items,
        total,
        offset,
        limit,
    }
}
