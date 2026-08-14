//! The two-page PDF the contract suite rasterizes.
//!
//! `assert_render_service_contract` builds a snapshot describing an unrotated
//! A4 page and a quarter-turned A4 page. This module writes a file matching it,
//! byte by byte, so that no binary blob has to be committed and so that the
//! fixture's geometry is visible in source rather than hidden in a hex dump.
//!
//! # Why the write is atomic
//!
//! Every test that rasterizes needs this file, the test harness runs those
//! tests on many threads at once, and `cargo test` runs several test binaries
//! at once. A plain `std::fs::write` truncates before it fills, so a reader
//! that opens the path at the wrong moment sees a partial file and Pdfium
//! answers `FormatError`. The bytes are therefore written to a private staging
//! path and renamed into place, which is atomic on a single filesystem: a
//! reader observes either the whole previous file or the whole new one.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};

/// Page content: a blue rectangle covering the whole media box, so a rendered
/// tile can be distinguished from a blank one by a single pixel read.
const PAGE_CONTENT: &str = "0 0 1 rg 0 0 595 842 re f";

/// Distinguishes the staging files of concurrent writers within one process.
static STAGING_COUNTER: AtomicU64 = AtomicU64::new(0);

/// The fixture path this process has already written.
static CONTRACT_FIXTURE: OnceLock<PathBuf> = OnceLock::new();

/// Write a two-page A4 fixture: page one unrotated, page two stored at 90 degrees.
///
/// The file appears at `pdf_path` atomically — see the module documentation.
pub fn write_contract_fixture(pdf_path: &Path) -> std::io::Result<()> {
    if let Some(parent_dir) = pdf_path.parent() {
        std::fs::create_dir_all(parent_dir)?;
    }

    //--- a staging name unique to this writer, beside the target so the rename stays on one filesystem ---
    let staging_name = format!(
        "{}.{}-{}.part",
        pdf_path.file_name().unwrap_or_default().to_string_lossy(),
        std::process::id(),
        STAGING_COUNTER.fetch_add(1, Ordering::Relaxed)
    );
    let staging_path = pdf_path.with_file_name(staging_name);

    std::fs::write(&staging_path, build_fixture_bytes())?;
    std::fs::rename(&staging_path, pdf_path)
}

/// The fixture's bytes.
fn build_fixture_bytes() -> Vec<u8> {
    let objects = [
        "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
        "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>".to_string(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 595 842] /Resources << >> /Contents 5 0 R >>".to_string(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 595 842] /Rotate 90 /Resources << >> /Contents 5 0 R >>".to_string(),
        format!("<< /Length {} >>\nstream\n{PAGE_CONTENT}\nendstream", PAGE_CONTENT.len()),
    ];

    let mut bytes: Vec<u8> = Vec::new();
    bytes.extend_from_slice(b"%PDF-1.7\n");

    let mut offsets: Vec<usize> = Vec::with_capacity(objects.len());
    for (index, body) in objects.iter().enumerate() {
        offsets.push(bytes.len());
        bytes.extend_from_slice(format!("{} 0 obj\n{body}\nendobj\n", index + 1).as_bytes());
    }

    //--- every xref entry is exactly twenty bytes, including the trailing space and newline ---
    let xref_offset = bytes.len();
    bytes.extend_from_slice(format!("xref\n0 {}\n", objects.len() + 1).as_bytes());
    bytes.extend_from_slice(b"0000000000 65535 f \n");
    for offset in &offsets {
        bytes.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }

    bytes.extend_from_slice(format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref_offset}\n%%EOF\n", objects.len() + 1).as_bytes());

    bytes
}

/// Path to the contract fixture, writing it if this process has not already.
///
/// The file lives under the workspace `target` directory: git-ignored, and
/// still inspectable when a test fails. Written at most once per process, and
/// atomically, so that the many tests calling this concurrently cannot observe
/// a half-written file.
pub fn ensure_contract_fixture() -> PathBuf {
    CONTRACT_FIXTURE
        .get_or_init(|| {
            let pdf_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../target/test-fixtures")
                .join("contract-two-page-a4.pdf");
            write_contract_fixture(&pdf_path).expect("the contract fixture must be writable");
            pdf_path
        })
        .clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_a_syntactically_valid_pdf() {
        let pdf_path = ensure_contract_fixture();
        let bytes = std::fs::read(&pdf_path).unwrap();

        assert!(bytes.starts_with(b"%PDF-1.7"), "the fixture must open with a PDF header");
        assert!(bytes.ends_with(b"%%EOF\n"), "the fixture must end with the end-of-file marker");

        let text = String::from_utf8(bytes.clone()).unwrap();
        assert!(text.contains("/MediaBox [0 0 595 842]"), "both pages must be A4");
        assert!(text.contains("/Rotate 90"), "page two must carry a quarter turn");

        //--- the offset in startxref must actually address the xref table ---
        let startxref_index = text.rfind("startxref\n").unwrap() + "startxref\n".len();
        let offset: usize = text[startxref_index..].lines().next().unwrap().parse().unwrap();
        assert_eq!(&text[offset..offset + 4], "xref", "startxref must address the cross-reference table");
    }

    #[test]
    fn pdfium_reads_two_a4_pages_with_the_expected_rotations() {
        use pdfium_render::prelude::*;

        let pdf_path = ensure_contract_fixture();
        let pdfium = crate::library::bind_pdfium().unwrap();
        //--- declared first so it outlives the document and pages below, whose drops are pdfium calls too ---
        let _guard = crate::library::lock_pdfium();
        let document = pdfium.load_pdf_from_file(&pdf_path, None).unwrap();
        let pages = document.pages();

        assert_eq!(pages.len(), 2, "the fixture must contain exactly two pages");

        let first = pages.get(0).unwrap();
        assert_eq!(first.rotation().unwrap(), PdfPageRenderRotation::None);

        let second = pages.get(1).unwrap();
        assert_eq!(second.rotation().unwrap(), PdfPageRenderRotation::Degrees90);
    }
}
