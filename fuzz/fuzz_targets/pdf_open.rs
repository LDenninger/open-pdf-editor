#![no_main]

use libfuzzer_sys::fuzz_target;
use opdf_core::{Document, DocumentIo};
use opdf_pdf::PdfDocument;

/// Cap on the input written to disk each iteration.
///
/// `open` is the only entry point that takes attacker-controlled bytes, so the
/// interesting inputs are structural (a malformed xref, a cyclic page tree, a
/// bogus `/Length`), not large. Past this size libFuzzer spends its budget on
/// file I/O rather than on structure.
const MAX_INPUT_BYTES: usize = 1 << 20;

fuzz_target!(|data: &[u8]| {
    if data.len() > MAX_INPUT_BYTES {
        return;
    }

    // `open` takes a path, not a byte slice, so the fuzzer's input has to be
    // written out each iteration. Slower than an in-memory entry point would
    // be, but `DocumentIo::open`'s signature is fixed by the core contract and
    // `opdf-pdf` is not this crate's to change.
    let Ok(temp_dir) = tempfile::tempdir() else {
        return;
    };
    let input_path = temp_dir.path().join("fuzz_input.pdf");
    if std::fs::write(&input_path, data).is_err() {
        return;
    }

    // Arbitrary bytes must never panic `open`: a clean `Err` and a successful
    // parse are both acceptable, an unwind is not.
    let Ok(document) = PdfDocument::open(&input_path) else {
        return;
    };

    //--- a document that opened at all must be self-consistent, otherwise
    //--- every caller downstream indexes into a page list that disagrees with
    //--- its own count ---
    let page_ids = document.page_ids();
    assert_eq!(
        page_ids.len(),
        document.page_count(),
        "page_ids() and page_count() disagree on a document open() accepted"
    );

    for (index, id) in page_ids.iter().enumerate() {
        // Every id `page_ids` hands out must resolve, and must resolve to the
        // position it was handed out at. A parser that accepts a malformed
        // page tree and then produces ids it cannot look up is the failure
        // mode this checks for.
        assert!(document.page(*id).is_ok(), "page_ids() returned {id:?}, which page() cannot resolve");
        assert_eq!(
            document.index_of(*id).ok(),
            Some(index),
            "index_of({id:?}) disagrees with its position in page_ids()"
        );
    }
});
