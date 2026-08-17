//! The four operations, over real files.
//!
//! Every subcommand follows the same shape: refuse to clobber anything first,
//! open what it needs, drive `opdf-ops` over `opdf_pdf::PdfDocument`, and save.
//!
//! # Which save path each subcommand uses
//!
//! [`DocumentIo::save_incremental`] appends to the bytes the document was
//! opened from, so the original file survives verbatim inside the output and
//! nothing this crate does not model can be lost.
//! [`DocumentIo::save_compacted`] rewrites the document and drops every object
//! the page tree does not reach.
//!
//! - `merge` and `extract` build their output from
//!   [`PdfDocument::empty`] and fill it by importing. Every object in the
//!   result arrived by being reachable from a page that was imported, so
//!   compaction has nothing of the user's to lose — but an incremental save
//!   would emit the empty skeleton as a first revision and append a second one
//!   on top of it, for a two-revision file describing a document that was
//!   never edited. **Compacted.**
//! - `split` removes half the pages from the input and writes what is left.
//!   Under the trash model those pages are unreferenced, not gone, and
//!   `save_incremental` keeps unreferenced objects: half A would ship the
//!   whole of half B inside it, invisible to a page count and recoverable by
//!   anyone who looks. Splitting a document is precisely a request not to do
//!   that. **Compacted, for both halves.**
//! - `rotate` changes one integer on each selected page and nothing else. The
//!   input is a real file full of structure this crate does not model, and
//!   nothing was removed, so there is nothing to purge and everything to lose.
//!   **Incremental** — which also means an in-place rotation appends rather
//!   than rewrites, leaving the pre-rotation revision intact in the file.
//!
//! Undo does not enter into it: the process exits after one operation, so the
//! rule that a compacting save destroys undo of deletions has no CLI-visible
//! consequence. What does matter is that a compacting save must not be used
//! where it would silently drop structure, and an incremental one must not be
//! used where it would silently retain pages the user asked to separate.

use std::path::Path;

use opdf_core::{Command, Document, DocumentIo, PageId};
use opdf_ops::{Merge, extract_range, rotate_selection, split_at};
use opdf_pdf::PdfDocument;

use crate::args::{ExtractArgs, MergeArgs, RotateArgs, SplitArgs};
use crate::error::CliError;
use crate::range::{PageSelection, contiguous_runs};

//---------------------------------------------------------------------
// Shared helpers
//---------------------------------------------------------------------

/// Open a document, naming the file in any failure.
fn open_document(path: &Path) -> Result<PdfDocument, CliError> {
    PdfDocument::open(path).map_err(|source| CliError::Open {
        path: path.to_path_buf(),
        source,
    })
}

/// Refuse to write over a file that already exists unless `force` was given.
///
/// Checked before any document is opened, so a refused run touches nothing at
/// all — neither the output it declined to write nor the inputs it would have
/// read.
fn guard_output(path: &Path, force: bool) -> Result<(), CliError> {
    if !force && path.exists() {
        return Err(CliError::OutputExists { path: path.to_path_buf() });
    }
    Ok(())
}

/// Resolve a `--pages` selection to the page identities it names.
fn select_pages(document: &PdfDocument, pages: &PageSelection) -> Result<Vec<PageId>, CliError> {
    let indices = pages.resolve(document.page_count())?;
    let ids = document.page_ids();
    let page_count = ids.len();
    indices
        .into_iter()
        .map(|index| {
            //--- resolve() already bounded these, so this is a belt-and-braces check that is never a panic ---
            ids.get(index)
                .copied()
                .ok_or(CliError::Operation(opdf_core::Error::IndexOutOfBounds { index, page_count }))
        })
        .collect()
}

/// Write a document as a fresh, single-revision file.
fn save_compacted(document: &mut PdfDocument, path: &Path) -> Result<(), CliError> {
    document.save_compacted(path).map_err(|source| CliError::Save {
        path: path.to_path_buf(),
        source,
    })
}

/// Write a document by appending an update to the bytes it was opened from.
fn save_incremental(document: &mut PdfDocument, path: &Path) -> Result<(), CliError> {
    document.save_incremental(path).map_err(|source| CliError::Save {
        path: path.to_path_buf(),
        source,
    })
}

//---------------------------------------------------------------------
// merge
//---------------------------------------------------------------------

/// Concatenate every input into one new document, in argument order.
pub fn run_merge(args: &MergeArgs) -> Result<(), CliError> {
    guard_output(&args.output_path, args.force)?;

    let mut sources = Vec::with_capacity(args.input_paths.len());
    for path in &args.input_paths {
        sources.push(open_document(path)?);
    }

    let mut output = PdfDocument::empty().map_err(CliError::Operation)?;
    Merge::new(sources).apply(&mut output)?;
    save_compacted(&mut output, &args.output_path)
}

//---------------------------------------------------------------------
// split
//---------------------------------------------------------------------

/// Split the input at a page boundary into two new documents.
pub fn run_split(args: &SplitArgs) -> Result<(), CliError> {
    if args.out_a_path == args.out_b_path {
        return Err(CliError::Usage("--out-a and --out-b must name different files".to_string()));
    }
    guard_output(&args.out_a_path, args.force)?;
    guard_output(&args.out_b_path, args.force)?;

    let mut head = open_document(&args.input_path)?;
    let page_count = head.page_count();
    //--- a document with no pages cannot be saved, so a boundary that empties either half is refused here, by name, rather than surfacing later as "cannot save a document with no pages" ---
    if args.at_page < 2 || args.at_page > page_count {
        return Err(CliError::Usage(format!(
            "--at must name a page from 2 to {page_count} in a document of {page_count} pages, so that neither half comes out empty; got {}",
            args.at_page
        )));
    }

    let mut tail = PdfDocument::empty().map_err(CliError::Operation)?;
    //--- --at is the 1-based first page of the second half, so the 0-based boundary is one less ---
    split_at(&mut head, &mut tail, args.at_page - 1)?;

    save_compacted(&mut head, &args.out_a_path)?;
    save_compacted(&mut tail, &args.out_b_path)
}

//---------------------------------------------------------------------
// rotate
//---------------------------------------------------------------------

/// Set the orientation of the selected pages.
pub fn run_rotate(args: &RotateArgs) -> Result<(), CliError> {
    //--- no --out is an in-place edit: the user named one file and no other, so there is nothing to clobber by accident ---
    let output_path = args.output_path.as_deref().unwrap_or(&args.input_path);
    if args.output_path.is_some() {
        guard_output(output_path, args.force)?;
    }

    let mut document = open_document(&args.input_path)?;
    let selected = select_pages(&document, &args.pages)?;
    rotate_selection(&selected, args.rotation).apply(&mut document)?;
    save_incremental(&mut document, output_path)
}

//---------------------------------------------------------------------
// extract
//---------------------------------------------------------------------

/// Copy the selected pages into a new document, in document order.
pub fn run_extract(args: &ExtractArgs) -> Result<(), CliError> {
    guard_output(&args.output_path, args.force)?;

    let source = open_document(&args.input_path)?;
    let indices = args.pages.resolve(source.page_count())?;
    let mut output = PdfDocument::empty().map_err(CliError::Operation)?;
    //--- extract_range copies one contiguous range and appends, so a selection with gaps is one call per run, in ascending order ---
    for (start_index, end_index) in contiguous_runs(&indices) {
        extract_range(&source, &mut output, start_index, end_index)?;
    }
    save_compacted(&mut output, &args.output_path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use opdf_core::{PageSize, Rotation};
    use std::path::PathBuf;
    use tempfile::TempDir;

    //---------------------------------------------------------------------
    // Reading results back off disk
    //---------------------------------------------------------------------

    /// A checked-in corpus specimen. `irs_f1040.pdf` is two US Letter pages;
    /// `custom_encoding.pdf` is one 200x200 page, which is what makes page
    /// order verifiable by geometry alone after a merge.
    fn corpus_path(file_name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../tests/corpus/files").join(file_name)
    }

    /// Reopen a written file and report its pages.
    ///
    /// Every assertion in this module goes through this rather than through
    /// the document that wrote the file: a save that never reached the disk,
    /// or wrote a page tree the parser cannot read back, is exactly the defect
    /// worth catching, and the in-memory document knows nothing about it.
    fn reopen(path: &Path) -> Vec<(PageSize, Rotation)> {
        let document = PdfDocument::open(path).expect("the written file must reopen");
        document
            .page_ids()
            .into_iter()
            .map(|id| {
                let info = document.page(id).expect("a listed page must resolve");
                (info.size, info.rotation)
            })
            .collect()
    }

    fn page_count_of(path: &Path) -> usize {
        reopen(path).len()
    }

    fn rotations_of(path: &Path) -> Vec<Rotation> {
        reopen(path).into_iter().map(|(_, rotation)| rotation).collect()
    }

    fn widths_of(path: &Path) -> Vec<u32> {
        reopen(path).into_iter().map(|(size, _)| size.width_pt as u32).collect()
    }

    /// Copy a corpus specimen into the scratch directory so it can be edited.
    fn scratch_copy(scratch: &TempDir, file_name: &str) -> PathBuf {
        let path = scratch.path().join(file_name);
        std::fs::copy(corpus_path(file_name), &path).expect("the corpus specimen must be copyable");
        path
    }

    //---------------------------------------------------------------------
    // Building the arguments
    //---------------------------------------------------------------------

    fn merge_args(output_path: &Path, input_paths: &[PathBuf], force: bool) -> MergeArgs {
        MergeArgs {
            output_path: output_path.to_path_buf(),
            input_paths: input_paths.to_vec(),
            force,
        }
    }

    fn rotate_args(input_path: &Path, pages: &str, rotation: Rotation, output_path: Option<&Path>, force: bool) -> RotateArgs {
        RotateArgs {
            input_path: input_path.to_path_buf(),
            pages: PageSelection::parse(pages).expect("the test's own selection must parse"),
            rotation,
            output_path: output_path.map(Path::to_path_buf),
            force,
        }
    }

    fn extract_args(input_path: &Path, pages: &str, output_path: &Path, force: bool) -> ExtractArgs {
        ExtractArgs {
            input_path: input_path.to_path_buf(),
            pages: PageSelection::parse(pages).expect("the test's own selection must parse"),
            output_path: output_path.to_path_buf(),
            force,
        }
    }

    fn split_args(input_path: &Path, at_page: usize, out_a_path: &Path, out_b_path: &Path, force: bool) -> SplitArgs {
        SplitArgs {
            input_path: input_path.to_path_buf(),
            at_page,
            out_a_path: out_a_path.to_path_buf(),
            out_b_path: out_b_path.to_path_buf(),
            force,
        }
    }

    /// A six-page document, built by the tool itself out of three copies of a
    /// two-page specimen. Nothing else in the corpus is long enough to make a
    /// selection with gaps in it mean anything.
    fn six_page_document(scratch: &TempDir) -> PathBuf {
        let path = scratch.path().join("six.pdf");
        let inputs = vec![corpus_path("irs_f1040.pdf"), corpus_path("irs_f1040.pdf"), corpus_path("irs_f1040.pdf")];
        run_merge(&merge_args(&path, &inputs, false)).expect("merging three two-page documents must succeed");
        assert_eq!(page_count_of(&path), 6, "the fixture the other tests build on must really have six pages");
        path
    }

    //---------------------------------------------------------------------
    // merge
    //---------------------------------------------------------------------

    #[test]
    fn merge_writes_a_document_holding_every_input_page() {
        let scratch = TempDir::new().expect("a scratch directory");
        let output_path = scratch.path().join("merged.pdf");
        let inputs = vec![corpus_path("irs_f1040.pdf"), corpus_path("custom_encoding.pdf")];

        run_merge(&merge_args(&output_path, &inputs, false)).expect("merge must succeed");

        assert_eq!(page_count_of(&output_path), 3, "two pages plus one page is three pages once reopened");
    }

    /// Argument order is page order. Verified by geometry rather than by the
    /// return value of the call that wrote the file: the two specimens have
    /// different page widths, so where each one landed is readable off disk.
    #[test]
    fn merge_lays_the_inputs_out_in_argument_order() {
        let scratch = TempDir::new().expect("a scratch directory");

        let forwards = scratch.path().join("forwards.pdf");
        run_merge(&merge_args(
            &forwards,
            &[corpus_path("custom_encoding.pdf"), corpus_path("irs_f1040.pdf")],
            false,
        ))
        .expect("merge must succeed");
        assert_eq!(widths_of(&forwards), vec![200, 612, 612]);

        let backwards = scratch.path().join("backwards.pdf");
        run_merge(&merge_args(
            &backwards,
            &[corpus_path("irs_f1040.pdf"), corpus_path("custom_encoding.pdf")],
            false,
        ))
        .expect("merge must succeed");
        assert_eq!(
            widths_of(&backwards),
            vec![612, 612, 200],
            "reversing the arguments must reverse the pages, not produce the same file"
        );
    }

    #[test]
    fn merge_leaves_every_input_byte_for_byte_untouched() {
        let scratch = TempDir::new().expect("a scratch directory");
        let input_path = scratch_copy(&scratch, "irs_f1040.pdf");
        let before = std::fs::read(&input_path).expect("the input must be readable");

        run_merge(&merge_args(
            &scratch.path().join("merged.pdf"),
            &[input_path.clone(), input_path.clone()],
            false,
        ))
        .expect("merge must succeed");

        assert_eq!(
            std::fs::read(&input_path).expect("the input must still be readable"),
            before,
            "merge reads its inputs and must never write them"
        );
    }

    #[test]
    fn merge_of_a_single_input_is_a_faithful_copy_of_it() {
        let scratch = TempDir::new().expect("a scratch directory");
        let output_path = scratch.path().join("copy.pdf");

        run_merge(&merge_args(&output_path, &[corpus_path("irs_f1040.pdf")], false)).expect("merge must succeed");

        assert_eq!(reopen(&output_path), reopen(&corpus_path("irs_f1040.pdf")));
    }

    //---------------------------------------------------------------------
    // split
    //---------------------------------------------------------------------

    #[test]
    fn split_puts_the_boundary_page_at_the_head_of_the_second_half() {
        let scratch = TempDir::new().expect("a scratch directory");
        let input_path = six_page_document(&scratch);
        let out_a = scratch.path().join("a.pdf");
        let out_b = scratch.path().join("b.pdf");

        run_split(&split_args(&input_path, 3, &out_a, &out_b, false)).expect("split must succeed");

        assert_eq!(
            page_count_of(&out_a),
            2,
            "--at 3 means page 3 starts the second half, so the first holds pages 1 and 2"
        );
        assert_eq!(page_count_of(&out_b), 4);
    }

    /// The reason both halves are written compacted: `remove_page` only
    /// unreferences a page, and an incremental save keeps unreferenced
    /// objects, so half A would carry the whole of half B inside it.
    #[test]
    fn neither_half_of_a_split_carries_the_other_halfs_pages() {
        let scratch = TempDir::new().expect("a scratch directory");
        let input_path = six_page_document(&scratch);
        //--- give each page a distinct orientation so the halves are told apart by content, not only by count ---
        for (page, rotation) in [("1", Rotation::Quarter), ("4", Rotation::Half), ("6", Rotation::ThreeQuarter)] {
            run_rotate(&rotate_args(&input_path, page, rotation, None, false)).expect("rotate must succeed");
        }
        let out_a = scratch.path().join("a.pdf");
        let out_b = scratch.path().join("b.pdf");

        run_split(&split_args(&input_path, 4, &out_a, &out_b, false)).expect("split must succeed");

        assert_eq!(
            rotations_of(&out_a),
            vec![Rotation::Quarter, Rotation::None, Rotation::None],
            "half A is pages 1 to 3 and nothing else"
        );
        assert_eq!(
            rotations_of(&out_b),
            vec![Rotation::Half, Rotation::None, Rotation::ThreeQuarter],
            "half B is pages 4 to 6 and nothing else"
        );

        let input_size = std::fs::metadata(&input_path).expect("the input must exist").len();
        let half_a_size = std::fs::metadata(&out_a).expect("half A must exist").len();
        assert!(
            half_a_size < input_size,
            "half A ({half_a_size} bytes) must not still contain the whole document ({input_size} bytes): an incremental save here would keep half B's unreferenced objects"
        );
    }

    #[test]
    fn split_refuses_a_boundary_that_would_leave_a_half_empty() {
        let scratch = TempDir::new().expect("a scratch directory");
        let input_path = corpus_path("irs_f1040.pdf");
        let out_a = scratch.path().join("a.pdf");
        let out_b = scratch.path().join("b.pdf");

        for at_page in [1, 3, 99] {
            let error = run_split(&split_args(&input_path, at_page, &out_a, &out_b, false)).expect_err("--at {at_page} must be refused");
            assert!(matches!(error, CliError::Usage(_)), "got {error:?}");
            assert!(!out_a.exists() && !out_b.exists(), "a refused split must write nothing");
        }
    }

    #[test]
    fn split_refuses_to_write_both_halves_to_one_file() {
        let scratch = TempDir::new().expect("a scratch directory");
        let out = scratch.path().join("both.pdf");

        let error = run_split(&split_args(&corpus_path("irs_f1040.pdf"), 2, &out, &out, false)).expect_err("one path for two halves must be refused");

        assert!(matches!(error, CliError::Usage(_)), "got {error:?}");
        assert!(!out.exists(), "the second half must not silently replace the first");
    }

    #[test]
    fn split_leaves_the_input_untouched() {
        let scratch = TempDir::new().expect("a scratch directory");
        let input_path = scratch_copy(&scratch, "irs_f1040.pdf");
        let before = std::fs::read(&input_path).expect("the input must be readable");

        run_split(&split_args(&input_path, 2, &scratch.path().join("a.pdf"), &scratch.path().join("b.pdf"), false)).expect("split must succeed");

        assert_eq!(
            std::fs::read(&input_path).expect("the input must still be readable"),
            before,
            "split writes two new files and edits neither the original"
        );
    }

    //---------------------------------------------------------------------
    // rotate
    //---------------------------------------------------------------------

    #[test]
    fn rotate_turns_the_selected_pages_and_only_those() {
        let scratch = TempDir::new().expect("a scratch directory");
        let input_path = six_page_document(&scratch);
        let output_path = scratch.path().join("rotated.pdf");

        run_rotate(&rotate_args(&input_path, "2,5-6", Rotation::Half, Some(&output_path), false)).expect("rotate must succeed");

        assert_eq!(
            rotations_of(&output_path),
            vec![Rotation::None, Rotation::Half, Rotation::None, Rotation::None, Rotation::Half, Rotation::Half],
            "pages 2, 5 and 6 turn and the rest stay upright"
        );
    }

    /// `--degrees` sets an orientation rather than adding one. Applying the
    /// same rotation twice must therefore be idempotent.
    #[test]
    fn rotate_sets_an_absolute_orientation_rather_than_accumulating_one() {
        let scratch = TempDir::new().expect("a scratch directory");
        let input_path = scratch_copy(&scratch, "irs_f1040.pdf");

        run_rotate(&rotate_args(&input_path, "1", Rotation::Quarter, None, false)).expect("the first rotate must succeed");
        assert_eq!(rotations_of(&input_path), vec![Rotation::Quarter, Rotation::None]);

        run_rotate(&rotate_args(&input_path, "1", Rotation::Quarter, None, false)).expect("the second rotate must succeed");
        assert_eq!(
            rotations_of(&input_path),
            vec![Rotation::Quarter, Rotation::None],
            "90 degrees twice is 90 degrees, not 180"
        );

        run_rotate(&rotate_args(&input_path, "1", Rotation::None, None, false)).expect("resetting must succeed");
        assert_eq!(
            rotations_of(&input_path),
            vec![Rotation::None, Rotation::None],
            "--degrees 0 sets a page back upright"
        );
    }

    /// Why an in-place rotation needs no `--force`: the incremental save
    /// appends, so the file it overwrites is still inside the file it writes.
    #[test]
    fn rotating_in_place_appends_and_keeps_the_original_bytes_as_a_prefix() {
        let scratch = TempDir::new().expect("a scratch directory");
        let input_path = scratch_copy(&scratch, "irs_f1040.pdf");
        let before = std::fs::read(&input_path).expect("the input must be readable");

        run_rotate(&rotate_args(&input_path, "1-", Rotation::Quarter, None, false)).expect("rotate must succeed");

        let after = std::fs::read(&input_path).expect("the rotated file must be readable");
        assert!(after.len() > before.len(), "an incremental save appends, so the file must grow");
        assert_eq!(
            &after[..before.len()],
            &before[..],
            "the pre-rotation revision must survive verbatim as the prefix of the new file"
        );
        assert_eq!(rotations_of(&input_path), vec![Rotation::Quarter, Rotation::Quarter]);
    }

    #[test]
    fn rotate_with_an_output_leaves_the_input_untouched() {
        let scratch = TempDir::new().expect("a scratch directory");
        let input_path = scratch_copy(&scratch, "irs_f1040.pdf");
        let before = std::fs::read(&input_path).expect("the input must be readable");
        let output_path = scratch.path().join("rotated.pdf");

        run_rotate(&rotate_args(&input_path, "1", Rotation::Half, Some(&output_path), false)).expect("rotate must succeed");

        assert_eq!(std::fs::read(&input_path).expect("the input must still be readable"), before);
        assert_eq!(rotations_of(&output_path), vec![Rotation::Half, Rotation::None]);
    }

    #[test]
    fn rotate_rejects_a_page_past_the_end_without_writing_anything() {
        let scratch = TempDir::new().expect("a scratch directory");
        let output_path = scratch.path().join("rotated.pdf");

        let error = run_rotate(&rotate_args(&corpus_path("irs_f1040.pdf"), "9", Rotation::Quarter, Some(&output_path), false))
            .expect_err("page 9 of a two-page document must be refused");

        assert!(
            matches!(error, CliError::Operation(opdf_core::Error::IndexOutOfBounds { index: 9, page_count: 2 })),
            "got {error:?}"
        );
        assert!(!output_path.exists(), "a refused rotation must not leave a partial file behind");
    }

    //---------------------------------------------------------------------
    // extract
    //---------------------------------------------------------------------

    #[test]
    fn extract_copies_the_selection_and_never_touches_the_source() {
        let scratch = TempDir::new().expect("a scratch directory");
        let input_path = scratch_copy(&scratch, "irs_f1040.pdf");
        let before = std::fs::read(&input_path).expect("the input must be readable");
        let output_path = scratch.path().join("page2.pdf");

        run_extract(&extract_args(&input_path, "2", &output_path, false)).expect("extract must succeed");

        assert_eq!(page_count_of(&output_path), 1);
        assert_eq!(
            std::fs::read(&input_path).expect("the input must still be readable"),
            before,
            "extraction copies; it never mutates the source"
        );
    }

    /// A selection with gaps becomes one `extract_range` call per contiguous
    /// run, and the runs must land in document order. Each selected page is
    /// given its own orientation first, so the order is readable off disk
    /// rather than inferred from the page count.
    #[test]
    fn extract_of_a_selection_with_gaps_keeps_document_order() {
        let scratch = TempDir::new().expect("a scratch directory");
        let input_path = six_page_document(&scratch);
        for (page, rotation) in [("1", Rotation::Quarter), ("4", Rotation::Half), ("6", Rotation::ThreeQuarter)] {
            run_rotate(&rotate_args(&input_path, page, rotation, None, false)).expect("rotate must succeed");
        }
        let output_path = scratch.path().join("picked.pdf");

        //--- deliberately out of order and overlapping: the result must still be pages 1, 4 and 6 in that order ---
        run_extract(&extract_args(&input_path, "6,1,4,6", &output_path, false)).expect("extract must succeed");

        assert_eq!(
            rotations_of(&output_path),
            vec![Rotation::Quarter, Rotation::Half, Rotation::ThreeQuarter],
            "the selection resolves to document order and de-duplicates, so page 6 appears once, last"
        );
    }

    #[test]
    fn extract_of_a_contiguous_range_copies_exactly_that_range() {
        let scratch = TempDir::new().expect("a scratch directory");
        let input_path = six_page_document(&scratch);
        run_rotate(&rotate_args(&input_path, "3", Rotation::Half, None, false)).expect("rotate must succeed");
        let output_path = scratch.path().join("middle.pdf");

        run_extract(&extract_args(&input_path, "2-4", &output_path, false)).expect("extract must succeed");

        assert_eq!(rotations_of(&output_path), vec![Rotation::None, Rotation::Half, Rotation::None]);
    }

    #[test]
    fn extract_rejects_a_page_past_the_end_without_writing_anything() {
        let scratch = TempDir::new().expect("a scratch directory");
        let output_path = scratch.path().join("nope.pdf");

        let error =
            run_extract(&extract_args(&corpus_path("irs_f1040.pdf"), "1-5", &output_path, false)).expect_err("1-5 of a two-page document must be refused");

        assert!(
            matches!(error, CliError::Operation(opdf_core::Error::IndexOutOfBounds { index: 5, page_count: 2 })),
            "got {error:?}"
        );
        assert!(!output_path.exists());
    }

    //---------------------------------------------------------------------
    // Not clobbering things
    //---------------------------------------------------------------------

    #[test]
    fn every_subcommand_refuses_an_existing_output_without_force() {
        let scratch = TempDir::new().expect("a scratch directory");
        let input_path = corpus_path("irs_f1040.pdf");
        let occupied = scratch.path().join("occupied.pdf");
        let sentinel: &[u8] = b"not a pdf, and must survive every one of these";
        std::fs::write(&occupied, sentinel).expect("the sentinel must be writable");

        //--- every refusal must look the same and, crucially, must leave the file it refused alone ---
        let check = |name: &str, outcome: Result<(), CliError>| {
            let error = outcome.expect_err(&format!("{name} must refuse an existing output"));
            assert!(matches!(error, CliError::OutputExists { .. }), "{name}: got {error:?}");
            assert_eq!(
                std::fs::read(&occupied).expect("the sentinel must still be readable"),
                sentinel,
                "{name} must leave the file it refused to overwrite exactly as it found it"
            );
        };

        check("merge", run_merge(&merge_args(&occupied, std::slice::from_ref(&input_path), false)));
        check(
            "split --out-a",
            run_split(&split_args(&input_path, 2, &occupied, &scratch.path().join("b.pdf"), false)),
        );
        check(
            "split --out-b",
            run_split(&split_args(&input_path, 2, &scratch.path().join("a.pdf"), &occupied, false)),
        );
        check("rotate", run_rotate(&rotate_args(&input_path, "1", Rotation::Quarter, Some(&occupied), false)));
        check("extract", run_extract(&extract_args(&input_path, "1", &occupied, false)));

        assert!(
            !scratch.path().join("a.pdf").exists() && !scratch.path().join("b.pdf").exists(),
            "a split refused for one of its outputs must not have written the other"
        );
    }

    #[test]
    fn force_overwrites_an_existing_output() {
        let scratch = TempDir::new().expect("a scratch directory");
        let output_path = scratch.path().join("out.pdf");
        std::fs::write(&output_path, b"not a pdf").expect("the placeholder must be writable");

        run_extract(&extract_args(&corpus_path("irs_f1040.pdf"), "1", &output_path, true)).expect("--force must overwrite");

        assert_eq!(page_count_of(&output_path), 1);
    }

    //---------------------------------------------------------------------
    // Failures that must not panic
    //---------------------------------------------------------------------

    #[test]
    fn a_missing_input_is_reported_by_name_rather_than_panicking() {
        let scratch = TempDir::new().expect("a scratch directory");
        let missing = scratch.path().join("nowhere.pdf");

        let error = run_extract(&extract_args(&missing, "1", &scratch.path().join("out.pdf"), false)).expect_err("a file that is not there cannot be opened");

        assert!(matches!(&error, CliError::Open { path, .. } if path == &missing), "got {error:?}");
        assert!(error.to_string().contains("nowhere.pdf"), "the message must name the file: {error}");
    }

    #[test]
    fn an_input_that_is_not_a_pdf_is_reported_rather_than_panicking() {
        let scratch = TempDir::new().expect("a scratch directory");
        let not_a_pdf = scratch.path().join("notes.txt");
        std::fs::write(&not_a_pdf, b"these are not the bytes you are looking for").expect("the decoy must be writable");

        let error =
            run_merge(&merge_args(&scratch.path().join("out.pdf"), std::slice::from_ref(&not_a_pdf), false)).expect_err("a text file is not a document");

        assert!(matches!(&error, CliError::Open { path, .. } if path == &not_a_pdf), "got {error:?}");
    }

    #[test]
    fn a_zero_byte_input_is_reported_rather_than_panicking() {
        let scratch = TempDir::new().expect("a scratch directory");

        let error = run_extract(&extract_args(&corpus_path("zero_byte.pdf"), "1", &scratch.path().join("out.pdf"), false))
            .expect_err("an empty file is not a document");

        assert!(matches!(error, CliError::Open { .. }), "got {error:?}");
    }
}
