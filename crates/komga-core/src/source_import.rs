//! Domain types shared by source-file import APIs and workers.

use serde::{Deserialize, Serialize};

/// Filesystem operation used to place an imported book in its destination series.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum CopyMode {
    /// Remove the source after placing it in the library.
    Move,
    /// Preserve the source and copy its contents.
    Copy,
    /// Create a hard link, falling back to copying when links are unavailable.
    Hardlink,
}
