//! A byte-bounded, least-recently-used cache keyed by [`RenderRequest`].
//!
//! Generic over its value so that the eviction and budgeting logic — the part
//! that goes wrong — is tested with a trivial payload and no display. The
//! concrete texture cache is `crate::tiles::TextureCache`.
//!
//! Dropping an entry is the only way a texture is freed: an `egui::TextureHandle`
//! releases its GPU allocation in `Drop`, so removing a map entry is the release.

use std::collections::{HashMap, HashSet};

use opdf_core::page::PageId;
use opdf_core::render::RenderRequest;

#[derive(Debug)]
struct CacheEntry<T> {
    value: T,
    bytes: usize,
    last_used: u64,
}

/// A cache of rendered values, bounded by a byte budget and evicted
/// least-recently-used first.
///
/// The cache also tracks which requests are **pending** — submitted to the render
/// service but not yet answered — so that a scrolling viewer does not resubmit the
/// same request on every one of the sixty frames before the tile arrives.
#[derive(Debug)]
pub struct TileCache<T> {
    entries: HashMap<RenderRequest, CacheEntry<T>>,
    pending: HashSet<RenderRequest>,
    budget_bytes: usize,
    used_bytes: usize,
    clock: u64,
}

impl<T> TileCache<T> {
    /// An empty cache that will evict down to `budget_bytes` when asked.
    pub fn new(budget_bytes: usize) -> Self {
        Self {
            entries: HashMap::new(),
            pending: HashSet::new(),
            budget_bytes,
            used_bytes: 0,
            clock: 0,
        }
    }

    /// The byte budget this cache evicts down to.
    pub fn budget_bytes(&self) -> usize {
        self.budget_bytes
    }

    /// Bytes currently held, as reported by callers of [`TileCache::insert`].
    pub fn used_bytes(&self) -> usize {
        self.used_bytes
    }

    /// Number of cached entries.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the cache holds nothing.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Number of requests submitted but not yet answered.
    pub fn pending_count(&self) -> usize {
        self.pending.len()
    }

    /// Mark the start of a frame, returning the clock value that separates
    /// "touched this frame" from "touched earlier".
    ///
    /// Pass the returned value to [`TileCache::evict_to_budget`] so that an entry
    /// drawn in this very frame is never the one thrown away.
    pub fn begin_frame(&mut self) -> u64 {
        self.clock += 1;
        self.clock
    }
}

//---------------------------------------------------------------------
// Storing and retrieving
//---------------------------------------------------------------------

impl<T> TileCache<T> {
    /// Store `value` under `request`, recording its footprint and clearing the
    /// request from the pending set.
    ///
    /// Replacing an existing entry subtracts the old footprint before adding the
    /// new one, so re-inserting the same key does not double-count.
    pub fn insert(&mut self, request: RenderRequest, value: T, bytes: usize) {
        self.clock += 1;
        self.pending.remove(&request);
        let entry = CacheEntry {
            value,
            bytes,
            last_used: self.clock,
        };
        if let Some(previous) = self.entries.insert(request, entry) {
            self.used_bytes = self.used_bytes.saturating_sub(previous.bytes);
        }
        self.used_bytes = self.used_bytes.saturating_add(bytes);
    }

    /// Look up an entry, marking it as used so eviction spares it.
    pub fn get(&mut self, request: &RenderRequest) -> Option<&T> {
        self.clock += 1;
        let clock = self.clock;
        let entry = self.entries.get_mut(request)?;
        entry.last_used = clock;
        Some(&entry.value)
    }

    /// Whether an entry is cached, without marking it as used.
    pub fn contains(&self, request: &RenderRequest) -> bool {
        self.entries.contains_key(request)
    }

    /// Every cached entry as a `(request, value)` pair, in no particular order,
    /// without marking any of them as used.
    ///
    /// The drawing code looks entries up by key and has no use for this; it
    /// exists so that a test can ask *what* the cache is holding rather than only
    /// how much, which is the difference between proving a tile arrived and
    /// proving the right pixels did.
    pub fn entries(&self) -> impl Iterator<Item = (&RenderRequest, &T)> {
        self.entries.iter().map(|(request, entry)| (request, &entry.value))
    }

    /// Whether this request still needs rasterizing — neither cached nor
    /// already in flight.
    ///
    /// The read-only half of [`TileCache::mark_pending`], for a caller that
    /// must decide whether it *would* submit before deciding whether it
    /// *can*. Planning a frame is exactly that: a request that does not fit
    /// this frame's budget must not be recorded as in flight, or nothing will
    /// ever clear it and the page stays blank forever.
    pub fn wants(&self, request: &RenderRequest) -> bool {
        !self.entries.contains_key(request) && !self.pending.contains(request)
    }

    /// Record that `request` has been submitted, returning whether the caller
    /// should actually submit it.
    ///
    /// Returns `false` when the request is already cached or already in flight —
    /// which is what stops a viewer from resubmitting the same tile on every frame
    /// while it waits.
    pub fn mark_pending(&mut self, request: RenderRequest) -> bool {
        if self.entries.contains_key(&request) || self.pending.contains(&request) {
            return false;
        }
        self.pending.insert(request);
        true
    }

    /// Whether a request has been submitted and not yet answered.
    ///
    /// This is what lets one poll of a shared render service be routed: a response
    /// belongs to the cache that asked for it, and there is no other way to tell,
    /// because the request itself carries no cache identity.
    pub fn is_pending(&self, request: &RenderRequest) -> bool {
        self.pending.contains(request)
    }

    /// Forget that a request is in flight, after a response arrives for it —
    /// including a failed one, which never becomes an entry.
    pub fn clear_pending(&mut self, request: &RenderRequest) {
        self.pending.remove(request);
    }

    /// The cached request for `page` at `revision` whose scale is closest to
    /// `wanted_scale`, if any.
    ///
    /// This is what removes flicker. When the exact key misses — the first frames
    /// after opening a document, and every frame during a zoom — the canvas draws
    /// the nearest scale it already has, stretched, instead of a blank rectangle.
    /// Only when nothing at all is cached for the page does a placeholder appear.
    pub fn find_nearest_scale(&self, page: PageId, revision: u64, wanted_scale: f32) -> Option<RenderRequest> {
        self.entries
            .keys()
            .filter(|request| request.page == page && request.revision == revision)
            .copied()
            .min_by(|left, right| (left.scale - wanted_scale).abs().total_cmp(&(right.scale - wanted_scale).abs()))
    }
}

//---------------------------------------------------------------------
// Releasing memory
//---------------------------------------------------------------------

impl<T> TileCache<T> {
    /// Drop every entry and every pending request belonging to a different
    /// document revision.
    ///
    /// Called when the snapshot is replaced. Without it, the tiles of the previous
    /// structure sit in the cache forever: they can never be served, because
    /// `RenderRequest` includes the revision in its equality and hash, but they
    /// still occupy the budget.
    pub fn retain_revision(&mut self, revision: u64) {
        let mut freed_bytes = 0_usize;
        self.entries.retain(|request, entry| {
            let keep = request.revision == revision;
            if !keep {
                freed_bytes = freed_bytes.saturating_add(entry.bytes);
            }
            keep
        });
        self.used_bytes = self.used_bytes.saturating_sub(freed_bytes);
        self.pending.retain(|request| request.revision == revision);
    }

    /// Drop every entry and every pending request, whatever revision it belongs to.
    ///
    /// Called when a *different document* is opened, where
    /// [`TileCache::retain_revision`] is not enough: a revision counts edits within
    /// one document, every document starts that count at zero, and page ids are
    /// allocated per document — so the previous document's entries are keyed
    /// exactly as the new document will look them up, and retaining them serves one
    /// document's pixels for another.
    pub fn clear(&mut self) {
        self.entries.clear();
        self.pending.clear();
        self.used_bytes = 0;
    }

    /// Evict least-recently-used entries until the cache is within budget,
    /// returning how many were dropped.
    ///
    /// Entries whose `last_used` is at or after `protected_since` are never
    /// evicted, so a frame that needs more than the budget draws correctly and
    /// merely overshoots for that frame rather than evicting a texture it is about
    /// to draw. Pass the value [`TileCache::begin_frame`] returned.
    pub fn evict_to_budget(&mut self, protected_since: u64) -> usize {
        if self.used_bytes <= self.budget_bytes {
            return 0;
        }
        let mut candidates: Vec<(RenderRequest, u64)> = self
            .entries
            .iter()
            .filter(|(_, entry)| entry.last_used < protected_since)
            .map(|(request, entry)| (*request, entry.last_used))
            .collect();
        candidates.sort_by_key(|(_, last_used)| *last_used);

        let mut evicted = 0_usize;
        for (request, _) in candidates {
            if self.used_bytes <= self.budget_bytes {
                break;
            }
            if let Some(entry) = self.entries.remove(&request) {
                self.used_bytes = self.used_bytes.saturating_sub(entry.bytes);
                evicted += 1;
            }
        }
        evicted
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build_request(page: u64, revision: u64, scale: f32) -> RenderRequest {
        RenderRequest::new(PageId::new(page), revision, scale).unwrap()
    }

    #[test]
    fn tracks_the_bytes_it_was_told_about() {
        let mut cache: TileCache<u32> = TileCache::new(1_000);
        cache.insert(build_request(0, 1, 1.0), 7, 400);
        cache.insert(build_request(1, 1, 1.0), 8, 300);
        assert_eq!(cache.used_bytes(), 700);
        assert_eq!(cache.len(), 2);
    }

    #[test]
    fn does_not_double_count_a_replaced_entry() {
        let mut cache: TileCache<u32> = TileCache::new(1_000);
        let request = build_request(0, 1, 1.0);
        cache.insert(request, 7, 400);
        cache.insert(request, 8, 250);
        assert_eq!(cache.used_bytes(), 250, "re-inserting a key must replace its footprint, not add to it");
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn submits_a_request_once_while_it_is_in_flight() {
        let mut cache: TileCache<u32> = TileCache::new(1_000);
        let request = build_request(0, 1, 1.0);
        assert!(cache.mark_pending(request), "the first submission must go through");
        assert!(!cache.mark_pending(request), "a request already in flight must not be resubmitted every frame");
        cache.insert(request, 7, 100);
        assert_eq!(cache.pending_count(), 0, "an answered request must leave the pending set");
        assert!(!cache.mark_pending(request), "a cached request must never be resubmitted");
    }

    #[test]
    fn clears_a_pending_request_that_failed() {
        let mut cache: TileCache<u32> = TileCache::new(1_000);
        let request = build_request(0, 1, 1.0);
        cache.mark_pending(request);
        cache.clear_pending(&request);
        assert_eq!(cache.pending_count(), 0);
        assert!(cache.mark_pending(request), "a failed request must be retryable, not stuck pending forever");
    }

    #[test]
    fn finds_the_closest_cached_scale_for_a_page() {
        let mut cache: TileCache<u32> = TileCache::new(10_000);
        cache.insert(build_request(3, 1, 0.5), 1, 10);
        cache.insert(build_request(3, 1, 2.0), 2, 10);
        cache.insert(build_request(4, 1, 1.0), 3, 10);
        let found = cache.find_nearest_scale(PageId::new(3), 1, 1.6).unwrap();
        assert_eq!(found.scale, 2.0, "1.6 is closer to 2.0 than to 0.5");
        assert_eq!(found.page, PageId::new(3), "the fallback must never borrow another page's pixels");
    }

    #[test]
    fn never_offers_a_tile_from_another_revision_as_a_fallback() {
        let mut cache: TileCache<u32> = TileCache::new(10_000);
        cache.insert(build_request(3, 1, 1.0), 1, 10);
        assert_eq!(
            cache.find_nearest_scale(PageId::new(3), 2, 1.0),
            None,
            "a pre-edit tile must not be drawn for the post-edit document"
        );
    }

    #[test]
    fn evicts_least_recently_used_entries_until_within_budget() {
        let mut cache: TileCache<u32> = TileCache::new(100);
        for ii in 0..4_u64 {
            cache.insert(build_request(ii, 1, 1.0), ii as u32, 50);
        }
        assert_eq!(cache.used_bytes(), 200);
        let frame = cache.begin_frame();
        assert_eq!(cache.evict_to_budget(frame), 2);
        assert_eq!(cache.used_bytes(), 100);
        assert!(cache.contains(&build_request(3, 1, 1.0)), "the newest entry must survive");
        assert!(!cache.contains(&build_request(0, 1, 1.0)), "the oldest entry must be the first to go");
    }

    #[test]
    fn spares_an_entry_used_since_the_frame_began() {
        let mut cache: TileCache<u32> = TileCache::new(100);
        for ii in 0..4_u64 {
            cache.insert(build_request(ii, 1, 1.0), ii as u32, 50);
        }
        let frame = cache.begin_frame();
        //--- the frame draws the oldest entry, which must therefore survive eviction ---
        assert!(cache.get(&build_request(0, 1, 1.0)).is_some());
        cache.evict_to_budget(frame);
        assert!(
            cache.contains(&build_request(0, 1, 1.0)),
            "a texture drawn this frame must never be evicted out from under the draw"
        );
    }

    #[test]
    fn overshoots_the_budget_rather_than_evicting_the_current_frame() {
        let mut cache: TileCache<u32> = TileCache::new(10);
        let frame = cache.begin_frame();
        cache.insert(build_request(0, 1, 1.0), 1, 500);
        cache.insert(build_request(1, 1, 1.0), 2, 500);
        assert_eq!(
            cache.evict_to_budget(frame),
            0,
            "when every entry belongs to this frame, eviction must give up rather than blank the canvas"
        );
        assert_eq!(cache.len(), 2);
    }

    #[test]
    fn does_nothing_when_already_within_budget() {
        let mut cache: TileCache<u32> = TileCache::new(1_000);
        cache.insert(build_request(0, 1, 1.0), 1, 50);
        let frame = cache.begin_frame();
        assert_eq!(cache.evict_to_budget(frame), 0);
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn drops_every_entry_from_a_superseded_revision() {
        let mut cache: TileCache<u32> = TileCache::new(10_000);
        cache.insert(build_request(0, 1, 1.0), 1, 100);
        cache.insert(build_request(1, 1, 1.0), 2, 100);
        cache.insert(build_request(0, 2, 1.0), 3, 100);
        cache.mark_pending(build_request(5, 1, 1.0));

        cache.retain_revision(2);

        assert_eq!(cache.len(), 1, "only the current revision may survive");
        assert_eq!(cache.used_bytes(), 100, "freed entries must return their bytes to the budget");
        assert_eq!(cache.pending_count(), 0, "a request in flight for a superseded revision must be forgotten too");
    }

    #[test]
    fn drops_everything_including_the_current_revision_when_cleared() {
        let mut cache: TileCache<u32> = TileCache::new(10_000);
        cache.insert(build_request(0, 0, 1.0), 1, 100);
        cache.insert(build_request(1, 0, 1.0), 2, 100);
        cache.mark_pending(build_request(5, 0, 1.0));

        cache.clear();

        assert_eq!(cache.len(), 0, "a different document must not inherit the previous one's tiles");
        assert_eq!(cache.used_bytes(), 0, "cleared entries must return their bytes to the budget");
        assert_eq!(cache.pending_count(), 0, "a request in flight for the previous document must be forgotten");
        assert!(
            cache.wants(&build_request(0, 0, 1.0)),
            "the new document must rasterize its own pixels for a colliding key, not reuse what is cached"
        );
    }

    #[test]
    fn releases_its_values_when_entries_are_dropped() {
        use std::rc::Rc;
        let witness = Rc::new(());
        let mut cache: TileCache<Rc<()>> = TileCache::new(10);
        cache.insert(build_request(0, 1, 1.0), Rc::clone(&witness), 500);
        assert_eq!(Rc::strong_count(&witness), 2);
        cache.retain_revision(2);
        assert_eq!(
            Rc::strong_count(&witness),
            1,
            "evicting an entry must drop its value; for a TextureHandle that drop is the only way GPU memory is freed"
        );
    }
}
