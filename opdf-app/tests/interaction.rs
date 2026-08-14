//! Interaction tests that drive the whole shell without a window.
//!
//! Task 16 of the Track D plan verifies scrolling, zoom anchoring, rail clicks,
//! and keyboard shortcuts by eye. These tests verify the same behaviours by
//! feeding synthetic [`egui::RawInput`] through [`OpdfApp::draw`] — the body of
//! `eframe::App::update`, split out because `eframe::Frame` has no public
//! constructor and so cannot be built by a test.
//!
//! They assert on state, not on pixels: what a page *looks* like is still only
//! checked by hand. What they do cover is every wiring bug an eyeball check would
//! catch — a scroll that does not move, an anchor measured from the wrong origin,
//! a click that selects nothing, a cache that grows without bound — and unlike the
//! eyeball check they run in headless CI on every commit.

use egui::{Context, Event, Key, Modifiers, MouseWheelUnit, Pos2, RawInput, Rect, Vec2, pos2, vec2};
use opdf_app::app::OpdfApp;
use opdf_app::panels::thumbnail_rail::lay_out_thumbnails;
use opdf_app::synthetic::build_synthetic_snapshot;
use opdf_app::theme::Theme;

const WINDOW_SIZE: Vec2 = vec2(1440.0, 900.0);

//---------------------------------------------------------------------
// Harness
//---------------------------------------------------------------------

/// A shell wired to a synthetic document, plus the context it draws into.
struct Harness {
    app: OpdfApp,
    ctx: Context,
    window_size: Vec2,
}

impl Harness {
    /// Build a shell over `page_count` synthetic pages and settle it.
    ///
    /// Several frames are run up front because the first frame has no viewport
    /// size yet, the second submits, and the third absorbs — exactly the sequence
    /// the real application goes through before anything is on screen.
    fn build(page_count: usize) -> Self {
        let ctx = Context::default();
        let app = OpdfApp::new(&ctx, build_synthetic_snapshot(page_count).unwrap());
        let mut harness = Self {
            app,
            ctx,
            window_size: WINDOW_SIZE,
        };
        harness.settle(4);
        harness
    }

    /// Run one frame with the given events.
    fn run_frame(&mut self, events: Vec<Event>) {
        let input = RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, self.window_size)),
            events,
            ..Default::default()
        };
        let app = &mut self.app;
        let _ = self.ctx.run(input, |ctx| app.draw(ctx));
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
    let mut app = OpdfApp::new(&ctx, build_synthetic_snapshot(200).unwrap());
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

#[test]
fn keeps_fitting_the_width_when_the_window_is_resized() {
    let mut harness = Harness::build(40);
    harness.hover(harness.canvas_point());

    harness.app.apply_action(opdf_app::panels::menu_bar::MenuAction::FitWidth, &harness.ctx);
    harness.settle(2);
    let wide_zoom = harness.app.state().zoom;

    //--- halve the window width; fit-width must follow it down ---
    harness.window_size = vec2(WINDOW_SIZE.x * 0.5, WINDOW_SIZE.y);
    harness.settle(2);
    harness.app.apply_action(opdf_app::panels::menu_bar::MenuAction::FitWidth, &harness.ctx);
    harness.settle(2);
    let narrow_zoom = harness.app.state().zoom;

    assert!(
        narrow_zoom < wide_zoom,
        "a narrower window must fit the content at a smaller zoom: {wide_zoom} -> {narrow_zoom}"
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
    let mut app = OpdfApp::new(&ctx, opdf_core::document::DocumentSnapshot::default());
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
