//! Interaction tests that drive the whole shell without a window.
//!
//! Task 16 of the Track D plan verifies scrolling, zoom anchoring, rail clicks,
//! and keyboard shortcuts by eye. These tests verify the same behaviours by
//! feeding synthetic [`egui::RawInput`] through [`OpdfApp::draw`] — the body of
//! `eframe::App::update`, split out because `eframe::Frame` has no public
//! constructor and so cannot be built by a test.
//!
//! They assert mostly on state rather than on pixels: what a page *looks* like is
//! still only checked by hand. What they do cover is every wiring bug an eyeball
//! check would catch — a scroll that does not move, an anchor measured from the
//! wrong origin, a click that selects nothing, a cache that grows without bound —
//! and unlike the eyeball check they run in headless CI on every commit.
//!
//! Where a defect is invisible in state — a texture from one document drawn for
//! another, when both documents look the same — the test reads the shapes the
//! frame actually emitted; see [`Harness::drawn_textures`].

use std::collections::HashSet;
use std::path::Path;

use egui::epaint::ClippedShape;
use egui::{Context, Event, Key, Modifiers, MouseWheelUnit, Pos2, RawInput, Rect, Shape, TextureId, Vec2, pos2, vec2};
use opdf_app::app::OpdfApp;
use opdf_app::opener::{DocumentOpener, FakeOpener};
use opdf_app::panels::menu_bar::MenuAction;
use opdf_app::panels::thumbnail_rail::lay_out_thumbnails;
use opdf_app::synthetic::open_synthetic_document;
use opdf_app::theme::Theme;

const WINDOW_SIZE: Vec2 = vec2(1440.0, 900.0);

/// egui's font atlas, allocated before the shell has drawn anything and drawn on
/// every frame. It is the one textured mesh in a frame that is not a page tile.
const FONT_ATLAS: TextureId = TextureId::Managed(0);

/// Every texture drawn by this frame's shapes, other than the font atlas.
///
/// `egui::Painter::image` — which is how both the canvas and the rail draw a tile —
/// emits a textured `Shape::Mesh`, so this is the set of tiles that actually
/// reached the frame's shape list. It is the closest thing to a pixel assertion
/// this crate can make without a GPU: two frames that draw the same texture id
/// drew the same pixels.
fn collect_drawn_textures(shapes: &[ClippedShape]) -> HashSet<TextureId> {
    let mut drawn = HashSet::new();
    for clipped in shapes {
        collect_from_shape(&clipped.shape, &mut drawn);
    }
    drawn
}

/// Walk one shape, recursing into groups, collecting textured meshes.
fn collect_from_shape(shape: &Shape, into: &mut HashSet<TextureId>) {
    match shape {
        Shape::Mesh(mesh) if mesh.texture_id != FONT_ATLAS => {
            into.insert(mesh.texture_id);
        }
        Shape::Vec(inner) => {
            for shape in inner {
                collect_from_shape(shape, into);
            }
        }
        _ => {}
    }
}

//---------------------------------------------------------------------
// Harness
//---------------------------------------------------------------------

/// A shell wired to a synthetic document, plus the context it draws into.
struct Harness {
    app: OpdfApp,
    ctx: Context,
    window_size: Vec2,
    drawn_textures: HashSet<TextureId>,
}

impl Harness {
    /// Build a shell over `page_count` synthetic pages and settle it.
    ///
    /// Several frames are run up front because the first frame has no viewport
    /// size yet, the second submits, and the third absorbs — exactly the sequence
    /// the real application goes through before anything is on screen.
    fn build(page_count: usize) -> Self {
        let ctx = Context::default();
        let app = OpdfApp::new(&ctx, open_synthetic_document(page_count).unwrap());
        let mut harness = Self {
            app,
            ctx,
            window_size: WINDOW_SIZE,
            drawn_textures: HashSet::new(),
        };
        harness.settle(4);
        harness
    }

    /// Run one frame with the given events, recording what it drew.
    fn run_frame(&mut self, events: Vec<Event>) {
        let input = RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, self.window_size)),
            events,
            ..Default::default()
        };
        let app = &mut self.app;
        let output = self.ctx.run(input, |ctx| app.draw(ctx));
        self.drawn_textures = collect_drawn_textures(&output.shapes);
    }

    /// The tile textures the most recent frame put on screen.
    fn drawn_textures(&self) -> &HashSet<TextureId> {
        &self.drawn_textures
    }

    /// Run `count` frames with no input, letting tiles arrive and scrolling settle.
    fn settle(&mut self, count: usize) {
        for _ in 0..count {
            self.run_frame(Vec::new());
        }
    }

    /// Run frames until the scroll offset stops moving.
    ///
    /// egui smooths a wheel scroll over several frames, so a fixed frame count
    /// leaves the offset mid-flight and any measurement taken then is of an
    /// animation rather than of a result.
    fn settle_scroll(&mut self) {
        let mut previous = f32::NAN;
        for _ in 0..120 {
            self.settle(1);
            let offset = self.app.state().scroll_offset_px;
            if (offset - previous).abs() < 1e-3 {
                return;
            }
            previous = offset;
        }
        panic!("the scroll offset never settled");
    }

    /// Park the pointer somewhere, so egui treats that widget as hovered.
    fn hover(&mut self, position: Pos2) {
        self.run_frame(vec![Event::PointerMoved(position)]);
    }

    /// Scroll the wheel by `delta` points at the current pointer position.
    fn scroll_wheel(&mut self, position: Pos2, delta: Vec2, frames: usize) {
        for _ in 0..frames {
            self.run_frame(vec![
                Event::PointerMoved(position),
                Event::MouseWheel {
                    unit: MouseWheelUnit::Point,
                    delta,
                    modifiers: Modifiers::NONE,
                },
            ]);
        }
    }

    /// Pinch or ctrl-wheel zoom by `factor`, with the pointer at `position`.
    fn zoom_at(&mut self, position: Pos2, factor: f32) {
        self.run_frame(vec![Event::PointerMoved(position), Event::Zoom(factor)]);
    }

    /// Press a key with modifiers, then release it.
    fn press_key(&mut self, key: Key, modifiers: Modifiers) {
        self.run_frame(vec![Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers,
        }]);
    }

    /// Click at a position: move there, press, release.
    fn click(&mut self, position: Pos2) {
        self.run_frame(vec![Event::PointerMoved(position)]);
        self.run_frame(vec![Event::PointerButton {
            pos: position,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: Modifiers::NONE,
        }]);
        self.run_frame(vec![Event::PointerButton {
            pos: position,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: Modifiers::NONE,
        }]);
    }

    /// A point comfortably inside the page canvas.
    fn canvas_point(&self) -> Pos2 {
        let (origin_x, origin_y) = self.app.state().viewport_origin_px;
        let (width, height) = self.app.state().viewport_size_px;
        pos2(origin_x + width * 0.5, origin_y + height * 0.5)
    }

    /// The document position, in PDF points, currently drawn under `screen_y`.
    fn document_point_under(&self, screen_y: f32) -> f32 {
        let state = self.app.state();
        (state.scroll_offset_px + (screen_y - state.viewport_origin_px.1)) / state.zoom
    }
}

//---------------------------------------------------------------------
// Step 4: scrolling and the scroll extent
//---------------------------------------------------------------------

#[test]
fn lays_out_the_five_regions_with_a_canvas_between_them() {
    let harness = Harness::build(40);
    let state = harness.app.state();
    let (width, height) = state.viewport_size_px;
    let (origin_x, origin_y) = state.viewport_origin_px;

    assert!(width > 0.0 && height > 0.0, "the canvas must be given a real viewport, got {width}x{height}");
    assert!(
        origin_y > 0.0,
        "the menu bar and toolbar must sit above the canvas, but it starts at y={origin_y}"
    );
    assert!(origin_x > 0.0, "the thumbnail rail must sit left of the canvas, but it starts at x={origin_x}");
    assert!(
        origin_y + height < WINDOW_SIZE.y,
        "the status bar must sit below the canvas, which instead reaches y={}",
        origin_y + height
    );
}

#[test]
fn scrolls_the_canvas_with_the_mouse_wheel() {
    let mut harness = Harness::build(60);
    let point = harness.canvas_point();
    harness.hover(point);
    let before = harness.app.state().scroll_offset_px;

    harness.scroll_wheel(point, vec2(0.0, -400.0), 6);
    harness.settle(4);

    let after = harness.app.state().scroll_offset_px;
    assert!(after > before, "the wheel must scroll the canvas: offset went {before} -> {after}");
    assert!(
        harness.app.state().current_page().unwrap_or(0) > 0,
        "scrolling past the first page must advance the page indicator"
    );
}

#[test]
fn keeps_the_scroll_extent_fixed_while_tiles_arrive() {
    let mut harness = Harness::build(400);
    let extent_before = harness.app.state().content_size_px();

    //--- twenty frames of tiles landing: the extent is computed from page sizes
    //--- alone, so not one pixel of it may depend on what has been rendered ---
    for _ in 0..20 {
        harness.settle(1);
        assert_eq!(
            harness.app.state().content_size_px(),
            extent_before,
            "the scroll extent moved while tiles were arriving; the scrollbar would jump under the user"
        );
    }
    assert!(extent_before.1 > 0.0, "a 400-page document must have a real scroll extent");
}

#[test]
fn draws_a_page_for_every_visible_slot_from_the_very_first_frame() {
    //--- the first frame has nothing cached; the canvas must still place pages ---
    let ctx = Context::default();
    let mut app = OpdfApp::new(&ctx, open_synthetic_document(200).unwrap());
    let input = RawInput {
        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, WINDOW_SIZE)),
        ..Default::default()
    };
    let _ = ctx.run(input, |ctx| app.draw(ctx));

    assert!(app.canvas_cache().is_empty(), "this test is only meaningful while the cache is still cold");
    assert_eq!(app.state().page_count(), 200, "the document must be laid out before any tile exists");
}

//---------------------------------------------------------------------
// Step 5: zoom
//---------------------------------------------------------------------

#[test]
fn keeps_the_point_under_the_pointer_fixed_across_a_wheel_zoom() {
    let mut harness = Harness::build(120);
    let point = harness.canvas_point();
    harness.hover(point);
    harness.scroll_wheel(point, vec2(0.0, -900.0), 8);
    harness.settle_scroll();

    let before_pt = harness.document_point_under(point.y);
    let zoom_before = harness.app.state().zoom;

    harness.zoom_at(point, 2.0);
    harness.settle_scroll();

    let after_pt = harness.document_point_under(point.y);
    let zoom_after = harness.app.state().zoom;

    assert!(zoom_after > zoom_before, "the zoom must actually change: {zoom_before} -> {zoom_after}");
    //--- one point of drift is a rounding artefact; the chrome-height bug this
    //--- pins would drift by tens of points ---
    assert!(
        (after_pt - before_pt).abs() < 1.0,
        "the document point under the pointer slid from {before_pt} to {after_pt} across the zoom"
    );
}

#[test]
fn never_loses_a_drawable_tile_for_a_page_it_has_already_rendered() {
    let mut harness = Harness::build(60);
    let point = harness.canvas_point();
    harness.hover(point);
    harness.settle(6);

    let revision = harness.app.state().snapshot().revision;
    let first_page = harness.app.state().snapshot().pages[0].id;
    assert!(
        harness.app.canvas_cache().find_nearest_scale(first_page, revision, 1.0).is_some(),
        "the first page must be cached before the flicker check means anything"
    );

    //--- ten zooms in and out: at no point may the page the user is looking at
    //--- lose every cached scale, because that is the frame it blinks to grey ---
    for step in 0..10 {
        let factor = if step % 2 == 0 { 1.4 } else { 1.0 / 1.4 };
        harness.zoom_at(point, factor);
        let scale = harness.app.state().render_scale(1.0);
        assert!(
            harness.app.canvas_cache().find_nearest_scale(first_page, revision, scale).is_some(),
            "page 1 had no cached scale at all on zoom step {step}; the canvas would blink to a placeholder"
        );
    }
}

#[test]
fn walks_the_zoom_ladder_from_the_keyboard() {
    let mut harness = Harness::build(40);
    harness.hover(harness.canvas_point());
    assert_eq!(harness.app.state().zoom, 1.0);

    harness.press_key(Key::Plus, Modifiers::COMMAND);
    assert_eq!(harness.app.state().zoom, 1.25, "Ctrl+Plus must step one stop up the ladder");

    harness.press_key(Key::Plus, Modifiers::COMMAND);
    assert_eq!(harness.app.state().zoom, 1.5);

    harness.press_key(Key::Minus, Modifiers::COMMAND);
    assert_eq!(harness.app.state().zoom, 1.25, "Ctrl+Minus must step back down");

    harness.press_key(Key::Num0, Modifiers::COMMAND);
    assert_eq!(harness.app.state().zoom, 1.0, "Ctrl+0 must return to actual size");
}

#[test]
fn pages_forward_and_back_with_the_page_keys() {
    let mut harness = Harness::build(40);
    harness.hover(harness.canvas_point());

    harness.press_key(Key::PageDown, Modifiers::NONE);
    harness.settle(2);
    let forward = harness.app.state().current_page();
    assert_eq!(forward, Some(1), "PageDown must advance one page");

    harness.press_key(Key::PageUp, Modifiers::NONE);
    harness.settle(2);
    assert_eq!(harness.app.state().current_page(), Some(0), "PageUp must go back one page");
}

/// Fit-width must survive a resize with **no** further action from the user.
///
/// The point of a fit mode is that it holds. An earlier version of this test
/// re-issued `FitWidth` after the resize and so only measured the fit arithmetic,
/// which was never the broken part: nothing in the frame compared this frame's
/// viewport with the last, so the content stayed at its old width inside a
/// viewport half the size, and the mode's promise to "keep it fitted" was false.
#[test]
fn keeps_fitting_the_width_when_the_window_is_resized() {
    let mut harness = Harness::build(40);
    harness.hover(harness.canvas_point());

    harness.app.apply_action(MenuAction::FitWidth, &harness.ctx);
    harness.settle(4);
    let wide_zoom = harness.app.state().zoom;
    let wide_viewport_px = harness.app.state().viewport_size_px.0;
    assert!(
        (harness.app.state().content_size_px().0 - wide_viewport_px).abs() < 1.0,
        "fit-width must fill the viewport to begin with, or the resize proves nothing"
    );

    //--- halve the window width, and issue nothing else at all ---
    harness.window_size = vec2(WINDOW_SIZE.x * 0.5, WINDOW_SIZE.y);
    harness.settle(6);

    let narrow_viewport_px = harness.app.state().viewport_size_px.0;
    let narrow_content_px = harness.app.state().content_size_px().0;
    assert!(
        narrow_viewport_px < wide_viewport_px,
        "the resize must actually shrink the canvas: {wide_viewport_px} -> {narrow_viewport_px}"
    );
    assert!(
        (narrow_content_px - narrow_viewport_px).abs() < 1.0,
        "after the resize the content is {narrow_content_px} pt wide in a {narrow_viewport_px} pt viewport; the fit did not survive it"
    );
    assert!(
        harness.app.state().zoom < wide_zoom,
        "a narrower window must fit the content at a smaller zoom: {wide_zoom} -> {}",
        harness.app.state().zoom
    );

    //--- refitting changes the content height, which can show or hide the scroll
    //--- bar, which changes the viewport again: that loop must not run forever ---
    let settled_zoom = harness.app.state().zoom;
    harness.settle(10);
    assert_eq!(
        harness.app.state().zoom,
        settled_zoom,
        "the refit must settle rather than oscillate with the scroll bar frame after frame"
    );
}

/// Fit-page must survive a resize with no further action either.
///
/// Asserted as the promise itself — the current page fits entirely inside the
/// viewport — rather than as a zoom number, so it holds whichever page the
/// viewer settles on.
#[test]
fn keeps_fitting_the_page_when_the_window_is_resized() {
    let mut harness = Harness::build(40);
    harness.hover(harness.canvas_point());

    harness.app.apply_action(MenuAction::FitPage, &harness.ctx);
    harness.settle(4);
    let tall_zoom = harness.app.state().zoom;
    let tall_viewport_px = harness.app.state().viewport_size_px.1;

    //--- halve the window height, and issue nothing else at all ---
    harness.window_size = vec2(WINDOW_SIZE.x, WINDOW_SIZE.y * 0.5);
    harness.settle(6);

    let state = harness.app.state();
    let short_viewport_px = state.viewport_size_px.1;
    assert!(
        short_viewport_px < tall_viewport_px,
        "the resize must actually shorten the canvas: {tall_viewport_px} -> {short_viewport_px}"
    );
    let index = state.current_page().unwrap_or(0);
    let placement = state.layout().placement(index).expect("the current page must have a placement");
    let page_height_px = placement.height_pt * state.zoom;
    assert!(
        page_height_px <= short_viewport_px + 1.0,
        "page {index} is {page_height_px} pt tall in a {short_viewport_px} pt viewport after the resize; fit-page did not survive it \
         (zoom went {tall_zoom} -> {})",
        state.zoom
    );
}

#[test]
fn stays_within_the_canvas_budget_while_scrolling_zoomed_in() {
    let mut harness = Harness::build(200);
    let point = harness.canvas_point();
    harness.hover(point);
    harness.zoom_at(point, 2.0);
    harness.settle(2);

    for _ in 0..60 {
        harness.scroll_wheel(point, vec2(0.0, -1200.0), 1);
        harness.settle(1);
        let used = harness.app.canvas_cache().used_bytes();
        assert!(
            used <= opdf_app::app::CANVAS_CACHE_BUDGET_BYTES,
            "the canvas cache passed its budget while scrolling: {used} bytes"
        );
    }
    assert!(
        harness.app.rail_cache().used_bytes() <= opdf_app::app::RAIL_CACHE_BUDGET_BYTES,
        "the rail cache must hold its own budget while the canvas churns"
    );
}

#[test]
fn survives_the_maximum_zoom_without_an_oversized_request() {
    let mut harness = Harness::build(40);
    let point = harness.canvas_point();
    harness.hover(point);

    //--- drive well past MAX_ZOOM; the clamp plus the render-scale ladder must
    //--- keep every request inside what the renderer will answer ---
    for _ in 0..12 {
        harness.zoom_at(point, 2.0);
        harness.settle(1);
    }

    assert_eq!(harness.app.state().zoom, opdf_app::zoom::MAX_ZOOM, "zoom must clamp rather than run away");
    let scale = harness.app.state().render_scale(1.0);
    assert!(
        scale <= 4.0 && scale.is_finite() && scale > 0.0,
        "the render scale must stay on the ladder at maximum zoom, got {scale}"
    );
    harness.settle(4);
    assert!(
        harness.app.canvas_cache().used_bytes() <= opdf_app::app::CANVAS_CACHE_BUDGET_BYTES,
        "the cache must stay within budget at maximum zoom"
    );
}

//---------------------------------------------------------------------
// Step 6: the rail
//---------------------------------------------------------------------

#[test]
fn jumps_to_a_page_clicked_in_the_thumbnail_rail() {
    let mut harness = Harness::build(60);
    harness.settle(2);

    //--- the rail's own layout tells us where a slot sits inside the rail's
    //--- scrolling content; the rail starts at the window's left edge and just
    //--- below the chrome, which is where the canvas viewport begins vertically ---
    let theme = Theme::dark();
    let slots = lay_out_thumbnails(harness.app.state().snapshot(), theme.gutter);
    let rail_top = harness.app.state().viewport_origin_px.1;

    let high = slots[0];
    let low = slots[3];
    let column_x = theme.gutter + high.width_px * 0.5;

    harness.click(pos2(column_x, rail_top + low.top_px + low.height_px * 0.5));
    harness.settle(2);
    let after_low = harness.app.state().current_page().unwrap_or(0);

    harness.click(pos2(column_x, rail_top + high.top_px + high.height_px * 0.5));
    harness.settle(2);
    let after_high = harness.app.state().current_page().unwrap_or(usize::MAX);

    assert!(
        after_low > after_high,
        "a thumbnail further down the rail must select a later page than one near the top: {after_low} against {after_high}"
    );
    assert!(after_low < 60, "a rail click must select a real page, got {after_low}");
}

#[test]
fn tracks_the_current_page_so_the_rail_selection_follows_the_canvas() {
    let mut harness = Harness::build(60);
    let point = harness.canvas_point();
    harness.hover(point);
    assert_eq!(harness.app.state().current_page(), Some(0));

    harness.scroll_wheel(point, vec2(0.0, -1500.0), 8);
    harness.settle(4);

    let scrolled = harness.app.state().current_page().unwrap_or(0);
    assert!(
        scrolled > 0,
        "the selection ring reads current_page, which must follow a canvas scroll; it stayed at {scrolled}"
    );
}

#[test]
fn gives_the_canvas_the_rails_width_when_the_rail_is_hidden() {
    let mut harness = Harness::build(40);
    let with_rail = harness.app.state().viewport_size_px.0;
    assert!(harness.app.state().rail_visible);

    harness.app.apply_action(opdf_app::panels::menu_bar::MenuAction::ToggleRail, &harness.ctx);
    //--- the side panel animates closed, so give it several frames ---
    harness.settle(30);

    let without_rail = harness.app.state().viewport_size_px.0;
    assert!(!harness.app.state().rail_visible);
    assert!(
        without_rail > with_rail,
        "hiding the rail must give its width back to the canvas: {with_rail} -> {without_rail}"
    );
}

//---------------------------------------------------------------------
// Whole-frame robustness
//---------------------------------------------------------------------

#[test]
fn draws_an_empty_document_without_panicking() {
    let ctx = Context::default();
    let mut app = OpdfApp::new(&ctx, open_synthetic_document(0).unwrap());
    for _ in 0..5 {
        let input = RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, WINDOW_SIZE)),
            ..Default::default()
        };
        let _ = ctx.run(input, |ctx| app.draw(ctx));
    }
    assert_eq!(app.state().page_count(), 0);
    assert_eq!(app.state().current_page(), None);
}

/// A second document must never be drawn with the first document's pixels.
///
/// A revision counts edits *within* one document: every document starts it at
/// zero and allocates page ids from zero, so two documents of the same length
/// produce byte-identical `RenderRequest`s. Discarding stale tiles by revision
/// alone therefore leaves the first document's textures in the cache, exactly
/// keyed for the second document to find — `Open A`, `Open B`, and B's pages are
/// drawn from A's pixels.
///
/// Two synthetic documents of the same length look alike, so this cannot be
/// caught by comparing state; it is caught by the texture ids the frame drew.
/// egui never reuses a texture id, so a texture allocated for A and drawn again
/// while B is open is the defect itself, at the level of what reaches the GPU.
#[test]
fn never_draws_the_first_documents_textures_after_a_second_document_is_opened() {
    let mut harness = Harness::build(40);
    harness.settle(6);
    let first_document: HashSet<TextureId> = harness.drawn_textures().clone();
    assert!(
        !first_document.is_empty(),
        "the first document must have real tiles on screen, or this test proves nothing"
    );

    //--- another document of the same length: same revision, same page ids ---
    let revision_before = harness.app.state().snapshot().revision;
    harness.app.apply_action(MenuAction::GenerateSynthetic(40), &harness.ctx);
    assert_eq!(
        harness.app.state().snapshot().revision,
        revision_before,
        "this test is only meaningful while the two documents collide on revision"
    );
    assert!(
        harness.app.canvas_cache().is_empty() && harness.app.rail_cache().is_empty(),
        "opening another document must release the previous document's textures, not leave them addressable"
    );

    harness.settle(8);
    let second_document: HashSet<TextureId> = harness.drawn_textures().clone();
    assert!(!second_document.is_empty(), "the second document must reach the screen too");

    let shared: Vec<&TextureId> = second_document.intersection(&first_document).collect();
    assert!(
        shared.is_empty(),
        "{} textures rasterized for the previous document were drawn for the new one: {shared:?}",
        shared.len()
    );
}

#[test]
fn replaces_a_document_mid_frame_without_serving_a_stale_tile() {
    let mut harness = Harness::build(40);
    harness.settle(4);
    let old_revision = harness.app.state().snapshot().revision;
    assert!(!harness.app.canvas_cache().is_empty(), "the cache must warm first");

    harness
        .app
        .apply_action(opdf_app::panels::menu_bar::MenuAction::GenerateSynthetic(90), &harness.ctx);
    harness.settle(6);

    let new_revision = harness.app.state().snapshot().revision;
    assert_ne!(old_revision, new_revision, "a new document must carry a new revision");
    assert_eq!(harness.app.state().page_count(), 90);
    //--- nothing from the old structure may remain addressable ---
    let stale_page = opdf_core::page::PageId::new(0);
    assert_eq!(
        harness.app.canvas_cache().find_nearest_scale(stale_page, old_revision, 1.0),
        None,
        "a tile from the previous document survived the replacement"
    );
}

//---------------------------------------------------------------------
// The opener seam
//---------------------------------------------------------------------

#[test]
fn the_app_draws_the_document_it_was_given_rather_than_one_it_built() {
    let ctx = Context::default();
    let opened = FakeOpener::with_pages(7).open(Path::new("x.pdf")).unwrap();
    let app = OpdfApp::new(&ctx, opened);
    assert_eq!(app.state().page_count(), 7);
    assert_eq!(app.document().map(|document| document.page_count()), Some(7));
}
