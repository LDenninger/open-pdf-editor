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
pub trait Command<D: Document>: Send {
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

    /// Compare whole snapshots rather than a single field: a lossy inverse that
    /// restored the rotation but disturbed page order or identity would pass a
    /// one-field check. Every command's inverse test should follow this shape.
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
            DocumentSnapshot::of(&document).unwrap(),
            before,
            "applying the command must actually change the document"
        );

        inverse.apply(&mut document).unwrap();
        assert_eq!(
            DocumentSnapshot::of(&document).unwrap(),
            before,
            "the inverse must restore the whole document, not merely the field the command touched"
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
