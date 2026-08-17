//! What the viewer knows between frames, and the one function that talks to the
//! render service.
//!
//! The shell holds a [`DocumentSnapshot`] and a
//! [`opdf_core::render::RenderService`] handle — never a
//! [`opdf_core::document::Document`], which the render worker owns because the
//! rasterizer is not thread-safe.

use std::ops::Range;
use std::sync::atomic::{AtomicU64, Ordering};

use opdf_core::document::DocumentSnapshot;
use opdf_core::page::Rotation;
use opdf_core::render::RenderService;

use crate::layout::{DocumentLayout, compute_document_layout, find_current_page, find_scroll_target, find_visible_pages};
use crate::scheduler::{MAX_SUBMISSIONS_PER_FRAME, RenderSettings, plan_render_requests};
use crate::theme::Theme;
use crate::tiles::{AbsorbReport, TextureCache, absorb_responses_routed};
use crate::zoom::{anchor_scroll_offset, clamp_zoom, fit_page_zoom, fit_width_zoom, quantize_render_scale};

/// How far beyond the viewport, in viewport heights, tiles are requested ahead of
/// time. Half a screen in each direction keeps ordinary scrolling ahead of the
/// renderer without doubling the work for a stationary view.
pub const OVERSCAN_SCREENS: f32 = 0.5;

/// How much the viewport must change before a fit mode is reapplied, in screen
/// points.
///
/// A scroll bar appearing or disappearing moves the viewport by a few points, and
/// refitting on that change would move it back — so the tolerance is wider than
/// any such wobble and far narrower than a real resize.
const FIT_VIEWPORT_TOLERANCE_PX: f32 = 4.0;

/// Which document the viewer is showing, distinct for every document opened in
/// this process.
///
/// `opdf_core` has no such notion and does not need one: a
/// [`DocumentSnapshot::revision`] counts edits *within* one document, and page ids
/// are allocated per document, so every document — synthetic or parsed — starts
/// both counts at zero. Two documents therefore produce colliding
/// [`opdf_core::render::RenderRequest`]s, and a tile cache keyed by request alone
/// cannot tell one document's pixels from another's. Only the shell knows that a
/// *different* document was opened, so identity is minted here, and
/// [`ViewerState::open_document`] is what turns it into a cleared cache.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct DocumentId(u64);

impl DocumentId {
    /// Mint an identity no other document in this process will be given.
    pub fn allocate() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        Self(NEXT.fetch_add(1, Ordering::Relaxed))
    }
}

/// How the zoom responds to a resize.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum FitMode {
    /// The user set the zoom; a resize leaves it alone.
    #[default]
    Free,
    /// Keep the content box filling the viewport width.
    Width,
    /// Keep the current page fully visible.
    Page,
}

/// Everything the viewer carries between frames.
#[derive(Debug)]
pub struct ViewerState {
    document: DocumentId,
    snapshot: DocumentSnapshot,
    layout: DocumentLayout,
    current_page: Option<usize>,
    /// Viewport size the current zoom was last fitted to, in screen points.
    ///
    /// A fit mode is a standing promise, so it has to be reapplied whenever the
    /// viewport changes and not only when the user asks for it. Comparing against
    /// the size last fitted to — rather than against last frame's size — is what
    /// makes the check idempotent: once the fit is applied, it does not fire again
    /// until the viewport genuinely moves.
    fitted_viewport_px: (f32, f32),
    /// Current zoom factor, where 1.0 is 72 dpi — one screen point per PDF point.
    pub zoom: f32,
    /// Scroll offset from the top of the content box, in screen points.
    pub scroll_offset_px: f32,
    /// Size of the canvas viewport in screen points, as `(width, height)`.
    pub viewport_size_px: (f32, f32),
    /// Top-left corner of the canvas viewport in screen points, as `(x, y)`.
    ///
    /// Written by the canvas each frame. A pointer-anchored zoom measures its
    /// anchor from here, not from the window origin: the menu bar, the toolbar,
    /// and the rail sit between the two, so anchoring at the window origin slides
    /// the page under the pointer by the height of that chrome on every zoom step.
    pub viewport_origin_px: (f32, f32),
    /// Whether the zoom tracks the viewport size.
    pub fit_mode: FitMode,
    /// View rotation the user applied, composed by the renderer with each page's
    /// stored rotation. Not a document edit.
    pub view_rotation: Rotation,
    /// Whether the thumbnail rail is shown.
    pub rail_visible: bool,
    /// A scroll offset the canvas should adopt on the next frame, set by
    /// navigation and by anchored zoom. Consumed by the canvas widget.
    pub scroll_request_px: Option<f32>,
}

//---------------------------------------------------------------------
// Construction and derived state
//---------------------------------------------------------------------

impl ViewerState {
    /// A viewer showing `snapshot` at 100%, scrolled to the top, with the rail open.
    pub fn new(snapshot: DocumentSnapshot, theme: &Theme) -> Self {
        let layout = compute_document_layout(&snapshot, theme.page_gap_pt, theme.canvas_margin_pt);
        let current_page = if layout.is_empty() { None } else { Some(0) };
        Self {
            document: DocumentId::allocate(),
            snapshot,
            layout,
            current_page,
            fitted_viewport_px: (0.0, 0.0),
            zoom: 1.0,
            scroll_offset_px: 0.0,
            viewport_size_px: (0.0, 0.0),
            viewport_origin_px: (0.0, 0.0),
            fit_mode: FitMode::Free,
            view_rotation: Rotation::None,
            rail_visible: true,
            scroll_request_px: None,
        }
    }

    /// Which document is being shown. Changes only through
    /// [`ViewerState::open_document`], never through an edit.
    pub fn document_id(&self) -> DocumentId {
        self.document
    }

    /// The snapshot being drawn. Every render request is built from this.
    pub fn snapshot(&self) -> &DocumentSnapshot {
        &self.snapshot
    }

    /// The layout of the current snapshot, in PDF points.
    pub fn layout(&self) -> &DocumentLayout {
        &self.layout
    }

    /// The page the user is on, or `None` for an empty document.
    pub fn current_page(&self) -> Option<usize> {
        self.current_page
    }

    /// Number of pages in the current snapshot.
    pub fn page_count(&self) -> usize {
        self.snapshot.page_count()
    }

    /// Show a **different document**: mint a new identity, recompute the layout,
    /// and empty every cache outright.
    ///
    /// This is the path `Open`, `Close`, and generating a synthetic document take.
    /// Discarding by revision would not be enough and is the bug this method
    /// exists to prevent: the new document's revision and page ids both start
    /// where the previous one's did, so its requests are keyed exactly as the
    /// previous document's cached tiles — open A, then B, and B's pages would be
    /// drawn from A's pixels.
    ///
    /// Use [`ViewerState::replace_snapshot`] instead for an edit to the document
    /// already open, which must keep the tiles it can still serve.
    pub fn open_document(&mut self, snapshot: DocumentSnapshot, theme: &Theme, caches: &mut [&mut TextureCache]) {
        self.document = DocumentId::allocate();
        self.layout = compute_document_layout(&snapshot, theme.page_gap_pt, theme.canvas_margin_pt);
        for cache in caches.iter_mut() {
            cache.clear();
        }
        self.snapshot = snapshot;
        self.refresh_current_page();
        //--- a fit mode is fitted to a content box, and this is a different one ---
        self.reapply_fit_mode();
    }

    /// Adopt a new snapshot **of the document already open** — an edit — recompute
    /// the layout, and drop every cached texture belonging to the superseded
    /// revision.
    ///
    /// Passing the caches in is not optional bookkeeping: entries from the old
    /// revision can never be served again, because the revision participates in
    /// `RenderRequest`'s equality, so leaving them in place is a pure leak. Tiles
    /// at the new revision are kept, which is what stops an undo from blanking the
    /// canvas.
    ///
    /// Never call this for a document the user opened; see
    /// [`ViewerState::open_document`].
    pub fn replace_snapshot(&mut self, snapshot: DocumentSnapshot, theme: &Theme, caches: &mut [&mut TextureCache]) {
        self.layout = compute_document_layout(&snapshot, theme.page_gap_pt, theme.canvas_margin_pt);
        for cache in caches.iter_mut() {
            cache.retain_revision(snapshot.revision);
        }
        self.snapshot = snapshot;
        self.refresh_current_page();
        //--- an edit can change the widest page, and with it the fit ---
        self.reapply_fit_mode();
    }

    /// The content box's size in screen points at the current zoom.
    pub fn content_size_px(&self) -> (f32, f32) {
        (self.layout.content_width_pt * self.zoom, self.layout.content_height_pt * self.zoom)
    }

    /// The page range the canvas should draw, widened by the overscan band.
    pub fn visible_pages(&self) -> Range<usize> {
        let zoom = clamp_zoom(self.zoom);
        let top_pt = self.scroll_offset_px / zoom;
        let height_pt = self.viewport_size_px.1 / zoom;
        find_visible_pages(&self.layout, top_pt, height_pt, height_pt * OVERSCAN_SCREENS)
    }

    /// The quantised scale tiles should be rasterized at, accounting for the
    /// display's pixel density so a HiDPI screen gets a sharp tile.
    pub fn render_scale(&self, pixels_per_point: f32) -> f32 {
        quantize_render_scale(clamp_zoom(self.zoom) * pixels_per_point.max(1.0))
    }

    /// This frame's rasterization settings: quantised scale, view rotation, and
    /// the backend's texture-size limit.
    ///
    /// The canvas and the scheduler both build their cache keys from this, so a
    /// page capped for its size is looked up at the same capped scale it was
    /// requested at.
    pub fn render_settings(&self, pixels_per_point: f32, max_texture_side: usize) -> RenderSettings {
        RenderSettings {
            render_scale: self.render_scale(pixels_per_point),
            view_rotation: self.view_rotation,
            max_texture_side,
        }
    }

    /// Recompute which page the user is on from the current scroll offset.
    pub fn refresh_current_page(&mut self) {
        let zoom = clamp_zoom(self.zoom);
        self.current_page = find_current_page(&self.layout, self.scroll_offset_px / zoom, self.viewport_size_px.1 / zoom);
    }
}

//---------------------------------------------------------------------
// Navigation and zoom
//---------------------------------------------------------------------

impl ViewerState {
    /// Change the zoom while keeping the document point `anchor_px` below the top
    /// of the viewport where it is.
    ///
    /// Pass the pointer's distance from the viewport top for a wheel zoom, or half
    /// the viewport height for a keyboard or toolbar zoom.
    pub fn set_zoom_anchored(&mut self, new_zoom: f32, anchor_px: f32) {
        let old_zoom = clamp_zoom(self.zoom);
        let new_zoom = clamp_zoom(new_zoom);
        let offset_px = anchor_scroll_offset(self.scroll_offset_px, anchor_px, old_zoom, new_zoom);
        self.zoom = new_zoom;
        self.scroll_offset_px = offset_px;
        self.scroll_request_px = Some(offset_px);
        self.refresh_current_page();
    }

    /// Scroll so page `index` sits at the top of the viewport.
    pub fn scroll_to_page(&mut self, index: usize, margin_pt: f32) {
        if let Some(target_pt) = find_scroll_target(&self.layout, index, margin_pt) {
            let offset_px = target_pt * clamp_zoom(self.zoom);
            self.scroll_offset_px = offset_px;
            self.scroll_request_px = Some(offset_px);
            self.current_page = Some(index);
        }
    }

    /// Reapply the current fit mode to the viewport as it is now.
    ///
    /// A no-op in [`FitMode::Free`], so a user-chosen zoom survives a window resize.
    pub fn reapply_fit_mode(&mut self) {
        let (width_px, height_px) = self.viewport_size_px;
        self.fitted_viewport_px = self.viewport_size_px;
        let new_zoom = match self.fit_mode {
            FitMode::Free => return,
            FitMode::Width => fit_width_zoom(self.layout.content_width_pt, width_px),
            FitMode::Page => match self.current_page.and_then(|index| self.layout.placement(index)) {
                Some(placement) => fit_page_zoom(placement.width_pt, placement.height_pt, width_px, height_px),
                None => return,
            },
        };
        self.set_zoom_anchored(new_zoom, height_px * 0.5);
    }

    /// Reapply the fit mode if the viewport has changed size since it was last
    /// fitted, returning whether the zoom was refitted.
    ///
    /// Call this once per frame, after the canvas has written back the viewport it
    /// actually got. Without it a fit mode is a one-shot zoom rather than a mode:
    /// `Fit width` on a wide window, then halve the window, and the content stays
    /// at its old width in a viewport half the size.
    ///
    /// [`FitMode::Width`] watches only the width, so shortening the window does not
    /// disturb a zoom that does not depend on the height.
    pub fn sync_fit_to_viewport(&mut self) -> bool {
        let (width_px, height_px) = self.viewport_size_px;
        let (fitted_width_px, fitted_height_px) = self.fitted_viewport_px;
        let width_moved = (width_px - fitted_width_px).abs() > FIT_VIEWPORT_TOLERANCE_PX;
        let height_moved = (height_px - fitted_height_px).abs() > FIT_VIEWPORT_TOLERANCE_PX;
        let stale = match self.fit_mode {
            FitMode::Free => false,
            FitMode::Width => width_moved,
            FitMode::Page => width_moved || height_moved,
        };
        if !stale {
            return false;
        }
        self.reapply_fit_mode();
        true
    }
}

//---------------------------------------------------------------------
// The render loop's service half
//---------------------------------------------------------------------

/// What one frame's exchange with the render service did.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct FrameOutcome {
    /// What came back from the service and what happened to it.
    pub absorbed: AbsorbReport,
    /// Requests submitted this frame.
    pub submitted: usize,
    /// Wanted requests that did not fit in this frame's budget.
    pub skipped: usize,
}

/// Poll the service once, absorb what arrived, then submit what is still missing.
///
/// Call this exactly once per frame, before drawing. It never blocks: `poll`
/// returns whatever is ready and nothing more, and this function never waits for a
/// particular response. When anything is submitted or still in flight, it schedules
/// a repaint, because an idle event loop would otherwise never run the frame that
/// collects the answer.
///
/// `caches[0]` is the canvas cache: page requests are planned into it, and it is
/// the one evicted here. The remaining entries are the other surfaces' caches —
/// the thumbnail rail's — which submit their own requests to the same service and
/// so must be handed back their own answers. There is exactly one service because
/// the rasterizer is not thread-safe, so a single `poll` drains every surface's
/// responses at once and they have to be routed; see
/// [`crate::tiles::absorb_responses_routed`].
///
/// Passing an empty slice is a no-op returning a default [`FrameOutcome`].
pub fn step_render_service(
    state: &ViewerState,
    service: &dyn RenderService,
    caches: &mut [&mut TextureCache],
    ctx: &egui::Context,
    pixels_per_point: f32,
) -> FrameOutcome {
    let Some(canvas) = caches.first_mut() else {
        return FrameOutcome::default();
    };
    let frame_clock = canvas.begin_frame();
    let snapshot = state.snapshot();

    //--- one poll, never waited on; each answer goes to the cache that asked ---
    let absorbed = absorb_responses_routed(caches, ctx, snapshot.revision, service.poll(), frame_clock);

    let Some(canvas) = caches.first_mut() else {
        return FrameOutcome::default();
    };
    let max_texture_side = ctx.input(|input| input.max_texture_side);
    let plan = plan_render_requests(
        snapshot,
        state.visible_pages(),
        state.current_page().unwrap_or(0),
        state.render_settings(pixels_per_point, max_texture_side),
        &|request| canvas.wants(request),
        MAX_SUBMISSIONS_PER_FRAME,
    );
    //--- record as in flight only what is actually submitted: a request marked
    //--- while planning but skipped for budget could never be cleared ---
    for request in &plan.requests {
        canvas.mark_pending(*request);
        service.submit(*request);
    }

    //--- an answered request nobody collects is an invisible page; keep the loop turning ---
    let still_in_flight = caches.iter().any(|cache| cache.pending_count() > 0);
    if !plan.requests.is_empty() || plan.skipped > 0 || still_in_flight {
        ctx.request_repaint();
    }

    FrameOutcome {
        absorbed,
        submitted: plan.requests.len(),
        skipped: plan.skipped,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::synthetic::build_synthetic_snapshot;
    use opdf_core::fakes::FakeRenderService;
    use opdf_core::render::RenderRequest;

    /// A texture limit no synthetic page reaches, so these tests exercise the
    /// scheduler rather than the per-page size cap.
    const TEST_MAX_TEXTURE_SIDE: usize = 16_384;

    fn build_viewer(page_count: usize) -> (ViewerState, Theme) {
        let theme = Theme::dark();
        let snapshot = build_synthetic_snapshot(page_count).unwrap();
        let mut state = ViewerState::new(snapshot, &theme);
        state.viewport_size_px = (1000.0, 800.0);
        state.refresh_current_page();
        (state, theme)
    }

    #[test]
    fn starts_at_the_top_of_the_first_page() {
        let (state, _theme) = build_viewer(10);
        assert_eq!(state.current_page(), Some(0));
        assert_eq!(state.scroll_offset_px, 0.0);
        assert_eq!(state.zoom, 1.0);
    }

    #[test]
    fn reports_no_current_page_for_an_empty_document() {
        let theme = Theme::dark();
        let state = ViewerState::new(DocumentSnapshot::default(), &theme);
        assert_eq!(state.current_page(), None);
        assert_eq!(state.content_size_px(), (0.0, 0.0));
        assert!(state.visible_pages().is_empty());
    }

    #[test]
    fn scales_the_content_box_with_the_zoom() {
        let (mut state, _theme) = build_viewer(10);
        let (width_px, height_px) = state.content_size_px();
        state.zoom = 2.0;
        let (doubled_width_px, doubled_height_px) = state.content_size_px();
        assert!((doubled_width_px - width_px * 2.0).abs() < 1e-2);
        assert!(
            (doubled_height_px - height_px * 2.0).abs() < 1e-2,
            "the scrollbar extent must track the zoom without any page being rendered"
        );
    }

    #[test]
    fn keeps_the_viewport_anchored_across_a_zoom_change() {
        let (mut state, _theme) = build_viewer(60);
        state.scroll_to_page(30, 0.0);
        let before_px = state.scroll_offset_px;
        state.set_zoom_anchored(2.0, 400.0);
        assert!((state.scroll_offset_px - ((before_px + 400.0) * 2.0 - 400.0)).abs() < 1e-2);
        assert_eq!(
            state.scroll_request_px,
            Some(state.scroll_offset_px),
            "the canvas must be told to adopt the anchored offset"
        );
    }

    #[test]
    fn tracks_the_current_page_while_scrolling() {
        let (mut state, _theme) = build_viewer(60);
        state.scroll_to_page(25, 0.0);
        state.refresh_current_page();
        assert_eq!(state.current_page(), Some(25));
    }

    #[test]
    fn ignores_a_jump_past_the_end_of_the_document() {
        let (mut state, _theme) = build_viewer(10);
        state.scroll_to_page(99, 0.0);
        assert_eq!(state.current_page(), Some(0), "an out-of-range jump must leave the viewer where it was");
    }

    #[test]
    fn quantises_the_render_scale_for_the_display_density() {
        let (mut state, _theme) = build_viewer(4);
        state.zoom = 1.0;
        assert_eq!(state.render_scale(1.0), 1.0);
        assert_eq!(state.render_scale(2.0), 2.0, "a HiDPI display must ask for a denser tile");
        state.zoom = 1.05;
        assert_eq!(state.render_scale(1.0), 1.5, "an off-ladder zoom must still land on a ladder step");
    }

    #[test]
    fn drops_superseded_textures_when_the_snapshot_is_replaced() {
        let (mut state, theme) = build_viewer(10);
        let mut cache = TextureCache::new(1 << 20);
        let stale = RenderRequest::new(state.snapshot().pages[0].id, state.snapshot().revision, 1.0).unwrap();
        cache.mark_pending(stale);

        let mut next = build_synthetic_snapshot(10).unwrap();
        next.revision = state.snapshot().revision + 1;
        state.replace_snapshot(next, &theme, &mut [&mut cache]);

        assert_eq!(cache.pending_count(), 0, "requests in flight for the old revision must be forgotten");
        assert_eq!(state.snapshot().revision, stale.revision + 1, "the viewer must now be drawing the new snapshot");
    }

    /// The case `retain_revision` cannot see: a *different* document whose
    /// revision and page ids collide with the one already open, which is every
    /// pair of freshly opened documents.
    #[test]
    fn drops_every_texture_when_a_different_document_is_opened() {
        let ctx = egui::Context::default();
        let (mut state, theme) = build_viewer(10);
        let service = FakeRenderService::new(state.snapshot().clone());
        let mut cache = TextureCache::new(1 << 26);
        step_render_service(&state, &service, &mut [&mut cache], &ctx, 1.0);
        step_render_service(&state, &service, &mut [&mut cache], &ctx, 1.0);
        assert!(!cache.is_empty(), "the cache must warm before this test means anything");

        let other = build_synthetic_snapshot(10).unwrap();
        assert_eq!(other.revision, state.snapshot().revision, "the colliding case is the one that matters here");
        assert_eq!(
            other.pages[0].id,
            state.snapshot().pages[0].id,
            "page ids are allocated per document and collide too"
        );

        state.open_document(other, &theme, &mut [&mut cache]);

        assert!(
            cache.is_empty(),
            "the previous document's tiles are keyed exactly as the new document looks them up, so keeping them draws A's pixels for B"
        );
        assert_eq!(cache.pending_count(), 0, "a request in flight for the previous document must be forgotten");
    }

    #[test]
    fn mints_a_new_identity_per_document_but_not_per_edit() {
        let (mut state, theme) = build_viewer(10);
        let opened = state.document_id();

        state.open_document(build_synthetic_snapshot(10).unwrap(), &theme, &mut []);
        let reopened = state.document_id();
        assert_ne!(opened, reopened, "a document that merely looks the same is still a different document");

        let mut edited = state.snapshot().clone();
        edited.revision += 1;
        state.replace_snapshot(edited, &theme, &mut []);
        assert_eq!(reopened, state.document_id(), "an edit must not change which document is open");
    }

    #[test]
    fn refits_the_width_when_the_viewport_changes_and_not_otherwise() {
        let (mut state, _theme) = build_viewer(10);
        state.fit_mode = FitMode::Width;
        state.reapply_fit_mode();
        let fitted_zoom = state.zoom;
        assert!(!state.sync_fit_to_viewport(), "an unchanged viewport must not refit");

        state.viewport_size_px = (500.0, 800.0);
        assert!(state.sync_fit_to_viewport(), "a resized viewport must refit, with no action from the user");
        assert!(state.zoom < fitted_zoom, "a narrower viewport must fit at a smaller zoom");
        assert!(
            (state.layout().content_width_pt * state.zoom - 500.0).abs() < 1e-2,
            "the content must fill the new viewport width, but is {} pt wide",
            state.layout().content_width_pt * state.zoom
        );
        assert!(!state.sync_fit_to_viewport(), "the refit must settle, not fire again on every following frame");
    }

    #[test]
    fn refits_a_fitted_page_only_when_an_axis_it_depends_on_moves() {
        let (mut state, _theme) = build_viewer(10);
        state.fit_mode = FitMode::Width;
        state.reapply_fit_mode();
        state.viewport_size_px = (1000.0, 400.0);
        assert!(
            !state.sync_fit_to_viewport(),
            "fit-width does not depend on the height, so shortening the window must leave the zoom alone"
        );

        state.fit_mode = FitMode::Page;
        state.reapply_fit_mode();
        //--- 300 pt still fits an A4 page above MIN_ZOOM, so the clamp is not what is being measured ---
        state.viewport_size_px = (1000.0, 300.0);
        assert!(state.sync_fit_to_viewport(), "fit-page depends on the height and must follow it");
        let placement = state.layout().placement(state.current_page().unwrap_or(0)).unwrap();
        assert!(
            placement.height_pt * state.zoom <= 300.0 + 1e-2,
            "the fitted page is {} pt tall in a 300 pt viewport",
            placement.height_pt * state.zoom
        );
    }

    #[test]
    fn refits_a_newly_opened_document_to_the_viewport_it_is_shown_in() {
        let (mut state, theme) = build_viewer(10);
        state.fit_mode = FitMode::Width;
        state.reapply_fit_mode();
        let wide_document_zoom = state.zoom;

        //--- one A4 page is a much narrower content box than ten varied ones ---
        state.open_document(build_synthetic_snapshot(1).unwrap(), &theme, &mut []);

        assert!(
            state.zoom > wide_document_zoom,
            "a narrower document under fit-width must fill the same viewport at a larger zoom: {wide_document_zoom} -> {}",
            state.zoom
        );
        assert!(
            (state.layout().content_width_pt * state.zoom - state.viewport_size_px.0).abs() < 1e-2,
            "the new document is {} pt wide in a {} pt viewport",
            state.layout().content_width_pt * state.zoom,
            state.viewport_size_px.0
        );
    }

    #[test]
    fn leaves_a_freely_chosen_zoom_alone_across_a_resize() {
        let (mut state, _theme) = build_viewer(10);
        state.set_zoom_anchored(2.0, 400.0);
        state.viewport_size_px = (400.0, 300.0);
        assert!(!state.sync_fit_to_viewport());
        assert_eq!(state.zoom, 2.0, "a user-chosen zoom must survive a window resize untouched");
    }

    #[test]
    fn submits_on_the_first_frame_and_absorbs_on_the_second() {
        let ctx = egui::Context::default();
        let (state, _theme) = build_viewer(40);
        let service = FakeRenderService::new(state.snapshot().clone());
        let mut cache = TextureCache::new(1 << 26);

        let first = step_render_service(&state, &service, &mut [&mut cache], &ctx, 1.0);
        assert_eq!(first.absorbed.stored, 0, "nothing can have arrived before anything was asked for");
        assert!(first.submitted > 0, "the first frame must ask for the visible pages");
        assert!(cache.is_empty(), "the first frame draws placeholders; that is the case the canvas must handle");

        let second = step_render_service(&state, &service, &mut [&mut cache], &ctx, 1.0);
        assert_eq!(second.absorbed.stored, first.submitted, "every request submitted must be answered exactly once");
        assert_eq!(second.submitted, 0, "a page already cached must not be requested again");
    }

    #[test]
    fn never_resubmits_a_request_that_is_still_in_flight() {
        let ctx = egui::Context::default();
        let (state, _theme) = build_viewer(40);
        let service = FakeRenderService::new(state.snapshot().clone());
        let mut cache = TextureCache::new(1 << 26);

        let first = step_render_service(&state, &service, &mut [&mut cache], &ctx, 1.0);
        //--- a second frame before the service has answered: the fake answers in poll, so drive the plan directly ---
        let mut wanted = 0;
        crate::scheduler::plan_render_requests(
            state.snapshot(),
            state.visible_pages(),
            0,
            state.render_settings(1.0, TEST_MAX_TEXTURE_SIDE),
            &|request: &opdf_core::render::RenderRequest| cache.wants(request),
            MAX_SUBMISSIONS_PER_FRAME,
        )
        .requests
        .iter()
        .for_each(|_| wanted += 1);
        assert_eq!(
            wanted, 0,
            "a request already pending must not be planned again, or a scrolling viewer floods the service"
        );
        assert!(first.submitted > 0);
    }

    #[test]
    fn caps_a_frames_submissions_no_matter_how_much_is_visible() {
        let ctx = egui::Context::default();
        let (mut state, _theme) = build_viewer(400);
        state.zoom = 0.1;
        state.viewport_size_px = (1000.0, 8000.0);
        let service = FakeRenderService::new(state.snapshot().clone());
        let mut cache = TextureCache::new(1 << 26);

        let outcome = step_render_service(&state, &service, &mut [&mut cache], &ctx, 1.0);
        assert!(
            outcome.submitted <= MAX_SUBMISSIONS_PER_FRAME,
            "a zoomed-out view of a long document must not queue hundreds of tiles in one frame"
        );
        assert!(outcome.skipped > 0, "what did not fit must be reported, so the caller repaints and asks again");
    }

    /// The budget must defer a request, not lose it.
    ///
    /// Marking pending while planning meant everything past the budget was
    /// recorded as in flight and never submitted. Nothing could clear it:
    /// only a response clears a pending slot, and no response was coming.
    /// The visible symptoms were a page that stayed a grey placeholder for
    /// the life of the session and a repaint loop that never settled, both
    /// reachable from six clicks of the toolbar's zoom-out button.
    ///
    /// Left settling on its own with no further input, the frame loop must
    /// reach a quiet state: nothing in flight, and every visible page drawn.
    #[test]
    fn a_view_that_exceeds_the_frame_budget_still_settles_with_nothing_in_flight() {
        let ctx = egui::Context::default();
        let (mut state, _theme) = build_viewer(400);
        state.zoom = 0.1;
        state.viewport_size_px = (1000.0, 8000.0);
        let service = FakeRenderService::new(state.snapshot().clone());
        let mut cache = TextureCache::new(1 << 26);

        let first = step_render_service(&state, &service, &mut [&mut cache], &ctx, 1.0);
        assert!(first.skipped > 0, "this view must actually exceed the frame budget, or the test proves nothing");

        //--- no input, only frames: everything deferred must come back round ---
        for _ in 0..200 {
            step_render_service(&state, &service, &mut [&mut cache], &ctx, 1.0);
        }

        assert_eq!(
            cache.pending_count(),
            0,
            "a request deferred by the frame budget must be submitted on a later frame, not stranded in flight forever"
        );

        let snapshot = state.snapshot();
        let settings = state.render_settings(1.0, TEST_MAX_TEXTURE_SIDE);
        let undrawn: Vec<usize> = state
            .visible_pages()
            .filter(|index| {
                snapshot.pages.get(*index).is_none_or(|page| {
                    let Ok(request) = RenderRequest::new(page.id, snapshot.revision, settings.scale_for_page(page.display_size())) else {
                        return false;
                    };
                    !cache.contains(&request.with_rotation(settings.view_rotation))
                })
            })
            .collect();
        assert!(
            undrawn.is_empty(),
            "pages {undrawn:?} never got a tile and would stay grey placeholders for the session"
        );
    }

    #[test]
    fn stays_within_the_cache_budget_while_scrolling_the_whole_document() {
        let ctx = egui::Context::default();
        let (mut state, _theme) = build_viewer(200);
        let service = FakeRenderService::new(state.snapshot().clone());
        let mut cache = TextureCache::new(8 << 20);

        for ii in 0..200 {
            state.scroll_to_page(ii, 0.0);
            state.refresh_current_page();
            //--- two frames per page: one to submit, one to absorb ---
            step_render_service(&state, &service, &mut [&mut cache], &ctx, 1.0);
            step_render_service(&state, &service, &mut [&mut cache], &ctx, 1.0);
        }
        assert!(
            cache.used_bytes() <= 8 << 20,
            "scrolling a 200-page document end to end must not grow the texture cache without bound, ended at {} bytes",
            cache.used_bytes()
        );
    }

    #[test]
    fn discards_tiles_that_answer_a_superseded_revision() {
        let ctx = egui::Context::default();
        let (mut state, theme) = build_viewer(20);
        //--- a service still holding the old snapshot answers requests for the old revision ---
        let stale_service = FakeRenderService::new(state.snapshot().clone());
        let mut cache = TextureCache::new(1 << 26);
        step_render_service(&state, &stale_service, &mut [&mut cache], &ctx, 1.0);

        let mut next = state.snapshot().clone();
        next.revision += 1;
        state.replace_snapshot(next, &theme, &mut [&mut cache]);

        let outcome = step_render_service(&state, &stale_service, &mut [&mut cache], &ctx, 1.0);
        assert_eq!(
            outcome.absorbed.stored, 0,
            "tiles rasterized for the previous revision must never enter the cache"
        );
        assert!(outcome.absorbed.discarded > 0, "they must be counted as discarded, not silently accepted");
    }
}
