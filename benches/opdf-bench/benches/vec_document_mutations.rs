//! Benchmarks VecDocument mutation cost at increasing page counts.
//!
//! VecDocument stores pages in a Vec, so remove_page/insert_page are O(n)
//! by construction. This benchmark makes that scaling visible -- a real
//! property of the fake, not a bug, but worth tracking since page
//! operations (Track C) are built and tested against this fake first.
//! Threshold: a single mutation at 10,000 pages should stay under 1ms.

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use opdf_core::document::Document;
use opdf_core::fakes::VecDocument;
use opdf_core::page::PageSize;

fn bench_remove_page_at_scale(criterion: &mut Criterion) {
    let mut group = criterion.benchmark_group("VecDocument::remove_page");
    for &page_count in &[100_usize, 1_000, 10_000] {
        group.bench_with_input(BenchmarkId::from_parameter(page_count), &page_count, |bencher, &page_count| {
            bencher.iter_batched(
                || VecDocument::with_pages(page_count, PageSize::A4),
                |mut document| {
                    let middle = document.page_ids()[page_count / 2];
                    document.remove_page(middle).unwrap();
                },
                criterion::BatchSize::LargeInput,
            );
        });
    }
    group.finish();
}

fn bench_move_page_at_scale(criterion: &mut Criterion) {
    let mut group = criterion.benchmark_group("VecDocument::move_page");
    for &page_count in &[100_usize, 1_000, 10_000] {
        group.bench_with_input(BenchmarkId::from_parameter(page_count), &page_count, |bencher, &page_count| {
            bencher.iter_batched(
                || VecDocument::with_pages(page_count, PageSize::A4),
                |mut document| {
                    let first = document.page_ids()[0];
                    document.move_page(first, page_count - 1).unwrap();
                },
                criterion::BatchSize::LargeInput,
            );
        });
    }
    group.finish();
}

criterion_group!(benches, bench_remove_page_at_scale, bench_move_page_at_scale);
criterion_main!(benches);
