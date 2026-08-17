#![no_main]

use libfuzzer_sys::fuzz_target;
use opdf_core::{PageId, RenderRequest};

#[derive(Debug, arbitrary::Arbitrary)]
struct RenderRequestInput {
    page_raw: u64,
    revision: u64,
    scale_bits: u32,
}

fuzz_target!(|input: RenderRequestInput| {
    let scale = f32::from_bits(input.scale_bits);
    let result = RenderRequest::new(PageId::new(input.page_raw), input.revision, scale);

    match result {
        Ok(request) => {
            // The one invariant new() does guarantee, per contracts.md:
            // finite and positive. It deliberately does NOT bound the
            // magnitude -- see Known gap #2 -- so a scale of 1e30 reaching
            // this branch is expected, not a bug this target reports.
            assert!(
                request.scale.is_finite() && request.scale > 0.0,
                "RenderRequest::new accepted a non-finite or non-positive scale"
            );
        }
        Err(_) => {
            // Rejected -- must be because the scale was non-finite or <= 0.
            assert!(
                !scale.is_finite() || scale <= 0.0,
                "RenderRequest::new rejected a finite, positive scale of {scale}"
            );
        }
    }
});
