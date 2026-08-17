//! The process-wide Pdfium binding, and the lock that makes it usable.
//!
//! Pdfium may be initialized exactly once per process: `Pdfium::new` asserts
//! that no bindings have been installed yet, and panics if they have. Several
//! [`crate::service::PdfiumRenderService`] instances may exist at once — the
//! contract suite alone builds ten — so the library instance is a singleton,
//! initialized on first use and never torn down.
//!
//! # The binding and the lock are one thing
//!
//! Pdfium makes no thread-safety guarantee, and `pdfium-render`'s `thread_safe`
//! feature does not supply one: in 0.9.3 it only adds `Send` and `Sync` to the
//! wrapper types, so nothing sequences the underlying C calls. **One** unlocked
//! call is enough to corrupt Pdfium's global state permanently — measured at
//! 167 to 184 failures out of 200 concurrent loads, after which every later
//! load in the process answers `FormatError`, on any thread, forever.
//!
//! A binding that can be obtained without the lock is therefore a binding that
//! can be misused exactly once. [`with_pdfium`] is the only way out of this
//! module: it takes the lock, hands the closure the instance, and drops the
//! guard when the closure returns. The higher-ranked lifetime on its argument
//! is load-bearing — it stops the closure from returning anything that borrows
//! from Pdfium, so no document, page, or bitmap can outlive the lock that
//! covers its own destructor.
//!
//! The one caller that genuinely needs a document to outlive a single locked
//! region is the render worker, which keeps one open for its whole life. It
//! uses `bind_pdfium` and `lock_pdfium` directly, and carries the
//! obligation described on `lock_pdfium` instead. That is why both are
//! crate-private: inside the crate the obligation is auditable, outside it is
//! not.

use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard, OnceLock};

use pdfium_render::prelude::Pdfium;

/// Environment variable naming the directory holding the Pdfium shared library.
///
/// When unset, the library is looked for in `vendor/pdfium/lib` relative to the
/// repository root, which is where `scripts/fetch-pdfium.sh` puts it.
pub const PDFIUM_LIB_DIR_VAR: &str = "OPDF_PDFIUM_LIB_DIR";

static PDFIUM: OnceLock<Result<Pdfium, String>> = OnceLock::new();

/// Run `action` with exclusive access to the process-wide Pdfium instance.
///
/// This is the whole public surface of the binding. The lock is held for the
/// duration of the closure and released when it returns, so every Pdfium call
/// made inside — including the destructors of anything created there — is
/// serialized against every other thread in the process.
///
/// Returns a human-readable reason when the shared library cannot be loaded;
/// the outcome is cached, so a failed load is not retried and the error is
/// stable for the lifetime of the process.
///
/// # Nothing borrowed may escape
///
/// `action` is higher-ranked over the lifetime of the `&Pdfium` it is given, so
/// its return type cannot mention that lifetime. A `PdfDocument`, `PdfPage`, or
/// bitmap borrowed from the instance therefore cannot be returned out of the
/// closure, and cannot be dropped — an FFI call in itself — after the guard is
/// gone.
///
/// ```
/// # use opdf_render::library::with_pdfium;
/// let version = with_pdfium(|_pdfium| 1_u32).unwrap();
/// assert_eq!(version, 1);
/// ```
///
/// ```compile_fail
/// # use opdf_render::library::with_pdfium;
/// // Carrying a document out of the closure would put its destructor — an FFI
/// // call — outside the lock. The higher-ranked lifetime rejects it.
/// let document = with_pdfium(|pdfium| pdfium.load_pdf_from_file(std::path::Path::new("a.pdf"), None));
/// ```
///
/// # The binding itself is not reachable
///
/// ```compile_fail
/// // Reaching the binding directly would let a caller skip the lock, and one
/// // unlocked load corrupts Pdfium for the rest of the process.
/// let _ = opdf_render::library::bind_pdfium();
/// ```
pub fn with_pdfium<T>(action: impl for<'pdfium> FnOnce(&'pdfium Pdfium) -> T) -> Result<T, String> {
    let pdfium = bind_pdfium()?;
    //--- declared before anything the closure builds, so those destructors are still covered ---
    let _guard = lock_pdfium();
    Ok(action(pdfium))
}

/// The process-wide Pdfium instance, initializing it on first call.
///
/// Returns a human-readable reason when the shared library cannot be loaded.
/// The outcome is cached: a failed load is not retried, so the error is stable
/// for the lifetime of the process.
///
/// Crate-private on purpose — see the module documentation. Callers outside
/// this crate get [`with_pdfium`], which cannot be used without the lock.
pub(crate) fn bind_pdfium() -> Result<&'static Pdfium, String> {
    PDFIUM.get_or_init(load_pdfium).as_ref().map_err(Clone::clone)
}

/// Serializes every call into Pdfium, process-wide.
static PDFIUM_LOCK: Mutex<()> = Mutex::new(());

/// Acquire exclusive access to Pdfium for the duration of the returned guard.
///
/// **Every** call into Pdfium must hold this — including the drops that close a
/// document, a page, or a bitmap, which are FFI calls like any other. Two
/// threads loading a document at once corrupt Pdfium's global state
/// *permanently*; see the module documentation for the measurement. One worker
/// thread per service is not enough isolation on its own, because several
/// services exist at once.
///
/// # Declare the guard first
///
/// Rust drops locals in reverse declaration order, so the guard must be
/// declared **before** every value whose own destructor calls into Pdfium:
///
/// ```ignore
/// let _guard = lock_pdfium();       // declared first, so dropped last
/// let document = pdfium.load_pdf_from_file(&path, None)?;
/// let pages = document.pages();     // dropped before the guard
/// ```
///
/// Reversing those two lines compiles, passes, and closes the document with the
/// lock already released. Prefer [`with_pdfium`], where the ordering is not the
/// caller's to get wrong; reach for this only where a handle must outlive a
/// single locked region, as it does in the render worker.
///
/// The lock is poison-tolerant: a panic while rendering says nothing about
/// Pdfium's own state, and refusing every later render would turn one bad page
/// into a dead renderer.
pub(crate) fn lock_pdfium() -> MutexGuard<'static, ()> {
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
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn binds_to_the_vendored_library() {
        let result = with_pdfium(|_pdfium| ());
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
                    //--- the document is built and dropped inside the closure, so both are covered ---
                    let outcome = with_pdfium(|pdfium| match pdfium.load_pdf_from_file(&thread_path, None) {
                        Ok(_document) => None,
                        Err(error) => Some(format!("{error:?}")),
                    })
                    .unwrap();
                    if let Some(error) = outcome {
                        errors.push(error);
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

    /// The load test above proves Pdfium survives; this proves *why* — that no
    /// two closures are ever inside `with_pdfium` at the same moment. Without
    /// the guard the counter would exceed one within a few iterations.
    #[test]
    fn never_runs_two_closures_at_once() {
        static INSIDE: AtomicUsize = AtomicUsize::new(0);
        static OVERLAPS: AtomicUsize = AtomicUsize::new(0);

        let mut handles = Vec::new();
        for _ii in 0..8 {
            handles.push(std::thread::spawn(|| {
                for _jj in 0..200 {
                    with_pdfium(|_pdfium| {
                        if INSIDE.fetch_add(1, Ordering::SeqCst) != 0 {
                            OVERLAPS.fetch_add(1, Ordering::SeqCst);
                        }
                        std::thread::yield_now();
                        INSIDE.fetch_sub(1, Ordering::SeqCst);
                    })
                    .unwrap();
                }
            }));
        }
        for handle in handles {
            handle.join().unwrap();
        }

        assert_eq!(
            OVERLAPS.load(Ordering::SeqCst),
            0,
            "with_pdfium must hold the lock for the whole closure, so no two calls into pdfium overlap"
        );
    }
}
