//! Binary entry point for the open-pdf-editor desktop shell.
//!
//! A document named on the command line is parsed by `opdf-pdf` and rasterized by
//! `opdf-render`. With no document named, the shell shows a synthetic one so the
//! viewer can still be exercised.

#![warn(missing_docs)]
#![warn(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;

use opdf_app::app::OpdfApp;
use opdf_app::opener::{DocumentOpener, PdfiumDocumentOpener};

/// Exit status for a document the user asked for and the parser could not read.
const EXIT_OPEN_FAILED: i32 = 1;

/// Command-line arguments.
#[derive(Debug)]
struct AppArgs {
    /// A document to open. When absent, a synthetic document is generated instead.
    open_path: Option<PathBuf>,
    /// Number of synthetic pages to generate when no document is given.
    pages: usize,
}

/// Open a window showing either the requested document or a synthetic one.
fn run_opdf(open_path: Option<PathBuf>, pages: usize) -> eframe::Result {
    //--- a user who asked for a file and got a fake one has been lied to, so a
    //--- failed open exits rather than falling back to the synthetic document ---
    let opened = match open_path {
        Some(path) => match PdfiumDocumentOpener.open(&path) {
            Ok(opened) => opened,
            Err(error) => {
                eprintln!("opdf: could not open {}: {error}", path.display());
                std::process::exit(EXIT_OPEN_FAILED);
            }
        },
        None => match opdf_app::synthetic::open_synthetic_document(pages) {
            Ok(opened) => opened,
            Err(error) => {
                eprintln!("opdf: could not build a synthetic document: {error}");
                std::process::exit(EXIT_OPEN_FAILED);
            }
        },
    };

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(opdf_app::APPLICATION_NAME)
            .with_inner_size([1440.0, 900.0])
            .with_min_inner_size([880.0, 560.0]),
        ..Default::default()
    };

    eframe::run_native(
        opdf_app::APPLICATION_NAME,
        options,
        Box::new(move |creation| Ok(Box::new(OpdfApp::new(&creation.egui_ctx, opened)))),
    )
}

/// Parse `--pages N` and an optional positional document path.
fn parse_args() -> AppArgs {
    const DEFAULT_PAGES: usize = 120;
    let mut open_path = None;
    let mut pages = DEFAULT_PAGES;
    let mut arguments = std::env::args().skip(1);
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--pages" => {
                if let Some(value) = arguments.next().and_then(|value| value.parse().ok()) {
                    pages = value;
                }
            }
            "--help" | "-h" => {
                println!("{}", opdf_app::describe_build());
                println!("usage: opdf [--pages N] [DOCUMENT]");
                std::process::exit(0);
            }
            other => open_path = Some(PathBuf::from(other)),
        }
    }
    AppArgs { open_path, pages }
}

fn main() -> eframe::Result {
    let args = parse_args();
    run_opdf(args.open_path, args.pages)
}
