//! The round-trip assertion: open a document, save it unedited, reopen the
//! result, and require the saved bytes to match the original exactly.
//!
//! This function is generic over [`opdf_core::DocumentIo`] rather than tied
//! to `opdf-pdf`, so the two can be developed apart. `tests/corpus_round_trip.rs`
//! is the integration test that drives it over the checked-in corpus with
//! the real parser.

use std::path::Path;

use opdf_core::DocumentIo;

use crate::diff::{DiffError, StructuralDiff, diff_bytes};

/// Which check `assert_round_trip` applies to a file's saved-vs-original bytes.
///
/// There is no default: a caller must choose explicitly, per file, mirroring
/// `RenderRequest::new` taking `document` and `revision` positionally rather than defaulting
/// it (`docs/architecture/contracts.md`) -- a caller who silently reaches for
/// the weaker check reintroduces exactly the regression this harness exists
/// to catch, one that produces a passing test rather than a compile error.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RoundTripStrength {
    /// The default, and by far the common case: `save_incremental` on an
    /// unedited document must reproduce the original bytes exactly. This is
    /// Track A's own completion criterion, not merely this crate's
    /// preference.
    ByteIdentical,
    /// Structural equality only (see [`StructuralDiff`]), for a corpus entry
    /// whose manifest `notes` field explicitly documents why it cannot
    /// round-trip byte for byte. Never chosen by default -- always a
    /// deliberate, per-entry exception.
    StructuralOnly,
}

/// Why a round-trip assertion failed.
#[derive(Debug, thiserror::Error)]
pub enum RoundTripFailure {
    /// Opening the original file failed.
    #[error("failed to open {path}: {source}")]
    Open {
        /// The file that failed to open.
        path: std::path::PathBuf,
        /// The underlying error.
        source: opdf_core::Error,
    },
    /// Saving the (unedited) document failed.
    #[error("failed to save {path}: {source}")]
    Save {
        /// The file that failed to save.
        path: std::path::PathBuf,
        /// The underlying error.
        source: opdf_core::Error,
    },
    /// Reopening the saved file failed.
    #[error("failed to reopen the saved copy at {path}: {source}")]
    Reopen {
        /// The file that failed to reopen.
        path: std::path::PathBuf,
        /// The underlying error.
        source: opdf_core::Error,
    },
    /// Reading either file's raw bytes failed.
    #[error("failed to read bytes for comparison: {0}")]
    Io(#[from] std::io::Error),
    /// `RoundTripStrength::ByteIdentical` was requested and the saved bytes
    /// differ from the original by even one byte.
    #[error("save_incremental changed the bytes of an unedited document: {original_len} bytes before, {saved_len} after")]
    NotByteIdentical {
        /// Length of the original file.
        original_len: usize,
        /// Length of the saved file.
        saved_len: usize,
    },
    /// `RoundTripStrength::StructuralOnly` was requested and the structural
    /// diff engine failed to parse one of the two files.
    #[error("structural diff failed: {0}")]
    Diff(#[from] DiffError),
    /// `RoundTripStrength::StructuralOnly` was requested and the two files
    /// are not even structurally identical -- the weaker check, and it
    /// still failed.
    #[error("round trip changed document structure:\n{0}")]
    NotStructurallyIdentical(StructuralDiff),
}

/// Open `original_path`, save it unedited via [`DocumentIo::save_incremental`]
/// to a fresh temporary path, reopen the saved copy, and compare the saved
/// bytes against the original per `strength`.
///
/// This is the round-trip half of the project's correctness promise. It is
/// intentionally generic over `D` rather than tied to `opdf-pdf`, so this
/// crate compiles and this function typechecks before any `DocumentIo`
/// implementation exists.
pub fn assert_round_trip<D: DocumentIo>(original_path: &Path, strength: RoundTripStrength) -> Result<(), RoundTripFailure> {
    let mut document = D::open(original_path).map_err(|source| RoundTripFailure::Open {
        path: original_path.to_path_buf(),
        source,
    })?;

    //--- a scratch directory, not `original_path.with_extension(...)`: the
    //--- corpus files are tracked in git, and writing the saved copy beside
    //--- them left artifacts in a tracked directory whenever an assertion
    //--- panicked before the cleanup at the end of this function ---
    let scratch = tempfile::tempdir()?;
    let saved_path = scratch.path().join("roundtrip.pdf");
    document.save_incremental(&saved_path).map_err(|source| RoundTripFailure::Save {
        path: saved_path.clone(),
        source,
    })?;

    // Reopen to confirm the saved file is not merely byte-plausible but
    // actually re-parseable -- a save that produces a file only the writer
    // that made it can read back is not a real round trip.
    let _reopened = D::open(&saved_path).map_err(|source| RoundTripFailure::Reopen {
        path: saved_path.clone(),
        source,
    })?;

    let original_bytes = std::fs::read(original_path)?;
    let saved_bytes = std::fs::read(&saved_path)?;

    let result = if original_bytes == saved_bytes {
        Ok(())
    } else {
        match strength {
            // The strong check failed outright -- there is no fallback to a
            // weaker one here. Falling back silently is exactly the
            // byte-identical-degrading-to-structural regression this
            // harness exists to catch; the caller must have asked for
            // StructuralOnly explicitly, in advance, for that path to run.
            RoundTripStrength::ByteIdentical => Err(RoundTripFailure::NotByteIdentical {
                original_len: original_bytes.len(),
                saved_len: saved_bytes.len(),
            }),
            RoundTripStrength::StructuralOnly => {
                let diff = diff_bytes(&original_bytes, &saved_bytes)?;
                if diff.is_empty() {
                    Ok(())
                } else {
                    Err(RoundTripFailure::NotStructurallyIdentical(diff))
                }
            }
        }
    };

    //--- the scratch directory removes itself on drop, including on a panic ---
    drop(scratch);
    result
}
