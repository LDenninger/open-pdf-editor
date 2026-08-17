//! The central scrolling page canvas.
//!
//! Draws every visible page at its full layout size regardless of whether its
//! pixels have arrived, so the scroll extent never shifts under the user. A page
//! with no exact tile falls back to the nearest cached scale of the same page and
//! revision, and only then to a placeholder — which is why scrolling and zooming
//! do not flicker.

use egui::{Color32, CornerRadius, Painter, Pos2, Rect, Sense, Stroke, StrokeKind, TextureHandle, Vec2, pos2};

use crate::layout::PagePlacement;
use crate::theme::Theme;
use crate::tiles::TextureCache;
use crate::viewer::ViewerState;

/// The whole of a texture, in normalised texture coordinates.
pub const FULL_TEXTURE_UV: Rect = Rect {
    min: pos2(0.0, 0.0),
    max: pos2(1.0, 1.0),
};

/// The screen rectangle a page occupies, given the content box's origin on screen
/// and the current zoom.
///
/// `placement` is in PDF points; multiplying by `zoom` is the whole transform,
/// because [`crate::layout`] expresses gaps and margins in points too.
pub fn place_page_rect(origin: Pos2, placement: &PagePlacement, zoom: f32) -> Rect {
    Rect::from_min_size(
        origin + Vec2::new(placement.left_pt * zoom, placement.top_pt * zoom),
        Vec2::new(placement.width_pt * zoom, placement.height_pt * zoom),
    )
}

/// Paint a page whose pixels have not arrived: a bordered rectangle at the page's
/// full size, with its one-based page number centred in muted text.
///
/// Drawing the rectangle rather than nothing is what keeps the first frames after
/// opening a document from looking like an empty window.
pub fn draw_page_placeholder(painter: &Painter, rect: Rect, page_number: usize, theme: &Theme) {
    let radius = CornerRadius::same(theme.corner_radius);
    painter.rect_filled(rect.translate(Vec2::new(0.0, 2.0)), radius, theme.page_shadow);
    painter.rect_filled(rect, radius, theme.page_placeholder);
    painter.rect_stroke(rect, radius, Stroke::new(1.0_f32, theme.page_border), StrokeKind::Inside);
    if rect.height() > 28.0 && rect.width() > 28.0 {
        painter.text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            format!("{page_number}"),
            egui::FontId::proportional(13.0),
            theme.text_muted,
        );
    }
}

/// Paint a page the rasterizer has refused: the placeholder's geometry, an alarm
/// border, and a label saying so.
///
/// A refusal is permanent — the rasterizer resolves a page through the index map
/// frozen when the file was opened — so this must not look like the ordinary
/// placeholder, which means "not yet". A user staring at a grey rectangle that
/// will never fill in has been told nothing.
pub fn draw_unrenderable_page(painter: &Painter, rect: Rect, page_number: usize, theme: &Theme) {
    let radius = CornerRadius::same(theme.corner_radius);
    painter.rect_filled(rect.translate(Vec2::new(0.0, 2.0)), radius, theme.page_shadow);
    painter.rect_filled(rect, radius, theme.page_placeholder);
    painter.rect_stroke(rect, radius, Stroke::new(1.0_f32, theme.error_text), StrokeKind::Inside);
    if rect.height() > 28.0 && rect.width() > 28.0 {
        painter.text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            format!("{page_number} · cannot be rendered"),
            egui::FontId::proportional(13.0),
            theme.error_text,
        );
    }
}

/// Paint a page from a cached texture, with the same shadow and border the
/// placeholder uses so the two do not jump when one replaces the other.
pub fn draw_page_tile(painter: &Painter, rect: Rect, texture: &TextureHandle, theme: &Theme) {
    let radius = CornerRadius::same(theme.corner_radius);
    painter.rect_filled(rect.translate(Vec2::new(0.0, 2.0)), radius, theme.page_shadow);
    painter.rect_filled(rect, radius, theme.page_paper);
    painter.image(texture.id(), rect, FULL_TEXTURE_UV, Color32::WHITE);
    painter.rect_stroke(rect, radius, Stroke::new(1.0_f32, theme.page_border), StrokeKind::Inside);
}

//---------------------------------------------------------------------
// The canvas widget
//---------------------------------------------------------------------

/// Draw the scrolling page canvas.
///
/// Reads `state.scroll_request_px` and consumes it, so that navigation and
/// anchored zoom can move the scroll area from outside the widget. Writes the
/// realised scroll offset and viewport size back into `state`, which the status
/// bar and the next frame's scheduling read.
///
/// This function submits nothing and polls nothing: [`crate::viewer::step_render_service`]
/// has already run for this frame.
pub fn show_canvas(ui: &mut egui::Ui, state: &mut ViewerState, cache: &mut TextureCache, theme: &Theme, pixels_per_point: f32) {
    let mut scroll_area = egui::ScrollArea::vertical().auto_shrink([false, false]);
    if let Some(offset_px) = state.scroll_request_px.take() {
        scroll_area = scroll_area.vertical_scroll_offset(offset_px);
    }

    let zoom = crate::zoom::clamp_zoom(state.zoom);
    let revision = state.snapshot().revision;
    //--- the same settings the scheduler built its requests from, so a page whose
    //--- scale was capped for its size is looked up at that capped scale ---
    let max_texture_side = ui.ctx().input(|input| input.max_texture_side);
    let settings = state.render_settings(pixels_per_point, max_texture_side);

    let output = scroll_area.show_viewport(ui, |ui, viewport| {
        let (content_width_px, content_height_px) = state.content_size_px();
        let (content_rect, _response) = ui.allocate_exact_size(Vec2::new(content_width_px, content_height_px), Sense::hover());
        let painter = ui.painter_at(ui.clip_rect());

        //--- the visible range is recomputed from this frame's real viewport, not last frame's ---
        let top_pt = viewport.min.y / zoom;
        let height_pt = viewport.height() / zoom;
        let visible = crate::layout::find_visible_pages(state.layout(), top_pt, height_pt, 0.0);

        for placement in state.layout().placements.get(visible).unwrap_or_default() {
            let page_rect = place_page_rect(content_rect.min, placement, zoom);

            //--- tier 1: the exact tile for this page's scale, capped as the scheduler capped it ---
            let page_scale = settings.scale_for_page(opdf_core::page::PageSize::new(placement.width_pt, placement.height_pt));
            let exact = opdf_core::render::RenderRequest::new(placement.id, revision, page_scale)
                .ok()
                .map(|request| request.with_rotation(settings.view_rotation));
            let key = match exact.filter(|request| cache.contains(request)) {
                Some(request) => Some(request),
                //--- tier 2: any cached scale of the same page at the same revision ---
                None => cache.find_nearest_scale(placement.id, revision, page_scale),
            };

            //--- a refusal is only known for the exact request; a page with any cached
            //--- scale is drawn from it, because pixels beat an explanation ---
            let refused = exact.is_some_and(|request| cache.has_failed(&request));

            match key.and_then(|key| cache.get(&key)) {
                Some(texture) => draw_page_tile(&painter, page_rect, texture, theme),
                //--- tier 3: a placeholder, never a blank — and one that says which kind it is ---
                None if refused => draw_unrenderable_page(&painter, page_rect, placement.index + 1, theme),
                None => draw_page_placeholder(&painter, page_rect, placement.index + 1, theme),
            }
        }
    });

    state.scroll_offset_px = output.state.offset.y;
    state.viewport_size_px = (output.inner_rect.width(), output.inner_rect.height());
    state.viewport_origin_px = (output.inner_rect.min.x, output.inner_rect.min.y);
    state.refresh_current_page();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::compute_document_layout;
    use crate::synthetic::build_synthetic_snapshot;

    #[test]
    fn places_a_page_at_its_layout_position_scaled_by_the_zoom() {
        let placement = PagePlacement {
            id: opdf_core::page::PageId::new(0),
            index: 0,
            left_pt: 20.0,
            top_pt: 100.0,
            width_pt: 595.0,
            height_pt: 842.0,
        };
        let rect = place_page_rect(pos2(10.0, 5.0), &placement, 2.0);
        assert_eq!(rect.min, pos2(50.0, 205.0));
        assert_eq!(rect.width(), 1190.0);
        assert_eq!(rect.height(), 1684.0);
    }

    #[test]
    fn keeps_pages_inside_the_content_box_at_every_zoom() {
        let snapshot = build_synthetic_snapshot(20).unwrap();
        let theme = Theme::dark();
        let layout = compute_document_layout(&snapshot, theme.page_gap_pt, theme.canvas_margin_pt);
        for zoom in [0.25_f32, 1.0, 3.0] {
            let content = Rect::from_min_size(pos2(0.0, 0.0), Vec2::new(layout.content_width_pt * zoom, layout.content_height_pt * zoom));
            for placement in &layout.placements {
                let rect = place_page_rect(content.min, placement, zoom);
                assert!(content.contains_rect(rect), "page {} escapes the content box at zoom {zoom}", placement.index);
            }
        }
    }

    #[test]
    fn covers_the_whole_texture_when_drawing_a_tile() {
        assert_eq!(FULL_TEXTURE_UV.min, pos2(0.0, 0.0));
        assert_eq!(FULL_TEXTURE_UV.max, pos2(1.0, 1.0));
    }
}
