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

    /// A half-open page range was given with its end before its start.
    ///
    /// Distinct from [`Error::IndexOutOfBounds`], which reports a single
    /// index outside the document: both ends of an inverted range can be
    /// perfectly valid indices on their own.
    #[error("invalid range {start}..{end}: the end precedes the start")]
    InvalidRange {
        /// The inclusive start of the offending range.
        start: usize,
        /// The exclusive end of the offending range.
        end: usize,
    },

    /// A command failed and the rollback of its already-applied parts also failed,
    /// so the document is in neither the original nor the intended state.
    ///
    /// The caller cannot recover at this layer: the sub-command that was asked to
    /// undo itself refused. Reload the document.
    ///
    /// Distinct from every other variant because the *response* differs. Every
    /// other error says "your command did not happen"; this one says "your
    /// command did not happen and the document is no longer what it was". A
    /// caller that cannot tell the two apart either treats a recoverable
    /// rejection as corruption, or keeps editing a document it should have
    /// reloaded.
    #[error("{original}; the rollback then failed: {rollback}")]
    RollbackFailed {
        /// Why the sequence failed in the first place.
        original: Box<Error>,
        /// Why the rollback of the applied prefix then failed.
        rollback: Box<Error>,
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
    fn formats_an_inverted_range_without_implying_either_end_is_out_of_bounds() {
        let error = Error::InvalidRange { start: 5, end: 2 };
        assert_eq!(error.to_string(), "invalid range 5..2: the end precedes the start");
    }

    /// The composed message must name both failures: the rollback failure is
    /// what tells the caller the document is no longer what it was, and the
    /// original is what says why anything was attempted.
    #[test]
    fn formats_a_failed_rollback_with_both_causes() {
        let error = Error::RollbackFailed {
            original: Box::new(Error::Unsupported("the step refused".to_string())),
            rollback: Box::new(Error::PageNotFound(PageId::new(3))),
        };
        assert_eq!(
            error.to_string(),
            "unsupported: the step refused; the rollback then failed: page not found: page#3"
        );
    }

    #[test]
    fn converts_io_errors_automatically() {
        let io_error = std::io::Error::new(std::io::ErrorKind::NotFound, "missing");
        let error: Error = io_error.into();
        assert!(matches!(error, Error::Io(_)));
    }
}
