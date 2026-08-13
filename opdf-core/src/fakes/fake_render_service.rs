//! Synchronous [`RenderService`] that draws flat colored rectangles, so that
//! the user interface can be developed before a rasterizer exists.

use std::sync::Mutex;

use crate::document::DocumentSnapshot;
use crate::page::PageInfo;
use crate::render::{RenderRequest, RenderResponse, RenderService, Tile};

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

        //--- a distinct, stable color per page position, so the UI is visually verifiable ---
        let red = (index.wrapping_mul(97) % 256) as u8;
        let green = (index.wrapping_mul(53) % 256) as u8;
        let blue = (index.wrapping_mul(29) % 256) as u8;

        let mut pixels = Vec::with_capacity(width as usize * height as usize * 4);
        for _ in 0..(width as usize * height as usize) {
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

#[cfg(all(test, feature = "contract-tests"))]
mod tests {
    use super::*;

    #[test]
    fn satisfies_the_render_service_contract() {
        crate::contract::assert_render_service_contract(FakeRenderService::new);
    }
}
