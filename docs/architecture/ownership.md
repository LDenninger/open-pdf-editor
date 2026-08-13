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
