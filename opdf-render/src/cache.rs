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
/// resident under normal scrolling.
///
/// # It is exactly one maximum-sized tile
///
/// [`crate::geometry::MAX_TILE_PIXELS`] is 64 megapixels, which at four bytes
/// per pixel is 268 435 456 bytes — the same number as this budget, not a
/// quarter of it as this comment used to claim. A single tile at the ceiling is
/// therefore cacheable (the refusal in [`TileCache::insert`] triggers only
/// *above* the budget) and evicting to make room for it empties the cache
/// completely. The user who zooms one page to the limit loses every thumbnail
/// and every neighbouring page they had.
///
/// That is a real cost and it is deliberate only in the sense that it is now
/// known: the ceiling cannot come down, because it is set equal to
/// [`opdf_core::fakes::FakeRenderService`]'s so the user interface sees one
/// area limit rather than two. Closing the gap means raising this budget to
/// 1 GiB, or refusing to cache a tile above some fraction of it. Neither is
/// done here.
///
/// `holds_exactly_one_maximum_sized_tile` pins the arithmetic, so the
/// relationship cannot drift again without a test saying so.
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
    ///
    /// Pruning here does not by itself make the cache revision-honest. A tile
    /// rasterized against the new snapshot but keyed on a request naming the
    /// old revision would be inserted *after* this call and survive it. The
    /// worker prevents that upstream, by superseding everything still queued
    /// when a rebind arrives rather than rendering it.
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

    /// The default budget and the tile ceiling were documented as differing by
    /// a factor of four in both directions — the cache claiming to be a quarter
    /// of a maximum tile's cost, the ceiling claiming no single tile could
    /// evict everything. They are equal. Pin it, so the next person to change
    /// either constant is told what they changed.
    #[test]
    fn holds_exactly_one_maximum_sized_tile() {
        const BYTES_PER_PIXEL: u64 = 4;
        let ceiling_bytes = crate::geometry::MAX_TILE_PIXELS * BYTES_PER_PIXEL;
        assert_eq!(
            ceiling_bytes, DEFAULT_CACHE_BYTES as u64,
            "a tile at MAX_TILE_PIXELS costs exactly the whole default budget; if this ever stops holding, both constants' doc comments must be rewritten"
        );
    }

    /// The consequence of the equality above, stated as behaviour: caching one
    /// maximum-sized tile leaves nothing else in the cache.
    #[test]
    fn a_maximum_sized_tile_evicts_the_entire_cache() {
        //--- scaled down by a factor of 1024 so the test costs kilobytes, not gigabytes ---
        let budget = DEFAULT_CACHE_BYTES / 1024;
        let mut cache = TileCache::with_budget(budget);
        cache.insert(build_request(1, 7), &build_tile(10));
        cache.insert(build_request(2, 7), &build_tile(10));
        assert_eq!(cache.len(), 2);

        let ceiling_tile = build_tile(u32::try_from(budget / 4).unwrap());
        cache.insert(build_request(3, 7), &ceiling_tile);

        assert_eq!(cache.len(), 1, "a tile the size of the whole budget leaves room for nothing else");
        assert_eq!(cache.bytes(), budget, "and it fills the budget exactly, so it is cached rather than refused");
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
