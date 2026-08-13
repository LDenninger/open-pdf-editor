# Contract reference

Normative reference for every contract defined in `opdf-core`. This document
is the thing a session reads instead of `opdf-core/src/`, so signatures are
copied verbatim from the source, not paraphrased. If this document and the
source ever disagree, the source wins and this document is out of date —
fix it in the same change that changes the source.

All items below live in the `opdf-core` crate
(`/home/luis/Projects/open-pdf-editor/workspaces/opdf/opdf-core/src/`) unless
otherwise noted. File paths are relative to that crate's `src/`.

## Table of contents

- [Page value types](#page-value-types) — `page.rs`
- [`Document`](#document) — `document.rs`
- [`DocumentIo`](#documentio) — `document.rs`
- [`DocumentSnapshot`](#documentsnapshot) — `document.rs`
- [`Command`](#command) — `command.rs`
- [`RenderRequest`](#renderrequest) — `render.rs`
- [`Tile`](#tile) — `render.rs`
- [`RenderResponse`](#renderresponse) — `render.rs`
- [`RenderService`](#renderservice) — `render.rs`
- [Why the render service mentions no document type](#why-the-render-service-mentions-no-document-type)
- [What the contract-assertion functions prove](#what-the-contract-assertion-functions-prove)
- [Known gaps](#known-gaps)

---

## Page value types

**File:** `page.rs`
**Implemented by:** `opdf-core` itself (these are concrete types, not traits).
**Used by:** every crate.

### `PageId`

```rust
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct PageId(u64);

impl PageId {
    pub const fn new(raw: u64) -> Self;
    pub const fn get(self) -> u64;
}

impl std::fmt::Display for PageId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result;
}
```

**Purpose:** stable page identity, unaffected by reordering, insertion, or
removal. Mutation APIs address pages by `PageId` and use indices only to
express where an insertion lands. `Display` renders as `page#{raw}` (e.g.
`page#3`) — this exact format is asserted by
`command.rs`'s `labels_describe_the_change_for_the_undo_menu` test, so do not
change it without updating that test and any UI text that embeds it.

**Requirement:** prefer `PageIdAllocator` over calling `PageId::new` directly
outside of tests and fakes, so identities within one document stay unique.

### `PageIdAllocator`

```rust
#[derive(Debug, Default)]
pub struct PageIdAllocator {
    next: u64,
}

impl PageIdAllocator {
    pub fn allocate(&mut self) -> PageId;
}
```

**Purpose:** hands out identifiers that are unique within one document.
**Requirement:** two consecutive `allocate()` calls on the same allocator
must return distinct ids (`allocates_distinct_identifiers`).

### `Rotation`

```rust
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub enum Rotation {
    #[default]
    None,
    Quarter,
    Half,
    ThreeQuarter,
}

impl Rotation {
    pub const fn degrees(self) -> u16;
    pub fn from_degrees(degrees: i32) -> Result<Self>;
    pub fn rotated_by(self, other: Self) -> Self;
    pub const fn swaps_axes(self) -> bool;
}
```

**Purpose:** page rotation in quarter turns clockwise — the only values PDF
permits.

**Behavioural requirements** (enforced by `page.rs`'s `#[cfg(test)]` module):

- `degrees()` maps `None → 0`, `Quarter → 90`, `Half → 180`,
  `ThreeQuarter → 270`.
- `from_degrees` accepts any `i32`, including negative and overlarge values,
  and normalizes modulo 360 (`accepts_negative_and_overlarge_degrees`:
  `-90 → ThreeQuarter`, `450 → Quarter`, `0 → None`).
- `from_degrees` returns `Error::Unsupported` for a value that is not a
  multiple of 90 (`rejects_degrees_that_are_not_quarter_turns`).
- `rotated_by` composes two rotations with wraparound
  (`composes_rotations_with_wraparound`: `ThreeQuarter.rotated_by(Half) ==
  Quarter`).
- `swaps_axes()` is `true` only for `Quarter` and `ThreeQuarter`.

`Hash` is derived so that `RenderRequest` (which contains a `Rotation`) can be
used as a cache key — see [`RenderRequest`](#renderrequest).

### `PageSize`

```rust
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct PageSize {
    pub width_pt: f32,
    pub height_pt: f32,
}

impl PageSize {
    pub const A4: Self;      // 595.0 x 842.0
    pub const LETTER: Self;  // 612.0 x 792.0
    pub const fn new(width_pt: f32, height_pt: f32) -> Self;
}
```

**Purpose:** page dimensions in PDF points (1 point = 1/72 inch), **before**
any rotation is applied.

### `PageInfo`

```rust
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct PageInfo {
    pub id: PageId,
    pub size: PageSize,
    pub rotation: Rotation,
}

impl PageInfo {
    pub const fn display_size(&self) -> PageSize;
}
```

**Purpose:** everything the UI needs to know about a page without reading its
content.

**Behavioural requirements:**

- `display_size()` swaps width and height when `rotation.swaps_axes()` is
  `true`, and returns `size` unchanged otherwise
  (`quarter_turns_swap_display_dimensions`,
  `half_turns_preserve_display_dimensions`).

---

## `Document`

**File:** `document.rs`
**Implemented by:** `opdf-core::fakes::VecDocument` (in-memory fake, no
content). `opdf-pdf` (Track A) implements it over a real PDF file; that
implementation does not exist yet.

```rust
pub trait Document {
    fn page_count(&self) -> usize;
    fn page_ids(&self) -> Vec<PageId>;
    fn page(&self, id: PageId) -> Result<PageInfo>;
    fn index_of(&self, id: PageId) -> Result<usize>;
    fn remove_page(&mut self, id: PageId) -> Result<()>;
    fn move_page(&mut self, id: PageId, to_index: usize) -> Result<()>;
    fn set_rotation(&mut self, id: PageId, rotation: Rotation) -> Result<()>;
    fn insert_page(&mut self, at_index: usize, size: crate::page::PageSize) -> Result<PageId>;
    fn import_pages(&mut self, source: &Self, ids: &[PageId], at_index: usize) -> Result<Vec<PageId>>
    where
        Self: Sized;
}
```

**Purpose:** a paginated document that can be inspected and structurally
modified. Pages are addressed by `PageId`, which survives reordering.
Indices appear only as insertion positions and are always interpreted
against the document's state at the moment of the call.

**Behavioural requirements**, as enforced by
`opdf_core::contract::assert_document_contract` (`contract/document.rs`) —
every implementation must pass this function unmodified:

| Requirement | Source assertion |
| --- | --- |
| `page_count()` and `page_ids().len()` agree with the number of pages present | `assert_reports_its_page_count` |
| `page_ids()` returns identities in document order — `index_of` on the `n`-th id returns `n` | `assert_lists_ids_in_order` |
| `page()` and `index_of()` return an error for an unknown `PageId` | `assert_rejects_unknown_page_ids` |
| `remove_page` reduces the count by one, makes the removed id unresolvable, and does not disturb the identity or order of surviving pages | `assert_removal_preserves_other_identities` |
| `move_page` reorders pages to the requested position without changing the page count or any page's identity | `assert_move_reorders_without_changing_identity` |
| `move_page` rejects a `to_index` beyond the valid range and leaves order untouched on rejection | `assert_move_rejects_out_of_range_targets` |
| `set_rotation` round-trips: setting then reading back returns the same rotation, including reverting to `Rotation::None` | `assert_rotation_round_trips` |
| `insert_page` returns an id not already in use, places the page at the requested index, increases the count by one, and rejects a position beyond the document | `assert_insert_returns_a_fresh_identity` |
| `import_pages` preserves the order of the requested source ids, allocates fresh ids in the target, inserts them at the requested position, and shifts existing target pages after the insertion point | `assert_import_preserves_order_and_allocates_fresh_ids` |
| `import_pages` rejects a position beyond the target document and leaves it untouched on rejection | `assert_import_rejects_out_of_range_positions` |
| `remove_page`, `move_page`, and `set_rotation` all reject an unknown `PageId` and leave the document untouched on rejection | `assert_mutations_reject_unknown_page_ids` |
| `import_pages` rejects a request naming an unknown source page and leaves the target untouched | `assert_import_rejects_unknown_source_pages` |
| `insert_page` and `import_pages` accept a position equal to `page_count()` as a valid append, not an out-of-bounds error | `assert_append_positions_are_valid` |

**Error semantics:** unknown `PageId` → `Error::PageNotFound`; index beyond
range → `Error::IndexOutOfBounds { index, page_count }`. These are not merely
documented: `assert_document_contract` pins each one with a `matches!`
assertion on the specific variant, so an implementation that returns
`Error::Malformed` for everything fails the suite. Where a call could plausibly
report either — `move_page(unknown_id, 0)` — `PageNotFound` wins: the identity
is checked before the index. `IndexOutOfBounds::page_count` is **the number of
pages present when the operation was attempted**, which for `move_page` means
the count *before* the page is lifted out, not after — a three-page document
rejecting `to_index = 99` reports "out of bounds for 3 pages"
(`reports_the_pre_move_page_count_when_rejecting_a_target`, in
`fakes/vec_document.rs`). See
[`error.rs`](../../opdf-core/src/error.rs), copied in full below for
convenience:

```rust
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("malformed pdf: {0}")]
    Malformed(String),
    #[error("unsupported: {0}")]
    Unsupported(String),
    #[error("page not found: {0}")]
    PageNotFound(PageId),
    #[error("index {index} out of bounds for {page_count} pages")]
    IndexOutOfBounds { index: usize, page_count: usize },
    #[error("render failed: {0}")]
    Render(String),
}
```

`move_page`'s bounds rule (from the doc comment on the trait, not restated by
the contract suite's assertions above but load-bearing): removing the page
first leaves `page_count() - 1` pages, so `to_index` is valid if
`to_index <= page_count() - 1`, where `page_count()` is the value **before**
the move.

---

## `DocumentIo`

**File:** `document.rs`
**Implemented by:** nobody yet. `opdf-pdf` (Track A) implements it; that
implementation does not exist yet. No fake implements `DocumentIo` — see
below.

```rust
pub trait DocumentIo: Document + Sized {
    fn open(path: &Path) -> Result<Self>;
    fn save_incremental(&mut self, path: &Path) -> Result<()>;
    fn save_compacted(&mut self, path: &Path) -> Result<()>;
}
```

**Purpose:** reading a document from disk and writing it back. Kept as a
separate trait from `Document` specifically so that in-memory fakes
(`VecDocument`) can satisfy the structural contract without inventing a file
format — `VecDocument` implements `Document` but not `DocumentIo`.

**Behavioural requirements (from the doc comments; no contract-suite function
exists for this trait yet — Track A adds one alongside its implementation):**

- `save_incremental` is the default save path: it appends an incremental
  update to the original bytes rather than rewriting the file. It must be
  fast on large files and must preserve structure the implementation does
  not understand, per the project's correctness promise (see the top-level
  `README.md`).
- `save_compacted` writes a freshly serialized document, discarding
  unreferenced objects. It is slower than `save_incremental` and lossy with
  respect to structure the implementation does not model, so it is only ever
  invoked on an explicit user request — never as a default or automatic
  fallback.

---

## `DocumentSnapshot`

**File:** `document.rs`
**Implemented by:** `opdf-core` itself (concrete struct).

```rust
#[derive(Clone, PartialEq, Debug, Default)]
pub struct DocumentSnapshot {
    pub pages: Vec<PageInfo>,
}

impl DocumentSnapshot {
    pub fn of<D: Document + ?Sized>(document: &D) -> Result<Self>;
    pub fn page_count(&self) -> usize;
}
```

**Purpose:** an immutable copy of a document's page list. The UI holds a
snapshot rather than the document itself — see
["Why the render service mentions no document type"](#why-the-render-service-mentions-no-document-type).

**Behavioural requirement:** `DocumentSnapshot::of` captures pages in
document order, matching `document.page_ids()`
(`snapshots_pages_in_document_order`, in `fakes/vec_document.rs`).

---

## `Command`

**File:** `command.rs`
**Implemented by:** no production implementation yet. `opdf-ops` (Track C)
implements it for merge, split, reorder, rotate, delete, and extract; that
implementation does not exist yet. `command.rs`'s own test module contains a
minimal example (`SetRotation`) that exists only to prove the trait is
usable — it is test-only, not a fake for reuse.

```rust
pub trait Command<D: Document>: Send {
    fn apply(&self, document: &mut D) -> Result<Box<dyn Command<D>>>;
    fn label(&self) -> String;
}
```

**Purpose:** every mutation to a document is expressed as an invertible
command. Undo/redo is a property of the architecture, not a feature added
later (see the top-level `README.md`'s "Load-bearing decisions").

**Why `Send` is a supertrait:** the render worker thread owns the document, so
the UI thread cannot hold a `Document` at all (see
["Why the render service mentions no document type"](#why-the-render-service-mentions-no-document-type)).
Commands are therefore built on one thread and applied on another, and the undo
stack storing the `Box<dyn Command<D>>` inverses may be owned by either side.
The bound sits on the trait rather than at each use site so that
`Box<dyn Command<D>>` is sendable everywhere it appears, including as `apply`'s
return type. A command that captures a non-`Send` value (an `Rc`, a raw handle)
will not compile — capture the data by value, or make it `Send`.

**Behavioural requirements**, demonstrated by `command.rs`'s test module
(there is no reusable contract-suite function for `Command` — each track's
commands are exercised directly by their own tests):

- Applying a command returns the command that reverses it; applying that
  inverse restores the document to its prior state exactly
  (`applying_the_returned_inverse_restores_the_original_state`). That test
  compares whole `DocumentSnapshot`s before and after, not a single field —
  copy that shape when testing a new command, because a one-field check
  passes a lossy inverse that disturbs page order or identity.
- `Box<dyn Command<D>>` is `Send`, so an undo stack of inverses can be moved
  between threads (`boxed_commands_cross_thread_boundaries`).
- On failure, the document must be left exactly as it was found (stated on
  the trait's doc comment; not separately unit-tested at this layer because
  `VecDocument`'s own mutation methods already guarantee it — see the
  `Document` contract table above).
- `label()` returns a short description in sentence case, suitable for an
  undo menu entry, e.g. `"Rotate page#3 to 180 degrees"`
  (`labels_describe_the_change_for_the_undo_menu`). Note the label embeds
  `PageId`'s `Display` format (`page#{raw}`) verbatim.

---

## `RenderRequest`

**File:** `render.rs`
**Implemented by:** `opdf-core` itself (concrete struct).

```rust
#[derive(Clone, Copy, Debug)]
pub struct RenderRequest {
    pub page: PageId,
    pub scale: f32,
    pub rotation: Rotation,
}

impl RenderRequest {
    pub fn new(page: PageId, scale: f32) -> Result<Self>;
    pub fn with_rotation(self, rotation: Rotation) -> Self;
}

impl PartialEq for RenderRequest { /* compares scale bitwise */ }
impl Eq for RenderRequest {}
impl std::hash::Hash for RenderRequest { /* hashes scale.to_bits() */ }
```

**Purpose:** a request to rasterize one page. `scale` is a zoom factor where
`1.0` renders at 72 dpi — one pixel per PDF point. `rotation` is a view
rotation applied **on top of** the rotation already stored on the page (see
`assert_view_rotation_composes_with_page_rotation` in the `RenderService`
table below).

**Behavioural requirement:** `new` returns `Error::Unsupported` for a scale
that is not finite and positive — this rejects `0.0`, negative values, `NaN`,
and infinities (`rejects_a_non_positive_scale`, in `render.rs`'s test
module).

**Usable as a cache key.** `Eq` and `Hash` are implemented by hand rather than
derived, because a derived `Eq` is impossible with an `f32` field. Both treat
`scale` **bitwise**, via `f32::to_bits`, and `PartialEq` is written by hand to
match so that equality and hashing agree. The consequence, which every track
must share rather than each inventing its own key type: two requests whose
scales differ only by floating-point noise are **distinct** keys. A caller that
wants nearby zoom levels to share one cache entry must quantise `scale` itself
before constructing the request. `RenderRequest::new` never produces `NaN` or a
signed zero, so bitwise equality is reflexive in practice
(`serves_as_a_hash_map_key`, in `render.rs`'s test module).

**Known gap:** `new` does not reject a finite positive scale that is
absurdly large (e.g. `1e30`) — see [Known gaps](#known-gaps).

---

## `Tile`

**File:** `render.rs`
**Implemented by:** `opdf-core` itself (concrete struct; fields are private,
only accessible through the methods below).

```rust
#[derive(Clone, PartialEq, Debug)]
pub struct Tile {
    width: u32,
    height: u32,
    pixels: Vec<u8>,
}

impl Tile {
    pub fn new(width: u32, height: u32, pixels: Vec<u8>) -> Result<Self>;
    pub const fn width(&self) -> u32;
    pub const fn height(&self) -> u32;
    pub fn pixels(&self) -> &[u8];
    pub fn pixel(&self, x_px: u32, y_px: u32) -> Result<[u8; 4]>;
}
```

**Purpose:** a rasterized image, stored as 8-bit RGBA in row-major order.

**Behavioural requirements** (`render.rs`'s test module):

- `new` returns `Error::Render` if either dimension is zero, or if
  `pixels.len() != width * height * 4` (`rejects_a_buffer_of_the_wrong_length`).
- `new` computes the expected length with `checked_mul` and returns
  `Error::Render` naming the dimensions when `width * height * 4` overflows
  `usize` on the target, rather than wrapping and admitting a buffer far too
  short for the dimensions claimed
  (`rejects_dimensions_whose_buffer_length_overflows`).
- `new` accepts a buffer of exactly the expected length
  (`accepts_a_buffer_of_the_exact_length`).
- `pixel(x_px, y_px)` reads the four bytes at row-major offset
  `(y_px * width + x_px) * 4`, returned as `[r, g, b, a]`
  (`reads_pixels_in_row_major_order`).
- `pixel` returns `Error::Render` for coordinates outside the tile
  (`rejects_pixels_outside_the_tile`).

---

## `RenderResponse`

**File:** `render.rs`
**Implemented by:** `opdf-core` itself (concrete enum).

```rust
#[derive(Clone, PartialEq, Debug)]
pub enum RenderResponse {
    Ready {
        request: RenderRequest,
        tile: Tile,
    },
    Failed {
        request: RenderRequest,
        reason: String,
    },
}

impl RenderResponse {
    pub const fn request(&self) -> &RenderRequest;
}
```

**Purpose:** the outcome of a submitted `RenderRequest`. `Failed` exists
because rasterization can fail per-page (a damaged page, an unsupported
feature) without that being fatal to the document — the UI shows a
placeholder instead of closing the document, per the doc comment on the
`Failed` variant.

**Behavioural requirement:** `request()` returns the originating request for
either variant, so a caller can match a response back to what it submitted
without matching on the enum first.

---

## `RenderService`

**File:** `render.rs`
**Implemented by:** `opdf-core::fakes::FakeRenderService` (draws flat colored
rectangles at the correct dimensions). `opdf-render` (Track B) implements it
over PDFium; that implementation does not exist yet.

```rust
pub trait RenderService: Send {
    fn submit(&self, request: RenderRequest);
    fn poll(&self) -> Vec<RenderResponse>;
}
```

**Purpose:** asynchronous rasterization, as seen by the user interface.

**Hard requirement, stated on the trait's doc comment:** implementations
must never block in `poll()`. The caller runs on the UI thread; a stalled
poll drops frames.

**Coalescing rule:** submitting the same request twice is permitted.
**Identical pending requests may be answered by a single response; distinct
requests each receive their own.** The contract suite's batch assertion submits
two *distinct* requests and requires two responses, so it does not constrain
coalescing either way.

**Asynchrony and the contract suite:** because a real implementation answers on
a worker thread, the suite never assumes a response is ready on the first
`poll`. It calls a private `drain_responses(&service, expected)` helper that
polls in a loop with a 5 ms sleep until `expected` responses arrive or a
2-second deadline passes, then asserts the count *before* indexing. A
synchronous implementation satisfies this on the first iteration; an
asynchronous one is given time; a broken one fails on the length assertion with
a message rather than hanging or panicking on a bare index. Two assertions
deliberately use a single direct `poll()` instead, because there the
requirement is that *nothing* arrives — waiting would defeat the check:
polling an idle service, and confirming a drained batch is not redelivered.

**Behavioural requirements**, as enforced by
`opdf_core::contract::assert_render_service_contract`
(`contract/render.rs`) — every implementation must pass this function
unmodified. The suite builds a fixed two-page `DocumentSnapshot` (page 1:
A4, `Rotation::None`; page 2: A4, `Rotation::Quarter`):

| Requirement | Source assertion |
| --- | --- |
| Polling a service with nothing submitted returns an empty vector | `assert_polling_an_idle_service_is_empty` |
| A submitted request produces exactly one response, identifying the request it answers; a response is never delivered twice (identical pending requests may share one response — see the coalescing rule above) | `assert_every_request_is_answered_once` |
| Tile pixel dimensions equal `page_size_pt * scale`, rounded (A4 at scale 2.0 → 1190x1684) | `assert_tile_dimensions_follow_scale` |
| The formula is exactly `round(size_pt * scale)` — round to **nearest**, floored at one pixel. A4 at scale 0.51 → 303x429 (rounding up would give 304x430); A4 at scale 0.0005 → 1x1, never a zero-sized tile | `assert_pixel_dimensions_round_to_nearest_with_a_one_pixel_floor` |
| A page's stored rotation swaps the tile's width and height when it is a quarter turn (A4 stored at `Rotation::Quarter`, scale 1.0 → 842x595) | `assert_rotation_swaps_tile_axes` |
| Submitting a request for an unknown `PageId` still produces exactly one response, and it is `RenderResponse::Failed` — never a panic | `assert_unknown_pages_fail_without_panicking` |
| The request's `rotation` composes with the page's stored rotation (via `Rotation::rotated_by`), and the resulting tile dimensions reflect the **composed** rotation, not either rotation alone | `assert_view_rotation_composes_with_page_rotation` |
| Submitting multiple **distinct** requests before polling produces one response per request, each answering its own request | `assert_batched_requests_each_receive_a_response` |

**Known gap:** `RenderRequest::new` (consumed here) accepts any finite
positive scale, including absurdly large ones — see
[Known gaps](#known-gaps).

---

## Why the render service mentions no document type

`RenderService::submit` takes a `RenderRequest` — a `PageId`, a scale, and a
rotation — never a `Document` or a document handle of any kind. This is
deliberate, not an oversight.

The rasterizer that eventually sits behind `RenderService` (PDFium, via
`opdf-render`) is **not thread-safe**. Per the top-level `README.md`'s
"Load-bearing decisions": a single dedicated render worker thread owns the
rasterizer, so the UI thread never blocks on rendering and no other thread
ever touches the rasterizer concurrently. That worker thread also owns
**the document** — because rendering page N requires reading the document's
object graph for page N, and a document handle capable of producing that is
exactly as unsafe to share across threads as the rasterizer itself in a
PDFium-backed implementation.

The UI thread therefore cannot hold a `Document` at all once a render
service is wired up. It holds a `DocumentSnapshot` — an immutable,
`Send`-safe copy of the page list — and submits `RenderRequest`s that name
pages by `PageId`. The render worker resolves `PageId → page content` against
the document *it* owns, off the UI thread, and returns `Tile`s. This is why
every type in `render.rs` (`RenderRequest`, `Tile`, `RenderResponse`,
`RenderService`) is expressed purely in terms of `PageId` and pixel data,
never `Document` or `DocumentIo` — the contract is built so that a
thread-unsafe document/rasterizer pair can be owned entirely by one thread,
while the UI only ever sees values it is safe to hold.

`FakeRenderService` mirrors this: it takes ownership of a `DocumentSnapshot`
at construction (not a live `Document`), which is why building it is
`Fn(DocumentSnapshot) -> S` in the contract suite rather than
`Fn(&mut dyn Document) -> S`.

---

## What the contract-assertion functions prove

Two functions exist to be called by every implementation crate's own test
suite, under the `contract-tests` feature:

```rust
pub fn assert_document_contract<D, F>(make_document: F)
where
    D: Document,
    F: Fn(usize) -> D;

pub fn assert_render_service_contract<S, F>(make_service: F)
where
    S: RenderService,
    F: Fn(DocumentSnapshot) -> S;
```

**What passing proves:** the implementation satisfies every behavioural
requirement listed in the tables above for `Document` and `RenderService`
respectively — identity stability, ordering, error semantics on invalid
input, and (for rendering) correct tile dimensions under scale and rotation
composition. Both functions panic with a descriptive message on the first
violated requirement, so a track failing the suite gets a specific pointer
to what broke, not a bare `assert` failure.

"Error semantics" here means the **specific variant**, not merely that an
error was returned: every failure assertion in `assert_document_contract`
uses `matches!` against the documented variant, so a CLI matching on
`Error::PageNotFound` to print "no such page" can rely on getting it.

**What passing does not prove:**

- **Nothing about `DocumentIo`.** No contract-suite function exists for
  `open`/`save_incremental`/`save_compacted` yet. `assert_document_contract`
  never touches disk. Track A must write its own coverage for I/O and the
  round-trip correctness promise (structurally identical output when nothing
  is edited) — see the top-level `README.md`.
- **Nothing about `Command`.** There is no `assert_command_contract`. Each
  command Track C writes needs its own apply/inverse test, following the
  pattern in `command.rs`'s `SetRotation` example.
- **Nothing about performance, concurrency stress, or real-world file
  survival.** The `Document` suite runs entirely in memory against small,
  synthetic page counts. The `RenderService` suite runs against a
  fixed two-page snapshot and never touches an actual rasterizer or real
  PDF bytes. Golden-image comparisons, fuzzing, and the ugly-PDF corpus
  (Track E) are separate, additional gates — passing the contract suite is
  necessary but not sufficient for those.
- **Nothing about thread-safety beyond the `Send` bound already required by
  `RenderService: Send`.** The suite calls `submit`/`poll` from a single
  thread; it does not exercise concurrent submission from multiple threads.

---

## Known gaps

Recorded during review so a track does not silently rediscover — and
silently "fix" or work around — the same issue twice.

1. ~~**`Tile::new` integer overflow.**~~ **Closed.** `Tile::new` now computes
   the expected buffer length with `checked_mul` and returns `Error::Render`
   naming the dimensions when it overflows, on any target width of `usize`.

2. **`RenderRequest::new` accepts absurd scales.** *Still open at the
   `RenderRequest` level.* The only validation is "finite and positive," so a
   scale of `1e30` passes `new`. There is no upper bound on the type and no
   contract-suite assertion exercising one, because the right ceiling depends
   on the rasterizer and the memory budget — that decision is left to whichever
   track first renders against a real rasterizer (most likely Track B).

   What **is** settled is that an oversized request must not take the process
   down. `FakeRenderService` clamps at a private
   `MAX_TILE_PIXELS = 64 * 1024 * 1024` (64 mega-pixels, 256 MiB of RGBA):
   it computes `width * height` with `checked_mul` and, when the result is
   absent or above the limit, returns `RenderResponse::Failed` naming the
   requested dimensions and the limit. It does **not** panic, and does **not**
   silently render a smaller tile than asked for
   (`fails_an_absurd_scale_instead_of_allocating_or_overflowing`). Without
   this, an `f32 as u32` cast saturating to `u32::MAX` made
   `width * height * 4` overflow `usize` even on 64-bit — a debug-build panic,
   or a release-build wrap followed by a ~1.8e19-iteration loop. Any real
   implementation is expected to behave the same way: fail the request, loudly,
   with a reason.
