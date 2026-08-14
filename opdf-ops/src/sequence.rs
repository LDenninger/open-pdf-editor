//! A composite command that applies several commands atomically.

use opdf_core::{Command, Document, Result};

/// A command built from an ordered list of sub-commands.
///
/// Applying a [`Sequence`] applies each sub-command in order. If any
/// sub-command fails, every sub-command applied so far is rolled back — by
/// applying the inverses already collected, most recently applied first —
/// before the original error is returned, so a partially-applied composite
/// never leaves the document changed. The returned inverse is itself a
/// [`Sequence`] of the collected sub-inverses, in reverse order, so undoing
/// a composite command is atomic the same way applying it is.
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
                    //--- roll back every step already applied, most recent first ---
                    //--- rollback commands are inverses of steps that just succeeded against this exact document state, so they are expected to succeed; there is no better recovery available at this layer if one does not ---
                    for rollback in applied.into_iter().rev() {
                        let _ = rollback.apply(document);
                    }
                    return Err(error);
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
    use opdf_core::{Error, PageId};

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
