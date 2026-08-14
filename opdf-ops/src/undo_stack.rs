//! The undo/redo stack.

use opdf_core::{Command, Document, Result};

/// A stack of applied commands' inverses, supporting undo and redo.
///
/// Applying a new command after an undo discards the redo stack: once the
/// user has diverged from a previously-undone branch, redoing back onto it
/// would silently resurrect state the new command never accounted for.
pub struct UndoStack<D: Document> {
    undo: Vec<Box<dyn Command<D>>>,
    redo: Vec<Box<dyn Command<D>>>,
}

impl<D: Document> UndoStack<D> {
    /// An empty stack.
    pub fn new() -> Self {
        Self {
            undo: Vec::new(),
            redo: Vec::new(),
        }
    }

    /// Apply a command, pushing its inverse onto the undo stack and
    /// discarding the redo stack.
    pub fn apply(&mut self, document: &mut D, command: Box<dyn Command<D>>) -> Result<()> {
        let inverse = command.apply(document)?;
        self.undo.push(inverse);
        self.redo.clear();
        Ok(())
    }

    /// Undo the most recently applied command. Returns whether there was
    /// anything to undo.
    ///
    /// A failing entry is returned to the stack rather than consumed, so a
    /// failed undo leaves the history exactly as it was. Consuming it would
    /// mean the *next* undo applied an inverse computed for a document state
    /// that never came about — a wrong result reported as success. The
    /// caller sees a repeatable error instead, which is what
    /// [`UndoStack::clear`] exists to resolve.
    pub fn undo(&mut self, document: &mut D) -> Result<bool> {
        let Some(command) = self.undo.pop() else {
            return Ok(false);
        };
        match command.apply(document) {
            Ok(inverse) => {
                self.redo.push(inverse);
                Ok(true)
            }
            Err(error) => {
                self.undo.push(command);
                Err(error)
            }
        }
    }

    /// Redo the most recently undone command. Returns whether there was
    /// anything to redo.
    ///
    /// A failing entry is returned to the redo stack, for the reason given
    /// on [`UndoStack::undo`].
    pub fn redo(&mut self, document: &mut D) -> Result<bool> {
        let Some(command) = self.redo.pop() else {
            return Ok(false);
        };
        match command.apply(document) {
            Ok(inverse) => {
                self.undo.push(inverse);
                Ok(true)
            }
            Err(error) => {
                self.redo.push(command);
                Err(error)
            }
        }
    }

    /// Whether [`UndoStack::undo`] currently has anything to undo.
    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    /// Whether [`UndoStack::redo`] currently has anything to redo.
    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    /// Drop every queued undo and redo entry.
    ///
    /// The caller — never `UndoStack` itself — must invoke this immediately
    /// after a successful `Document::save_compacted`. `opdf-ops` performs no
    /// I/O and cannot observe a save succeeding, so nothing in this crate
    /// can call this automatically; wiring it to the save action is the
    /// responsibility of whichever crate drives both the document and this
    /// stack together (at integration, `opdf-app`).
    ///
    /// Compaction purges every trashed page, so any queued command that
    /// resolves to [`crate::RestorePage`] — including a `redo` entry
    /// produced by undoing an *insertion*, not only an `undo` entry
    /// produced by a deletion — would return `Error::PageNotFound` if
    /// applied afterward. `Box<dyn Command<D>>` is opaque, so there is no
    /// way to inspect a queued entry to tell whether it, or something
    /// nested inside a `Sequence`, touches `restore_page`; dropping both
    /// stacks wholesale is the only sound response available here.
    pub fn clear(&mut self) {
        self.undo.clear();
        self.redo.clear();
    }
}

impl<D: Document> Default for UndoStack<D> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use opdf_core::DocumentSnapshot;
    use opdf_core::Error;
    use opdf_core::fakes::VecDocument;
    use opdf_core::page::{PageId, PageSize};

    use crate::remove_page::RemovePage;

    /// Applies cleanly `remaining` more times, handing back a successor each
    /// time, and fails on the application after that. Chaining is what lets a
    /// test place the unusable entry on the *undo* stack (depth 1) or on the
    /// *redo* stack (depth 2), which is the only way to reach either through
    /// the public API — and also how one reaches them in production, when
    /// `save_compacted` purges the trash a queued `RestorePage` needs.
    ///
    /// It never touches the document, so any observed mutation across a
    /// failed operation came from the stack, not from this command.
    struct FailsAfter {
        remaining: u8,
    }

    impl Command<VecDocument> for FailsAfter {
        fn apply(&self, _document: &mut VecDocument) -> Result<Box<dyn Command<VecDocument>>> {
            match self.remaining {
                0 => Err(Error::PageNotFound(PageId::new(9_999))),
                n => Ok(Box::new(FailsAfter { remaining: n - 1 })),
            }
        }

        fn label(&self) -> String {
            format!("Fails after {} more applications", self.remaining)
        }
    }

    #[test]
    fn undo_reverses_the_most_recent_apply() {
        let mut document = VecDocument::with_pages(2, PageSize::A4);
        let ids = document.page_ids();
        let before = DocumentSnapshot::of(&document).unwrap();
        let mut stack: UndoStack<VecDocument> = UndoStack::new();

        stack.apply(&mut document, Box::new(RemovePage { page: ids[0] })).unwrap();
        assert_eq!(document.page_count(), 1);
        assert!(stack.can_undo());

        stack.undo(&mut document).unwrap();
        assert_eq!(
            DocumentSnapshot::of(&document).unwrap().pages,
            before.pages,
            "undoing a removal is exact now, via RestorePage"
        );
    }

    #[test]
    fn undoing_with_nothing_applied_reports_no_op() {
        let mut document = VecDocument::with_pages(1, PageSize::A4);
        let mut stack: UndoStack<VecDocument> = UndoStack::new();

        let undid_something = stack.undo(&mut document).unwrap();

        assert!(!undid_something);
        assert!(!stack.can_undo());
    }

    #[test]
    fn redo_reapplies_an_undone_command() {
        let mut document = VecDocument::with_pages(2, PageSize::A4);
        let ids = document.page_ids();
        let mut stack: UndoStack<VecDocument> = UndoStack::new();

        stack.apply(&mut document, Box::new(RemovePage { page: ids[0] })).unwrap();
        stack.undo(&mut document).unwrap();
        assert_eq!(document.page_count(), 2);
        assert!(stack.can_redo());

        stack.redo(&mut document).unwrap();
        assert_eq!(document.page_count(), 1);
        assert!(!stack.can_redo());
    }

    #[test]
    fn applying_a_new_command_after_an_undo_clears_the_redo_stack() {
        let mut document = VecDocument::with_pages(3, PageSize::A4);
        let ids = document.page_ids();
        let mut stack: UndoStack<VecDocument> = UndoStack::new();

        stack.apply(&mut document, Box::new(RemovePage { page: ids[0] })).unwrap();
        stack.undo(&mut document).unwrap();
        assert!(stack.can_redo(), "there must be something to redo before the new apply");

        let remaining_ids = document.page_ids();
        stack.apply(&mut document, Box::new(RemovePage { page: remaining_ids[1] })).unwrap();

        assert!(!stack.can_redo(), "applying a new command after an undo must discard the redo branch");
    }

    /// The corruption this guards against is not the failed undo — it is the
    /// *next* one. Consuming the failing entry used to leave the stack one
    /// deep and silently apply an inverse computed for a document state that
    /// never came about, reporting success while producing the wrong page
    /// order. A repeatable error is the correct observable behaviour.
    #[test]
    fn a_failing_undo_keeps_its_entry_instead_of_silently_advancing_the_history() {
        let mut document = VecDocument::with_pages(3, PageSize::A4);
        let ids = document.page_ids();
        let mut stack: UndoStack<VecDocument> = UndoStack::new();

        stack.apply(&mut document, Box::new(RemovePage { page: ids[0] })).unwrap();
        stack.apply(&mut document, Box::new(FailsAfter { remaining: 1 })).unwrap();
        let after_apply = DocumentSnapshot::of(&document).unwrap();

        let first = stack.undo(&mut document);
        assert!(first.is_err(), "the top entry cannot apply, so undo must report the failure");
        assert!(stack.can_undo(), "a failed undo must not consume its entry");
        assert!(!stack.can_redo(), "a failed undo must not queue anything for redo");

        let second = stack.undo(&mut document);
        assert!(second.is_err(), "the same unusable entry must still be on top, not the entry beneath it");
        assert_eq!(
            DocumentSnapshot::of(&document).unwrap().pages,
            after_apply.pages,
            "no failed undo may mutate the document"
        );
    }

    /// The redo stack has the same hazard and needed the same fix, so it gets
    /// the same test. Depth 2 puts the unusable entry on the redo stack: the
    /// apply consumes one application, the successful undo consumes the next,
    /// and the entry it queues for redo is the one that fails.
    #[test]
    fn a_failing_redo_keeps_its_entry_instead_of_silently_advancing_the_history() {
        let mut document = VecDocument::with_pages(3, PageSize::A4);
        let ids = document.page_ids();
        let mut stack: UndoStack<VecDocument> = UndoStack::new();

        stack.apply(&mut document, Box::new(RemovePage { page: ids[0] })).unwrap();
        stack.apply(&mut document, Box::new(FailsAfter { remaining: 2 })).unwrap();
        stack.undo(&mut document).unwrap();
        assert!(stack.can_redo(), "the successful undo must have queued something to redo");
        let after_undo = DocumentSnapshot::of(&document).unwrap();

        let first = stack.redo(&mut document);
        assert!(first.is_err(), "the queued redo entry cannot apply, so redo must report the failure");
        assert!(stack.can_redo(), "a failed redo must not consume its entry");

        let second = stack.redo(&mut document);
        assert!(second.is_err(), "the same unusable entry must still be on top of the redo stack");
        assert_eq!(
            DocumentSnapshot::of(&document).unwrap().pages,
            after_undo.pages,
            "no failed redo may mutate the document"
        );
    }

    /// Stands in for the moment a caller invokes `clear()` after a
    /// successful `save_compacted`: both stacks must be non-empty going in
    /// (proving `clear` cannot be a no-op that merely looks correct because
    /// there was nothing queued), and `undo` afterward must be a
    /// well-defined no-op rather than surfacing the `Error::PageNotFound`
    /// a stale `RestorePage` would otherwise produce.
    #[test]
    fn clearing_empties_both_stacks_and_leaves_undo_a_well_defined_no_op() {
        let mut document = VecDocument::with_pages(3, PageSize::A4);
        let ids = document.page_ids();
        let mut stack: UndoStack<VecDocument> = UndoStack::new();

        stack.apply(&mut document, Box::new(RemovePage { page: ids[0] })).unwrap();
        stack.apply(&mut document, Box::new(RemovePage { page: ids[1] })).unwrap();
        stack.undo(&mut document).unwrap();
        assert!(stack.can_undo(), "one apply must remain queued on the undo stack before clear is exercised");
        assert!(stack.can_redo(), "the undone apply must be queued on the redo stack before clear is exercised");

        stack.clear();

        assert!(!stack.can_undo(), "clear must drop the undo stack");
        assert!(!stack.can_redo(), "clear must drop the redo stack too, not only the undo stack");

        let undid_something = stack.undo(&mut document).unwrap();
        assert!(!undid_something, "undo on a cleared stack must be a well-defined no-op, never an error");
    }
}
