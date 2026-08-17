//! Synchronous [`RenderService`] that draws flat colored rectangles, so that
//! the user interface can be developed before a rasterizer exists.

use std::sync::Mutex;

use crate::document::DocumentSnapshot;
use crate::page::PageInfo;
use crate::render::{RenderRequest, RenderResponse, RenderService, Tile};

/// Largest tile this fake will allocate, in pixels — 64 mega-pixels, or 256 MiB
/// of RGBA.
///
/// [`RenderRequest::new`] accepts any finite positive scale, including `1e30`.
/// The `f32 as u32` casts below saturate at [`u32::MAX`], so an unclamped
/// `width * height * 4` overflows `usize` even on a 64-bit target: a debug build
/// panics and a release build wraps and then loops for effectively ever. A zoom
/// control that reached such a scale would take the application down with it, so
/// the request is failed instead.
const MAX_TILE_PIXELS: usize = 64 * 1024 * 1024;

/// Renders each page as a single flat color derived from its position, at the
/// correct dimensions for the requested scale and rotation.
#[derive(Debug)]
pub struct FakeRenderService {
    snapshot: DocumentSnapshot,
    pending: Mutex<Vec<RenderRequest>>,
}

impl FakeRenderService {
    /// A service answering requests for the pages in `snapshot`.
    pub fn new(snapshot: DocumentSnapshot) -> Self {
        Self {
            snapshot,
            pending: Mutex::new(Vec::new()),
        }
    }

    fn find_page(&self, request: &RenderRequest) -> Option<(usize, PageInfo)> {
        self.snapshot
            .pages
            .iter()
            .position(|page| page.id == request.page)
            .map(|index| (index, self.snapshot.pages[index]))
    }

    fn render_flat_tile(index: usize, page: PageInfo, request: &RenderRequest) -> Result<Tile, String> {
        let display = page.rotation.rotated_by(request.rotation);
        let size = PageInfo { rotation: display, ..page }.display_size();

        let width = (size.width_pt * request.scale).round().max(1.0) as u32;
        let height = (size.height_pt * request.scale).round().max(1.0) as u32;

        //--- refuse an oversized tile outright rather than overflowing, or silently rendering a smaller one ---
        let pixel_count = (width as usize).checked_mul(height as usize);
        let Some(pixel_count) = pixel_count.filter(|count| *count <= MAX_TILE_PIXELS) else {
            return Err(format!(
                "requested tile of {width}x{height} pixels at scale {} exceeds the fake renderer's limit of {MAX_TILE_PIXELS} pixels",
                request.scale
            ));
        };

        //--- a distinct, stable color per page position, so the UI is visually verifiable ---
        let red = (index.wrapping_mul(97) % 256) as u8;
        let green = (index.wrapping_mul(53) % 256) as u8;
        let blue = (index.wrapping_mul(29) % 256) as u8;

        let mut pixels = Vec::with_capacity(pixel_count * 4);
        for _ in 0..pixel_count {
            pixels.extend_from_slice(&[red, green, blue, 255]);
        }

        Tile::new(width, height, pixels).map_err(|error| error.to_string())
    }
}

impl RenderService for FakeRenderService {
    fn submit(&self, request: RenderRequest) {
        if let Ok(mut pending) = self.pending.lock() {
            pending.push(request);
        }
    }

    fn poll(&self) -> Vec<RenderResponse> {
        let Ok(mut pending) = self.pending.lock() else {
            return Vec::new();
        };
        let requests = std::mem::take(&mut *pending);
        drop(pending);

        requests
            .into_iter()
            .map(|request| match self.find_page(&request) {
                Some((index, page)) => match Self::render_flat_tile(index, page, &request) {
                    Ok(tile) => RenderResponse::Ready { request, tile },
                    Err(reason) => RenderResponse::Failed { request, reason },
                },
                None => RenderResponse::Failed {
                    request,
                    reason: format!("unknown page {}", request.page),
                },
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::page::{PageId, PageSize, Rotation};

    fn build_single_page_service() -> FakeRenderService {
        FakeRenderService::new(DocumentSnapshot {
            document: crate::document::DocumentId::new_unique(),
            revision: 3,
            pages: vec![PageInfo {
                id: PageId::new(1),
                size: PageSize::A4,
                rotation: Rotation::None,
            }],
        })
    }

    #[test]
    fn fails_an_absurd_scale_instead_of_allocating_or_overflowing() {
        let service = build_single_page_service();
        let document = service.snapshot.document;
        service.submit(RenderRequest::new(document, PageId::new(1), 3, 1e30).unwrap());

        let responses = service.poll();
        assert_eq!(responses.len(), 1, "an oversized request must still be answered");
        match &responses[0] {
            RenderResponse::Failed { reason, .. } => {
                assert!(reason.contains("exceeds"), "the failure must name the limit it exceeded, got: {reason}");
            }
            RenderResponse::Ready { tile, .. } => panic!("an absurd scale must fail, not yield a {}x{} tile", tile.width(), tile.height()),
        }
    }

    #[cfg(feature = "contract-tests")]
    #[test]
    fn satisfies_the_render_service_contract() {
        crate::contract::assert_render_service_contract(FakeRenderService::new);
    }
}
