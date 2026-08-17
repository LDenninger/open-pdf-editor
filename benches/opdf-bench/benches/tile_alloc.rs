//! Benchmarks Tile::new allocation cost and FakeRenderService throughput.
//!
//! Thresholds (see the task description in the Track E plan for rationale):
//! this file does not assert pass/fail -- criterion reports timings and
//! regressions against a saved baseline, it does not fail a build on its
//! own. Treat a sudden jump here as a signal to investigate, not a gate.

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use opdf_core::Tile;
use opdf_core::document::{DocumentId, DocumentSnapshot};
use opdf_core::fakes::FakeRenderService;
use opdf_core::page::{PageId, PageInfo, PageSize, Rotation};
use opdf_core::render::{RenderRequest, RenderService};

fn bench_tile_new(criterion: &mut Criterion) {
    let mut group = criterion.benchmark_group("Tile::new");
    for &side in &[64_u32, 512, 2048] {
        let pixels = vec![0_u8; side as usize * side as usize * 4];
        group.bench_with_input(BenchmarkId::from_parameter(side), &side, |bencher, &side| {
            bencher.iter(|| Tile::new(side, side, pixels.clone()).unwrap());
        });
    }
    group.finish();
}

fn bench_fake_render_service_throughput(criterion: &mut Criterion) {
    let document = DocumentId::new_unique();
    let snapshot = DocumentSnapshot {
        document,
        pages: vec![PageInfo {
            id: PageId::new(1),
            size: PageSize::A4,
            rotation: Rotation::None,
        }],
        revision: 0,
    };

    criterion.bench_function("FakeRenderService: submit+poll one A4 tile at scale 2.0", |bencher| {
        bencher.iter(|| {
            let service = FakeRenderService::new(snapshot.clone());
            let request = RenderRequest::new(document, PageId::new(1), 0, 2.0).unwrap();
            service.submit(request);
            let responses = service.poll();
            assert_eq!(responses.len(), 1);
        });
    });
}

criterion_group!(benches, bench_tile_new, bench_fake_render_service_throughput);
criterion_main!(benches);
