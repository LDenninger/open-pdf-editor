//! The correctness promise, enforced.
//!
//! > Opening a document and saving it without editing produces a structurally
//! > identical file.
//!
//! Every checked-in corpus file is driven through
//! [`opdf_roundtrip::assert_round_trip`] with the real parser. Until this
//! existed the harness had no callers at all: the machinery was written, the
//! corpus was checked in and hash-verified, and nothing ever opened a single
//! one of the files. Every CI run was green while the promise the project is
//! built around went untested.

use std::path::{Path, PathBuf};

use opdf_core::DocumentIo;
use opdf_pdf::PdfDocument;
use opdf_roundtrip::{CorpusEntry, CorpusManifest, RoundTripStrength, assert_round_trip};

/// Files whose whole purpose is to be broken. `open` must reject them or
/// recover from them, and must never panic; asking them to round-trip would
/// be asking the wrong question.
const PATHOLOGICAL: &str = "pathological";

fn corpus_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../corpus")
}

fn load_manifest() -> CorpusManifest {
    CorpusManifest::load(&corpus_dir().join("manifest.toml")).expect("the corpus manifest must parse")
}

fn path_of(entry: &CorpusEntry) -> PathBuf {
    corpus_dir().join("files").join(&entry.file)
}

/// The corpus files are what every other test in this file depends on, so
/// verify they are the bytes the manifest describes before trusting any
/// result taken from them.
#[test]
fn the_checked_in_corpus_matches_its_recorded_hashes() {
    load_manifest()
        .verify_checked_in(&corpus_dir().join("files"))
        .expect("every checked-in corpus file must match its recorded sha256");
}

/// The promise itself, over every corpus file that is a valid document.
///
/// `ByteIdentical` is deliberate and is the project's own bar: `opdf-pdf`
/// appends an incremental update, so an unedited save must reproduce the
/// original bytes exactly and then extend them. Any entry needing the weaker
/// structural check would have to say so in its manifest `notes`, and none
/// currently does.
#[test]
fn every_well_formed_corpus_file_round_trips_unchanged() {
    let manifest = load_manifest();
    let mut checked = 0;
    let mut failures: Vec<String> = Vec::new();

    for entry in manifest.checked_in() {
        if entry.tags.iter().any(|tag| tag == PATHOLOGICAL) {
            continue;
        }
        checked += 1;
        if let Err(failure) = assert_round_trip::<PdfDocument>(&path_of(entry), RoundTripStrength::ByteIdentical) {
            failures.push(format!("{}: {failure}", entry.file));
        }
    }

    assert!(
        checked > 0,
        "the manifest listed no well-formed checked-in files; the corpus loader or the tags are wrong"
    );
    assert!(
        failures.is_empty(),
        "{checked} files checked, {} failed the round trip:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

/// A file that is broken on purpose must fail cleanly.
///
/// The bar is deliberately weak — an error *or* a successful recovery both
/// pass — because `damaged_xref.pdf`'s manifest entry says either is
/// acceptable. What is not acceptable is a panic, and a panic in any of
/// these fails the test by unwinding out of it. This is the only thing that
/// opens these four files at all; before it they were hash-verified and
/// never parsed.
#[test]
fn every_pathological_corpus_file_is_survived_rather_than_panicked_on() {
    let manifest = load_manifest();
    let mut checked = 0;

    for entry in manifest.checked_in() {
        if !entry.tags.iter().any(|tag| tag == PATHOLOGICAL) {
            continue;
        }
        checked += 1;
        //--- the result is genuinely not asserted on; surviving the call is
        //--- the whole assertion, and it is the one that used to be missing ---
        let _ = PdfDocument::open(&path_of(entry));
    }

    assert!(
        checked > 0,
        "no corpus file carries the pathological tag; this test would silently prove nothing"
    );
}

/// A zero-byte file is not a PDF and must be rejected rather than opened as
/// an empty document, which would let a save write a valid PDF over it.
#[test]
fn a_zero_byte_file_is_rejected_rather_than_opened_as_an_empty_document() {
    let manifest = load_manifest();
    let entry = manifest
        .checked_in()
        .find(|entry| entry.file == "zero_byte.pdf")
        .expect("zero_byte.pdf must be in the manifest");

    assert!(
        PdfDocument::open(&path_of(entry)).is_err(),
        "an empty file must not open; opening it as a document with no pages would let a later save overwrite it with a valid PDF"
    );
}
