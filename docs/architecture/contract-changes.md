# Contract changes

Breaking changes to `opdf-core`, newest first. Additive changes are not
recorded here.

| Date | Change | Rationale | Landed in |
| --- | --- | --- | --- |
| 2026-08-13 | `Document` gains `fn revision(&self) -> u64`; `DocumentSnapshot` gains a `revision` field, captured by `of`; `RenderRequest` gains a `revision` field, taken explicitly by `new(page, revision, scale)` and included in `PartialEq`/`Eq`/`Hash` | Nothing in a render request identified document state, so after `set_rotation(page_3, Quarter)` a tile cached for the byte-identical `RenderRequest` was stale and the canvas kept showing the old orientation. Every mutation must now advance the revision on success and leave it untouched on failure; the renderer carries and echoes the revision but never validates it. `new` takes it positionally rather than defaulting it, because a caller who forgets it silently reintroduces the same stale-tile bug. Adding this after the tracks start would break the one crate all five depend on. | Phase 0, `ab507f4..fc59548` — `ddfcae3` the contract, `fc59548` the reference, plus this entry |
| 2026-08-13 | `Command<D>` gains `Send` as a supertrait | The render worker owns the document, so commands and the undo stack of `Box<dyn Command<D>>` inverses cross threads. Adding the bound after Track C starts would invalidate every command written in the meantime. | Phase 0 |
| 2026-08-13 | `RenderRequest` implements `Eq` and `Hash`; `PartialEq` becomes a manual impl comparing `scale` bitwise | A tile cache and the UI's tile map both need `RenderRequest` as a key. Without this each track invents its own quantised-scale key, and those keys disagree at integration. `Rotation` gains a derived `Hash` to support it. | Phase 0 |
| 2026-08-13 | Initial contract surface | Phase 0 | — |
