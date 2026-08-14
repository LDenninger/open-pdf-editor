//! Rasterizing one Pdfium page into an [`opdf_core::Tile`].
//!
//! Pdfium applies the rotation stored in the file itself, so the rotation this
//! module hands it is the *difference* between the rotation the snapshot asks
//! for and the rotation the file already has. That keeps an unsaved rotation
//! command visible on screen without rewriting the document.

use opdf_core::{Rotation, Tile};
use pdfium_render::prelude::{PdfPage, PdfPageRenderRotation, PdfRenderConfig, Pixels};

use crate::geometry::TileGeometry;

/// Translate a core rotation into Pdfium's render rotation.
pub fn convert_rotation(rotation: Rotation) -> PdfPageRenderRotation {
    match rotation {
        Rotation::None => PdfPageRenderRotation::None,
        Rotation::Quarter => PdfPageRenderRotation::Degrees90,
        Rotation::Half => PdfPageRenderRotation::Degrees180,
        Rotation::ThreeQuarter => PdfPageRenderRotation::Degrees270,
    }
}

/// The rotation Pdfium must apply on top of the one already stored in the file.
fn compute_residual_rotation(total: Rotation, file_rotation: Rotation) -> Result<Rotation, String> {
    let degrees = i32::from(total.degrees()) - i32::from(file_rotation.degrees());
    Rotation::from_degrees(degrees).map_err(|error| format!("cannot express a rotation of {degrees} degrees: {error}"))
}

/// Render one page at the resolved geometry.
///
/// `file_rotation` is the rotation Pdfium reports for the page, which it has
/// already applied; `geometry.total_rotation` is what the snapshot asks for.
///
/// Returns a human-readable reason, suitable for
/// [`opdf_core::RenderResponse::Failed`], on any failure — including a bitmap
/// whose buffer does not match the dimensions requested.
///
/// # Locking
///
/// The caller must already hold [`crate::library::lock_pdfium`], and must keep
/// holding it until `page` itself has been dropped. This function calls into
/// Pdfium throughout, and so do the drops of the page it borrows.
pub fn rasterize_page(page: &PdfPage<'_>, geometry: TileGeometry, file_rotation: Rotation) -> Result<Tile, String> {
    let residual = compute_residual_rotation(geometry.total_rotation, file_rotation)?;

    let width: Pixels = geometry
        .width_px
        .try_into()
        .map_err(|_| format!("tile width {} exceeds the pixel range pdfium accepts", geometry.width_px))?;
    let height: Pixels = geometry
        .height_px
        .try_into()
        .map_err(|_| format!("tile height {} exceeds the pixel range pdfium accepts", geometry.height_px))?;

    let config = PdfRenderConfig::new().set_fixed_size(width, height).rotate(convert_rotation(residual), false);

    let bitmap = page
        .render_with_config(&config)
        .map_err(|error| format!("pdfium failed to render the page: {error}"))?;

    Tile::new(geometry.width_px, geometry.height_px, bitmap.as_rgba_bytes()).map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixture::ensure_contract_fixture;
    use crate::geometry::compute_tile_geometry;
    use opdf_core::{PageId, PageInfo, PageSize, RenderRequest};
    use pdfium_render::prelude::PdfPageIndex;

    fn build_page_info(rotation: Rotation) -> PageInfo {
        PageInfo {
            id: PageId::new(1),
            size: PageSize::A4,
            rotation,
        }
    }

    fn render_fixture_page(index: PdfPageIndex, page_info: PageInfo, request: &RenderRequest, file_rotation: Rotation) -> Tile {
        let pdf_path = ensure_contract_fixture();
        let pdfium = crate::library::bind_pdfium().unwrap();
        //--- declared before the document so it is dropped after it: closing a document is a pdfium call too ---
        let _guard = crate::library::lock_pdfium();
        let document = pdfium.load_pdf_from_file(&pdf_path, None).unwrap();
        let pages = document.pages();
        let page = pages.get(index).unwrap();
        let geometry = compute_tile_geometry(page_info, request).unwrap();
        rasterize_page(&page, geometry, file_rotation).unwrap()
    }

    #[test]
    fn renders_an_unrotated_page_at_its_point_size() {
        let request = RenderRequest::new(PageId::new(1), 7, 1.0).unwrap();
        let tile = render_fixture_page(0, build_page_info(Rotation::None), &request, Rotation::None);

        assert_eq!(tile.width(), 595);
        assert_eq!(tile.height(), 842);
        assert_eq!(tile.pixels().len(), 595 * 842 * 4, "an RGBA tile holds four bytes per pixel");
    }

    #[test]
    fn draws_the_page_content_rather_than_a_blank_bitmap() {
        let request = RenderRequest::new(PageId::new(1), 7, 1.0).unwrap();
        let tile = render_fixture_page(0, build_page_info(Rotation::None), &request, Rotation::None);

        //--- the fixture fills its media box with pure blue, so the centre pixel proves real rasterization ---
        let centre = tile.pixel(tile.width() / 2, tile.height() / 2).unwrap();
        assert!(centre[2] > 200, "the blue channel must dominate at the centre, got {centre:?}");
        assert!(centre[0] < 60, "the red channel must be near zero at the centre, got {centre:?}");
        assert_eq!(centre[3], 255, "the tile must be fully opaque");
    }

    #[test]
    fn renders_a_stored_quarter_turn_with_swapped_axes() {
        let request = RenderRequest::new(PageId::new(2), 7, 1.0).unwrap();
        let tile = render_fixture_page(1, build_page_info(Rotation::Quarter), &request, Rotation::Quarter);

        assert_eq!(tile.width(), 842, "a quarter-turned A4 page is 842 pixels wide at scale 1.0");
        assert_eq!(tile.height(), 595);
    }

    #[test]
    fn asks_pdfium_only_for_the_rotation_the_file_does_not_already_have() {
        //--- snapshot and file agree at a quarter turn, so pdfium is asked for no extra rotation ---
        assert_eq!(compute_residual_rotation(Rotation::Quarter, Rotation::Quarter).unwrap(), Rotation::None);
        //--- the snapshot asks for a half turn on a file stored at a quarter turn: one more quarter ---
        assert_eq!(compute_residual_rotation(Rotation::Half, Rotation::Quarter).unwrap(), Rotation::Quarter);
        //--- and the negative direction normalizes rather than failing ---
        assert_eq!(compute_residual_rotation(Rotation::None, Rotation::Quarter).unwrap(), Rotation::ThreeQuarter);
    }
}
