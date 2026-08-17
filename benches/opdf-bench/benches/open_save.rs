//! Benchmarks open + save_incremental latency against the checked-in corpus.
//!
//! Thresholds: opening and enumerating a 5,000-page document under 200ms;
//! `save_incremental` on an *unedited* document under 50ms regardless of the
//! original file's size -- the entire premise of incremental save is that its
//! cost tracks the edit, not the file. A regression here that scales with file
//! size rather than staying flat is a correctness-adjacent bug, not a
//! curiosity, because it means the save path stopped appending and started
//! rewriting.
//!
//! The save benchmarks deliberately span three files of very different sizes
//! (220 KB, 2.9 MB, and a 5,000-page synthetic) so that the flatness claim is
//! measurable rather than merely asserted: the three numbers should sit close
//! together, and a fan-out across them is the signal.

use std::path::{Path, PathBuf};

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use opdf_core::document::{Document, DocumentIo};
use opdf_pdf::PdfDocument;

/// Corpus files this benchmark opens, smallest first.
///
/// Only well-formed entries belong here; the pathological fixtures are the
/// round-trip test suite's business, and a file that fails to open would
/// simply panic the benchmark on its first iteration.
const SPECIMENS: &[&str] = &["irs_f1040.pdf", "zh_wiki_monthly.pdf", "huge_page_count.pdf"];

fn corpus_file(file_name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/corpus/files").join(file_name)
}

//---------------------------------------------------------------------
// Open
//---------------------------------------------------------------------

fn bench_open_and_enumerate(criterion: &mut Criterion) {
    let mut group = criterion.benchmark_group("PdfDocument::open + page_ids");
    for &file_name in SPECIMENS {
        group.bench_with_input(BenchmarkId::from_parameter(file_name), &file_name, |bencher, &file_name| {
            let path = corpus_file(file_name);
            bencher.iter(|| {
                let document = PdfDocument::open(&path).expect("a well-formed corpus file must open");
                let page_ids = document.page_ids();
                assert_eq!(page_ids.len(), document.page_count());
            });
        });
    }
    group.finish();
}

//---------------------------------------------------------------------
// Save
//---------------------------------------------------------------------

fn bench_save_incremental_unedited(criterion: &mut Criterion) {
    let mut group = criterion.benchmark_group("PdfDocument::save_incremental, unedited");
    for &file_name in SPECIMENS {
        group.bench_with_input(BenchmarkId::from_parameter(file_name), &file_name, |bencher, &file_name| {
            let path = corpus_file(file_name);
            //--- one scratch directory for the whole measurement, so the
            //--- timing covers the save and not tempdir creation ---
            let scratch = tempfile::tempdir().expect("a scratch directory must be creatable");
            let out_path = scratch.path().join("bench_save.pdf");
            bencher.iter_batched(
                || PdfDocument::open(&path).expect("a well-formed corpus file must open"),
                |mut document| {
                    document.save_incremental(&out_path).expect("an unedited save must succeed");
                },
                criterion::BatchSize::LargeInput,
            );
        });
    }
    group.finish();
}

criterion_group!(benches, bench_open_and_enumerate, bench_save_incremental_unedited);
criterion_main!(benches);
