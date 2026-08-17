//! The application shell: the one place that owns state, runs the render loop, and
//! applies user actions.
//!
//! This module routes; it does not compute. Everything it decides was decided by
//! [`crate::layout`], [`crate::zoom`], [`crate::scheduler`], and [`crate::viewer`].

use std::path::Path;

use opdf_core::document::{Document, DocumentSnapshot};
use opdf_core::fakes::FakeRenderService;
use opdf_core::page::Rotation;
use opdf_core::render::RenderService;

use crate::opener::{DocumentOpener, EditableDocument, NativePathChooser, OpenedDocument, PathChooser, PdfiumDocumentOpener};
use crate::panels::menu_bar::MenuAction;
use crate::panels::status_bar::RenderStatus;
use crate::panels::toolbar::ToolbarOutcome;
use crate::theme::Theme;
use crate::tiles::TextureCache;
use crate::viewer::{FitMode, ViewerState, step_render_service};

/// Texture budget for the page canvas: enough for roughly a screenful of pages at
/// several zoom levels, on a HiDPI display.
pub const CANVAS_CACHE_BUDGET_BYTES: usize = 256 * 1024 * 1024;

/// Texture budget for the thumbnail rail. Thumbnails are small, and the rail must
/// keep its own working set even while the canvas churns.
pub const RAIL_CACHE_BUDGET_BYTES: usize = 32 * 1024 * 1024;

/// The application.
pub struct OpdfApp {
    theme: Theme,
    state: ViewerState,
    document: Option<Box<dyn EditableDocument>>,
    service: Box<dyn RenderService>,
    canvas_cache: TextureCache,
    rail_cache: TextureCache,
    page_entry: String,
    show_about: bool,
    last_error: Option<String>,
    chooser: Box<dyn PathChooser>,
    opener: Box<dyn DocumentOpener>,
}

impl OpdfApp {
    /// Build the application around an already-opened document, installing the
    /// theme into `ctx`.
    ///
    /// The service is handed in rather than constructed here: that is what lets
    /// the same shell draw an in-memory fake in tests and real PDFium in
    /// production, and it guarantees the service and the snapshot the shell draws
    /// were built from the same document at the same revision.
    pub fn new(ctx: &egui::Context, opened: OpenedDocument) -> Self {
        let theme = Theme::dark();
        crate::theme::apply_theme(ctx, &theme);
        Self {
            state: ViewerState::new(opened.snapshot, &theme),
            theme,
            document: Some(opened.document),
            service: opened.service,
            canvas_cache: TextureCache::new(CANVAS_CACHE_BUDGET_BYTES),
            rail_cache: TextureCache::new(RAIL_CACHE_BUDGET_BYTES),
            page_entry: "1".to_owned(),
            show_about: false,
            last_error: None,
            chooser: Box::new(NativePathChooser),
            opener: Box::new(PdfiumDocumentOpener),
        }
    }

    /// Replace the route File ▸ Open takes: which dialog asks the user, and what
    /// opens the answer.
    ///
    /// Production uses the platform dialog and PDFium. A test injects both,
    /// because a native file dialog cannot be driven headlessly — and what the
    /// shell does with the dialog's answer is the part worth testing anyway.
    pub fn with_open_route(mut self, chooser: Box<dyn PathChooser>, opener: Box<dyn DocumentOpener>) -> Self {
        self.chooser = chooser;
        self.opener = opener;
        self
    }

    /// The viewer state this frame will draw: zoom, scroll offset, current page,
    /// rail visibility.
    pub fn state(&self) -> &ViewerState {
        &self.state
    }

    /// The document currently open, if any.
    ///
    /// The shell draws from the snapshot, never from this — it is kept so that a
    /// later edit or save has something to act on, and so a test can check that
    /// the shell is holding the document it was handed.
    pub fn document(&self) -> Option<&dyn Document> {
        self.document.as_deref().map(|document| document as &dyn Document)
    }

    /// The canvas's texture cache, for the status bar and for tests.
    pub fn canvas_cache(&self) -> &TextureCache {
        &self.canvas_cache
    }

    /// The thumbnail rail's texture cache, which has its own budget.
    pub fn rail_cache(&self) -> &TextureCache {
        &self.rail_cache
    }

    /// Show `opened` as a newly opened document: its own render service, a fresh
    /// document identity, both caches emptied, and the view back at page 1.
    ///
    /// Every way a document arrives goes through here — opening a file,
    /// generating a synthetic one — because this is the single place that
    /// guarantees no pixel of the previous document survives into the new one.
    /// Emptying the caches is not an optimisation: the previous document's tiles
    /// can never be served again, so keeping them is pure memory cost. It is no
    /// longer what makes the change *correct* — a render request now names its
    /// [`opdf_core::DocumentId`], so a late response from the previous document
    /// is rejected by key rather than filed as a tile of this one. Replacing the
    /// service is likewise kept for its own reasons: the rasterizer resolves page
    /// positions against the file it opened, which is a different file now.
    pub fn open_document(&mut self, opened: OpenedDocument) {
        self.install_document(Some(opened.document), opened.service, opened.snapshot);
    }

    /// Open `path` through `opener`, replacing whatever is currently open.
    ///
    /// A failed open is reported and otherwise inert: the document already on
    /// screen stays exactly as it was. Losing the user's document because the file
    /// they picked was malformed would be the worst possible response to an error
    /// this API is guaranteed to produce.
    pub fn open_path(&mut self, opener: &dyn DocumentOpener, path: &Path) {
        let result = opener.open(path);
        self.absorb_open(result, path);
    }

    /// Ask the configured chooser for a file and open it, doing nothing at all if
    /// the user cancels.
    ///
    /// A cancelled dialog is not a failure and must not be reported as one.
    fn open_chosen_path(&mut self) {
        let Some(path) = self.chooser.choose_pdf() else {
            return;
        };
        //--- the borrow of `self.opener` ends with the call, so the result can be
        //--- absorbed through `&mut self` on the next line ---
        let result = self.opener.open(&path);
        self.absorb_open(result, &path);
    }

    /// Install an opened document, or record why it could not be opened.
    fn absorb_open(&mut self, result: opdf_core::Result<OpenedDocument>, path: &Path) {
        match result {
            Ok(opened) => {
                self.open_document(opened);
                self.last_error = None;
            }
            Err(error) => self.last_error = Some(format!("could not open {}: {error}", path.display())),
        }
    }

    /// The most recent failure the user has not yet dismissed.
    pub fn last_error(&self) -> Option<&str> {
        self.last_error.as_deref()
    }

    /// Show no document at all, releasing the one that was open.
    ///
    /// Routed through the same installation as [`OpdfApp::open_document`] so that
    /// closing cannot forget a step opening remembers.
    fn close_document(&mut self) {
        let snapshot = DocumentSnapshot::default();
        self.install_document(None, Box::new(FakeRenderService::new(snapshot.clone())), snapshot);
    }

    /// The one place a document, its service, and its snapshot are installed
    /// together.
    fn install_document(&mut self, document: Option<Box<dyn EditableDocument>>, service: Box<dyn RenderService>, snapshot: DocumentSnapshot) {
        self.document = document;
        //--- the previous service is dropped here, which is what keeps a late
        //--- response from the old document out of the new one's cache ---
        self.service = service;
        self.state
            .open_document(snapshot, &self.theme, &mut [&mut self.canvas_cache, &mut self.rail_cache]);
        self.state.scroll_to_page(0, 0.0);
        self.page_entry = "1".to_owned();
    }

    /// Replace the document with a freshly generated synthetic one.
    fn load_synthetic(&mut self, page_count: usize) {
        let Ok(opened) = crate::synthetic::open_synthetic_document(page_count) else {
            return;
        };
        self.open_document(opened);
    }
}

//---------------------------------------------------------------------
// Applying actions
//---------------------------------------------------------------------

impl OpdfApp {
    /// Apply one user action, from whichever surface produced it.
    pub fn apply_action(&mut self, action: MenuAction, ctx: &egui::Context) {
        let anchor_px = self.state.viewport_size_px.1 * 0.5;
        let last_page = self.state.page_count().saturating_sub(1);
        let current = self.state.current_page().unwrap_or(0);
        match action {
            MenuAction::OpenDocument => self.open_chosen_path(),
            MenuAction::CloseDocument => self.close_document(),
            MenuAction::Quit => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
            MenuAction::GenerateSynthetic(page_count) => self.load_synthetic(page_count),
            MenuAction::ZoomIn => {
                self.state.fit_mode = FitMode::Free;
                self.state.set_zoom_anchored(crate::zoom::step_zoom_in(self.state.zoom), anchor_px);
            }
            MenuAction::ZoomOut => {
                self.state.fit_mode = FitMode::Free;
                self.state.set_zoom_anchored(crate::zoom::step_zoom_out(self.state.zoom), anchor_px);
            }
            MenuAction::ZoomActual => {
                self.state.fit_mode = FitMode::Free;
                self.state.set_zoom_anchored(1.0, anchor_px);
            }
            MenuAction::FitWidth => {
                self.state.fit_mode = FitMode::Width;
                self.state.reapply_fit_mode();
            }
            MenuAction::FitPage => {
                self.state.fit_mode = FitMode::Page;
                self.state.reapply_fit_mode();
            }
            MenuAction::FirstPage => self.state.scroll_to_page(0, 0.0),
            MenuAction::LastPage => self.state.scroll_to_page(last_page, 0.0),
            MenuAction::NextPage => self.state.scroll_to_page((current + 1).min(last_page), 0.0),
            MenuAction::PreviousPage => self.state.scroll_to_page(current.saturating_sub(1), 0.0),
            MenuAction::ToggleRail => self.state.rail_visible = !self.state.rail_visible,
            MenuAction::RotateViewClockwise => self.state.view_rotation = self.state.view_rotation.rotated_by(Rotation::Quarter),
            MenuAction::RotateViewCounterClockwise => self.state.view_rotation = self.state.view_rotation.rotated_by(Rotation::ThreeQuarter),
            MenuAction::ShowAbout => self.show_about = true,
        }
    }

    /// The action, if any, the keyboard asked for this frame.
    pub fn collect_keyboard_action(ctx: &egui::Context) -> Option<MenuAction> {
        ctx.input_mut(|input| {
            const COMMAND: egui::Modifiers = egui::Modifiers::COMMAND;
            let bindings: [(egui::Modifiers, egui::Key, MenuAction); 8] = [
                (COMMAND, egui::Key::Plus, MenuAction::ZoomIn),
                (COMMAND, egui::Key::Equals, MenuAction::ZoomIn),
                (COMMAND, egui::Key::Minus, MenuAction::ZoomOut),
                (COMMAND, egui::Key::Num0, MenuAction::ZoomActual),
                (COMMAND, egui::Key::Home, MenuAction::FirstPage),
                (COMMAND, egui::Key::End, MenuAction::LastPage),
                (egui::Modifiers::NONE, egui::Key::PageDown, MenuAction::NextPage),
                (egui::Modifiers::NONE, egui::Key::PageUp, MenuAction::PreviousPage),
            ];
            for (modifiers, key, action) in bindings {
                if input.consume_shortcut(&egui::KeyboardShortcut::new(modifiers, key)) {
                    return Some(action);
                }
            }
            None
        })
    }

    /// Apply a wheel or pinch zoom, anchored at the pointer.
    ///
    /// egui reports a pinch or a modifier-held wheel as `zoom_delta`, which is a
    /// multiplier. Anchoring at the pointer rather than the viewport centre is what
    /// makes zooming feel like the document is being pulled toward the cursor.
    ///
    /// The anchor is measured from the **canvas viewport's** top edge, not the
    /// window's: the chrome above the canvas is tens of points tall, and
    /// [`crate::zoom::anchor_scroll_offset`] multiplies any error in the anchor by
    /// `new_zoom / old_zoom - 1`, so a window-relative anchor visibly slides the
    /// page out from under the pointer on every step.
    pub fn apply_wheel_zoom(&mut self, ctx: &egui::Context) {
        let (zoom_delta, pointer) = ctx.input(|input| (input.zoom_delta(), input.pointer.hover_pos()));
        if (zoom_delta - 1.0).abs() < 1e-4 {
            return;
        }
        let anchor_px = match pointer {
            Some(position) => (position.y - self.state.viewport_origin_px.1).clamp(0.0, self.state.viewport_size_px.1),
            None => self.state.viewport_size_px.1 * 0.5,
        };
        self.state.fit_mode = FitMode::Free;
        self.state.set_zoom_anchored(self.state.zoom * zoom_delta, anchor_px);
    }
}

//---------------------------------------------------------------------
// The frame
//---------------------------------------------------------------------

impl OpdfApp {
    /// Run one whole frame: the service step, then every panel, then the actions
    /// they produced.
    ///
    /// This is the body of [`eframe::App::update`], split out so it can be driven
    /// without a window. `eframe::Frame` has no public constructor, so a test
    /// cannot call `update` — but it can build an [`egui::Context`], feed it
    /// synthetic [`egui::RawInput`], and run this. That is what makes scrolling,
    /// wheel zoom, thumbnail clicks, and keyboard shortcuts testable in headless
    /// CI rather than only by eye.
    pub fn draw(&mut self, ctx: &egui::Context) {
        //--- the render loop's service half, once per frame, before anything is drawn ---
        //--- both caches are handed in: one service answers both surfaces, and a
        //--- response has to reach the cache that asked or the rail never fills in ---
        let pixels_per_point = ctx.pixels_per_point();
        step_render_service(
            &self.state,
            self.service.as_ref(),
            &mut [&mut self.canvas_cache, &mut self.rail_cache],
            ctx,
            pixels_per_point,
        );

        let mut pending_action = Self::collect_keyboard_action(ctx);
        self.apply_wheel_zoom(ctx);

        egui::TopBottomPanel::top("opdf_menu_bar").show(ctx, |ui| {
            if let Some(action) = crate::panels::menu_bar::show_menu_bar(ui) {
                pending_action = Some(action);
            }
        });

        egui::TopBottomPanel::top("opdf_toolbar").show(ctx, |ui| {
            match crate::panels::toolbar::show_toolbar(ui, &self.state, &mut self.page_entry, &self.theme) {
                Some(ToolbarOutcome::Action(action)) => pending_action = Some(action),
                Some(ToolbarOutcome::JumpToPage(index)) => self.state.scroll_to_page(index, 0.0),
                None => {}
            }
        });

        egui::TopBottomPanel::bottom("opdf_status_bar").show(ctx, |ui| {
            crate::panels::status_bar::show_status_bar(ui, &self.state, &RenderStatus::of(&self.canvas_cache), self.last_error.as_deref(), &self.theme);
        });

        egui::SidePanel::left("opdf_thumbnail_rail")
            .resizable(true)
            .default_width(self.theme.rail_width)
            .show_animated(ctx, self.state.rail_visible, |ui| {
                let clicked = crate::panels::thumbnail_rail::show_thumbnail_rail(ui, &mut self.state, &mut self.rail_cache, self.service.as_ref(), &self.theme);
                if let Some(index) = clicked {
                    self.state.scroll_to_page(index, 0.0);
                    self.page_entry = format!("{}", index + 1);
                }
            });

        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(self.theme.canvas_background))
            .show(ctx, |ui| {
                if self.state.page_count() == 0 {
                    ui.centered_and_justified(|ui| {
                        ui.colored_label(self.theme.text_muted, "No document. Use File to generate a synthetic one.");
                    });
                    return;
                }
                crate::panels::canvas::show_canvas(ui, &mut self.state, &mut self.canvas_cache, &self.theme, pixels_per_point);
            });

        //--- the canvas has just written back the viewport it really got; a fit mode
        //--- is a standing promise and has to follow that viewport, resize or not ---
        if self.state.sync_fit_to_viewport() {
            //--- the refit lands after this frame's shapes, so ask for the frame that draws it ---
            ctx.request_repaint();
        }

        if self.show_about {
            let mut open = true;
            egui::Window::new("About opdf").open(&mut open).resizable(false).show(ctx, |ui| {
                ui.label(crate::describe_build());
                ui.label("Documents are parsed by lopdf and rasterized by PDFium. Text selection and search are not implemented.");
                ui.label("Icons: Phosphor (MIT). No Adobe assets are used.");
            });
            self.show_about = open;
        }

        //--- keep the page field in step with where the user actually is ---
        if let Some(index) = self.state.current_page() {
            let expected = format!("{}", index + 1);
            if self.page_entry != expected && !ctx.wants_keyboard_input() {
                self.page_entry = expected;
            }
        }

        if let Some(action) = pending_action {
            self.apply_action(action, ctx);
        }
    }
}

impl eframe::App for OpdfApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.draw(ctx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::synthetic::open_synthetic_document;

    fn build_app(page_count: usize) -> (OpdfApp, egui::Context) {
        let ctx = egui::Context::default();
        let mut app = OpdfApp::new(&ctx, open_synthetic_document(page_count).unwrap());
        app.state.viewport_size_px = (1000.0, 800.0);
        app.state.refresh_current_page();
        (app, ctx)
    }

    #[test]
    fn walks_the_zoom_ladder_from_the_menu() {
        let (mut app, ctx) = build_app(20);
        app.apply_action(MenuAction::ZoomIn, &ctx);
        assert_eq!(app.state.zoom, 1.25);
        app.apply_action(MenuAction::ZoomOut, &ctx);
        assert_eq!(app.state.zoom, 1.0);
        app.apply_action(MenuAction::ZoomIn, &ctx);
        app.apply_action(MenuAction::ZoomActual, &ctx);
        assert_eq!(app.state.zoom, 1.0);
    }

    #[test]
    fn a_manual_zoom_cancels_a_fit_mode() {
        let (mut app, ctx) = build_app(20);
        app.apply_action(MenuAction::FitWidth, &ctx);
        assert_eq!(app.state.fit_mode, FitMode::Width);
        app.apply_action(MenuAction::ZoomIn, &ctx);
        assert_eq!(app.state.fit_mode, FitMode::Free, "a user-chosen zoom must not be undone by the next resize");
    }

    #[test]
    fn navigates_without_running_off_either_end() {
        let (mut app, ctx) = build_app(10);
        app.apply_action(MenuAction::PreviousPage, &ctx);
        assert_eq!(app.state.current_page(), Some(0), "paging back from the first page must stay put");
        app.apply_action(MenuAction::LastPage, &ctx);
        assert_eq!(app.state.current_page(), Some(9));
        app.apply_action(MenuAction::NextPage, &ctx);
        assert_eq!(app.state.current_page(), Some(9), "paging on from the last page must stay put");
        app.apply_action(MenuAction::FirstPage, &ctx);
        assert_eq!(app.state.current_page(), Some(0));
    }

    #[test]
    fn toggles_the_rail() {
        let (mut app, ctx) = build_app(10);
        assert!(app.state.rail_visible);
        app.apply_action(MenuAction::ToggleRail, &ctx);
        assert!(!app.state.rail_visible);
        app.apply_action(MenuAction::ToggleRail, &ctx);
        assert!(app.state.rail_visible);
    }

    #[test]
    fn composes_view_rotation_a_quarter_turn_at_a_time() {
        let (mut app, ctx) = build_app(10);
        app.apply_action(MenuAction::RotateViewClockwise, &ctx);
        assert_eq!(app.state.view_rotation, Rotation::Quarter);
        app.apply_action(MenuAction::RotateViewClockwise, &ctx);
        assert_eq!(app.state.view_rotation, Rotation::Half);
        app.apply_action(MenuAction::RotateViewCounterClockwise, &ctx);
        assert_eq!(app.state.view_rotation, Rotation::Quarter);
    }

    #[test]
    fn replaces_the_document_and_clears_both_caches() {
        let (mut app, ctx) = build_app(10);
        //--- warm both caches ---
        for _ in 0..2 {
            step_render_service(&app.state, app.service.as_ref(), &mut [&mut app.canvas_cache], &ctx, 1.0);
        }
        assert!(!app.canvas_cache.is_empty(), "the canvas cache must warm before this test means anything");

        app.apply_action(MenuAction::GenerateSynthetic(30), &ctx);

        assert_eq!(app.state.page_count(), 30);
        assert!(
            app.canvas_cache.is_empty(),
            "textures from the previous document must be released, not merely orphaned"
        );
        assert!(app.rail_cache.is_empty());
        assert_eq!(app.state.current_page(), Some(0));
    }

    /// The dangerous case the previous test misses: two documents of the same
    /// length share a revision, so nothing about the *snapshot* says the cache
    /// must be emptied — only the fact that a document was opened does.
    #[test]
    fn generating_the_same_document_again_still_releases_its_textures() {
        let (mut app, ctx) = build_app(10);
        for _ in 0..2 {
            step_render_service(&app.state, app.service.as_ref(), &mut [&mut app.canvas_cache], &ctx, 1.0);
        }
        assert!(!app.canvas_cache.is_empty(), "the canvas cache must warm before this test means anything");
        let revision = app.state.snapshot().revision;
        let document = app.state.document_id();

        app.apply_action(MenuAction::GenerateSynthetic(10), &ctx);

        assert_eq!(
            app.state.snapshot().revision,
            revision,
            "the two documents collide on revision; that is the case that matters"
        );
        assert_ne!(document, app.state.document_id(), "a document the user opened must carry a new identity");
        assert!(
            app.canvas_cache.is_empty(),
            "the previous document's textures would be served for the new one, whose requests are keyed identically"
        );
    }

    #[test]
    fn survives_closing_the_document() {
        let (mut app, ctx) = build_app(10);
        app.apply_action(MenuAction::CloseDocument, &ctx);
        assert_eq!(app.state.page_count(), 0);
        assert_eq!(app.state.current_page(), None);
        //--- every action must remain safe with no document open ---
        for action in [MenuAction::NextPage, MenuAction::LastPage, MenuAction::FitPage, MenuAction::ZoomIn] {
            app.apply_action(action, &ctx);
        }
        assert_eq!(app.state.page_count(), 0);
    }

    #[test]
    fn steps_the_render_loop_without_a_display() {
        let (app, ctx) = build_app(40);
        let mut cache = TextureCache::new(1 << 24);
        let first = step_render_service(&app.state, app.service.as_ref(), &mut [&mut cache], &ctx, 1.0);
        let second = step_render_service(&app.state, app.service.as_ref(), &mut [&mut cache], &ctx, 1.0);
        assert!(first.submitted > 0);
        assert_eq!(second.absorbed.stored, first.submitted);
    }
}
