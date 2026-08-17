# open-pdf-editor

A free, native, fast PDF editor for the things people actually open Acrobat to do.

Editing a PDF should not cost a subscription, and it should not damage the file.
`opdf` aims to cover the everyday range of Adobe Acrobat — organize pages,
annotate and comment, fill and sign forms, edit content, redact — as a native
desktop application written in Rust, fast enough to stay responsive on documents
of hundreds of megabytes.

## Table of Contents

- [Status](#status)
- [Background](#background)
- [Goals](#goals)
- [Non-Goals](#non-goals)
- [Roadmap](#roadmap)
- [Architecture](#architecture)
- [Development Model](#development-model)
- [Install](#install)
- [Usage](#usage)
- [Prior Art and Licensing](#prior-art-and-licensing)
- [Contributing](#contributing)
- [License](#license)

## Status

**Phase 0 (contracts) complete.** The design is settled. `opdf-core` defines
the `Document`, `DocumentIo`, `Command`, and `RenderService` contracts, their
value types, in-memory fakes (`VecDocument`, `FakeRenderService`), and the
contract test suites every implementation must pass — see
[`docs/architecture/contracts.md`](docs/architecture/contracts.md) for the
normative reference. The workspace compiles and its tests pass. The
`opdf-cli` and `opdf` binaries do nothing yet; no PDF can be opened, rendered,
or edited.

The next step is five parallel implementation tracks — Document, Rendering,
Ops & CLI, Shell, and Verification — working against these contracts
concurrently. See [`docs/architecture/ownership.md`](docs/architecture/ownership.md)
for the track map and the protocol for changing a contract once tracks are
underway.

**Track B (rendering) is complete.** `opdf-render` implements `RenderService`
over PDFium and passes `assert_render_service_contract` unmodified: one worker
thread per service owns the document, a bounded newest-first backlog absorbs
fast scrolling, and a byte-budgeted LRU tile cache keyed on the request drops
tiles from superseded revisions on rebind. Every call into PDFium is
serialized process-wide, because PDFium is not thread-safe and
`pdfium-render`'s `thread_safe` feature does not sequence calls. The library
itself is fetched by `scripts/fetch-pdfium.sh` rather than vendored. Text
extraction, text selection geometry, search, and printing are deliberately not
implemented — the crate turns a `RenderRequest` into pixels and nothing else.

## Background

Free PDF tools force a choice between viewing and editing.

Viewers such as Okular and Evince render well but barely edit. Editors such as
LibreOffice Draw and most browser-based tools edit by rebuilding the document,
discarding structure they do not understand and visibly degrading the file.
Adobe Acrobat does the job properly and costs a subscription.

Nothing free covers reordering pages, annotating, filling a form, or fixing a
typo without either failing at the task or damaging the document.

### The correctness promise

> Opening a document and saving it without editing produces a structurally
> identical file.

Edits are written as **incremental updates** appended to the original bytes,
never as a wholesale rewrite. This keeps saves fast on large files and preserves
structure the implementation does not yet understand, instead of silently
dropping it. The promise is enforced by round-trip tests in CI, not by good
intentions.

## Goals

| Area | What it means |
| --- | --- |
| **Page organization** | Merge, split, reorder, rotate, delete, extract |
| **Viewing** | Continuous scroll, zoom, search, text selection and copy |
| **Annotations** | Highlight, note, ink, shapes, stamps, comment threads |
| **Forms** | AcroForm field filling, flattening, drawn and image signatures |
| **Content editing** | Move and edit existing text and page objects |
| **OCR** | Searchable text layer over scanned pages |
| **Redaction** | True content removal plus metadata scrubbing |

Across all of it: fast startup, responsive scrolling on large documents, and a
dense professional interface that follows the conventions of established PDF
editors rather than inventing new ones.

## Non-Goals

Stated explicitly so the project stays finishable:

- Cryptographic PKI signatures (PAdES signing or validation)
- PDF/A archival conversion and validation
- Document comparison, Bates numbering, and other Acrobat Pro workflow features
- Cloud storage, accounts, collaboration servers
- Mobile and web versions
- A general-purpose `pdftk` replacement — the CLI serves the app and its tests

## Roadmap

Each milestone is specified and planned separately. **v0.1 is M0 through M2.**

| | Milestone | Content |
| --- | --- | --- |
| M0 | Core + CLI | Document model, merge, split, reorder, rotate, delete, extract; round-trip test suite |
| M1 | Viewer shell | Render worker, canvas, continuous scroll, zoom, thumbnails, text selection, search |
| M2 | Page organizer | Drag-and-drop thumbnail grid, cross-document drag, undo/redo — **v0.1** |
| M3 | Annotations | Highlight, note, ink, shapes, stamps, comment sidebar, reply threads |
| M4 | Forms | AcroForm filling, flattening, drawn and image signatures |
| M5 | Content editing | Move and edit existing text and page objects |
| M6 | OCR and redaction | Tesseract text layer; true content removal and metadata scrubbing |

Page operations anchor v0.1 because they are the least rendering-dependent
feature, the most immediately useful, and the easiest to prove correct.

## Architecture

A Cargo workspace of four crates. All PDF logic is UI-agnostic; the GUI is a
thin shell over the core.

```
.
├── opdf-core               # Contracts only: traits, types, errors, fakes, contract tests
├── opdf-pdf                # Parsing, object model, incremental save
├── opdf-render             # Renderer implementations (PDFium), render worker, tile cache
├── opdf-ops                # Command implementations and undo stack
├── opdf-cli                # Headless page ops; the core's primary test harness
├── opdf-app                # egui shell: canvas, thumbnail rail, toolbars, panels
├── docs                    # Design specs and development logs
├── LICENSE-APACHE
├── LICENSE-MIT
└── README.md
```

`opdf-core` holds no implementation — only trait and type definitions plus the
fakes and contract test suites that every implementation must satisfy. Crate
boundaries are deliberately drawn to match parallel development tracks, so that
concurrent work never touches the same files.

### Technology choices

| Decision | Choice | Rationale |
| --- | --- | --- |
| Language | **Rust** | The core mutates a binary format supplied by strangers; memory safety matters most exactly there |
| Rasterization | **PDFium** (BSD-3) via `pdfium-render` | Chromium's PDF engine — correct on the long tail of malformed real-world files |
| Object layer | **`lopdf`** (MIT) | Mature Rust PDF object model, wrapped so it stays replaceable |
| GUI | **`egui`** | Pure Rust, GPU-drawn, fast, cheap custom widgets |
| Platforms | **Linux first** | Windows and macOS stay supported in code and ship when actually tested |

### Load-bearing decisions

- **A `Renderer` trait from day one.** PDFium sits behind it and is never called
  directly from the core or the UI, so a future pure-Rust rasterizer replaces
  one crate rather than the application.
- **Serialized access to PDFium.** PDFium is not thread-safe, and
  `pdfium-render`'s `thread_safe` feature only marks it `Send`/`Sync` without
  serializing anything, so every call goes through a scoped `with_pdfium` that
  holds a process-wide lock. One worker thread per open document, never two
  inside the library at once. `submit` and `poll` never block the UI thread;
  `open` does, measurably, and `open_deferred` is the non-blocking alternative.
- **Every mutation is a `Command` with an inverse.** Undo/redo is a property of
  the architecture, not a feature added later.
- **Incremental save by default.** A full rewrite is offered explicitly as a
  separate "compact" operation.

### Testing

The correctness promise is the product, so tests carry it: round-trip
structural diffs, golden-image render comparisons, `cargo-fuzz` against the
parser, and a checked-in corpus of deliberately ugly real-world PDFs.

## Development Model

Milestones are release checkpoints, not units of work — they are inherently
ordered. The unit of parallel work is the **subsystem track**: one crate, one
owner. Milestones become integration checkpoints where tracks meet.

A short sequential **contracts phase** comes first, defining the `Document` and
`Renderer` traits together with fakes — a `FakeRenderer` drawing numbered
rectangles and an in-memory `VecDocument` — and the contract test suites every
implementation must pass. Its completion criterion is that the test suite is
green while the application does nothing.

The fakes are what make parallelism possible: the UI is built against
`FakeRenderer` before PDFium is wired up, and page operations against
`VecDocument` before the parser exists. Five tracks then run concurrently:

| Track | Owns | Delivers |
| --- | --- | --- |
| A — Document | `opdf-pdf` | Open, enumerate, save unchanged |
| B — Rendering | `opdf-render` | PDFium worker and tile cache — **complete** |
| C — Ops & CLI | `opdf-ops`, `opdf-cli` | Invertible page operations |
| D — Shell | `opdf-app` | Application chrome and canvas |
| E — Verification | `tests`, `fuzz`, `benches` | Corpus, round-trip harness, fuzzing |

Integration is trunk-based: tracks merge to master as soon as a task is green,
several times a day, with CI as a hard gate. Changes to the shared `opdf-core`
contracts follow a protocol — additive changes are free, breaking changes are
coordinated across all tracks. See
[`docs/architecture/ownership.md`](docs/architecture/ownership.md) for the
track-to-crate map and the full contract change protocol, and
[`docs/architecture/contracts.md`](docs/architecture/contracts.md) for the
contracts themselves.

## Install

Not yet available — there is nothing to install. Binary releases for Linux will
accompany v0.1, with Windows and macOS builds when they are tested rather than
merely compiled.

### Dependencies

- Rust (stable, 2024 edition)
- A PDFium shared library, obtained prebuilt from
  [`bblanchon/pdfium-binaries`](https://github.com/bblanchon/pdfium-binaries) —
  building PDFium from source is not required

### Building

`opdf-render` binds at runtime to PDFium, which is not vendored into this
repository. Fetch the prebuilt library once before running the test suite:

    ./scripts/fetch-pdfium.sh

It downloads `chromium/7881` from
[bblanchon/pdfium-binaries](https://github.com/bblanchon/pdfium-binaries) into
`vendor/pdfium/`, which is git-ignored. Set `OPDF_PDFIUM_LIB_DIR` to override
the directory the library is loaded from. PDFium is BSD-3-Clause licensed.

## Usage

Planned interfaces, for orientation only. Neither exists yet.

The desktop application opens documents directly:

```bash
opdf document.pdf
```

The CLI covers page operations headlessly, and doubles as the core's test
harness:

```bash
opdf-cli merge a.pdf b.pdf -o combined.pdf
opdf-cli split input.pdf --pages 1-10 -o part1.pdf
opdf-cli rotate input.pdf --pages 3,5 --degrees 90 -o rotated.pdf
```

## Prior Art and Licensing

This project reuses as much existing work as its license permits, and the
boundary is drawn deliberately.

**Linked and shipped:** PDFium (BSD-3), `pdfium-render` (MIT/Apache-2), `lopdf`
(MIT), `egui` (MIT/Apache-2), Tesseract (Apache-2), and a permissively licensed
icon set — Lucide (ISC) or Phosphor (MIT).

**Read and ported from:** [pdf.js](https://github.com/mozilla/pdf.js)
(Apache-2), the best available reference implementation for the format's
difficult corners.

**Studied for interaction design only, never copied from:** Okular, Xournal++,
PDF Arranger, and MuPDF. These are GPL or AGPL licensed; their code cannot enter
a permissively licensed project.

**Not used at all:** Adobe icons, artwork, or trademarks. The interface follows
the layout and interaction conventions of professional PDF editors — which are
not protected — but copies none of Adobe's assets and claims no affiliation with
Adobe. *Adobe* and *Acrobat* are trademarks of Adobe Inc., used here only to
describe the class of software this project targets.

The normative reference throughout is ISO 32000-1 (PDF 1.7), published free of
charge by Adobe.

## Contributing

The project is in its design phase and not yet accepting code. Issues raising
design questions, correctness concerns, or real-world PDFs that break other
tools are welcome now — the last of those is genuinely valuable, since the test
corpus is what makes the correctness promise enforceable.

Once implementation starts, pull requests will be welcome under the conventions
recorded in `CONTRIBUTING.md`.

## License

MIT OR Apache-2.0, at your option.

MIT © Luis Denninger
