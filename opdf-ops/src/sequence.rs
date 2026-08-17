//! A composite command that applies several commands atomically.

use opdf_core::{Command, Document, Error, Result};

//---------------------------------------------------------------------
// Rollback
//---------------------------------------------------------------------

/// Undo `applied`, most recently applied first, and produce the error to
/// return to the caller.
///
/// Shared with [`crate::Merge`], which collects its own inverses as it goes
/// and needs exactly this behaviour on failure.
///
/// When every rollback step succeeds the caller gets `original` back
/// unchanged: the composite failed, and the document is as it was. When a
/// rollback step *fails*, the composite is stuck part-applied, and two things
/// happen. Rollback stops rather than continuing — every remaining inverse was
/// computed against a document state that has now not come about, so applying
/// them would compound the damage in exactly the way `UndoStack` refuses to.
/// And the returned error names both failures, because the rollback failure is
/// the one that tells the caller the document is no longer what it was, and
/// the original failure is the one that says why.
///
/// The composed error is [`Error::Unsupported`]: `opdf_core::Error` has no
/// variant for a compound failure, and adding one is a breaking change to a
/// contract this crate does not own.
pub(crate) fn roll_back<D: Document>(document: &mut D, applied: Vec<Box<dyn Command<D>>>, label: &str, original: Error) -> Error {
    for rollback in applied.into_iter().rev() {
        if let Err(rollback_error) = rollback.apply(document) {
            return Error::Unsupported(format!(
                "'{label}' failed and could not be rolled back, so the document is left partially modified: \
                 the step that failed reported '{original}', and undoing '{}' then reported '{rollback_error}'",
                rollback.label()
            ));
        }
    }
    original
}

//---------------------------------------------------------------------
// The composite command
//---------------------------------------------------------------------

/// A command built from an ordered list of sub-commands.
///
/// Applying a [`Sequence`] applies each sub-command in order. If any
/// sub-command fails, every sub-command applied so far is rolled back — by
/// applying the inverses already collected, most recently applied first —
/// before the original error is returned. The returned inverse is itself a
/// [`Sequence`] of the collected sub-inverses, in reverse order, so undoing
/// a composite command is atomic the same way applying it is.
///
/// # The one case where atomicity does not hold
///
/// Rollback is itself a sequence of applications, and an application can fail.
/// If one does, the document is left part-applied and no recovery is available
/// at this layer — the sub-command that failed has already been asked to undo
/// itself and refused. `Sequence` therefore does not promise atomicity
/// unconditionally; it promises that a failed rollback is *reported* rather
/// than swallowed. The error returned in that case is an
/// [`Error::Unsupported`] naming both the original failure and the rollback
/// failure, and it is the caller's signal that the document must be reloaded
/// rather than edited further.
///
/// This is reachable only for a [`Document`] whose mutations can fail for
/// reasons other than a bad argument — `opdf_core::fakes::VecDocument` cannot
/// reach it, a document backed by a real file can.
pub struct Sequence<D: Document> {
    label: String,
    commands: Vec<Box<dyn Command<D>>>,
}

impl<D: Document + 'static> Sequence<D> {
    /// Build a sequence from an ordered list of sub-commands.
    pub fn new(label: String, commands: Vec<Box<dyn Command<D>>>) -> Self {
        Self { label, commands }
    }
}

impl<D: Document + 'static> Command<D> for Sequence<D> {
    fn apply(&self, document: &mut D) -> Result<Box<dyn Command<D>>> {
        let mut applied: Vec<Box<dyn Command<D>>> = Vec::with_capacity(self.commands.len());
        for command in &self.commands {
            match command.apply(document) {
                Ok(inverse) => applied.push(inverse),
                Err(error) => {
                    //--- roll back every step already applied, most recent first, and report a rollback that itself fails ---
                    return Err(roll_back(document, applied, &self.label, error));
                }
            }
        }
        applied.reverse();
        Ok(Box::new(Sequence::new(format!("Undo: {}", self.label), applied)))
    }

    fn label(&self) -> String {
        self.label.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use opdf_core::DocumentSnapshot;
    use opdf_core::fakes::VecDocument;
    use opdf_core::page::PageSize;
    use opdf_core::{Error, PageId, Rotation};

    use crate::remove_page::RemovePage;

    /// A command that always fails, used to force `Sequence` down its
    /// rollback path by construction rather than by inspecting the code.
    struct AlwaysFails;

    impl<D: Document> Command<D> for AlwaysFails {
        fn apply(&self, _document: &mut D) -> Result<Box<dyn Command<D>>> {
            Err(Error::Unsupported("deliberately fails".to_string()))
        }

        fn label(&self) -> String {
            "Always fails".to_string()
        }
    }

    fn boxed_remove(page: PageId) -> Box<dyn Command<VecDocument>> {
        Box::new(RemovePage { page })
    }

    #[test]
    fn applies_every_sub_command_in_order() {
        let mut document = VecDocument::with_pages(3, PageSize::A4);
        let ids = document.page_ids();
        let commands: Vec<Box<dyn Command<VecDocument>>> = vec![boxed_remove(ids[0]), boxed_remove(ids[2])];
        let sequence = Sequence::new("Remove two pages".to_string(), commands);

        sequence.apply(&mut document).unwrap();

        assert_eq!(document.page_ids(), vec![ids[1]]);
    }

    #[test]
    fn a_failing_step_rolls_back_every_step_already_applied() {
        let mut document = VecDocument::with_pages(3, PageSize::A4);
        let ids = document.page_ids();
        let before = DocumentSnapshot::of(&document).unwrap();
        let commands: Vec<Box<dyn Command<VecDocument>>> = vec![boxed_remove(ids[0]), boxed_remove(ids[1]), Box::new(AlwaysFails)];
        let sequence = Sequence::new("Remove then fail".to_string(), commands);

        let result = sequence.apply(&mut document);

        assert!(result.is_err(), "the third step's failure must surface as an error");
        assert_eq!(
            DocumentSnapshot::of(&document).unwrap().pages,
            before.pages,
            "rollback applies RemovePage's inverse, RestorePage, which is now exact — the document must come back identical, ids included"
        );
    }

    /// A sub-command whose `apply` succeeds but whose *inverse* fails. It is
    /// the shape `opdf-pdf`'s real `Document` can take and `VecDocument`
    /// cannot: the forward step lands, so `Sequence` collects the inverse and
    /// keeps going, and only the rollback discovers the inverse is unusable.
    struct SucceedsWithAFailingInverse;

    impl Command<VecDocument> for SucceedsWithAFailingInverse {
        fn apply(&self, document: &mut VecDocument) -> Result<Box<dyn Command<VecDocument>>> {
            //--- a real mutation, so a swallowed rollback leaves an observable change ---
            let page = document.page_ids()[0];
            document.set_rotation(page, Rotation::Half)?;
            Ok(Box::new(AlwaysFails))
        }

        fn label(&self) -> String {
            "Succeeds with a failing inverse".to_string()
        }
    }

    /// F15 reproduction. `let _ = rollback.apply(document)` discards the
    /// rollback's error, so the caller is told only that the sequence failed
    /// while the document is left mutated — the exact opposite of the
    /// unconditional atomicity the type's own documentation promised.
    #[test]
    fn a_failing_rollback_is_reported_rather_than_discarded() {
        let mut document = VecDocument::with_pages(3, PageSize::A4);
        let before = DocumentSnapshot::of(&document).unwrap();
        let commands: Vec<Box<dyn Command<VecDocument>>> = vec![Box::new(SucceedsWithAFailingInverse), Box::new(AlwaysFails)];
        let sequence = Sequence::new("Mutate then fail".to_string(), commands);

        let Err(error) = sequence.apply(&mut document) else {
            panic!("the second step's failure must surface as an error");
        };

        assert_ne!(
            DocumentSnapshot::of(&document).unwrap().pages,
            before.pages,
            "this test is only meaningful while the rollback genuinely cannot restore the document"
        );
        let message = error.to_string();
        assert!(
            message.contains("rolled back"),
            "the caller must be told the rollback failed and the document is left modified, not merely that a step failed: {message}"
        );
        assert!(
            message.contains("deliberately fails"),
            "the discarded rollback error must be surfaced, not swallowed: {message}"
        );
    }

    #[test]
    fn the_returned_inverse_undoes_the_whole_sequence() {
        let mut document = VecDocument::with_pages(3, PageSize::A4);
        let ids = document.page_ids();
        let before = DocumentSnapshot::of(&document).unwrap();
        let commands: Vec<Box<dyn Command<VecDocument>>> = vec![boxed_remove(ids[0]), boxed_remove(ids[1])];
        let sequence = Sequence::new("Remove two pages".to_string(), commands);

        let inverse = sequence.apply(&mut document).unwrap();
        assert_eq!(document.page_count(), 1);

        inverse.apply(&mut document).unwrap();
        assert_eq!(
            DocumentSnapshot::of(&document).unwrap().pages,
            before.pages,
            "undoing a sequence must restore every page the sequence removed, under its original identity"
        );
    }
}
