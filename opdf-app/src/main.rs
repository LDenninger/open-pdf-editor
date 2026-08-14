//! Binary entry point for the open-pdf-editor desktop shell.
//!
//! Owned by **Track D**. Built against `opdf_core::fakes::FakeRenderService` and a
//! synthetic document until Track A lands real document loading and Track B a real
//! rasterizer.

#![warn(missing_docs)]
#![warn(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;

use opdf_app::app::OpdfApp;

/// Command-line arguments.
#[derive(Debug)]
struct AppArgs {
    /// A document to open. Rejected for now: Track A owns real document loading.
    open_path: Option<PathBuf>,
    /// Number of synthetic pages to generate when no document is given.
    pages: usize,
}

/// Open a window showing either the requested document or a synthetic one.
fn run_opdf(open_path: Option<PathBuf>, pages: usize) -> eframe::Result {
    if let Some(path) = open_path {
        eprintln!(
            "opdf: opening {} is not implemented yet — real document loading is Track A's work",
            path.display()
        );
        eprintln!("opdf: showing a synthetic document of {pages} pages instead");
    }

    let snapshot = match opdf_app::synthetic::build_synthetic_snapshot(pages) {
        Ok(snapshot) => snapshot,
        Err(error) => {
            eprintln!("opdf: could not build a synthetic document: {error}");
            opdf_core::document::DocumentSnapshot::default()
        }
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
        Box::new(move |creation| Ok(Box::new(OpdfApp::new(&creation.egui_ctx, snapshot)))),
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
