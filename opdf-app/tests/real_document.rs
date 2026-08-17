//! End-to-end: a real file on disk becomes real pixels in the canvas cache.
//!
//! Every other test in this crate stops at state — a page count, a scroll offset,
//! a texture id. This one is the project's only assertion about *pixels*: it
//! opens a corpus PDF through the production opener, drives frames until the
//! rasterizer answers, and checks that the tile which arrived is not a uniform
//! rectangle. A shell that draws a blank page passes every other test here.
//!
//! The pixels are read out of the frame's `textures_delta`, because that is where
//! they exist. The canvas cache holds `egui::TextureHandle`s, which are handles to
//! GPU allocations with no readback; the `ImageDelta` that egui hands the painter
//! is the same buffer on its way there, keyed by the texture id the cache is
//! holding. Matching the two is what makes this an assertion about the tile the
//! canvas would actually draw, rather than about any image that passed through.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use egui::epaint::ImageData;
use egui::{ColorImage, Context, Pos2, RawInput, Rect, TextureId, Vec2, vec2};
use opdf_app::app::OpdfApp;
use opdf_app::opener::{DocumentOpener, PdfiumDocumentOpener};

/// A window large enough that the first page is visible without scrolling.
const WINDOW_SIZE: Vec2 = vec2(1440.0_f32, 900.0_f32);

/// How long a frame takes in this test, standing in for the event loop's own pace.
///
/// The interval is not decoration. PDFium rasterizes on a worker thread behind a
/// process-wide lock, so the wait is for *work*, not for frames — and a headless
/// loop with no interval spins hundreds of frames in the time that work takes.
/// Bounding this test by a frame count alone made it pass at 70 ms and fail on a
/// cold start where loading `libpdfium` pushed the first tile past the bound, for
/// no reason connected to what it asserts.
const FRAME_INTERVAL: Duration = Duration::from_millis(5);

/// How long the rasterizer gets before the test calls it a failure.
///
/// Far longer than the ~70 ms an unoptimised build needs for one Letter page, so
/// a timeout here means no tile is coming rather than that the machine was busy.
const RENDER_DEADLINE: Duration = Duration::from_secs(10);

fn corpus_path(file_name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../tests/corpus/files").join(file_name)
}

/// Run one frame, recording every whole texture it uploaded.
fn run_frame(ctx: &Context, app: &mut OpdfApp, uploaded: &mut HashMap<TextureId, ColorImage>) {
    let input = RawInput {
        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, WINDOW_SIZE)),
        ..Default::default()
    };
    let output = ctx.run(input, |ctx| app.draw(ctx));
    for (id, delta) in output.textures_delta.set {
        //--- a patch updates part of an existing texture; only a full upload is a whole tile ---
        if delta.pos.is_some() {
            continue;
        }
        match delta.image {
            ImageData::Color(image) => {
                uploaded.insert(id, (*image).clone());
            }
        }
    }
}

#[test]
fn a_real_pdf_rasterizes_to_a_tile_that_is_not_blank() {
    let ctx = Context::default();
    let opened = PdfiumDocumentOpener.open(&corpus_path("irs_f1040.pdf")).unwrap();
    let expected_pages = opened.snapshot.page_count();
    let first_page = opened.snapshot.pages[0];
    let mut app = OpdfApp::new(&ctx, opened);

    assert!(expected_pages > 0, "the fixture must have pages or this test proves nothing");

    //--- drive frames until the first tile arrives, with a hard deadline ---
    let mut uploaded: HashMap<TextureId, ColorImage> = HashMap::new();
    let mut frames = 0;
    let started = Instant::now();
    while app.canvas_cache().is_empty() {
        assert!(
            started.elapsed() < RENDER_DEADLINE,
            "no tile arrived in {frames} frames over {:?}",
            started.elapsed()
        );
        run_frame(&ctx, &mut app, &mut uploaded);
        frames += 1;
        std::thread::sleep(FRAME_INTERVAL);
    }

    let (request, texture) = app.canvas_cache().entries().next().unwrap();
    let image = uploaded
        .get(&texture.id())
        .unwrap_or_else(|| panic!("the tile for {request:?} is cached but its pixels never reached the painter"));

    //--- the tile must be this page at this scale, not merely some image ---
    let expected_size = first_page.display_size();
    assert_eq!(
        image.size,
        [
            (expected_size.width_pt * request.scale).round() as usize,
            (expected_size.height_pt * request.scale).round() as usize
        ],
        "the tile does not match the page it claims to be, at scale {}",
        request.scale
    );

    let first = image.pixels[0];
    let differs = image.pixels.iter().step_by(8).any(|pixel| *pixel != first);
    assert!(
        differs,
        "every sampled pixel of the tile for {request:?} was {first:?} — the page rendered blank"
    );
}
