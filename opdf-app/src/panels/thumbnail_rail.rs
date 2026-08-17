//! The collapsible left rail: one small rendered thumbnail per page, clickable to
//! jump.
//!
//! Reordering by dragging a thumbnail is **not** implemented here. That is
//! integration checkpoint I3 and requires Track C's page-operation commands and
//! undo stack. Each thumbnail is allocated and sensed individually so that a drag
//! layer can be added on top without rewriting the rail.

use egui::{Rect, Sense, Vec2, pos2};
use opdf_core::document::DocumentSnapshot;
use opdf_core::render::{RenderRequest, RenderService};

use crate::theme::Theme;
use crate::tiles::TextureCache;
use crate::viewer::ViewerState;

/// Target width for a thumbnail, in PDF points at scale 1.0 — that is, the width
/// in screen points the thumbnail image occupies.
pub const THUMBNAIL_WIDTH_PT: f32 = 132.0;

/// Grid the per-page thumbnail scale is rounded onto.
///
/// Each page computes its own scale from its own width, so the scales are not on
/// a shared ladder. Rounding to 1/512 makes the value bit-stable across frames,
/// which matters because `RenderRequest` hashes `scale` bitwise.
pub const THUMBNAIL_SCALE_GRID: f32 = 512.0;

/// Vertical space reserved under each thumbnail for its page number.
pub const THUMBNAIL_LABEL_HEIGHT: f32 = 16.0;

/// Where one thumbnail sits in the rail's scrolling content, in screen points.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct ThumbnailSlot {
    /// Zero-based document position of the page shown.
    pub index: usize,
    /// Distance from the top of the rail's content to the top of the image.
    pub top_px: f32,
    /// Image width in screen points.
    pub width_px: f32,
    /// Image height in screen points.
    pub height_px: f32,
}

/// The scale a page of `display_width_pt` must be rendered at to fill
/// [`THUMBNAIL_WIDTH_PT`], rounded onto a stable grid.
pub fn compute_thumbnail_scale(display_width_pt: f32) -> f32 {
    if !display_width_pt.is_finite() || display_width_pt <= 0.0 {
        return crate::zoom::quantize_scale(1.0, THUMBNAIL_SCALE_GRID);
    }
    crate::zoom::quantize_scale(THUMBNAIL_WIDTH_PT / display_width_pt, THUMBNAIL_SCALE_GRID)
}

/// Stack every page's thumbnail vertically, each at the target width and its own
/// aspect ratio, with `spacing_px` and a label strip between them.
pub fn lay_out_thumbnails(snapshot: &DocumentSnapshot, spacing_px: f32) -> Vec<ThumbnailSlot> {
    let mut slots = Vec::with_capacity(snapshot.pages.len());
    let mut cursor_px = spacing_px;
    for (index, page) in snapshot.pages.iter().enumerate() {
        let size = page.display_size();
        let height_px = if size.width_pt > 0.0 {
            THUMBNAIL_WIDTH_PT * size.height_pt / size.width_pt
        } else {
            THUMBNAIL_WIDTH_PT
        };
        slots.push(ThumbnailSlot {
            index,
            top_px: cursor_px,
            width_px: THUMBNAIL_WIDTH_PT,
            height_px,
        });
        cursor_px += height_px + THUMBNAIL_LABEL_HEIGHT + spacing_px;
    }
    slots
}

/// The half-open range of thumbnails intersecting a scrolled viewport.
///
/// Virtualising the rail is what keeps a thousand-page document from allocating a
/// thousand widgets and a thousand thumbnail requests on the first frame.
pub fn find_visible_thumbnails(slots: &[ThumbnailSlot], top_px: f32, height_px: f32) -> std::ops::Range<usize> {
    let bottom_px = top_px + height_px;
    let first = slots.partition_point(|slot| slot.top_px + slot.height_px + THUMBNAIL_LABEL_HEIGHT < top_px);
    let last = slots.partition_point(|slot| slot.top_px <= bottom_px);
    first..last.max(first)
}

//---------------------------------------------------------------------
// The rail widget
//---------------------------------------------------------------------

/// Draw the thumbnail rail, returning the index of a page the user clicked.
///
/// Submits thumbnail requests for the visible slots only, into `cache`, which is
/// the rail's own cache with its own budget — a canvas zoom storm must not evict
/// the rail, nor the rail the canvas.
pub fn show_thumbnail_rail(ui: &mut egui::Ui, state: &mut ViewerState, cache: &mut TextureCache, service: &dyn RenderService, theme: &Theme) -> Option<usize> {
    let snapshot = state.snapshot().clone();
    let slots = lay_out_thumbnails(&snapshot, theme.gutter);
    let content_height_px = slots
        .last()
        .map_or(0.0, |slot| slot.top_px + slot.height_px + THUMBNAIL_LABEL_HEIGHT + theme.gutter);
    let current = state.current_page();
    let max_texture_side = ui.ctx().input(|input| input.max_texture_side);
    let frame_clock = cache.begin_frame();
    let mut clicked = None;

    egui::ScrollArea::vertical().auto_shrink([false, false]).show_viewport(ui, |ui, viewport| {
        let (content_rect, _response) = ui.allocate_exact_size(Vec2::new(THUMBNAIL_WIDTH_PT + 2.0 * theme.gutter, content_height_px), Sense::hover());
        let visible = find_visible_thumbnails(&slots, viewport.min.y, viewport.height());
        let painter = ui.painter_at(ui.clip_rect());

        for slot in slots.get(visible).unwrap_or_default() {
            let Some(page) = snapshot.pages.get(slot.index) else {
                continue;
            };
            let image_rect = Rect::from_min_size(
                content_rect.min + Vec2::new(theme.gutter, slot.top_px),
                Vec2::new(slot.width_px, slot.height_px),
            );

            //--- request this thumbnail if it is not already cached or in flight ---
            //--- capped for the same reason the canvas caps: a page of extreme aspect
            //--- ratio is 132 points wide and still taller than the backend allows ---
            let size = page.display_size();
            let scale = crate::zoom::fit_render_scale_to_texture_limit(compute_thumbnail_scale(size.width_pt), size.width_pt, size.height_pt, max_texture_side);
            let Ok(request) = RenderRequest::new(page.id, snapshot.revision, scale) else {
                continue;
            };
            if cache.mark_pending(request) {
                service.submit(request);
                ui.ctx().request_repaint();
            }

            let refused = cache.has_failed(&request);
            match cache.get(&request) {
                Some(texture) => super::canvas::draw_page_tile(&painter, image_rect, texture, theme),
                //--- a thumbnail the rasterizer refused must read as refused here too,
                //--- or the rail alone still promises a page that is never coming ---
                None if refused => super::canvas::draw_unrenderable_page(&painter, image_rect, slot.index + 1, theme),
                None => super::canvas::draw_page_placeholder(&painter, image_rect, slot.index + 1, theme),
            }

            //--- selection ring on the page the canvas is showing ---
            if current == Some(slot.index) {
                painter.rect_stroke(
                    image_rect.expand(2.0),
                    egui::CornerRadius::same(theme.corner_radius),
                    egui::Stroke::new(2.0_f32, theme.accent),
                    egui::StrokeKind::Outside,
                );
            }

            painter.text(
                pos2(image_rect.center().x, image_rect.max.y + THUMBNAIL_LABEL_HEIGHT * 0.5),
                egui::Align2::CENTER_CENTER,
                format!("{}", slot.index + 1),
                egui::FontId::proportional(11.0),
                theme.text_muted,
            );

            //--- sensed per thumbnail, so a drag layer can be added here at I3 ---
            let response = ui.allocate_rect(image_rect, Sense::click());
            if response.clicked() {
                clicked = Some(slot.index);
            }
        }
    });

    cache.evict_to_budget(frame_clock);
    clicked
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::synthetic::build_synthetic_snapshot;

    #[test]
    fn scales_every_page_to_the_same_thumbnail_width() {
        let snapshot = build_synthetic_snapshot(20).unwrap();
        for page in &snapshot.pages {
            let width_pt = page.display_size().width_pt;
            let rendered_px = width_pt * compute_thumbnail_scale(width_pt);
            assert!(
                (rendered_px - THUMBNAIL_WIDTH_PT).abs() < 1.5,
                "page rendered to {rendered_px} points wide, wanted {THUMBNAIL_WIDTH_PT}"
            );
        }
    }

    #[test]
    fn produces_a_bit_stable_scale_across_frames() {
        let first = compute_thumbnail_scale(595.0);
        let second = compute_thumbnail_scale(595.0 + 1e-6);
        assert_eq!(
            first.to_bits(),
            second.to_bits(),
            "an unstable thumbnail scale would mint a new cache key every frame"
        );
    }

    #[test]
    fn survives_a_degenerate_page_width() {
        let scale = compute_thumbnail_scale(0.0);
        assert!(
            scale.is_finite() && scale > 0.0,
            "a zero-width page must not produce a scale RenderRequest::new rejects"
        );
    }

    #[test]
    fn stacks_thumbnails_without_overlap_and_preserves_aspect_ratio() {
        let snapshot = build_synthetic_snapshot(12).unwrap();
        let slots = lay_out_thumbnails(&snapshot, 6.0);
        assert_eq!(slots.len(), 12);
        for pair in slots.windows(2) {
            assert!(pair[0].top_px + pair[0].height_px + THUMBNAIL_LABEL_HEIGHT <= pair[1].top_px);
        }
        for (slot, page) in slots.iter().zip(snapshot.pages.iter()) {
            let size = page.display_size();
            let expected_px = THUMBNAIL_WIDTH_PT * size.height_pt / size.width_pt;
            assert!(
                (slot.height_px - expected_px).abs() < 1e-3,
                "thumbnail {} must keep the page's aspect ratio",
                slot.index
            );
        }
    }

    #[test]
    fn virtualises_a_long_rail() {
        let snapshot = build_synthetic_snapshot(1_000).unwrap();
        let slots = lay_out_thumbnails(&snapshot, 6.0);
        let visible = find_visible_thumbnails(&slots, 0.0, 800.0);
        assert!(
            visible.len() < 20,
            "a thousand-page rail must not draw a thousand thumbnails per frame, drew {}",
            visible.len()
        );
        assert_eq!(visible.start, 0);
    }

    #[test]
    fn selects_nothing_past_the_end_of_the_rail() {
        let snapshot = build_synthetic_snapshot(10).unwrap();
        let slots = lay_out_thumbnails(&snapshot, 6.0);
        let visible = find_visible_thumbnails(&slots, 1_000_000.0, 800.0);
        assert!(visible.is_empty());
        assert!(slots.get(visible).is_some(), "the range must always be safe to slice with");
    }

    #[test]
    fn lays_out_nothing_for_an_empty_document() {
        assert!(lay_out_thumbnails(&opdf_core::document::DocumentSnapshot::default(), 6.0).is_empty());
    }
}
