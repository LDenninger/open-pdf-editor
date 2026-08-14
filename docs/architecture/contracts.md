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
- [Compaction destroys undo of deletions](#compaction-destroys-undo-of-deletions)
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

**Requirement: never persist a `PageId`.** It is unique within one document *in
one process* and has no meaning after a save-and-reopen — the PDF format has
nowhere to keep it. `PageId::get`'s doc comment ("for storage in formats that
cannot hold a `PageId`") is about in-memory interchange, not about writing the
number to disk. See [Known gaps](#known-gaps) item 5, which is binding on every
track.

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
content) and `opdf-pdf::PdfDocument` (Track A), over a real PDF file.

```rust
pub trait Document {
    fn revision(&self) -> u64;
    fn page_count(&self) -> usize;
    fn page_ids(&self) -> Vec<PageId>;
    fn page(&self, id: PageId) -> Result<PageInfo>;
    fn index_of(&self, id: PageId) -> Result<usize>;
    fn remove_page(&mut self, id: PageId) -> Result<()>;
    fn restore_page(&mut self, id: PageId, at_index: usize) -> Result<()>;
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

**The trait is object-safe, and must stay that way.** `import_pages` is the
only method that names `Self` in its signature, and it carries
`where Self: Sized` precisely so that it is excluded from the vtable rather
than poisoning the whole trait. No track may add an associated type, a generic
method, or another `Self`-typed argument without the same escape hatch.
`document.rs`'s `stays_object_safe` test pins this with a single line —
`let _: Option<&dyn Document> = None;` — so the breakage surfaces here, as a
compile error in `opdf-core`, rather than in whichever dependent crate first
tries to hold a `&dyn Document`.

### The trash model

`restore_page` exists because undo of a page deletion could not restore the
page. `remove_page` destroyed it and `insert_page` only ever creates a *blank*
page under a *fresh* `PageId`, so deleting page 3 and undoing gave an empty
page with a new identity — and every operation built on deletion (split,
delete-selection) inherited the defect.

```rust
/// Restore a page previously removed from this document, with its original
/// identity, geometry, and content, at `at_index`.
///
/// A removed page is retained by the document, unreferenced, until an explicit
/// compaction purges it. This mirrors how PDF incremental save already works:
/// objects are never deleted, only unreferenced. It is what makes undo of a
/// deletion exact rather than approximate.
///
/// Returns [`crate::Error::PageNotFound`] if `id` was never a page of this
/// document, or has been purged. Returns [`crate::Error::IndexOutOfBounds`] if
/// `at_index` exceeds the page count. Returns [`crate::Error::Unsupported`] if
/// `id` is currently present — restoring a live page is a caller error, not a
/// no-op.
///
/// Advances the revision on success.
fn restore_page(&mut self, id: PageId, at_index: usize) -> Result<()>;
```

**Why a trash rather than a caller-held copy.** The alternative — having the
delete command carry the removed page's data in its inverse — requires the
`Document` contract to expose a page's full content as a value type that a
command can hold and hand back. It does not, and making it do so would mean
modelling every PDF page's object graph in `opdf-core`, which is exactly the
job the contract layer refuses to take on. Retaining the page inside the
document instead costs one indirection and is what the file format already
does: an incremental save never deletes an object, it only stops referencing
it. The trash model therefore aligns the in-memory representation with the
on-disk one, and `save_compacted` — already specified as the only lossy,
explicitly-requested save path — is the natural place for the purge.

The rules, all enforced by the contract suite:

- **A restored page is the original page**, not a reconstruction: same
  `PageId`, same `PageSize`, same `Rotation` as it had at the moment of
  removal. An implementation that returns a blank page of default geometry
  under the right id fails the suite.
- **`restore_page` returns `()`, not a `PageId`.** The identity is the one the
  caller passed in. That is the entire point of the method, and a returned id
  would invite a caller to believe it might differ.
- **Restoring a page that is currently present is `Error::Unsupported`**, never
  a silent success and never a duplicate. It is a caller error — a sign the
  undo stack has lost track of what it undid.
- **`at_index == page_count()` is a valid append**, not an out-of-range error.
  Without this the deletion of a *last* page could not be undone.
- **Identity is resolved before the index**, matching `move_page`: a restore
  naming an unknown id and an out-of-range index reports `PageNotFound`.
- **A rejected restore consumes nothing.** The page stays in the trash and a
  later, valid restore still succeeds.
- **Purging is explicit.** A removed page is retained until a compaction
  discards it; nothing in the contract permits an implementation to drop a
  trashed page on its own schedule.

### The revision counter

`revision()` is a counter that advances whenever the document's structure
changes. It exists so that a tile cache can key on document state: without it,
a `RenderRequest` built after `set_rotation(page_3, Quarter)` is byte-identical
to one built before it, and the cache serves the old orientation forever.

The rules, all enforced by the contract suite:

- **Every mutating method advances it on success** — `remove_page`,
  `restore_page`, `move_page`, `set_rotation`, `insert_page`, `import_pages`.
  Each is checked individually, so an implementation that advances on five of
  the six fails. `restore_page` is checked inside
  `assert_removed_pages_can_be_restored` rather than alongside the others,
  because it needs a removed page to work with; it is no less binding for that.
  Note in particular that restoring a page advances the revision like any other
  mutation — it does not rewind to the revision the document held before the
  removal, for the same reason undo does not (below).
- **A failed mutation leaves it untouched.** This binds even when the
  implementation mutated internally before discovering the failure:
  `move_page` removes the page before bounds-checking `to_index` and reinserts
  it on rejection, and that path must not advance the revision.
- **Read-only calls never advance it** — `page_count`, `page_ids`, `page`,
  `index_of`, and `revision()` itself.
- **Values are opaque.** Only equality is meaningful — never ordering, never
  arithmetic. A caller may ask "is this the revision my tile was rendered at?"
  and nothing else. `VecDocument` happens to increment by one, and no caller
  may rely on that.
- **Undo advances it like any other mutation.** Applying a command's inverse
  restores the page list but *not* the revision the document had before the
  command; a revision that went backwards would let a cache resurrect entries
  it had already invalidated
  (`applying_the_returned_inverse_restores_the_original_state`, in
  `command.rs`, which therefore compares `snapshot.pages` rather than whole
  snapshots).

**Behavioural requirements**, as enforced by
`opdf_core::contract::assert_document_contract` (`contract/document.rs`) —
every implementation must pass this function unmodified:

| Requirement | Source assertion |
| --- | --- |
| `page_count()` and `page_ids().len()` agree with the number of pages present | `assert_reports_its_page_count` |
| `page_ids()` returns identities in document order — `index_of` on the `n`-th id returns `n` | `assert_lists_ids_in_order` |
| `page()` and `index_of()` return an error for an unknown `PageId` | `assert_rejects_unknown_page_ids` |
| `remove_page` reduces the count by one, makes the removed id unresolvable, and does not disturb the identity or order of surviving pages | `assert_removal_preserves_other_identities` |
| `restore_page` brings a removed page back with its original `PageId`, `PageSize`, and `Rotation`, at the requested index, increasing the count by one | `assert_removed_pages_can_be_restored` |
| `restore_page` accepts `at_index == page_count()` as an append, so the deletion of a last page can be undone | `assert_removed_pages_can_be_restored` |
| `restore_page` rejects an out-of-range index with `Error::IndexOutOfBounds`, leaves the document untouched, and does **not** consume the page — a later valid restore still succeeds | `assert_removed_pages_can_be_restored` |
| `restore_page` rejects an id the document never held with `Error::PageNotFound`, and an id that is currently present with `Error::Unsupported` — never a silent no-op or a duplicated page | `assert_removed_pages_can_be_restored` |
| `restore_page` advances `revision()` on success and leaves it untouched on every failure | `assert_removed_pages_can_be_restored` |
| `move_page` reorders pages to the requested position without changing the page count or any page's identity | `assert_move_reorders_without_changing_identity` |
| `move_page` rejects a `to_index` beyond the valid range and leaves order untouched on rejection | `assert_move_rejects_out_of_range_targets` |
| `set_rotation` round-trips: setting then reading back returns the same rotation, including reverting to `Rotation::None` | `assert_rotation_round_trips` |
| `insert_page` returns an id not already in use, places the page at the requested index, increases the count by one, and rejects a position beyond the document | `assert_insert_returns_a_fresh_identity` |
| `import_pages` preserves the order of the requested source ids, allocates fresh ids in the target, inserts them at the requested position, and shifts existing target pages after the insertion point | `assert_import_preserves_order_and_allocates_fresh_ids` |
| `import_pages` rejects a position beyond the target document and leaves it untouched on rejection | `assert_import_rejects_out_of_range_positions` |
| `remove_page`, `move_page`, and `set_rotation` all reject an unknown `PageId` and leave the document untouched on rejection | `assert_mutations_reject_unknown_page_ids` |
| `import_pages` rejects a request naming an unknown source page and leaves the target untouched | `assert_import_rejects_unknown_source_pages` |
| `insert_page` and `import_pages` accept a position equal to `page_count()` as a valid append, not an out-of-bounds error | `assert_append_positions_are_valid` |
| Each of `remove_page`, `move_page`, `set_rotation`, `insert_page`, and `import_pages` advances `revision()` on success — checked one method at a time (`restore_page` likewise, in its own function above) | `assert_every_mutation_advances_the_revision` |
| A mutation rejected for an unknown `PageId` **or** for an out-of-range index leaves `revision()` untouched, including `move_page`'s remove-then-reinsert path | `assert_failed_mutations_leave_the_revision_untouched` |
| `page_count`, `page_ids`, `page`, and `index_of` never advance `revision()`, and two consecutive `revision()` reads with no mutation between them agree | `assert_read_only_calls_never_advance_the_revision` |

**Error semantics:** unknown `PageId` → `Error::PageNotFound`; index beyond
range → `Error::IndexOutOfBounds { index, page_count }`; a `restore_page`
naming a page that is currently present → `Error::Unsupported`. These are not merely
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

## Compaction destroys undo of deletions

Three already-stated rules combine into a consequence none of them implies
alone, so it is written down here rather than left for a track to
rediscover mid-implementation.

[The trash model](#the-trash-model) keeps a page removed by `remove_page` in
the document, unreferenced, precisely so `restore_page` can bring it back
exactly. `save_compacted` (below, under [`DocumentIo`](#documentio)) exists
to purge unreferenced objects — that is what "compacted" means. A trashed
page **is** an unreferenced object. Put together: **a compacting save
discards everything currently in the trash**, and `restore_page` for any
page removed before that save returns `Error::PageNotFound` afterward,
exactly as if the page had never existed. `save_incremental`, the default
save path, does not purge, so undo of a deletion survives it — compaction is
the one save path that is destructive here, which is consistent with it
already being the only explicitly-requested, non-default save path.

This binds the three tracks starting in parallel on top of `Document`:

- **Track C** (`opdf-ops`, the undo stack): a queued inverse of a deletion
  becomes invalid the instant a compacting save succeeds. The stack must
  drop or invalidate those entries at that point, not let them fail later
  when the user actually presses undo. An inverse returning
  `Error::PageNotFound` when applied is a defect in the undo stack, not an
  acceptable error path.
- **Track D** (`opdf-app`, the interface): compacting is a destructive
  action from the user's point of view — it forecloses undo of every
  deletion made so far in the session — so the interface must say so before
  performing it, not perform it and silently drop undo history.
- **Track A** (`opdf-pdf`): must purge unreferenced objects, including
  trashed pages, in `save_compacted`, and must not purge them in
  `save_incremental`. Already covered by Track A's own plan; recorded here
  so the other two tracks can see why it matters to them too.

This is distinct from [Known gaps item 5](#known-gaps): that item is about
`PageId` having no meaning after a save-and-reopen at all, compacted or not,
which follows from the existing rule that a `PageId` is never persisted. The
rule here is narrower and applies within a single session — it is about
compaction specifically discarding the trash, not about identity failing to
survive a reopen.

**In `opdf-pdf` the purge is not a policy choice.** `save_compacted` builds the
rewritten document and calls `lopdf`'s `prune_objects`, which drops every object
the new page tree does not reach — and a trashed page is exactly that. Its
objects therefore leave the file whether or not the in-memory trash is cleared,
so `save_compacted` clears the trash to match what it just wrote, and does so
only after the file is written successfully. `save_incremental` never prunes and
never purges.

---

## `DocumentIo`

**File:** `document.rs`
**Implemented by:** `opdf-pdf::PdfDocument` (Track A). No fake implements
`DocumentIo` — see below.

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
exists for this trait; `opdf-pdf` covers it with its own round-trip tests in
`src/save.rs`, which assert byte-identical output for an unedited document and
an append-only prefix after an edit):**

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
    pub revision: u64,
}

impl DocumentSnapshot {
    pub fn of<D: Document + ?Sized>(document: &D) -> Result<Self>;
    pub fn page_count(&self) -> usize;
}
```

**Purpose:** an immutable copy of a document's page list, together with the
`Document::revision` it was captured at. The UI holds a snapshot rather than
the document itself — see
["Why the render service mentions no document type"](#why-the-render-service-mentions-no-document-type).

**Behavioural requirements:**

- `DocumentSnapshot::of` captures pages in document order, matching
  `document.page_ids()` (`snapshots_pages_in_document_order`, in
  `fakes/vec_document.rs`).
- `DocumentSnapshot::of` captures `document.revision()` into `revision`, so a
  snapshot taken after a mutation never reports the revision of one taken
  before it (`snapshots_the_revision_alongside_the_pages`, same file). This is
  the value a caller feeds to `RenderRequest::new`: the snapshot is the UI's
  only view of the document, so it must carry everything a render request
  needs.

**Note on `PartialEq`:** because `revision` is a field, two snapshots of a
structurally identical document taken at different revisions are **not** equal.
A test asserting that some operation round-trips should compare
`snapshot.pages`, not the whole snapshot — see the undo rule under
[`Document`](#document).

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
    pub revision: u64,
    pub scale: f32,
    pub rotation: Rotation,
}

impl RenderRequest {
    pub fn new(page: PageId, revision: u64, scale: f32) -> Result<Self>;
    pub fn with_rotation(self, rotation: Rotation) -> Self;
}

impl PartialEq for RenderRequest { /* compares scale bitwise; revision participates */ }
impl Eq for RenderRequest {}
impl std::hash::Hash for RenderRequest { /* hashes revision and scale.to_bits() */ }
```

**Purpose:** a request to rasterize one page. `scale` is a zoom factor where
`1.0` renders at 72 dpi — one pixel per PDF point. `rotation` is a view
rotation applied **on top of** the rotation already stored on the page (see
`assert_view_rotation_composes_with_page_rotation` in the `RenderService`
table below). `revision` is the `Document::revision` the request was built
against, normally read straight off the `DocumentSnapshot` the UI holds.

**Behavioural requirement:** `new` returns `Error::Unsupported` for a scale
that is not finite and positive — this rejects `0.0`, negative values, `NaN`,
and infinities (`rejects_a_non_positive_scale`, in `render.rs`'s test
module). It performs no validation on `revision`, which is an opaque `u64`
and may be any value.

**`revision` is a required argument, on purpose.** There is no default and no
`with_revision` builder step, deliberately, and a track must not add one. A
caller who omits the revision silently reintroduces exactly the bug the field
exists to prevent — a tile cached before an edit served after it — and a bug
that produces stale pixels rather than a compile error is one nobody finds. So
the compiler forces the decision at every call site. `with_rotation` remains a
builder step because forgetting a view rotation is visible on screen
immediately; forgetting a revision is not.

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

`revision` participates in both `PartialEq` and `Hash`, which is the entire
point of the field: **two requests differing only in `revision` are distinct
keys**, so a tile rasterized before a structural change is never returned for a
request built after one, and the pre-change entry stays addressable under its
own revision rather than being overwritten
(`distinguishes_requests_by_revision`, in `render.rs`'s test module, and
`assert_revision_distinguishes_cache_keys` in the `RenderService` suite).

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

**Hard requirement: a renderer does not validate `RenderRequest::revision`.**
Also stated on the trait's doc comment, and enforced by the suite. The revision
exists for the benefit of *caches*, not of the rasterizer. An implementation
carries it and echoes it back unchanged inside the response's `request`, so a
cache can file the tile under the key it asked for. It must **never** compare
the revision against whatever state it happens to hold, and must never fail,
drop, or defer a request because the two disagree.

This is written down because rejecting a mismatched revision is a plausible
thing for a track to implement and it would be wrong. A real rasterizer may
legitimately hold several revisions at once — a request queued before an edit,
a snapshot taken after it — so a service that rejected unfamiliar revisions
would fail exactly the requests a cache most needs answered. **A service
holding a snapshot at one revision must still answer a request naming
another**, rasterizing it exactly as it would any other request.

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
A4, `Rotation::None`; page 2: A4, `Rotation::Quarter`) at a private
`SNAPSHOT_REVISION = 7` — deliberately not zero, so that an implementation
quietly assuming a fresh document fails here rather than in a track's own
tests:

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
| A request naming a revision the service does not hold is still answered, rasterized identically, with the requested revision echoed back unchanged — never `Failed`, never substituted | `assert_a_foreign_revision_is_still_answered` |
| Two requests differing only in `revision` are distinct `HashMap` keys, so a pre-change tile is not addressable by a post-change request | `assert_revision_distinguishes_cache_keys` |

**Known gap:** `RenderRequest::new` (consumed here) accepts any finite
positive scale, including absurdly large ones — see
[Known gaps](#known-gaps).

---

## Why the render service mentions no document type

`RenderService::submit` takes a `RenderRequest` — a `PageId`, a revision, a
scale, and a rotation — never a `Document` or a document handle of any kind.
This is deliberate, not an oversight. The revision is a bare `u64` for the same
reason: it names document *state* without naming the document.

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
input, revision advancement on success and non-advancement on failure, and
(for rendering) correct tile dimensions under scale and rotation composition
plus revision pass-through. Both functions panic with a descriptive message on the first
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

3. **`RenderRequest`'s fields are public, so `new`'s validation is optional.**
   Writing `RenderRequest { page, revision, scale: f32::NAN }` compiles and
   bypasses the finite-and-positive check entirely. The `Eq`/`Hash`
   implementations stay lawful either way, because both compare `scale.to_bits()`
   rather than the float, so a `NaN` request is merely self-equal and useless —
   not unsound. Prefer `RenderRequest::new` everywhere; if a track finds itself
   wanting the struct literal, that is a signal the constructor is missing
   something, and the fix is an additive change to `new`, not a bypass.

4. **The revision counter proves freshness only if the caller threads the right
   one.** Nothing in the type system forces a `RenderRequest` to carry the
   revision of the document it is actually about — a track holding a stale
   variable reintroduces exactly the stale-tile bug the field exists to prevent.
   Whichever track builds the first real tile cache should derive requests from
   the `DocumentSnapshot` it is drawing, rather than from a separately tracked
   number, so that the two cannot drift.

   Note also that `import_pages` advances the revision even when handed an empty
   `ids` slice. That is deliberate: a spurious cache miss is cheap, a stale tile
   is a visible defect. A real implementation should match it.

5. **`PageId` is a within-session concept only.** *Open by construction; not a
   defect to be fixed.* Supplied by Track A's plan author and verified against
   `lopdf`: **the PDF format has nowhere to persist a `PageId`.** A page is a
   dictionary reachable from the page tree, addressed by an object number that
   an incremental save may renumber and a compacting save certainly does; there
   is no standard key in which to stash an editor-assigned identity, and adding
   a private one would be structure a conforming reader is free to discard.
   `PageIdAllocator` accordingly hands out identities that are unique within one
   document *in one process*, and nothing more.

   **Therefore no track may store a `PageId` across a save-and-reopen.** An undo
   stack that survives a save is wrong — the `Box<dyn Command<D>>` inverses it
   holds name pages by `PageId`, and after a reopen those ids address different
   pages or nothing at all. A session file recording "the user had page#7
   selected" is wrong for the same reason. This binds the trash model too: the
   pages `restore_page` hands back live in memory for as long as the document
   object does and no longer, so undo of a deletion is exact *within a session*
   and simply unavailable across one. If a track needs identity that outlives a
   process, that is a new contract — a durable key derived from content or an
   explicitly written-out mapping — and it must be designed as one, not
   improvised by serializing a `PageId::get()`.

   Within a session, compaction cuts that window short deliberately rather
   than by construction — see
   [Compaction destroys undo of deletions](#compaction-destroys-undo-of-deletions).
