# Contract changes

Breaking changes to `opdf-core`, newest first. Additive changes are not
recorded here.

| Date | Change | Rationale | Landed in |
| --- | --- | --- | --- |
| 2026-08-13 | `Command<D>` gains `Send` as a supertrait | The render worker owns the document, so commands and the undo stack of `Box<dyn Command<D>>` inverses cross threads. Adding the bound after Track C starts would invalidate every command written in the meantime. | Phase 0 |
| 2026-08-13 | `RenderRequest` implements `Eq` and `Hash`; `PartialEq` becomes a manual impl comparing `scale` bitwise | A tile cache and the UI's tile map both need `RenderRequest` as a key. Without this each track invents its own quantised-scale key, and those keys disagree at integration. `Rotation` gains a derived `Hash` to support it. | Phase 0 |
| 2026-08-13 | Initial contract surface | Phase 0 | — |
