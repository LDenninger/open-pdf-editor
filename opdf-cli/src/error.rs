//! What the CLI reports to the user, and the exit code it reports it with.
//!
//! [`opdf_core::Error`] says what went wrong but never which file it went
//! wrong on — it is the library's error, and the library is handed one
//! document at a time. A command line names several, so every path that
//! touches a file wraps the library error in one that carries the path.

use std::path::PathBuf;

/// Everything the CLI can fail with.
#[derive(Debug, thiserror::Error)]
pub enum CliError {
    /// The command line itself was wrong: an unknown flag, a missing value, a
    /// subcommand that does not exist. The help text is the remedy.
    #[error("{0}")]
    Usage(String),

    /// The value given to a flag was not one that flag accepts.
    ///
    /// Separate from [`CliError::Usage`] so that the library's own diagnosis —
    /// `InvalidRange` for `--pages 5-2`, `Unsupported` for `--degrees 45` —
    /// survives all the way to the user instead of being flattened into a
    /// string at the point of parsing.
    #[error("--{flag}: {source}")]
    Argument {
        /// The flag whose value was rejected, without its leading dashes.
        flag: String,
        /// What the library reported about the value.
        source: opdf_core::Error,
    },

    /// An output path already names a file and `--force` was not given.
    #[error("{} already exists; pass --force to overwrite it", path.display())]
    OutputExists {
        /// The path that already exists.
        path: PathBuf,
    },

    /// A document could not be opened.
    #[error("cannot open {}: {source}", path.display())]
    Open {
        /// The file that could not be opened.
        path: PathBuf,
        /// What the library reported.
        source: opdf_core::Error,
    },

    /// A document could not be written.
    #[error("cannot write {}: {source}", path.display())]
    Save {
        /// The file that could not be written.
        path: PathBuf,
        /// What the library reported.
        source: opdf_core::Error,
    },

    /// An operation on an open document failed — a page selection that does
    /// not fit, a rotation that is not a quarter turn, a failed import.
    #[error(transparent)]
    Operation(#[from] opdf_core::Error),
}

impl CliError {
    /// The process exit code this failure should produce.
    ///
    /// `2` for a misuse of the command line, matching the convention most
    /// GNU tools follow, and `1` for a request that was well-formed but could
    /// not be carried out. Never `0`, which is the point: a shell script that
    /// checks the exit status must see every one of these as a failure.
    pub const fn exit_code(&self) -> u8 {
        match self {
            Self::Usage(_) | Self::Argument { .. } => 2,
            _ => 1,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn a_usage_error_exits_with_two_and_everything_else_with_one() {
        assert_eq!(CliError::Usage("no".to_string()).exit_code(), 2);
        assert_eq!(
            CliError::Argument {
                flag: "pages".to_string(),
                source: opdf_core::Error::InvalidRange { start: 5, end: 2 },
            }
            .exit_code(),
            2,
            "a rejected flag value is a misuse of the command line, not a failed operation"
        );
        assert_eq!(
            CliError::OutputExists {
                path: PathBuf::from("out.pdf")
            }
            .exit_code(),
            1
        );
        assert_eq!(CliError::Operation(opdf_core::Error::Unsupported("no".to_string())).exit_code(), 1);
    }

    #[test]
    fn no_failure_ever_reports_success() {
        //--- a CLI that exits 0 on failure is worse than one that crashes: the script downstream believes it ---
        let failures = [
            CliError::Usage("no".to_string()),
            CliError::OutputExists { path: PathBuf::from("a") },
            CliError::Open {
                path: PathBuf::from("a"),
                source: opdf_core::Error::Malformed("no".to_string()),
            },
            CliError::Save {
                path: PathBuf::from("a"),
                source: opdf_core::Error::Malformed("no".to_string()),
            },
            CliError::Operation(opdf_core::Error::PageNotFound(opdf_core::PageId::new(1))),
            CliError::Argument {
                flag: "pages".to_string(),
                source: opdf_core::Error::InvalidRange { start: 5, end: 2 },
            },
        ];
        for failure in failures {
            assert_ne!(failure.exit_code(), 0, "{failure} must not report success");
        }
    }

    /// The whole reason these variants exist: the library says "malformed
    /// pdf", the user needs to know *which* of the four files they named.
    #[test]
    fn open_and_save_failures_name_the_file_they_are_about() {
        let opening = CliError::Open {
            path: Path::new("/tmp/broken.pdf").to_path_buf(),
            source: opdf_core::Error::Malformed("no xref".to_string()),
        };
        let message = opening.to_string();
        assert!(message.contains("/tmp/broken.pdf"), "got {message}");
        assert!(message.contains("no xref"), "the library's reason must survive the wrapping: {message}");

        let saving = CliError::Save {
            path: Path::new("/tmp/out.pdf").to_path_buf(),
            source: opdf_core::Error::Unsupported("cannot save a document with no pages".to_string()),
        };
        let message = saving.to_string();
        assert!(message.contains("/tmp/out.pdf"), "got {message}");
        assert!(message.contains("no pages"), "got {message}");
    }

    #[test]
    fn a_rejected_flag_value_names_the_flag_and_keeps_the_librarys_reason() {
        let rejected = CliError::Argument {
            flag: "pages".to_string(),
            source: opdf_core::Error::InvalidRange { start: 5, end: 2 },
        };
        let message = rejected.to_string();
        assert!(message.starts_with("--pages:"), "got {message}");
        assert!(message.contains("the end precedes the start"), "got {message}");
    }

    #[test]
    fn refusing_to_overwrite_says_how_to_proceed() {
        let refusal = CliError::OutputExists {
            path: PathBuf::from("merged.pdf"),
        };
        let message = refusal.to_string();
        assert!(message.contains("merged.pdf"), "got {message}");
        assert!(message.contains("--force"), "a refusal the user cannot act on is a dead end: {message}");
    }
}
