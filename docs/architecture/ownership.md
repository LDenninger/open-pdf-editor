# Crate ownership

| Crate | Track | Session branch |
| --- | --- | --- |
| `opdf-core` | none — shared contract | changed only under the protocol below |
| `opdf-pdf` | A — Document | `track/a-document` |
| `opdf-render` | B — Rendering | `track/b-rendering` |
| `opdf-ops`, `opdf-cli` | C — Ops & CLI | `track/c-ops` |
| `opdf-app` | D — Shell | `track/d-shell` |
| `tests/`, `fuzz/`, `benches/`, `.github/` | E — Verification | `track/e-verification` |

Ownership is a default, not a lock. A session needing a small change in another
crate makes it, provided the full gate stays green. Rigid ownership would
deadlock a project driven by one person running several sessions.

## Contract change protocol

**Additive changes are free.** A new trait method with a default body, a new
enum variant, a new type: make it, keep the fakes and contract suites passing,
merge.

**Breaking changes stop the world.** Changing a signature requires appending
the change and its rationale to `contract-changes.md`, landing it on master
together with updated fakes and contract suites, and every other session
rebasing before continuing.

More than a couple of breaking changes per week means the contracts are wrong.
Stop and fix the abstraction rather than absorbing the churn.

## Shared files and how to change them

`.github/` is owned by Track E, but other tracks legitimately need to touch
`.github/workflows/ci.yml`: Track B's crate cannot build or be tested in CI
without the PDFium shared library present on the runner, and Track D's `egui`/
`eframe` dependency may need `libxkbcommon-dev` and `libwayland-dev` installed
if the bare runner fails to link them.

**The rule:** a track adding *only* an install step or a cache entry for its
own dependency may make that change directly, provided the full gate stays
green and the change is a separate, single-file commit touching nothing but
`ci.yml`. Anything that restructures the workflow — a new job, a matrix, a
changed trigger — belongs to Track E.

**Why:** CI is the gate every track depends on, so a broken workflow blocks
all five tracks — but routing every `apt-get install` line through one track
would serialise the whole project.

`scripts/` is a new shared directory, introduced by Track B's
`scripts/fetch-pdfium.sh`. Any track may add a script it owns, named for its
purpose. No track edits another track's script without the same coordination
as a contract change.

A track that needs a runner dependency should say so in the commit message of
its first commit touching `ci.yml`, so the other tracks can see why the
workflow changed when they rebase.
