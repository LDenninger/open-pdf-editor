//! The mutation contract: every change to a document is an invertible command.

use crate::Result;
use crate::document::Document;

/// A single invertible mutation.
///
/// Applying a command returns the command that undoes it. The inverse is
/// produced at apply time because it usually depends on state the command
/// replaced, such as a page's previous rotation.
///
/// # Why `Send`
///
/// The render worker thread owns the document, so the UI thread cannot hold a
/// [`Document`] at all — it holds a [`crate::DocumentSnapshot`] and sends
/// commands across a channel to the thread that can apply them. Both the
/// command and the inverse it produces therefore cross threads, and the undo
/// stack that stores those inverses may be owned by either side. `Send` is a
/// supertrait rather than a bound added at each use site so that
/// `Box<dyn Command<D>>` is sendable everywhere it appears, including as the
/// return type of [`Command::apply`].
///
/// # Why `D: ?Sized`
///
/// A consumer that cannot name the concrete document type holds a
/// `Box<dyn Document>` — the shell does, because its opener hands it one. With
/// the implicit `Sized` bound, `Command<dyn Document>` and any undo stack built
/// on it were unnameable, so such a consumer could hold the document but no
/// history over it.
///
/// Relaxing the bound is additive: every `Command<ConcreteDocument>` that
/// compiled before still does, because a sized type satisfies `?Sized`. A
/// command that genuinely needs a sized document — one owning a `Vec<D>`, or
/// calling [`Document::import_pages`], whose `&Self` source is itself only
/// available on a sized type — simply keeps `D: Document` in its own `impl`.
pub trait Command<D: Document + ?Sized>: Send {
    /// Apply this change, returning the command that reverses it.
    ///
    /// On failure the document must be left exactly as it was found.
    fn apply(&self, document: &mut D) -> Result<Box<dyn Command<D>>>;

    /// Short description for the undo menu, in sentence case: `"Rotate page 3"`.
    fn label(&self) -> String;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::DocumentSnapshot;
    use crate::fakes::VecDocument;
    use crate::page::{PageId, Rotation};

    /// Minimal command proving the trait can express an invertible change.
    struct SetRotation {
        page: PageId,
        rotation: Rotation,
    }

    impl Command<VecDocument> for SetRotation {
        fn apply(&self, document: &mut VecDocument) -> Result<Box<dyn Command<VecDocument>>> {
            let previous = document.page(self.page)?.rotation;
            document.set_rotation(self.page, self.rotation)?;
            Ok(Box::new(SetRotation {
                page: self.page,
                rotation: previous,
            }))
        }

        fn label(&self) -> String {
            format!("Rotate {} to {} degrees", self.page, self.rotation.degrees())
        }
    }

    /// Compare whole page lists rather than a single field: a lossy inverse that
    /// restored the rotation but disturbed page order or identity would pass a
    /// one-field check. Every command's inverse test should follow this shape.
    ///
    /// The comparison is on `pages`, not on the whole snapshot, because undo is
    /// itself a mutation: it advances [`Document::revision`] like any other, so
    /// the restored document is structurally identical to the original but never
    /// reports the original's revision. That is deliberate — a revision that went
    /// backwards would let a tile cache serve entries it had already invalidated.
    #[test]
    fn applying_the_returned_inverse_restores_the_original_state() {
        let mut document = VecDocument::with_pages(3, crate::page::PageSize::A4);
        let page = document.page_ids()[0];
        let before = DocumentSnapshot::of(&document).unwrap();

        let command = SetRotation {
            page,
            rotation: Rotation::Quarter,
        };
        let inverse = command.apply(&mut document).unwrap();
        assert_eq!(document.page(page).unwrap().rotation, Rotation::Quarter);
        assert_ne!(
            DocumentSnapshot::of(&document).unwrap().pages,
            before.pages,
            "applying the command must actually change the document"
        );

        inverse.apply(&mut document).unwrap();
        let restored = DocumentSnapshot::of(&document).unwrap();
        assert_eq!(
            restored.pages, before.pages,
            "the inverse must restore the whole page list, not merely the field the command touched"
        );
        assert_ne!(
            restored.revision, before.revision,
            "undo is a mutation: it must advance the revision rather than rewind it, so a cache cannot resurrect tiles it invalidated"
        );
    }

    #[test]
    fn boxed_commands_cross_thread_boundaries() {
        fn assert_send<T: Send>(_value: &T) {}

        let boxed: Box<dyn Command<VecDocument>> = Box::new(SetRotation {
            page: PageId::new(0),
            rotation: Rotation::Half,
        });
        assert_send(&boxed);
    }

    #[test]
    fn labels_describe_the_change_for_the_undo_menu() {
        let command = SetRotation {
            page: PageId::new(3),
            rotation: Rotation::Half,
        };
        assert_eq!(command.label(), "Rotate page#3 to 180 degrees");
    }
}
