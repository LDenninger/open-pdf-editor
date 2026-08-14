//! The worker's tile cache.
//!
//! Keyed directly on [`RenderRequest`], whose `Eq`/`Hash` already treat `scale`
//! bitwise and include `revision` — so a tile rasterized before a structural
//! change can never be served for a request built after one.
//!
//! Eviction is least-recently-used and bounded by bytes rather than entry
//! count, because a thumbnail and a 600 dpi page differ by four orders of
//! magnitude in size. An entry-counted cache on a 500-page document is an
//! out-of-memory bug waiting for a user who zooms in.

use std::collections::HashMap;
use std::collections::VecDeque;

use opdf_core::{RenderRequest, Tile};

/// Bytes of rasterized pixels the cache will hold — 256 MiB.
///
/// About thirty A4 pages at scale 2.0, so several screens of scrollback stay
/// resident, and a quarter of what one request at the tile ceiling could ask
/// for, so no single tile can evict everything else.
pub const DEFAULT_CACHE_BYTES: usize = 256 * 1024 * 1024;

/// A byte-budgeted, least-recently-used tile cache.
#[derive(Debug)]
pub struct TileCache {
    entries: HashMap<RenderRequest, Tile>,
    /// Least-recently-used first.
    recency: VecDeque<RenderRequest>,
    bytes: usize,
    budget_bytes: usize,
}

impl TileCache {
    /// An empty cache holding at most `budget_bytes` of pixels.
    pub fn with_budget(budget_bytes: usize) -> Self {
        Self {
            entries: HashMap::new(),
            recency: VecDeque::new(),
            bytes: 0,
            budget_bytes,
        }
    }

    /// The tile cached for `request`, marking it most recently used.
    pub fn get(&mut self, request: &RenderRequest) -> Option<Tile> {
        let tile = self.entries.get(request)?.clone();
        self.touch(request);
        Some(tile)
    }

    /// Cache a tile, evicting least-recently-used entries to stay in budget.
    ///
    /// A tile larger than the whole budget is not cached at all.
    pub fn insert(&mut self, request: RenderRequest, tile: &Tile) {
        let tile_bytes = tile.pixels().len();
        if tile_bytes > self.budget_bytes {
            return;
        }
        if self.entries.contains_key(&request) {
            self.touch(&request);
            return;
        }
        while self.bytes + tile_bytes > self.budget_bytes {
            if !self.evict_oldest() {
                break;
            }
        }
        self.entries.insert(request, tile.clone());
        self.recency.push_back(request);
        self.bytes += tile_bytes;
    }

    /// Drop every tile not rasterized at `revision`.
    ///
    /// Called when the worker is rebound to a new snapshot: tiles for a
    /// superseded revision can never be served again, so holding them is pure
    /// memory cost.
    pub fn retain_revision(&mut self, revision: u64) {
        self.entries.retain(|request, _| request.revision == revision);
        self.recency.retain(|request| request.revision == revision);
        self.bytes = self.entries.values().map(|tile| tile.pixels().len()).sum();
    }

    /// Number of cached tiles.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the cache holds nothing.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Bytes of pixels currently held.
    pub fn bytes(&self) -> usize {
        self.bytes
    }

    /// Move a request to the most-recently-used end.
    fn touch(&mut self, request: &RenderRequest) {
        if let Some(position) = self.recency.iter().position(|queued| queued == request) {
            self.recency.remove(position);
        }
        self.recency.push_back(*request);
    }

    /// Drop the least recently used entry. Returns whether anything was dropped.
    fn evict_oldest(&mut self) -> bool {
        let Some(oldest) = self.recency.pop_front() else {
            return false;
        };
        if let Some(tile) = self.entries.remove(&oldest) {
            self.bytes -= tile.pixels().len();
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use opdf_core::PageId;

    fn build_request(page: u64, revision: u64) -> RenderRequest {
        RenderRequest::new(PageId::new(page), revision, 1.0).unwrap()
    }

    /// A tile of `pixel_count` pixels — four bytes each.
    fn build_tile(pixel_count: u32) -> Tile {
        Tile::new(pixel_count, 1, vec![7; pixel_count as usize * 4]).unwrap()
    }

    #[test]
    fn serves_a_cached_tile_for_an_identical_request() {
        let mut cache = TileCache::with_budget(DEFAULT_CACHE_BYTES);
        cache.insert(build_request(1, 7), &build_tile(10));

        let served = cache.get(&build_request(1, 7)).unwrap();
        assert_eq!(served.width(), 10);
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn never_serves_a_tile_from_another_revision() {
        let mut cache = TileCache::with_budget(DEFAULT_CACHE_BYTES);
        cache.insert(build_request(1, 7), &build_tile(10));

        assert!(
            cache.get(&build_request(1, 8)).is_none(),
            "a tile rendered at revision 7 must not answer a request at revision 8"
        );
    }

    #[test]
    fn evicts_the_least_recently_used_entry_to_stay_in_budget() {
        //--- budget of 120 bytes holds exactly three ten-pixel tiles ---
        let mut cache = TileCache::with_budget(120);
        cache.insert(build_request(1, 7), &build_tile(10));
        cache.insert(build_request(2, 7), &build_tile(10));
        cache.insert(build_request(3, 7), &build_tile(10));
        assert_eq!(cache.bytes(), 120);

        //--- touch page one so page two becomes the oldest ---
        assert!(cache.get(&build_request(1, 7)).is_some());
        cache.insert(build_request(4, 7), &build_tile(10));

        assert_eq!(cache.len(), 3, "the cache must stay within its budget");
        assert!(
            cache.get(&build_request(2, 7)).is_none(),
            "the least recently used entry must be the one evicted"
        );
        assert!(cache.get(&build_request(1, 7)).is_some(), "a recently used entry must survive");
        assert!(cache.get(&build_request(4, 7)).is_some());
    }

    #[test]
    fn refuses_to_cache_a_tile_larger_than_the_whole_budget() {
        let mut cache = TileCache::with_budget(100);
        cache.insert(build_request(1, 7), &build_tile(1000));

        assert!(cache.is_empty(), "a tile bigger than the budget must not be stored");
        assert_eq!(cache.bytes(), 0);
    }

    #[test]
    fn drops_every_tile_from_a_superseded_revision() {
        let mut cache = TileCache::with_budget(DEFAULT_CACHE_BYTES);
        cache.insert(build_request(1, 7), &build_tile(10));
        cache.insert(build_request(2, 7), &build_tile(10));
        cache.insert(build_request(1, 8), &build_tile(10));

        cache.retain_revision(8);

        assert_eq!(cache.len(), 1, "only the current revision survives a rebind");
        assert_eq!(cache.bytes(), 40);
        assert!(cache.get(&build_request(1, 8)).is_some());
    }

    #[test]
    fn stays_bounded_under_a_long_scroll() {
        let mut cache = TileCache::with_budget(4_000);
        for ii in 0..500_u64 {
            cache.insert(build_request(ii, 7), &build_tile(100));
        }
        assert!(cache.bytes() <= 4_000, "the cache must never exceed its budget, held {} bytes", cache.bytes());
        assert_eq!(cache.len(), 10);
    }
}
