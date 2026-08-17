//! The application shell: the one place that owns state, runs the render loop, and
//! applies user actions.
//!
//! This module routes; it does not compute. Everything it decides was decided by
//! [`crate::layout`], [`crate::zoom`], [`crate::scheduler`], and [`crate::viewer`].

use std::path::{Path, PathBuf};

use opdf_core::command::Command;
use opdf_core::document::{Document, DocumentSnapshot};
use opdf_core::fakes::FakeRenderService;
use opdf_core::page::Rotation;
use opdf_core::render::RenderService;
use opdf_ops::{RemovePage, SetRotation, UndoStack};

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

/// Which of the two save paths a write takes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SaveMode {
    /// Append the changes to the original bytes, preserving everything else.
    Incremental,
    /// Serialize the document afresh, discarding unreferenced objects.
    ///
    /// Discards trashed pages along with them, which is why reaching this
    /// requires a confirmation and costs the undo history.
    Compacted,
}

/// What the user asked for that would abandon the open document.
///
/// Each of these ends the document's life on screen, so each has to be held back
/// until the user has answered for the edits they have not saved.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ExitIntent {
    /// Close the document and show the empty state.
    Close,
    /// Exit the application.
    Quit,
    /// Open a different document in its place.
    Open,
}

/// The application.
pub struct OpdfApp {
    theme: Theme,
    state: ViewerState,
    document: Option<Box<dyn EditableDocument>>,
    document_path: Option<PathBuf>,
    /// The edit history for the document currently open, and only that one.
    ///
    /// A queued entry names pages by [`opdf_core::PageId`], which is meaningful
    /// within one document, so the stack is emptied whenever the document is
    /// replaced. It is typed over the trait object rather than a concrete
    /// document because that is what the shell owns; `Command` and `UndoStack`
    /// accept a `?Sized` document precisely so this is possible.
    undo: UndoStack<dyn EditableDocument>,
    /// Whether the shell is waiting for the user to confirm a compacting save.
    ///
    /// Raised by [`MenuAction::Compact`] and answered by
    /// [`OpdfApp::confirm_compaction`] or [`OpdfApp::cancel_compaction`]. Nothing
    /// is written while this is set: the write is what the user is being asked
    /// about.
    compaction_pending: bool,
    /// The revision the document was at when it was last written to disk.
    ///
    /// Compared against [`Document::revision`] to tell whether there is anything
    /// to lose. The contract requires every mutation to advance the revision and
    /// every failure and read-only call to leave it alone, which is exactly the
    /// property a dirty flag needs and exactly what a hand-maintained one gets
    /// wrong. `None` for a document that has never been written, whose every
    /// edit is therefore unsaved.
    saved_revision: Option<u64>,
    /// The exit the user asked for and has not yet answered for.
    exit_pending: Option<ExitIntent>,
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
        //--- opening a file does not modify it, so it starts clean at whatever
        //--- revision it was parsed at ---
        let saved_revision = Some(opened.document.revision());
        Self {
            state: ViewerState::new(opened.snapshot, &theme),
            theme,
            document: Some(opened.document),
            document_path: opened.path,
            undo: UndoStack::new(),
            compaction_pending: false,
            saved_revision,
            exit_pending: None,
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
        self.install_document(Some(opened.document), opened.path, opened.service, opened.snapshot);
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
        self.install_document(None, None, Box::new(FakeRenderService::new(snapshot.clone())), snapshot);
    }

    /// The one place a document, its origin, its service, and its snapshot are
    /// installed together.
    fn install_document(
        &mut self,
        document: Option<Box<dyn EditableDocument>>,
        path: Option<PathBuf>,
        service: Box<dyn RenderService>,
        snapshot: DocumentSnapshot,
    ) {
        self.document = document;
        //--- the path travels with the document, so closing or replacing one
        //--- cannot leave Save aimed at the previous document's file ---
        self.document_path = path;
        //--- a queued entry addresses pages of the document it was recorded
        //--- against; against this one those ids mean nothing, or worse, something ---
        self.undo.clear();
        //--- whatever arrives here arrives as it is on disk, so it starts clean ---
        self.saved_revision = self.document.as_deref().map(Document::revision);
        //--- the previous service is dropped here, which is what keeps a late
        //--- response from the old document out of the new one's cache ---
        self.service = service;
        self.state
            .open_document(snapshot, &self.theme, &mut [&mut self.canvas_cache, &mut self.rail_cache]);
        self.state.scroll_to_page(0, 0.0);
        self.page_entry = "1".to_owned();
    }

    /// Where the open document will be written by Save, if it has an origin.
    pub fn document_path(&self) -> Option<&Path> {
        self.document_path.as_deref()
    }

    /// Write the open document to `path`, remembering it as the document's home.
    ///
    /// Does nothing at all when no document is open: Save with an empty window is
    /// a no-op, not an error worth interrupting anyone over. A failed write is
    /// reported through `last_error` and leaves the document exactly as it was —
    /// the file on disk may be damaged, but the copy the user is editing is not,
    /// and that is the copy they can still save elsewhere.
    ///
    /// Returns whether the document was written.
    pub fn save_to(&mut self, path: &Path, mode: SaveMode) -> bool {
        let Some(document) = self.document.as_mut() else {
            return false;
        };
        let result = match mode {
            SaveMode::Incremental => document.save_incremental(path),
            SaveMode::Compacted => document.save_compacted(path),
        };
        match result {
            Ok(()) => {
                let revision = document.revision();
                self.document_path = Some(path.to_owned());
                //--- what is on disk is now what is in memory, up to this revision ---
                self.saved_revision = Some(revision);
                self.last_error = None;
                true
            }
            Err(error) => {
                self.last_error = Some(format!("could not save {}: {error}", path.display()));
                false
            }
        }
    }

    /// Save to the document's own path, asking for one if it has none.
    ///
    /// Incremental is the default path for a reason worth restating here: it
    /// appends to the original bytes, so every structure the implementation does
    /// not model survives, and it does not purge the trash, so undo of a deletion
    /// survives it too. Compaction does neither, which is why it is a separate,
    /// confirmed action rather than a faster default.
    fn save_in_place(&mut self) {
        if self.document.is_none() {
            return;
        }
        match self.document_path.clone() {
            Some(path) => {
                self.save_to(&path, SaveMode::Incremental);
            }
            None => self.save_to_chosen_path(),
        }
    }

    /// Ask where to write the document, then write it, doing nothing if the user
    /// cancels.
    ///
    /// The check for an open document comes *before* the dialog, not after it:
    /// asking someone to name a file and then writing nothing to it is worse than
    /// doing nothing at all, and with no document open there is nothing to name.
    fn save_to_chosen_path(&mut self) {
        if self.document.is_none() {
            return;
        }
        let Some(path) = self.chooser.choose_save_path() else {
            return;
        };
        self.save_to(&path, SaveMode::Incremental);
    }

    /// How many edits can currently be undone.
    pub fn undo_depth(&self) -> usize {
        self.undo.undo_depth()
    }

    /// How many undone edits can currently be redone.
    pub fn redo_depth(&self) -> usize {
        self.undo.redo_depth()
    }

    /// Apply `command` to the open document through the undo stack, then show the
    /// result.
    ///
    /// Every document edit goes through here, so that no edit can reach the
    /// document without being recorded, and none can be recorded without the
    /// canvas being told to redraw the revision it produced.
    fn apply_command(&mut self, command: Box<dyn Command<dyn EditableDocument>>) {
        let Some(document) = self.document.as_mut() else {
            return;
        };
        if let Err(error) = self.undo.apply(document.as_mut(), command) {
            self.last_error = Some(format!("could not apply the edit: {error}"));
            return;
        }
        self.resnapshot();
    }

    /// Undo the most recent edit, if there is one.
    fn undo_edit(&mut self) {
        let Some(document) = self.document.as_mut() else {
            return;
        };
        match self.undo.undo(document.as_mut()) {
            Ok(true) => self.resnapshot(),
            //--- nothing to undo is not a failure; it is an empty history ---
            Ok(false) => {}
            Err(error) => self.last_error = Some(format!("could not undo: {error}")),
        }
    }

    /// Redo the most recently undone edit, if there is one.
    fn redo_edit(&mut self) {
        let Some(document) = self.document.as_mut() else {
            return;
        };
        match self.undo.redo(document.as_mut()) {
            Ok(true) => self.resnapshot(),
            Ok(false) => {}
            Err(error) => self.last_error = Some(format!("could not redo: {error}")),
        }
    }

    /// Re-derive the snapshot from the document after an edit and hand it to the
    /// viewer.
    ///
    /// This is [`ViewerState::replace_snapshot`], not `open_document`: the
    /// document is the same one, so tiles still valid at the new revision must
    /// survive, which is what stops an undo from blanking the canvas.
    fn resnapshot(&mut self) {
        let Some(document) = self.document.as_deref() else {
            return;
        };
        match DocumentSnapshot::of(document) {
            Ok(snapshot) => self
                .state
                .replace_snapshot(snapshot, &self.theme, &mut [&mut self.canvas_cache, &mut self.rail_cache]),
            Err(error) => self.last_error = Some(format!("could not read the edited document: {error}")),
        }
    }

    //---------------------------------------------------------------------
    // Unsaved changes
    //---------------------------------------------------------------------

    /// Whether the document has been edited since it was last written to disk.
    ///
    /// Read from [`Document::revision`] rather than from a flag the shell
    /// maintains: the contract requires every mutation to advance it and every
    /// failed or read-only call to leave it alone, so it cannot drift out of step
    /// with the document the way a hand-set flag does. Undo advances the revision
    /// too, so undoing back to the saved state still reports unsaved changes —
    /// the safe direction to be wrong in.
    pub fn has_unsaved_changes(&self) -> bool {
        match self.document.as_deref() {
            Some(document) => self.saved_revision != Some(document.revision()),
            None => false,
        }
    }

    /// The exit the shell is holding back until the user answers for their
    /// unsaved edits.
    pub fn discard_prompt(&self) -> Option<ExitIntent> {
        self.exit_pending
    }

    /// Whether `intent` may go ahead immediately, raising the prompt if not.
    ///
    /// Returns `true` when there is nothing to lose. Every route out of a
    /// document goes through here, so none of them can forget to ask.
    fn may_abandon_document(&mut self, intent: ExitIntent) -> bool {
        if !self.has_unsaved_changes() {
            return true;
        }
        self.exit_pending = Some(intent);
        false
    }

    /// Go ahead with the exit the user was asked about, abandoning the edits.
    pub fn confirm_discard(&mut self, ctx: &egui::Context) {
        let Some(intent) = self.exit_pending.take() else {
            return;
        };
        match intent {
            ExitIntent::Close => self.close_document(),
            ExitIntent::Quit => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
            //--- the file dialog comes after the question, not before it: there is
            //--- no point naming a file for an open the user may still call off ---
            ExitIntent::Open => self.open_chosen_path(),
        }
    }

    /// Think better of the exit, keeping the document and its edits.
    pub fn cancel_discard(&mut self) {
        self.exit_pending = None;
    }

    //---------------------------------------------------------------------
    // Compaction, and the history it costs — F16
    //---------------------------------------------------------------------

    /// Whether the shell is waiting for an answer about a compacting save.
    pub fn compaction_pending(&self) -> bool {
        self.compaction_pending
    }

    /// Ask the user whether to compact, writing nothing yet.
    ///
    /// Compaction is the one save that cannot be silently offered. It purges
    /// unreferenced objects, and a page deleted in this session is exactly that:
    /// it sits in the trash, referenced only by the undo entry that would put it
    /// back. Since the compacted bytes become the document's base, that entry is
    /// dead afterwards — so the user is told what it costs before it happens.
    fn ask_to_compact(&mut self) {
        if self.document.is_none() {
            return;
        }
        self.compaction_pending = true;
    }

    /// Go ahead with the compacting save the user was asked about.
    ///
    /// The history is cleared **only after** `save_compacted` reports success, so
    /// a compaction that failed costs nothing — mirroring `opdf-pdf`, where a
    /// failed compaction leaves the trash intact. Both stacks go: a redo entry
    /// produced by undoing an insertion resolves to `RestorePage` just as an undo
    /// entry produced by a deletion does, and a queued command is opaque, so
    /// there is no way to keep only the survivors.
    pub fn confirm_compaction(&mut self) {
        if !self.compaction_pending {
            return;
        }
        self.compaction_pending = false;
        let path = match self.document_path.clone() {
            Some(path) => Some(path),
            None => self.chooser.choose_save_path(),
        };
        let Some(path) = path else {
            return;
        };
        if self.save_to(&path, SaveMode::Compacted) {
            self.undo.clear();
        }
    }

    /// Think better of the compacting save, writing nothing and keeping the
    /// history.
    pub fn cancel_compaction(&mut self) {
        self.compaction_pending = false;
    }

    /// Delete the page the user is looking at.
    fn delete_current_page(&mut self) {
        let Some(index) = self.state.current_page() else {
            return;
        };
        let Some(page) = self.state.snapshot().pages.get(index) else {
            return;
        };
        self.apply_command(Box::new(RemovePage { page: page.id }));
    }

    /// Turn the page the user is looking at a quarter turn clockwise.
    fn rotate_current_page(&mut self) {
        let Some(index) = self.state.current_page() else {
            return;
        };
        let Some(page) = self.state.snapshot().pages.get(index) else {
            return;
        };
        let command = SetRotation {
            page: page.id,
            rotation: page.rotation.rotated_by(Rotation::Quarter),
        };
        self.apply_command(Box::new(command));
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
            MenuAction::OpenDocument => {
                if self.may_abandon_document(ExitIntent::Open) {
                    self.open_chosen_path();
                }
            }
            MenuAction::Save => self.save_in_place(),
            MenuAction::SaveAs => self.save_to_chosen_path(),
            MenuAction::CloseDocument => {
                if self.may_abandon_document(ExitIntent::Close) {
                    self.close_document();
                }
            }
            MenuAction::Undo => self.undo_edit(),
            MenuAction::Redo => self.redo_edit(),
            MenuAction::RotatePageClockwise => self.rotate_current_page(),
            MenuAction::DeletePage => self.delete_current_page(),
            MenuAction::Compact => self.ask_to_compact(),
            MenuAction::Quit => {
                if self.may_abandon_document(ExitIntent::Quit) {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            }
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

        //--- unsaved edits are not discarded without a word ---
        if let Some(intent) = self.exit_pending {
            let mut confirmed = false;
            let mut cancelled = false;
            let mut save_first = false;
            egui::Modal::new(egui::Id::new("opdf_unsaved_changes")).show(ctx, |ui| {
                ui.set_max_width(420.0);
                ui.heading("Save your changes?");
                ui.add_space(8.0);
                let what = match intent {
                    ExitIntent::Close => "closing this document",
                    ExitIntent::Quit => "quitting",
                    ExitIntent::Open => "opening another document",
                };
                ui.label(format!("You have edits that have not been saved. {what} will discard them."));
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    if ui.button("Cancel").clicked() {
                        cancelled = true;
                    }
                    if ui.button("Discard changes").clicked() {
                        confirmed = true;
                    }
                    if ui.button("Save").clicked() {
                        save_first = true;
                    }
                });
            });
            if save_first {
                self.save_in_place();
                //--- only go through with the exit if the save actually worked ---
                if self.has_unsaved_changes() {
                    self.cancel_discard();
                } else {
                    self.confirm_discard(ctx);
                }
            } else if confirmed {
                self.confirm_discard(ctx);
            } else if cancelled {
                self.cancel_discard();
            }
        }

        //--- the compaction warning: what it costs, in the user's terms, before it costs it ---
        if self.compaction_pending {
            let mut confirmed = false;
            let mut cancelled = false;
            egui::Modal::new(egui::Id::new("opdf_compaction_warning")).show(ctx, |ui| {
                ui.set_max_width(420.0);
                ui.heading("Save compacted?");
                ui.add_space(8.0);
                ui.label(
                    "Compacting rewrites the file without the parts nothing refers to any more. \
                     Pages you deleted in this session are among them, so they become permanently \
                     unrecoverable.",
                );
                ui.add_space(4.0);
                ui.label("Your undo history will be discarded. This cannot be undone.");
                ui.add_space(4.0);
                ui.colored_label(self.theme.text_muted, "Save (incremental) keeps both your deleted pages and your undo history.");
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    if ui.button("Cancel").clicked() {
                        cancelled = true;
                    }
                    if ui.button("Compact and discard history").clicked() {
                        confirmed = true;
                    }
                });
            });
            if confirmed {
                self.confirm_compaction();
            } else if cancelled {
                self.cancel_compaction();
            }
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
