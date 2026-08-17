//! Continuous-scroll layout maths, in PDF points.
//!
//! The layout is computed once per document snapshot and is **independent of
//! zoom**: gaps and margins are points like the pages themselves, so a zoom change
//! is a single multiplication rather than a recomputation, and the scroll extent
//! for a ten-thousand-page document is known before a single pixel is rasterized.
//!
//! Nothing here refers to egui. That is deliberate: this is the part of the
//! viewer that can be tested in headless CI, so it is kept free of anything that
//! needs a display.

use std::ops::Range;

use opdf_core::document::DocumentSnapshot;
use opdf_core::page::PageId;

/// Where one page sits in the document's content box, in PDF points.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct PagePlacement {
    /// Identity of the placed page.
    pub id: PageId,
    /// Zero-based position in document order, for page numbering and rail sync.
    pub index: usize,
    /// Distance from the content box's left edge to the page's left edge.
    pub left_pt: f32,
    /// Distance from the content box's top edge to the page's top edge.
    pub top_pt: f32,
    /// Page width **after** its stored rotation, from `PageInfo::display_size`.
    pub width_pt: f32,
    /// Page height **after** its stored rotation, from `PageInfo::display_size`.
    pub height_pt: f32,
}

impl PagePlacement {
    /// Distance from the content box's top edge to the page's bottom edge.
    pub fn bottom_pt(&self) -> f32 {
        self.top_pt + self.height_pt
    }

    /// Distance from the content box's left edge to the page's right edge.
    pub fn right_pt(&self) -> f32 {
        self.left_pt + self.width_pt
    }
}

/// Every page's placement, plus the extent of the box that contains them all.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct DocumentLayout {
    /// Placements in document order. Tops are strictly increasing, which
    /// [`find_visible_pages`] relies on for its binary search.
    pub placements: Vec<PagePlacement>,
    /// Width of the content box: the widest page plus a margin on each side.
    pub content_width_pt: f32,
    /// Height of the content box: every page, every gap, and a margin top and bottom.
    pub content_height_pt: f32,
}

impl DocumentLayout {
    /// Whether the layout contains no pages.
    pub fn is_empty(&self) -> bool {
        self.placements.is_empty()
    }

    /// The placement at a document position, or `None` if the index is past the end.
    pub fn placement(&self, index: usize) -> Option<&PagePlacement> {
        self.placements.get(index)
    }
}

//---------------------------------------------------------------------
// Building a layout from a snapshot
//---------------------------------------------------------------------

/// Stack every page of `snapshot` vertically, centred, with `gap_pt` between
/// consecutive pages and `margin_pt` above the first and below the last.
///
/// Page extents come from [`opdf_core::page::PageInfo::display_size`], which has
/// already applied each page's stored rotation — so a quarter-turned A4 lays out
/// as 842 by 595, not as 595 by 842.
///
/// An empty snapshot yields an empty layout with zero extent, not a layout with
/// two margins and nothing between them.
pub fn compute_document_layout(snapshot: &DocumentSnapshot, gap_pt: f32, margin_pt: f32) -> DocumentLayout {
    if snapshot.pages.is_empty() {
        return DocumentLayout::default();
    }

    let widest_pt = snapshot.pages.iter().map(|page| page.display_size().width_pt).fold(0.0_f32, f32::max);
    let content_width_pt = widest_pt + 2.0 * margin_pt;

    let mut placements = Vec::with_capacity(snapshot.pages.len());
    let mut cursor_pt = margin_pt;
    for (index, page) in snapshot.pages.iter().enumerate() {
        let size = page.display_size();
        placements.push(PagePlacement {
            id: page.id,
            index,
            left_pt: (content_width_pt - size.width_pt) * 0.5,
            top_pt: cursor_pt,
            width_pt: size.width_pt,
            height_pt: size.height_pt,
        });
        cursor_pt += size.height_pt + gap_pt;
    }

    DocumentLayout {
        placements,
        content_width_pt,
        content_height_pt: cursor_pt - gap_pt + margin_pt,
    }
}

//---------------------------------------------------------------------
// Viewport queries
//---------------------------------------------------------------------

/// The half-open range of page indices intersecting the viewport, widened by
/// `overscan_pt` on each side.
///
/// Two binary searches rather than a scan, because this runs every frame and a
/// linear pass over a ten-thousand-page document is a frame-time cost that grows
/// with the document. Correctness rests on `placements` being sorted by both
/// `top_pt` and `bottom_pt()`, which [`compute_document_layout`] guarantees since
/// pages never overlap.
///
/// The returned range is always valid to slice with, and is empty when nothing
/// intersects.
pub fn find_visible_pages(layout: &DocumentLayout, top_pt: f32, height_pt: f32, overscan_pt: f32) -> Range<usize> {
    let low_pt = top_pt - overscan_pt;
    let high_pt = top_pt + height_pt + overscan_pt;
    let first = layout.placements.partition_point(|placement| placement.bottom_pt() < low_pt);
    let last = layout.placements.partition_point(|placement| placement.top_pt <= high_pt);
    first..last.max(first)
}

/// The page the user would call "the page I am on": the one covering the most of
/// the viewport, breaking ties toward the earlier page.
///
/// Returns `None` for an empty layout.
pub fn find_current_page(layout: &DocumentLayout, top_pt: f32, height_pt: f32) -> Option<usize> {
    if layout.is_empty() {
        return None;
    }
    let visible = find_visible_pages(layout, top_pt, height_pt, 0.0);
    let bottom_pt = top_pt + height_pt;
    let mut best: Option<(usize, f32)> = None;
    for placement in layout.placements.get(visible)? {
        let covered_pt = placement.bottom_pt().min(bottom_pt) - placement.top_pt.max(top_pt);
        if covered_pt <= 0.0 {
            continue;
        }
        if best.is_none_or(|(_, best_covered_pt)| covered_pt > best_covered_pt) {
            best = Some((placement.index, covered_pt));
        }
    }
    //--- nothing intersects when the viewport sits in a gap or past the end: fall back to the nearest page ---
    best.map(|(index, _)| index).or_else(|| {
        let nearest = layout.placements.partition_point(|placement| placement.bottom_pt() < top_pt);
        Some(nearest.min(layout.placements.len().saturating_sub(1)))
    })
}

/// The scroll offset in points that puts page `index` at the top of the viewport,
/// with `margin_pt` of breathing room above it.
///
/// Returns `None` if the index is past the end of the document.
pub fn find_scroll_target(layout: &DocumentLayout, index: usize, margin_pt: f32) -> Option<f32> {
    layout.placement(index).map(|placement| (placement.top_pt - margin_pt).max(0.0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::synthetic::build_synthetic_snapshot;
    use opdf_core::page::{PageInfo, PageSize, Rotation};

    fn build_snapshot(pages: &[(f32, f32, Rotation)]) -> DocumentSnapshot {
        DocumentSnapshot {
            document: opdf_core::document::DocumentId::new_unique(),
            pages: pages
                .iter()
                .enumerate()
                .map(|(index, (width_pt, height_pt, rotation))| PageInfo {
                    id: PageId::new(index as u64),
                    size: PageSize::new(*width_pt, *height_pt),
                    rotation: *rotation,
                })
                .collect(),
            revision: 1,
        }
    }

    #[test]
    fn stacks_pages_in_order_without_overlap() {
        let snapshot = build_synthetic_snapshot(30).unwrap();
        let layout = compute_document_layout(&snapshot, 14.0, 20.0);
        assert_eq!(layout.placements.len(), 30);
        for pair in layout.placements.windows(2) {
            assert_eq!(pair[1].index, pair[0].index + 1);
            assert!(pair[0].bottom_pt() < pair[1].top_pt, "page {} overlaps page {}", pair[0].index, pair[1].index);
        }
    }

    #[test]
    fn separates_consecutive_pages_by_exactly_the_gap() {
        let snapshot = build_snapshot(&[(100.0, 200.0, Rotation::None), (100.0, 300.0, Rotation::None)]);
        let layout = compute_document_layout(&snapshot, 14.0, 20.0);
        assert!((layout.placements[1].top_pt - layout.placements[0].bottom_pt() - 14.0).abs() < 1e-4);
    }

    #[test]
    fn sizes_the_content_box_from_the_widest_page_and_the_full_stack() {
        let snapshot = build_snapshot(&[(100.0, 200.0, Rotation::None), (400.0, 300.0, Rotation::None)]);
        let layout = compute_document_layout(&snapshot, 10.0, 20.0);
        assert!((layout.content_width_pt - 440.0).abs() < 1e-4, "widest page plus a margin each side");
        assert!((layout.content_height_pt - 550.0).abs() < 1e-4, "20 + 200 + 10 + 300 + 20");
    }

    #[test]
    fn centres_a_narrow_page_in_the_content_box() {
        let snapshot = build_snapshot(&[(100.0, 200.0, Rotation::None), (400.0, 300.0, Rotation::None)]);
        let layout = compute_document_layout(&snapshot, 10.0, 20.0);
        let narrow = layout.placements[0];
        assert!((narrow.left_pt - (layout.content_width_pt - narrow.width_pt) * 0.5).abs() < 1e-4);
        assert!(
            (narrow.left_pt + narrow.right_pt() - layout.content_width_pt).abs() < 1e-3,
            "left and right margins must match"
        );
    }

    #[test]
    fn lays_a_quarter_turned_page_out_at_its_rotated_extent() {
        let snapshot = build_snapshot(&[(595.0, 842.0, Rotation::Quarter)]);
        let layout = compute_document_layout(&snapshot, 10.0, 20.0);
        assert!((layout.placements[0].width_pt - 842.0).abs() < 1e-4, "a quarter turn must widen the page");
        assert!((layout.placements[0].height_pt - 595.0).abs() < 1e-4, "a quarter turn must shorten the page");
    }

    #[test]
    fn produces_a_zero_extent_layout_for_an_empty_document() {
        let layout = compute_document_layout(&DocumentSnapshot::default(), 14.0, 20.0);
        assert!(layout.is_empty());
        assert_eq!(layout.content_height_pt, 0.0, "an empty document must not claim two margins of scroll extent");
    }

    #[test]
    fn selects_only_the_pages_the_viewport_touches() {
        let snapshot = build_snapshot(&[(100.0, 100.0, Rotation::None); 10]);
        let layout = compute_document_layout(&snapshot, 10.0, 0.0);
        //--- pages occupy 0..100, 110..210, 220..320, 330..430, ... ---
        let visible = find_visible_pages(&layout, 115.0, 100.0, 0.0);
        assert_eq!(visible, 1..2, "a viewport spanning 115..215 touches page 1 only: page 2 starts at 220");
    }

    #[test]
    fn widens_the_selection_by_the_overscan_band() {
        let snapshot = build_snapshot(&[(100.0, 100.0, Rotation::None); 10]);
        let layout = compute_document_layout(&snapshot, 10.0, 0.0);
        let tight = find_visible_pages(&layout, 115.0, 100.0, 0.0);
        let widened = find_visible_pages(&layout, 115.0, 100.0, 120.0);
        assert!(
            widened.start < tight.start && widened.end > tight.end,
            "overscan must prefetch on both sides, got {widened:?} against {tight:?}"
        );
    }

    #[test]
    fn returns_an_empty_range_past_the_end_of_the_document() {
        let snapshot = build_snapshot(&[(100.0, 100.0, Rotation::None); 3]);
        let layout = compute_document_layout(&snapshot, 10.0, 0.0);
        let visible = find_visible_pages(&layout, 10_000.0, 100.0, 0.0);
        assert!(visible.is_empty(), "scrolling past the end must select nothing, not panic or wrap");
        assert!(layout.placements.get(visible).is_some(), "the returned range must always be safe to slice with");
    }

    #[test]
    fn returns_an_empty_range_for_an_empty_document() {
        let layout = compute_document_layout(&DocumentSnapshot::default(), 14.0, 20.0);
        assert!(find_visible_pages(&layout, 0.0, 800.0, 200.0).is_empty());
        assert_eq!(find_current_page(&layout, 0.0, 800.0), None);
    }

    #[test]
    fn reports_the_page_covering_most_of_the_viewport() {
        let snapshot = build_snapshot(&[(100.0, 100.0, Rotation::None); 5]);
        let layout = compute_document_layout(&snapshot, 10.0, 0.0);
        //--- viewport 90..190: 10pt of page 0, 80pt of page 1 ---
        assert_eq!(find_current_page(&layout, 90.0, 100.0), Some(1));
        //--- viewport 20..120: 80pt of page 0, 10pt of page 1 ---
        assert_eq!(find_current_page(&layout, 20.0, 100.0), Some(0));
    }

    #[test]
    fn reports_a_page_even_when_the_viewport_sits_entirely_in_a_gap() {
        let snapshot = build_snapshot(&[(100.0, 100.0, Rotation::None); 5]);
        let layout = compute_document_layout(&snapshot, 40.0, 0.0);
        let current = find_current_page(&layout, 105.0, 20.0);
        assert!(current.is_some(), "a gap must not blank the page indicator");
    }

    #[test]
    fn scrolls_a_page_to_the_top_with_a_margin_above_it() {
        let snapshot = build_snapshot(&[(100.0, 100.0, Rotation::None); 5]);
        let layout = compute_document_layout(&snapshot, 10.0, 0.0);
        assert_eq!(find_scroll_target(&layout, 2, 8.0), Some(212.0));
        assert_eq!(
            find_scroll_target(&layout, 0, 8.0),
            Some(0.0),
            "the first page must not scroll to a negative offset"
        );
        assert_eq!(find_scroll_target(&layout, 99, 8.0), None);
    }
}
