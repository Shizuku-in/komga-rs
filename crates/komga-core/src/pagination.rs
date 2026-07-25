//! Framework-independent pagination contracts.

/// Direction of one requested sort field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortDirection {
    /// Ascending order.
    Ascending,
    /// Descending order.
    Descending,
}

/// A validated sort request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SortOrder {
    /// API sort property.
    pub property: String,
    /// Sort direction.
    pub direction: SortDirection,
}

/// Pagination and sorting supplied to storage queries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageRequest {
    /// Zero-based page number.
    pub page: u64,
    /// Requested page size.
    pub size: u64,
    /// Whether all matching rows should be returned.
    pub unpaged: bool,
    /// Ordered sort fields.
    pub sort: Vec<SortOrder>,
}

impl Default for PageRequest {
    fn default() -> Self {
        Self {
            page: 0,
            size: 20,
            unpaged: false,
            sort: Vec::new(),
        }
    }
}

/// One page returned by content persistence.
#[derive(Debug, Clone, PartialEq)]
pub struct ContentPage<T> {
    /// Rows in this page.
    pub content: Vec<T>,
    /// Total matching row count.
    pub total_elements: u64,
    /// Effective zero-based page number.
    pub page: u64,
    /// Effective page size.
    pub size: u64,
    /// Sort fields applied to the query.
    pub sort: Vec<SortOrder>,
}
