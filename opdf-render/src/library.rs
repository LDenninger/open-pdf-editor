//! The process-wide Pdfium binding.
//!
//! Pdfium may be initialized exactly once per process: `Pdfium::new` asserts
//! that no bindings have been installed yet, and panics if they have. Several
//! [`crate::service::PdfiumRenderService`] instances may exist at once — the
//! contract suite alone builds ten — so the library instance is a singleton,
//! initialized on first use and never torn down.

use std::path::PathBuf;
use std::sync::OnceLock;

use pdfium_render::prelude::Pdfium;

/// Environment variable naming the directory holding the Pdfium shared library.
///
/// When unset, the library is looked for in `vendor/pdfium/lib` relative to the
/// repository root, which is where `scripts/fetch-pdfium.sh` puts it.
pub const PDFIUM_LIB_DIR_VAR: &str = "OPDF_PDFIUM_LIB_DIR";

static PDFIUM: OnceLock<Result<Pdfium, String>> = OnceLock::new();

/// The process-wide Pdfium instance, initializing it on first call.
///
/// Returns a human-readable reason when the shared library cannot be loaded.
/// The outcome is cached: a failed load is not retried, so the error is stable
/// for the lifetime of the process.
pub fn bind_pdfium() -> Result<&'static Pdfium, String> {
    PDFIUM.get_or_init(load_pdfium).as_ref().map_err(Clone::clone)
}

/// Directory the shared library is loaded from.
fn resolve_library_dir() -> PathBuf {
    match std::env::var_os(PDFIUM_LIB_DIR_VAR) {
        Some(configured) => PathBuf::from(configured),
        None => PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../vendor/pdfium/lib"),
    }
}

/// Load and initialize Pdfium. Called at most once per process.
fn load_pdfium() -> Result<Pdfium, String> {
    let library_dir = resolve_library_dir();
    let library_path = Pdfium::pdfium_platform_library_name_at_path(&library_dir);
    let bindings = Pdfium::bind_to_library(&library_path).map_err(|error| {
        format!(
            "could not load the pdfium shared library from {}: {error}. Run scripts/fetch-pdfium.sh, or set {PDFIUM_LIB_DIR_VAR} to the directory holding it",
            library_path.display()
        )
    })?;
    Ok(Pdfium::new(bindings))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binds_to_the_vendored_library() {
        let result = bind_pdfium();
        assert!(result.is_ok(), "pdfium must load; run scripts/fetch-pdfium.sh first. {:?}", result.err());
    }

    #[test]
    fn returns_the_same_instance_on_every_call() {
        let first = bind_pdfium().unwrap();
        let second = bind_pdfium().unwrap();
        assert!(std::ptr::eq(first, second), "pdfium must be initialized exactly once per process");
    }
}
