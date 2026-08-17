//! Headless page operations.
//!
//! Owned by **Track C**. Wraps `opdf-ops` over `opdf_pdf::PdfDocument` so that
//! merge, split, rotate and extract can be run over real files without a
//! window. This is integration checkpoint I1: the first place the document
//! model and the page operations meet on actual bytes.
//!
//! # Why the argument parser is hand-rolled
//!
//! The workspace has no command-line parsing dependency, and four subcommands
//! with nine flags between them do not justify introducing one — `clap` and
//! its derive machinery would be, by a wide margin, the largest dependency in
//! a project whose entire third-party surface is `thiserror`, `lopdf`,
//! `crossbeam-channel`, and the two GUI crates. Two further things fall out of
//! writing it here: paths stay `OsString` end to end, so a file name that is
//! not valid UTF-8 is opened rather than rejected, and a bad `--pages` value
//! reaches the user as the library's own [`opdf_core::Error::InvalidRange`]
//! rather than as a string a parsing crate composed. See `args.rs`.
//!
//! `main` does two things only: parse, and dispatch. Every decision lives in
//! [`args::parse_args`] or in `run.rs`.

#![warn(missing_docs)]
#![warn(clippy::unwrap_used, clippy::expect_used)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

mod args;
mod error;
mod range;
mod run;

use std::ffi::OsString;
use std::process::ExitCode;

use crate::args::Invocation;
use crate::error::CliError;

/// What `--help` prints.
const HELP: &str = "\
opdf-cli - headless page operations on PDF documents

USAGE:
    opdf-cli <SUBCOMMAND> [OPTIONS]

SUBCOMMANDS:
    merge OUT.pdf IN1.pdf [IN2.pdf ...]
            Concatenate the input documents into OUT.pdf, in the order given.
            The inputs are read and never modified.

    split IN.pdf --at N --out-a A.pdf --out-b B.pdf
            Split IN.pdf at a page boundary. Page N is the FIRST page of B, so
            A holds pages 1 to N-1 and B holds N to the end. N must be between
            2 and the page count, so that neither half comes out empty.
            IN.pdf is not modified.

    rotate IN.pdf --pages RANGE --degrees D [--out OUT.pdf]
            Set the orientation of the selected pages to D degrees clockwise.
            D is any multiple of 90; negative and overlarge values are
            normalized, so -90 and 270 are the same thing. This SETS the
            orientation rather than adding to it: --degrees 90 twice leaves a
            page at 90, and --degrees 0 puts a page back upright.
            Without --out the document is edited in place.

    extract IN.pdf --pages RANGE --out OUT.pdf
            Copy the selected pages into a new document OUT.pdf, in document
            order. IN.pdf is not modified.

OPTIONS:
    --force     Overwrite an output file that already exists. Without it, an
                existing output is refused and nothing at all is written.
    -h, --help  Print this help.
    -V, --version
                Print the version.

PAGE RANGES:
    --pages takes a comma-separated list. Page numbers are 1-BASED: page 1 is
    the first page of the document.

        1           page 1 on its own
        2-5         pages 2 through 5, both ends included
        4-          page 4 through the last page
        -3          the first page through page 3
        1-3,7,9-    all of the above, combined

    Parts may overlap and may be given in any order; the selection is sorted
    into document order and de-duplicated, so `6,1,4,6` selects pages 1, 4 and
    6, once each, in that order. A range whose end precedes its start, such as
    `5-2`, is refused before any file is opened.

OUTPUT FILES:
    merge, split and extract write a freshly serialized document, so nothing
    the user asked to leave behind is carried into the output -- in particular
    neither half of a split contains the other half's pages. rotate appends an
    incremental update instead, preserving the input bytes verbatim, which is
    also why rotating in place needs no --force: the pre-rotation revision is
    still inside the file afterwards.

EXIT STATUS:
    0   success
    1   the request was well-formed but could not be carried out
    2   the command line was wrong
";

//---------------------------------------------------------------------
// Entry point
//---------------------------------------------------------------------

fn main() -> ExitCode {
    let items: Vec<OsString> = std::env::args_os().skip(1).collect();
    let invocation = args::parse_args(&items);
    run_opdf_cli(invocation)
}

/// Carry out one parsed command line and report the process exit code.
///
/// Takes the parse result rather than a successful parse so that `main` stays
/// two statements: reporting a bad command line is the same job as reporting a
/// failed operation, and both belong here.
fn run_opdf_cli(invocation: Result<Invocation, CliError>) -> ExitCode {
    let outcome = match invocation {
        Ok(Invocation::Help) => {
            print!("{HELP}");
            return ExitCode::SUCCESS;
        }
        Ok(Invocation::Version) => {
            println!("opdf-cli {}", env!("CARGO_PKG_VERSION"));
            return ExitCode::SUCCESS;
        }
        Ok(Invocation::Merge(arguments)) => run::run_merge(&arguments),
        Ok(Invocation::Split(arguments)) => run::run_split(&arguments),
        Ok(Invocation::Rotate(arguments)) => run::run_rotate(&arguments),
        Ok(Invocation::Extract(arguments)) => run::run_extract(&arguments),
        Err(error) => Err(error),
    };

    match outcome {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("opdf-cli: error: {error}");
            if matches!(error, CliError::Usage(_) | CliError::Argument { .. }) {
                eprintln!("Run `opdf-cli --help` for usage.");
            }
            ExitCode::from(error.exit_code())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The help text is the only documentation this tool has, so the two
    /// things a user cannot guess -- that pages are 1-based and how a range is
    /// written -- must actually be in it.
    #[test]
    fn the_help_text_documents_the_page_range_syntax_and_its_base() {
        assert!(HELP.contains("1-BASED"), "the help must say which end pages are counted from");
        for form in ["2-5", "4-", "-3", "1-3,7,9-"] {
            assert!(HELP.contains(form), "the help must show the '{form}' form of a page range");
        }
        assert!(HELP.contains("5-2"), "the help must say what happens to an inverted range");
    }

    #[test]
    fn the_help_text_covers_every_subcommand_and_its_flags() {
        for subcommand in ["merge", "split", "rotate", "extract"] {
            assert!(HELP.contains(subcommand), "the help must mention `{subcommand}`");
        }
        for flag in ["--at", "--out-a", "--out-b", "--pages", "--degrees", "--out", "--force"] {
            assert!(HELP.contains(flag), "the help must document `{flag}`");
        }
    }

    /// `--degrees` sets an absolute orientation, which is the opposite of what
    /// "rotate" suggests to most people. If that is not in the help, it is
    /// nowhere the user will look.
    #[test]
    fn the_help_text_says_that_rotation_is_absolute_and_where_a_split_boundary_falls() {
        assert!(HELP.contains("SETS the"), "the help must say that --degrees is absolute, not a delta");
        assert!(
            HELP.contains("FIRST page of B"),
            "the help must say which side of --at the boundary page lands on"
        );
    }

    #[test]
    fn the_help_text_documents_the_exit_codes_it_actually_uses() {
        assert!(HELP.contains("EXIT STATUS"), "a tool meant for scripts must document its exit codes");
        assert_eq!(CliError::Usage("x".to_string()).exit_code(), 2, "the help promises 2 for a bad command line");
        assert_eq!(
            CliError::Operation(opdf_core::Error::Unsupported("x".to_string())).exit_code(),
            1,
            "the help promises 1 for a failed operation"
        );
    }

    #[test]
    fn help_and_version_succeed_and_every_failure_does_not() {
        assert_eq!(run_opdf_cli(Ok(Invocation::Help)), ExitCode::SUCCESS);
        assert_eq!(run_opdf_cli(Ok(Invocation::Version)), ExitCode::SUCCESS);
        assert_eq!(run_opdf_cli(Err(CliError::Usage("no".to_string()))), ExitCode::from(2));
        assert_eq!(
            run_opdf_cli(Err(CliError::Operation(opdf_core::Error::Malformed("no".to_string())))),
            ExitCode::from(1)
        );
    }
}
