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
use opdf_app::app::{ExitIntent, OpdfApp};
use opdf_app::opener::{DocumentOpener, FakeChooser, FakeOpener};
use opdf_app::panels::menu_bar::MenuAction;
use opdf_app::panels::thumbnail_rail::lay_out_thumbnails;
use opdf_app::synthetic::open_synthetic_document;
use opdf_app::theme::Theme;
use opdf_core::page::Rotation;

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

    let document = harness.app.state().snapshot().document;
    let revision = harness.app.state().snapshot().revision;
    let first_page = harness.app.state().snapshot().pages[0].id;
    assert!(
        harness.app.canvas_cache().find_nearest_scale(document, first_page, revision, 1.0).is_some(),
        "the first page must be cached before the flicker check means anything"
    );

    //--- ten zooms in and out: at no point may the page the user is looking at
    //--- lose every cached scale, because that is the frame it blinks to grey ---
    for step in 0..10 {
        let factor = if step % 2 == 0 { 1.4 } else { 1.0 / 1.4 };
        harness.zoom_at(point, factor);
        let scale = harness.app.state().render_scale(1.0);
        assert!(
            harness.app.canvas_cache().find_nearest_scale(document, first_page, revision, scale).is_some(),
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
    let old_document = harness.app.state().snapshot().document;
    let old_revision = harness.app.state().snapshot().revision;
    assert!(!harness.app.canvas_cache().is_empty(), "the cache must warm first");

    harness
        .app
        .apply_action(opdf_app::panels::menu_bar::MenuAction::GenerateSynthetic(90), &harness.ctx);
    harness.settle(6);

    let new_revision = harness.app.state().snapshot().revision;
    assert_ne!(old_revision, new_revision, "a new document must carry a new revision");
    assert_ne!(
        old_document,
        harness.app.state().snapshot().document,
        "a new document must carry a new identity, which is the part a revision cannot express"
    );
    assert_eq!(harness.app.state().page_count(), 90);
    //--- nothing from the old structure may remain addressable ---
    let stale_page = opdf_core::page::PageId::new(0);
    assert_eq!(
        harness.app.canvas_cache().find_nearest_scale(old_document, stale_page, old_revision, 1.0),
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

#[test]
fn opening_a_second_document_replaces_the_first() {
    let ctx = Context::default();
    let opened = FakeOpener::with_pages(3).open(Path::new("a.pdf")).unwrap();
    let mut app = OpdfApp::new(&ctx, opened);
    assert_eq!(app.state().page_count(), 3);

    app.open_path(&FakeOpener::with_pages(11), Path::new("b.pdf"));

    assert_eq!(app.state().page_count(), 11);
    assert_eq!(app.document().map(|document| document.page_count()), Some(11));
    //--- F14: the previous document's tiles must not survive into the new one ---
    assert!(app.canvas_cache().is_empty());
    assert!(app.last_error().is_none(), "a successful open must clear whatever failed before it");
}

#[test]
fn a_failed_open_leaves_the_current_document_untouched_and_reports_why() {
    let ctx = Context::default();
    let opened = FakeOpener::with_pages(3).open(Path::new("a.pdf")).unwrap();
    let mut app = OpdfApp::new(&ctx, opened);

    app.open_path(&FakeOpener::failing(), Path::new("broken.pdf"));

    assert_eq!(app.state().page_count(), 3, "a failed open must not close the open document");
    assert!(app.last_error().is_some(), "a failed open must be surfaced, not swallowed");
    let message = app.last_error().unwrap_or_default();
    assert!(message.contains("broken.pdf"), "the message must name the file that failed, got: {message}");
}

//---------------------------------------------------------------------
// Pages the rasterizer cannot resolve
//---------------------------------------------------------------------

/// Every page-border stroke colour this frame's shapes used.
///
/// The unrenderable placeholder is told apart from the ordinary one by the colour
/// of its border, which is the only part of it that survives into the shape list
/// as something a headless test can name.
fn collect_rect_stroke_colours(shapes: &[ClippedShape]) -> HashSet<[u8; 4]> {
    let mut colours = HashSet::new();
    for clipped in shapes {
        collect_strokes_from_shape(&clipped.shape, &mut colours);
    }
    colours
}

fn collect_strokes_from_shape(shape: &Shape, into: &mut HashSet<[u8; 4]>) {
    match shape {
        Shape::Rect(rect) => {
            into.insert(rect.stroke.color.to_array());
        }
        Shape::Vec(inner) => {
            for shape in inner {
                collect_strokes_from_shape(shape, into);
            }
        }
        _ => {}
    }
}

/// A page the rasterizer refuses is a permanent condition, not a slow one.
///
/// The F5 fix froze the page-to-file index map at open, so a page inserted since
/// then has no position in the file and fails by design. Clearing the pending slot
/// and nothing else makes the scheduler ask again on the very next frame: the page
/// stays a grey rectangle, the status bar reads "Rendering 1 page" for the life of
/// the session, and the event loop never sleeps. That is the F3 shape exactly.
#[test]
fn an_unrenderable_page_shows_that_it_failed_and_stops_being_requested() {
    let ctx = Context::default();
    let opened = FakeOpener::with_unrenderable_page(3, 0).open(Path::new("a.pdf")).unwrap();
    let mut app = OpdfApp::new(&ctx, opened);

    let mut last: Option<egui::FullOutput> = None;
    for _ in 0..12 {
        let input = RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, WINDOW_SIZE)),
            ..Default::default()
        };
        last = Some(ctx.run(input, |ctx| app.draw(ctx)));
    }
    let output = last.unwrap();

    assert_eq!(
        app.canvas_cache().pending_count(),
        0,
        "a request that was answered — with a failure — must not stay in flight"
    );

    let theme = Theme::dark();
    let colours = collect_rect_stroke_colours(&output.shapes);
    assert!(
        colours.contains(&theme.error_text.to_array()),
        "the page the rasterizer refused must be drawn as refused, not as still loading"
    );

    let repaint_delay = output
        .viewport_output
        .get(&egui::ViewportId::ROOT)
        .map(|viewport| viewport.repaint_delay)
        .unwrap_or_default();
    assert!(
        repaint_delay > std::time::Duration::ZERO,
        "the shell must settle once every page has an answer, but it asked to be repainted immediately"
    );
}

//---------------------------------------------------------------------
// The File menu
//---------------------------------------------------------------------

#[test]
fn the_file_menu_opens_the_document_the_chooser_returned() {
    let ctx = Context::default();
    let opened = FakeOpener::with_pages(3).open(Path::new("a.pdf")).unwrap();
    let mut app = OpdfApp::new(&ctx, opened).with_open_route(Box::new(FakeChooser::choosing("chosen.pdf")), Box::new(FakeOpener::with_pages(9)));

    app.apply_action(MenuAction::OpenDocument, &ctx);

    assert_eq!(app.state().page_count(), 9, "File ▸ Open must open the file the dialog returned");
    assert_eq!(app.document().map(|document| document.page_count()), Some(9));
    assert!(app.last_error().is_none());
}

#[test]
fn a_cancelled_file_dialog_leaves_the_document_alone_and_reports_nothing() {
    let ctx = Context::default();
    let opened = FakeOpener::with_pages(3).open(Path::new("a.pdf")).unwrap();
    let mut app = OpdfApp::new(&ctx, opened).with_open_route(Box::new(FakeChooser::cancelling()), Box::new(FakeOpener::with_pages(9)));

    app.apply_action(MenuAction::OpenDocument, &ctx);

    assert_eq!(app.state().page_count(), 3, "cancelling the dialog must not disturb the open document");
    assert!(
        app.last_error().is_none(),
        "changing your mind is not a failure and must not be reported as one"
    );
}

#[test]
fn a_file_menu_open_that_fails_keeps_the_document_and_says_why() {
    let ctx = Context::default();
    let opened = FakeOpener::with_pages(3).open(Path::new("a.pdf")).unwrap();
    let mut app = OpdfApp::new(&ctx, opened).with_open_route(Box::new(FakeChooser::choosing("broken.pdf")), Box::new(FakeOpener::failing()));

    app.apply_action(MenuAction::OpenDocument, &ctx);

    assert_eq!(app.state().page_count(), 3);
    let message = app.last_error().unwrap_or_default();
    assert!(message.contains("broken.pdf"), "the failure must name the file the user picked, got: {message}");
}

//---------------------------------------------------------------------
// Saving
//---------------------------------------------------------------------

/// A shell over a fake document opened from a real, writable path.
fn app_opened_from(path: &Path) -> (OpdfApp, Context) {
    let ctx = Context::default();
    let opened = FakeOpener::with_pages(3).open(path).unwrap();
    (OpdfApp::new(&ctx, opened), ctx)
}

#[test]
fn saving_with_no_document_open_is_inert_and_reports_nothing() {
    let directory = tempfile::tempdir().unwrap();
    let (mut app, ctx) = app_opened_from(&directory.path().join("orig.pdf"));
    app.apply_action(MenuAction::CloseDocument, &ctx);

    app.apply_action(MenuAction::Save, &ctx);

    assert!(
        !directory.path().join("orig.pdf").exists(),
        "saving with nothing open must not write the document that was closed"
    );
    assert!(
        app.last_error().is_none(),
        "saving with nothing open is a no-op, not a failure the user must be told about"
    );
}

#[test]
fn saving_writes_to_the_path_the_document_was_opened_from() {
    let directory = tempfile::tempdir().unwrap();
    let origin = directory.path().join("orig.pdf");
    let (mut app, ctx) = app_opened_from(&origin);

    app.apply_action(MenuAction::Save, &ctx);

    assert!(origin.exists(), "Save must write back to the file the document came from");
    assert!(app.last_error().is_none(), "a save that succeeded must not report an error");
}

#[test]
fn save_as_writes_elsewhere_and_later_saves_follow_it() {
    let directory = tempfile::tempdir().unwrap();
    let origin = directory.path().join("orig.pdf");
    let elsewhere = directory.path().join("elsewhere.pdf");
    let ctx = Context::default();
    let opened = FakeOpener::with_pages(3).open(&origin).unwrap();
    let mut app = OpdfApp::new(&ctx, opened).with_open_route(Box::new(FakeChooser::choosing(elsewhere.clone())), Box::new(FakeOpener::with_pages(9)));

    app.apply_action(MenuAction::SaveAs, &ctx);

    assert!(elsewhere.exists(), "Save As must write to the path the dialog returned");
    assert!(!origin.exists(), "Save As must not also write the file the document came from");

    //--- a plain Save now has to follow the document to its new home ---
    std::fs::remove_file(&elsewhere).unwrap();
    app.apply_action(MenuAction::Save, &ctx);

    assert!(elsewhere.exists(), "after Save As, a plain Save must write to the new path");
    assert!(!origin.exists(), "after Save As, a plain Save must not go back to the original path");
}

#[test]
fn saving_a_document_with_no_origin_asks_where_to_put_it() {
    let directory = tempfile::tempdir().unwrap();
    let chosen = directory.path().join("named.pdf");
    let ctx = Context::default();
    //--- a synthetic document was never opened from a file, so Save has no path to reuse ---
    let mut app = OpdfApp::new(&ctx, open_synthetic_document(4).unwrap())
        .with_open_route(Box::new(FakeChooser::choosing(chosen.clone())), Box::new(FakeOpener::with_pages(9)));

    app.apply_action(MenuAction::Save, &ctx);

    assert!(chosen.exists(), "Save on a document with no origin must fall back to asking, not fail silently");
    assert!(app.last_error().is_none());
}

#[test]
fn a_cancelled_save_dialog_writes_nothing_and_reports_nothing() {
    let directory = tempfile::tempdir().unwrap();
    let origin = directory.path().join("orig.pdf");
    let ctx = Context::default();
    let opened = FakeOpener::with_pages(3).open(&origin).unwrap();
    let mut app = OpdfApp::new(&ctx, opened).with_open_route(Box::new(FakeChooser::cancelling()), Box::new(FakeOpener::with_pages(9)));

    app.apply_action(MenuAction::SaveAs, &ctx);

    assert!(!origin.exists(), "cancelling Save As must not fall back to writing the original file");
    assert!(app.last_error().is_none(), "changing your mind is not a failure");
}

#[test]
fn a_failed_save_leaves_the_document_open_and_says_why() {
    let directory = tempfile::tempdir().unwrap();
    //--- a directory that does not exist: the write fails for an ordinary, real reason ---
    let unwritable = directory.path().join("no-such-directory").join("orig.pdf");
    let (mut app, ctx) = app_opened_from(&unwritable);

    app.apply_action(MenuAction::Save, &ctx);

    assert_eq!(app.state().page_count(), 3, "a failed save must not close or disturb the document");
    let message = app.last_error().unwrap_or_default();
    assert!(
        message.contains("orig.pdf"),
        "the failure must name the file it could not write, got: {message}"
    );
}

//---------------------------------------------------------------------
// Editing, undo and redo
//---------------------------------------------------------------------

/// The rotation the document actually reports for the page at `index`.
fn rotation_at(app: &OpdfApp, index: usize) -> Rotation {
    app.state().snapshot().pages[index].rotation
}

#[test]
fn rotating_a_page_is_undoable_and_redoable() {
    let ctx = Context::default();
    let opened = FakeOpener::with_pages(3).open(Path::new("a.pdf")).unwrap();
    let mut app = OpdfApp::new(&ctx, opened);
    let before = rotation_at(&app, 0);
    assert_eq!(app.undo_depth(), 0, "a freshly opened document has nothing to undo");

    app.apply_action(MenuAction::RotatePageClockwise, &ctx);

    let rotated = rotation_at(&app, 0);
    assert_eq!(rotated, before.rotated_by(Rotation::Quarter), "the page must actually turn");
    assert_eq!(app.undo_depth(), 1, "an edit must be recorded on the undo stack");

    app.apply_action(MenuAction::Undo, &ctx);

    assert_eq!(rotation_at(&app, 0), before, "undo must put the page back");
    assert_eq!(app.undo_depth(), 0);
    assert_eq!(app.redo_depth(), 1);

    app.apply_action(MenuAction::Redo, &ctx);

    assert_eq!(rotation_at(&app, 0), rotated, "redo must turn the page again");
    assert_eq!(app.undo_depth(), 1);
    assert_eq!(app.redo_depth(), 0);
}

#[test]
fn an_edit_reaches_the_document_and_not_only_the_snapshot() {
    let ctx = Context::default();
    let opened = FakeOpener::with_pages(3).open(Path::new("a.pdf")).unwrap();
    let mut app = OpdfApp::new(&ctx, opened);
    let revision = app.document().map(|document| document.revision());

    app.apply_action(MenuAction::RotatePageClockwise, &ctx);

    assert_ne!(
        app.document().map(|document| document.revision()),
        revision,
        "the edit must be applied to the document itself; a snapshot the document does not back is a lie"
    );
    assert_eq!(
        app.state().snapshot().revision,
        app.document().map(|document| document.revision()).unwrap_or_default(),
        "the snapshot the shell draws must be the one the document is at"
    );
}

#[test]
fn undo_and_redo_are_inert_with_nothing_to_undo() {
    let ctx = Context::default();
    let opened = FakeOpener::with_pages(3).open(Path::new("a.pdf")).unwrap();
    let mut app = OpdfApp::new(&ctx, opened);

    app.apply_action(MenuAction::Undo, &ctx);
    app.apply_action(MenuAction::Redo, &ctx);

    assert_eq!(app.undo_depth(), 0);
    assert_eq!(app.redo_depth(), 0);
    assert!(app.last_error().is_none(), "pressing undo with an empty history is not a failure");
}

#[test]
fn opening_another_document_starts_a_fresh_history() {
    let ctx = Context::default();
    let opened = FakeOpener::with_pages(3).open(Path::new("a.pdf")).unwrap();
    let mut app = OpdfApp::new(&ctx, opened);
    app.apply_action(MenuAction::RotatePageClockwise, &ctx);
    assert_eq!(app.undo_depth(), 1);

    app.open_path(&FakeOpener::with_pages(5), Path::new("b.pdf"));

    assert_eq!(
        app.undo_depth(),
        0,
        "an entry addresses pages of the document it was recorded against; applying it to another document would address the wrong pages"
    );
    assert_eq!(app.redo_depth(), 0);
}

//---------------------------------------------------------------------
// Compaction — F16
//---------------------------------------------------------------------
//
// A compacting save purges unreferenced objects, and a page deleted in this
// session is exactly that: it sits in the trash, referenced only by the undo
// entry that would restore it. After a compaction that entry cannot succeed —
// since Track A's fix the compacted bytes become the document's base, so a
// queued RestorePage is unambiguously dead. The user must therefore be asked
// first, and on confirming, the history must be discarded rather than left to
// fail when they press undo.

/// A shell over a fake document, opened from a real path, with one page deleted.
///
/// The deletion is what puts a page in the trash, which is what makes a
/// compaction destructive. Without it these tests would prove nothing.
fn app_with_a_deleted_page(path: &Path) -> (OpdfApp, Context) {
    let ctx = Context::default();
    let opened = FakeOpener::with_pages(3).open(path).unwrap();
    let mut app = OpdfApp::new(&ctx, opened);
    app.apply_action(MenuAction::DeletePage, &ctx);
    assert_eq!(app.state().page_count(), 2, "the deletion must land before the test means anything");
    assert_eq!(app.undo_depth(), 1, "the deletion must be undoable before the test means anything");
    (app, ctx)
}

#[test]
fn compacting_asks_before_it_destroys_undo_of_a_deletion() {
    let directory = tempfile::tempdir().unwrap();
    let origin = directory.path().join("orig.pdf");
    let (mut app, ctx) = app_with_a_deleted_page(&origin);

    app.apply_action(MenuAction::Compact, &ctx);

    assert!(app.compaction_pending(), "Compact must ask before it discards the user's history");
    assert!(!origin.exists(), "Compact must write nothing until the user has confirmed");
    assert_eq!(app.undo_depth(), 1, "merely asking must not cost the history");
}

/// The F16 regression test.
///
/// Verified to fail against a build of this same code with the `clear()` call
/// removed — a test that passes either way would prove nothing.
#[test]
fn confirming_a_compaction_clears_the_undo_stack() {
    let directory = tempfile::tempdir().unwrap();
    let origin = directory.path().join("orig.pdf");
    let (mut app, ctx) = app_with_a_deleted_page(&origin);
    app.apply_action(MenuAction::Compact, &ctx);

    app.confirm_compaction();

    assert!(origin.exists(), "confirming must actually write the compacted document");
    assert!(!app.compaction_pending(), "the dialog must close once it has been answered");
    assert_eq!(
        app.undo_depth(),
        0,
        "compaction purged the trashed page, so the queued RestorePage is dead; leaving it on the stack \
         hands the user an undo that fails instead of an undo that is honestly gone"
    );
    assert_eq!(app.redo_depth(), 0, "a redo entry can resolve to RestorePage just as an undo entry can");
}

#[test]
fn cancelling_a_compaction_writes_nothing_and_keeps_the_history() {
    let directory = tempfile::tempdir().unwrap();
    let origin = directory.path().join("orig.pdf");
    let (mut app, ctx) = app_with_a_deleted_page(&origin);
    app.apply_action(MenuAction::Compact, &ctx);

    app.cancel_compaction();

    assert!(!origin.exists(), "cancelling must write nothing at all");
    assert!(!app.compaction_pending());
    assert_eq!(app.undo_depth(), 1, "cancelling must leave the history exactly as it was");

    //--- and the history must still work, not merely still be counted ---
    app.apply_action(MenuAction::Undo, &ctx);
    assert_eq!(app.state().page_count(), 3, "the deleted page must come back");
    assert!(app.last_error().is_none());
}

/// A compaction that fails must leave the history intact, mirroring `opdf-pdf`,
/// where a failed compaction leaves the trash intact. Clearing the stack after a
/// write that never happened would discard history for nothing.
#[test]
fn a_failed_compaction_keeps_the_history() {
    let directory = tempfile::tempdir().unwrap();
    let unwritable = directory.path().join("no-such-directory").join("orig.pdf");
    let (mut app, ctx) = app_with_a_deleted_page(&unwritable);
    app.apply_action(MenuAction::Compact, &ctx);

    app.confirm_compaction();

    assert_eq!(app.undo_depth(), 1, "the compaction failed, so nothing was purged and nothing may be discarded");
    let message = app.last_error().unwrap_or_default();
    assert!(message.contains("orig.pdf"), "the failure must name the file, got: {message}");
}

#[test]
fn a_compaction_the_user_never_confirmed_cannot_be_confirmed_into_a_write() {
    let directory = tempfile::tempdir().unwrap();
    let origin = directory.path().join("orig.pdf");
    let (mut app, _ctx) = app_with_a_deleted_page(&origin);

    //--- no Compact action was raised, so there is nothing to confirm ---
    app.confirm_compaction();

    assert!(!origin.exists(), "confirming a dialog that was never raised must not write the document");
    assert_eq!(app.undo_depth(), 1, "nor may it cost the history");
}

//---------------------------------------------------------------------
// Unsaved changes
//---------------------------------------------------------------------

#[test]
fn a_freshly_opened_document_has_nothing_unsaved() {
    let ctx = Context::default();
    let opened = FakeOpener::with_pages(3).open(Path::new("a.pdf")).unwrap();
    let app = OpdfApp::new(&ctx, opened);
    assert!(!app.has_unsaved_changes(), "opening a file does not modify it");
}

#[test]
fn closing_with_unsaved_edits_asks_before_discarding_them() {
    let ctx = Context::default();
    let opened = FakeOpener::with_pages(3).open(Path::new("a.pdf")).unwrap();
    let mut app = OpdfApp::new(&ctx, opened);
    app.apply_action(MenuAction::RotatePageClockwise, &ctx);
    assert!(app.has_unsaved_changes());

    app.apply_action(MenuAction::CloseDocument, &ctx);

    assert_eq!(app.discard_prompt(), Some(ExitIntent::Close), "closing must ask rather than discard silently");
    assert_eq!(app.state().page_count(), 3, "the document must still be open while the question stands");
}

#[test]
fn closing_a_clean_document_does_not_ask() {
    let ctx = Context::default();
    let opened = FakeOpener::with_pages(3).open(Path::new("a.pdf")).unwrap();
    let mut app = OpdfApp::new(&ctx, opened);

    app.apply_action(MenuAction::CloseDocument, &ctx);

    assert_eq!(app.discard_prompt(), None, "there is nothing to lose, so there is nothing to ask about");
    assert_eq!(app.state().page_count(), 0, "a clean document must close immediately");
}

#[test]
fn quitting_and_opening_with_unsaved_edits_both_ask_first() {
    for (action, expected) in [(MenuAction::Quit, ExitIntent::Quit), (MenuAction::OpenDocument, ExitIntent::Open)] {
        let ctx = Context::default();
        let opened = FakeOpener::with_pages(3).open(Path::new("a.pdf")).unwrap();
        let mut app = OpdfApp::new(&ctx, opened).with_open_route(Box::new(FakeChooser::choosing("other.pdf")), Box::new(FakeOpener::with_pages(9)));
        app.apply_action(MenuAction::RotatePageClockwise, &ctx);

        app.apply_action(action, &ctx);

        assert_eq!(app.discard_prompt(), Some(expected), "{action:?} must ask before it abandons unsaved edits");
        assert_eq!(app.state().page_count(), 3, "{action:?} must not have gone through yet");
    }
}

#[test]
fn confirming_the_discard_goes_through_with_it() {
    let ctx = Context::default();
    let opened = FakeOpener::with_pages(3).open(Path::new("a.pdf")).unwrap();
    let mut app = OpdfApp::new(&ctx, opened);
    app.apply_action(MenuAction::RotatePageClockwise, &ctx);
    app.apply_action(MenuAction::CloseDocument, &ctx);

    app.confirm_discard(&ctx);

    assert_eq!(app.state().page_count(), 0, "confirming must actually close the document");
    assert_eq!(app.discard_prompt(), None);
}

#[test]
fn cancelling_the_discard_keeps_the_document_and_its_edits() {
    let ctx = Context::default();
    let opened = FakeOpener::with_pages(3).open(Path::new("a.pdf")).unwrap();
    let mut app = OpdfApp::new(&ctx, opened);
    app.apply_action(MenuAction::RotatePageClockwise, &ctx);
    app.apply_action(MenuAction::CloseDocument, &ctx);

    app.cancel_discard();

    assert_eq!(app.state().page_count(), 3, "cancelling must leave the document exactly where it was");
    assert_eq!(app.discard_prompt(), None);
    assert!(app.has_unsaved_changes(), "cancelling does not save; the edits are still unsaved");
    assert_eq!(app.undo_depth(), 1);
}

#[test]
fn saving_makes_the_document_clean_again() {
    let directory = tempfile::tempdir().unwrap();
    let origin = directory.path().join("orig.pdf");
    let (mut app, ctx) = app_opened_from(&origin);
    app.apply_action(MenuAction::RotatePageClockwise, &ctx);
    assert!(app.has_unsaved_changes());

    app.apply_action(MenuAction::Save, &ctx);

    assert!(!app.has_unsaved_changes(), "a saved document has nothing outstanding");

    app.apply_action(MenuAction::CloseDocument, &ctx);
    assert_eq!(app.discard_prompt(), None, "a saved document must close without a question");
    assert_eq!(app.state().page_count(), 0);
}

#[test]
fn editing_after_a_save_makes_the_document_dirty_again() {
    let directory = tempfile::tempdir().unwrap();
    let origin = directory.path().join("orig.pdf");
    let (mut app, ctx) = app_opened_from(&origin);
    app.apply_action(MenuAction::RotatePageClockwise, &ctx);
    app.apply_action(MenuAction::Save, &ctx);

    app.apply_action(MenuAction::RotatePageClockwise, &ctx);

    assert!(app.has_unsaved_changes(), "an edit after a save is unsaved work like any other");
}

/// Undoing back to the state that was saved is genuinely clean again — the
/// comparison is against a revision, and undo advances the revision rather than
/// rewinding it, so this is the case a naive "dirty flag" gets wrong in the
/// opposite direction. Being asked about work that no longer differs is a
/// nuisance, not a data-loss risk, so erring here is acceptable; what matters is
/// that the shell never *fails* to ask.
#[test]
fn undoing_back_to_the_saved_state_still_errs_toward_asking() {
    let directory = tempfile::tempdir().unwrap();
    let origin = directory.path().join("orig.pdf");
    let (mut app, ctx) = app_opened_from(&origin);
    app.apply_action(MenuAction::Save, &ctx);
    app.apply_action(MenuAction::RotatePageClockwise, &ctx);
    app.apply_action(MenuAction::Undo, &ctx);

    assert!(
        app.has_unsaved_changes(),
        "the revision moved on, so the shell asks; asking needlessly is safe, failing to ask is not"
    );
}
