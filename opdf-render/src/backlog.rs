//! The worker's pending-request queue.
//!
//! Requests arrive faster than they can be served whenever a user scrolls, so
//! the queue is bounded and served newest-first: the tile under the viewport
//! now matters more than the one that was under it four frames ago. A request
//! evicted to keep the bound is reported to the caller so it can be answered,
//! because the contract promises exactly one response per submitted request.

use std::collections::VecDeque;

use opdf_core::RenderRequest;

/// Requests the worker will hold before superseding the oldest.
///
/// Sixty-four tiles is several screens' worth at any plausible zoom, so a user
/// scrolling normally never reaches it, while a user dragging the scrollbar
/// across a 500-page document cannot make the queue grow without bound.
pub const MAX_BACKLOG: usize = 64;

/// A bounded, newest-first queue of pending render requests.
#[derive(Debug)]
pub struct Backlog {
    queued: VecDeque<RenderRequest>,
    capacity: usize,
}

impl Backlog {
    /// An empty backlog holding at most `capacity` requests.
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            queued: VecDeque::with_capacity(capacity),
            capacity: capacity.max(1),
        }
    }

    /// Queue a request, returning the one evicted to make room, if any.
    ///
    /// A request identical to one already queued is coalesced: it is not queued
    /// twice and nothing is evicted.
    pub fn push(&mut self, request: RenderRequest) -> Option<RenderRequest> {
        if self.queued.contains(&request) {
            return None;
        }
        self.queued.push_back(request);
        if self.queued.len() > self.capacity {
            return self.queued.pop_front();
        }
        None
    }

    /// Take the most recently queued request.
    pub fn take_newest(&mut self) -> Option<RenderRequest> {
        self.queued.pop_back()
    }

    /// Whether anything is waiting.
    pub fn is_empty(&self) -> bool {
        self.queued.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use opdf_core::{DocumentId, PageId};
    /// The document identity every request in this module names.
    ///
    /// Fixed for the whole module so that two requests differ only in the fields
    /// the test varies. Minted rather than a constant because an identity is
    /// deliberately unforgeable: [`DocumentId`] has no `const` constructor.
    fn test_document() -> DocumentId {
        static DOCUMENT: std::sync::OnceLock<DocumentId> = std::sync::OnceLock::new();
        *DOCUMENT.get_or_init(DocumentId::new_unique)
    }

    fn build_request(page: u64) -> RenderRequest {
        RenderRequest::new(test_document(), PageId::new(page), 7, 1.0).unwrap()
    }

    #[test]
    fn serves_the_newest_request_first() {
        let mut backlog = Backlog::with_capacity(MAX_BACKLOG);
        backlog.push(build_request(1));
        backlog.push(build_request(2));
        backlog.push(build_request(3));

        assert_eq!(backlog.take_newest(), Some(build_request(3)));
        assert_eq!(backlog.take_newest(), Some(build_request(2)));
        assert_eq!(backlog.take_newest(), Some(build_request(1)));
        assert!(backlog.is_empty());
    }

    #[test]
    fn coalesces_an_identical_pending_request() {
        let mut backlog = Backlog::with_capacity(MAX_BACKLOG);
        assert_eq!(backlog.push(build_request(1)), None);
        assert_eq!(backlog.push(build_request(1)), None, "a duplicate must not evict anything");

        assert_eq!(backlog.take_newest(), Some(build_request(1)));
        assert!(backlog.is_empty(), "an identical pending request must be queued once");
    }

    #[test]
    fn supersedes_the_oldest_request_when_full() {
        let mut backlog = Backlog::with_capacity(2);
        assert_eq!(backlog.push(build_request(1)), None);
        assert_eq!(backlog.push(build_request(2)), None);

        let evicted = backlog.push(build_request(3));
        assert_eq!(evicted, Some(build_request(1)), "the oldest queued request must be the one superseded");
        assert_eq!(backlog.take_newest(), Some(build_request(3)));
        assert_eq!(backlog.take_newest(), Some(build_request(2)));
        assert!(backlog.is_empty());
    }

    #[test]
    fn never_grows_beyond_its_capacity() {
        let mut backlog = Backlog::with_capacity(8);
        let mut evictions = 0_usize;
        for ii in 0..200_u64 {
            if backlog.push(build_request(ii)).is_some() {
                evictions += 1;
            }
        }
        assert_eq!(evictions, 192, "every request beyond the capacity must evict exactly one older request");

        let mut drained = 0_usize;
        while backlog.take_newest().is_some() {
            drained += 1;
        }
        assert_eq!(drained, 8, "the backlog must hold exactly its capacity after 200 pushes");
    }
}
