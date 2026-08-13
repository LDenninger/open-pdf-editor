//! Error type shared by every open-pdf-editor crate.

use crate::page::PageId;

/// Result alias used throughout the workspace.
pub type Result<T> = std::result::Result<T, Error>;

/// Everything that can go wrong while reading, mutating, or rendering a document.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// The underlying file could not be read or written.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    /// The file is not a structurally valid PDF, or is damaged beyond repair.
    #[error("malformed pdf: {0}")]
    Malformed(String),

    /// The file uses a PDF feature this implementation does not handle.
    #[error("unsupported: {0}")]
    Unsupported(String),

    /// The referenced page does not exist in this document.
    #[error("page not found: {0}")]
    PageNotFound(PageId),

    /// A positional argument fell outside the document's page range.
    #[error("index {index} out of bounds for {page_count} pages")]
    IndexOutOfBounds {
        /// The offending index.
        index: usize,
        /// The number of pages present when the operation was attempted.
        page_count: usize,
    },

    /// Rasterization failed.
    #[error("render failed: {0}")]
    Render(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_index_out_of_bounds_with_both_numbers() {
        let error = Error::IndexOutOfBounds { index: 7, page_count: 3 };
        assert_eq!(error.to_string(), "index 7 out of bounds for 3 pages");
    }

    #[test]
    fn converts_io_errors_automatically() {
        let io_error = std::io::Error::new(std::io::ErrorKind::NotFound, "missing");
        let error: Error = io_error.into();
        assert!(matches!(error, Error::Io(_)));
    }
}
