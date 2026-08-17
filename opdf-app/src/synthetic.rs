//! A synthetic document, because no real PDF exists yet.
//!
//! Track A owns real document loading. Until it lands, the shell needs something
//! to lay out, and a document of uniform A4 portrait pages hides exactly the bugs
//! that matter: a layout that assumes one page width, a renderer request that
//! forgets a page's stored rotation, a scrollbar extent computed from a page count
//! rather than from real heights. The generator therefore cycles deliberately
//! awkward sizes and every rotation.
//!
//! This module produces a [`VecDocument`], which the caller immediately snapshots
//! and hands to a render service. The shell itself never holds the document — see
//! the crate documentation.

use opdf_core::Result;
use opdf_core::document::{Document, DocumentSnapshot};
use opdf_core::fakes::{FakeRenderService, VecDocument};
use opdf_core::page::{PageSize, Rotation};

use crate::opener::OpenedDocument;

/// Page sizes the generator cycles through, in points.
///
/// A4 and Letter are the common cases; the wide, tall, and tiny entries exist to
/// break a layout that assumes a single column width or a minimum page height.
pub const SYNTHETIC_SIZES: [PageSize; 5] = [
    PageSize::A4,
    PageSize::LETTER,
    PageSize::new(1224.0, 792.0),
    PageSize::new(420.0, 1200.0),
    PageSize::new(288.0, 288.0),
];

/// Rotations the generator cycles through, so that quarter turns — which swap a
/// page's display width and height — appear early and often.
pub const SYNTHETIC_ROTATIONS: [Rotation; 4] = [Rotation::None, Rotation::Quarter, Rotation::Half, Rotation::ThreeQuarter];

/// Build an in-memory document of `page_count` pages with varied sizes and rotations.
///
/// The size and rotation cycles have coprime lengths (5 and 4), so the combination
/// does not repeat until page 20 — a document of 40 pages exercises every pairing
/// twice.
///
/// Returns an error only if the underlying document rejects an append, which
/// [`VecDocument`] does not; the `Result` exists so callers do not have to unwrap.
pub fn build_synthetic_document(page_count: usize) -> Result<VecDocument> {
    let mut document = VecDocument::new();
    for ii in 0..page_count {
        let id = document.insert_page(ii, SYNTHETIC_SIZES[ii % SYNTHETIC_SIZES.len()])?;
        document.set_rotation(id, SYNTHETIC_ROTATIONS[ii % SYNTHETIC_ROTATIONS.len()])?;
    }
    Ok(document)
}

/// Build a synthetic document and immediately snapshot it.
///
/// This is what the shell actually calls: it never keeps the [`VecDocument`],
/// only the snapshot, which is the shape the real wiring will take once a render
/// worker owns the document.
pub fn build_synthetic_snapshot(page_count: usize) -> Result<DocumentSnapshot> {
    let document = build_synthetic_document(page_count)?;
    DocumentSnapshot::of(&document)
}

/// Build a synthetic document in the shape the shell accepts: the document, a
/// service built from the same snapshot, and that snapshot.
///
/// The shell takes a [`crate::opener::OpenedDocument`] however the document was
/// produced, so the synthetic path is one more producer rather than a second
/// route into the shell.
pub fn open_synthetic_document(page_count: usize) -> Result<OpenedDocument> {
    let document = build_synthetic_document(page_count)?;
    let snapshot = DocumentSnapshot::of(&document)?;
    let service = Box::new(FakeRenderService::new(snapshot.clone()));
    Ok(OpenedDocument {
        document: Box::new(document),
        service,
        snapshot,
        //--- generated, not opened: there is nowhere for Save to write it back to ---
        path: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn produces_the_requested_page_count_in_order() {
        let snapshot = build_synthetic_snapshot(40).unwrap();
        assert_eq!(snapshot.page_count(), 40);
        let unique: HashSet<_> = snapshot.pages.iter().map(|page| page.id).collect();
        assert_eq!(unique.len(), 40, "every page must have a distinct identity");
    }

    #[test]
    fn varies_display_size_so_layout_bugs_surface() {
        let snapshot = build_synthetic_snapshot(20).unwrap();
        let widths: HashSet<u32> = snapshot.pages.iter().map(|page| page.display_size().width_pt.to_bits()).collect();
        assert!(
            widths.len() >= 5,
            "a generator that yields fewer than five distinct display widths hides column-width bugs, got {}",
            widths.len()
        );
    }

    #[test]
    fn includes_every_rotation_within_the_first_four_pages() {
        let snapshot = build_synthetic_snapshot(4).unwrap();
        let rotations: HashSet<Rotation> = snapshot.pages.iter().map(|page| page.rotation).collect();
        assert_eq!(
            rotations.len(),
            4,
            "all four rotations must appear immediately, not only deep in a long document"
        );
    }

    #[test]
    fn swaps_axes_on_quarter_turned_pages() {
        let snapshot = build_synthetic_snapshot(2).unwrap();
        let quarter_turned = snapshot.pages[1];
        assert_eq!(quarter_turned.rotation, Rotation::Quarter);
        assert_eq!(
            quarter_turned.display_size(),
            PageSize::new(quarter_turned.size.height_pt, quarter_turned.size.width_pt),
            "display_size must report the rotated extent; layout depends on it"
        );
    }

    #[test]
    fn produces_an_empty_snapshot_for_zero_pages() {
        let snapshot = build_synthetic_snapshot(0).unwrap();
        assert_eq!(
            snapshot.page_count(),
            0,
            "an empty document is a case the canvas must survive, so the generator must be able to produce one"
        );
    }
}
