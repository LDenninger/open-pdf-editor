//! Reading page geometry out of a PDF object graph.

use lopdf::ObjectId;
use opdf_core::{PageSize, Rotation};

use crate::objects::{ObjectSource, find_inherited_attribute, resolve_reference};

/// Size assumed for a page whose `/MediaBox` is missing or unreadable.
///
/// ISO 32000-1 makes `/MediaBox` required but leaves the recovery behaviour to
/// the reader. US Letter matches what Acrobat assumes, so a file that opens
/// there opens the same way here rather than at some third size.
pub(crate) const DEFAULT_PAGE_SIZE: PageSize = PageSize::LETTER;

/// Media box dimensions for a page, following inheritance and indirect references.
///
/// Falls back to [`DEFAULT_PAGE_SIZE`] rather than failing: a page whose box is
/// missing, the wrong length, non-numeric, or degenerate is still a page the
/// user expects to see, and refusing to open the document would be worse than
/// showing it at a plausible size.
pub(crate) fn read_page_size<S: ObjectSource + ?Sized>(source: &S, page_object_id: ObjectId) -> PageSize {
    let Some(value) = find_inherited_attribute(source, page_object_id, b"MediaBox") else {
        return DEFAULT_PAGE_SIZE;
    };
    let Some(resolved) = resolve_reference(source, value) else {
        return DEFAULT_PAGE_SIZE;
    };
    let Ok(corners) = resolved.as_array() else {
        return DEFAULT_PAGE_SIZE;
    };
    if corners.len() != 4 {
        return DEFAULT_PAGE_SIZE;
    }

    let mut bounds = [0.0_f32; 4];
    for (index, corner) in corners.iter().enumerate() {
        let Some(number) = resolve_reference(source, corner).and_then(|object| object.as_float().ok()) else {
            return DEFAULT_PAGE_SIZE;
        };
        bounds[index] = number;
    }

    let width_pt = (bounds[2] - bounds[0]).abs();
    let height_pt = (bounds[3] - bounds[1]).abs();
    if !width_pt.is_finite() || !height_pt.is_finite() || width_pt <= 0.0 || height_pt <= 0.0 {
        return DEFAULT_PAGE_SIZE;
    }
    PageSize::new(width_pt, height_pt)
}

/// Rotation recorded for a page, following inheritance and indirect references.
///
/// Falls back to [`Rotation::None`] for a missing, non-integer, or
/// non-quarter-turn value. Nothing is written back, so a file carrying an
/// illegal `/Rotate` keeps it until the page is explicitly rotated.
pub(crate) fn read_page_rotation<S: ObjectSource + ?Sized>(source: &S, page_object_id: ObjectId) -> Rotation {
    let Some(value) = find_inherited_attribute(source, page_object_id, b"Rotate") else {
        return Rotation::None;
    };
    let Some(resolved) = resolve_reference(source, value) else {
        return Rotation::None;
    };
    let Ok(degrees) = resolved.as_i64() else {
        return Rotation::None;
    };
    let Ok(degrees) = i32::try_from(degrees) else {
        return Rotation::None;
    };
    Rotation::from_degrees(degrees).unwrap_or(Rotation::None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixture;

    fn load(bytes: &[u8]) -> lopdf::Document {
        lopdf::Document::load_mem(bytes).expect("fixture must parse")
    }

    #[test]
    fn reads_the_media_box_a_page_carries_itself() {
        let document = load(&fixture::build_flat_pages(&[PageSize::A4, PageSize::LETTER]));
        let ids: Vec<lopdf::ObjectId> = document.page_iter().collect();
        assert_eq!(read_page_size(&document, ids[0]), PageSize::A4);
        assert_eq!(read_page_size(&document, ids[1]), PageSize::LETTER);
    }

    #[test]
    fn reads_geometry_inherited_from_the_root_node() {
        let document = load(&fixture::build_inherited_geometry());
        for page_id in document.page_iter() {
            assert_eq!(read_page_size(&document, page_id), PageSize::A4);
            assert_eq!(read_page_rotation(&document, page_id), Rotation::Quarter);
        }
    }

    #[test]
    fn reads_a_media_box_stored_as_an_indirect_reference() {
        let document = load(&fixture::build_indirect_media_box());
        let page_id = document.page_iter().next().expect("the fixture has one page");
        assert_eq!(read_page_size(&document, page_id), PageSize::new(300.0, 500.0));
    }

    #[test]
    fn falls_back_to_us_letter_when_no_media_box_exists() {
        let document = load(&fixture::build_missing_media_box());
        let page_id = document.page_iter().next().expect("the fixture has one page");
        assert_eq!(read_page_size(&document, page_id), DEFAULT_PAGE_SIZE);
        assert_eq!(DEFAULT_PAGE_SIZE, PageSize::LETTER);
    }

    #[test]
    fn normalises_rotations_including_negative_quarter_turns() {
        let document = load(&fixture::build_rotated_pages());
        let rotations: Vec<Rotation> = document.page_iter().map(|page_id| read_page_rotation(&document, page_id)).collect();
        assert_eq!(rotations, vec![Rotation::Quarter, Rotation::Half, Rotation::ThreeQuarter]);
    }

    #[test]
    fn treats_an_unrotated_page_as_rotation_none() {
        let document = load(&fixture::build_flat_pages(&[PageSize::A4]));
        let page_id = document.page_iter().next().expect("the fixture has one page");
        assert_eq!(read_page_rotation(&document, page_id), Rotation::None);
    }
}
