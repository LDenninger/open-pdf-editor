//! The process-wide Pdfium binding.
//!
//! Pdfium may be initialized exactly once per process: `Pdfium::new` asserts
//! that no bindings have been installed yet, and panics if they have. Several
//! [`crate::service::PdfiumRenderService`] instances may exist at once — the
//! contract suite alone builds ten — so the library instance is a singleton,
//! initialized on first use and never torn down.

use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard, OnceLock};

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

/// Serializes every call into Pdfium, process-wide.
static PDFIUM_LOCK: Mutex<()> = Mutex::new(());

/// Acquire exclusive access to Pdfium for the duration of the returned guard.
///
/// **Every** call into Pdfium must hold this — including the drops that close a
/// document, a page, or a bitmap, which are FFI calls like any other.
///
/// Pdfium makes no thread-safety guarantee, and `pdfium-render`'s `thread_safe`
/// feature does not supply one: in 0.9.3 it only adds `Send` and `Sync` to the
/// wrapper types, so nothing sequences the underlying C calls. Two threads
/// loading a document at once corrupt Pdfium's global state *permanently* —
/// every later load in the process answers `FormatError`, on any thread. One
/// worker thread per service is therefore not enough isolation on its own,
/// because several services exist at once.
///
/// The lock is poison-tolerant: a panic while rendering says nothing about
/// Pdfium's own state, and refusing every later render would turn one bad page
/// into a dead renderer.
pub fn lock_pdfium() -> MutexGuard<'static, ()> {
    PDFIUM_LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
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

    #[test]
    fn serializes_concurrent_document_loads() {
        //--- without the lock this corrupts pdfium for the whole process: every later load, on any
        //--- thread, answers FormatError. Measured at 167 to 184 failures out of 200 ---
        let pdf_path = crate::fixture::ensure_contract_fixture();
        let mut handles = Vec::new();
        for _ii in 0..8 {
            let thread_path = pdf_path.clone();
            handles.push(std::thread::spawn(move || {
                let mut errors: Vec<String> = Vec::new();
                for _jj in 0..25 {
                    let pdfium = bind_pdfium().unwrap();
                    let _guard = lock_pdfium();
                    if let Err(error) = pdfium.load_pdf_from_file(&thread_path, None) {
                        errors.push(format!("{error:?}"));
                    }
                }
                errors
            }));
        }

        let errors: Vec<String> = handles.into_iter().flat_map(|handle| handle.join().unwrap()).collect();
        assert!(
            errors.is_empty(),
            "{} of 200 concurrent loads failed, first three: {:?}",
            errors.len(),
            &errors[..errors.len().min(3)]
        );
    }
}
