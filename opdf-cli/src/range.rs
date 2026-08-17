//! The `--pages` selection syntax, and its resolution against a document.
//!
//! Kept deliberately free of any file or document type so that the syntax can
//! be tested on its own: everything here is a pure function over text and a
//! page count. A `--pages` argument is a comma-separated list of parts, each
//! one of
//!
//! | Part | Meaning |
//! | --- | --- |
//! | `N` | page `N` alone |
//! | `A-B` | pages `A` through `B`, both included |
//! | `A-` | page `A` through the last page |
//! | `-B` | the first page through page `B` |
//!
//! Page numbers are **1-based**, matching what every other PDF tool prints.
//! The selection is resolved to sorted, de-duplicated 0-based indices, so
//! `3,1,1-2` and `1-3` name the same three pages in the same order.

use opdf_core::{Error, Result};

//---------------------------------------------------------------------
// The parsed selection
//---------------------------------------------------------------------

/// One comma-separated part of a `--pages` argument, in 1-based page numbers.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct SelectionPart {
    /// First page of the part, 1-based and inclusive.
    first: usize,
    /// Last page of the part, 1-based and inclusive. `None` means "the last
    /// page of the document", which is only knowable at resolution time.
    last: Option<usize>,
}

/// A parsed `--pages` argument, not yet checked against any document.
///
/// Parsing validates the syntax and the internal consistency of each part —
/// an inverted `5-2` is rejected here — but cannot validate that the pages
/// exist. That happens in [`PageSelection::resolve`].
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct PageSelection {
    parts: Vec<SelectionPart>,
}

impl PageSelection {
    /// Parse a `--pages` argument.
    ///
    /// Returns [`Error::InvalidRange`] for a part whose end precedes its start,
    /// and [`Error::Unsupported`] for anything the syntax does not admit —
    /// an empty selection, an empty part, a page number of zero, a number too
    /// large for `usize`, or a part that is not made of digits and one dash.
    pub fn parse(text: &str) -> Result<Self> {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return Err(Error::Unsupported(
                "empty page selection: give at least one page, for example --pages 1-3".to_string(),
            ));
        }

        let mut parts = Vec::new();
        for raw_part in trimmed.split(',') {
            parts.push(parse_part(raw_part.trim())?);
        }
        Ok(Self { parts })
    }

    /// Resolve the selection against a document of `page_count` pages.
    ///
    /// Returns 0-based page indices, sorted ascending and de-duplicated, so a
    /// caller may use them to index `page_ids()` directly.
    ///
    /// Returns [`Error::IndexOutOfBounds`] naming the offending 1-based page
    /// number if any part reaches past the end of the document.
    pub fn resolve(&self, page_count: usize) -> Result<Vec<usize>> {
        let mut indices = Vec::new();
        for part in &self.parts {
            //--- an open end means "to the last page", which only the document knows ---
            let last = part.last.unwrap_or(page_count);
            //--- report the first offending number, so the message names what the user typed ---
            let offending = if part.first > page_count {
                Some(part.first)
            } else if last > page_count {
                Some(last)
            } else {
                None
            };
            if let Some(index) = offending {
                return Err(Error::IndexOutOfBounds { index, page_count });
            }
            //--- `A-` on a document shorter than `A` is caught above, so this range is never inverted ---
            for page_number in part.first..=last {
                indices.push(page_number - 1);
            }
        }

        indices.sort_unstable();
        indices.dedup();
        if indices.is_empty() {
            return Err(Error::Unsupported(format!(
                "the page selection resolves to no pages in a document of {page_count} pages"
            )));
        }
        Ok(indices)
    }
}

//---------------------------------------------------------------------
// Parsing one part
//---------------------------------------------------------------------

/// Parse a single comma-separated part, already trimmed of whitespace.
fn parse_part(part: &str) -> Result<SelectionPart> {
    if part.is_empty() {
        return Err(Error::Unsupported(
            "empty part in the page selection: two commas with nothing between them".to_string(),
        ));
    }

    match part.split_once('-') {
        None => {
            let page_number = parse_page_number(part)?;
            Ok(SelectionPart {
                first: page_number,
                last: Some(page_number),
            })
        }
        Some((before, after)) => {
            //--- a second dash is not a range, it is a typo; `split_once` would silently keep it in `after` ---
            if after.contains('-') {
                return Err(Error::Unsupported(format!("'{part}' is not a page range: a range holds one dash")));
            }
            match (before.trim(), after.trim()) {
                ("", "") => Err(Error::Unsupported(
                    "'-' is not a page range: give at least one end, for example 3- or -3".to_string(),
                )),
                ("", end) => Ok(SelectionPart {
                    first: 1,
                    last: Some(parse_page_number(end)?),
                }),
                (start, "") => Ok(SelectionPart {
                    first: parse_page_number(start)?,
                    last: None,
                }),
                (start, end) => {
                    let first = parse_page_number(start)?;
                    let last = parse_page_number(end)?;
                    //--- both ends can be perfectly valid pages on their own, which is exactly what InvalidRange is for ---
                    if last < first {
                        return Err(Error::InvalidRange { start: first, end: last });
                    }
                    Ok(SelectionPart { first, last: Some(last) })
                }
            }
        }
    }
}

/// Parse one 1-based page number.
fn parse_page_number(text: &str) -> Result<usize> {
    if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(Error::Unsupported(format!("'{text}' is not a page number")));
    }
    let page_number: usize = text.parse().map_err(|_| Error::Unsupported(format!("page number '{text}' is too large")))?;
    if page_number == 0 {
        return Err(Error::Unsupported("page numbers are 1-based, so 0 is not a page".to_string()));
    }
    Ok(page_number)
}

//---------------------------------------------------------------------
// Contiguous runs
//---------------------------------------------------------------------

/// Split sorted, de-duplicated indices into half-open contiguous runs.
///
/// `[0, 1, 2, 6, 8, 9]` becomes `[(0, 3), (6, 7), (8, 10)]`. This is what
/// turns an arbitrary selection into a sequence of calls to
/// `opdf_ops::extract_range`, which only ever copies one contiguous range.
pub fn contiguous_runs(indices: &[usize]) -> Vec<(usize, usize)> {
    let mut runs: Vec<(usize, usize)> = Vec::new();
    for &index in indices {
        match runs.last_mut() {
            //--- extend the open run while the indices stay consecutive ---
            Some(run) if run.1 == index => run.1 = index + 1,
            _ => runs.push((index, index + 1)),
        }
    }
    runs
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resolve(text: &str, page_count: usize) -> Result<Vec<usize>> {
        PageSelection::parse(text)?.resolve(page_count)
    }

    //---------------------------------------------------------------------
    // Syntax
    //---------------------------------------------------------------------

    #[test]
    fn a_single_page_number_selects_that_page_zero_based() {
        assert_eq!(resolve("1", 5).unwrap(), vec![0], "page 1 on the command line is index 0 in the document");
        assert_eq!(resolve("5", 5).unwrap(), vec![4]);
    }

    #[test]
    fn a_closed_range_includes_both_ends() {
        assert_eq!(resolve("2-4", 5).unwrap(), vec![1, 2, 3], "2-4 is three pages, not two");
    }

    #[test]
    fn an_open_ended_range_runs_to_the_last_page() {
        assert_eq!(resolve("3-", 5).unwrap(), vec![2, 3, 4]);
    }

    #[test]
    fn a_range_open_at_the_front_starts_at_the_first_page() {
        assert_eq!(resolve("-3", 5).unwrap(), vec![0, 1, 2]);
    }

    #[test]
    fn parts_combine_and_are_sorted_and_de_duplicated() {
        assert_eq!(
            resolve("9-,1,1-3,7", 10).unwrap(),
            vec![0, 1, 2, 6, 8, 9],
            "overlapping and out-of-order parts resolve to one ascending run of distinct pages"
        );
    }

    #[test]
    fn whitespace_around_parts_and_ends_is_ignored() {
        assert_eq!(resolve(" 1 , 3 - 4 ", 5).unwrap(), vec![0, 2, 3]);
    }

    //---------------------------------------------------------------------
    // Rejections -- none of these may panic
    //---------------------------------------------------------------------

    /// The finding this module exists to prevent: `extract_range` used to
    /// panic on an inverted range, and a CLI hands one straight through.
    #[test]
    fn an_inverted_range_is_a_clean_invalid_range_error() {
        let error = PageSelection::parse("5-2").expect_err("5-2 must not parse");
        assert!(
            matches!(error, Error::InvalidRange { start: 5, end: 2 }),
            "an inverted range must report InvalidRange with the numbers the user typed, got {error:?}"
        );
    }

    #[test]
    fn an_inverted_range_is_rejected_before_any_document_is_consulted() {
        //--- parse() takes no page count, so the rejection cannot depend on a document ---
        assert!(PageSelection::parse("10-9").is_err());
        assert!(PageSelection::parse("2-1").is_err());
    }

    #[test]
    fn page_zero_is_rejected_because_pages_are_one_based() {
        let error = PageSelection::parse("0").expect_err("0 must not parse");
        assert!(matches!(error, Error::Unsupported(_)), "got {error:?}");
        assert!(PageSelection::parse("0-3").is_err(), "an inverted-looking range starting at 0 is still page 0");
    }

    #[test]
    fn an_empty_selection_is_rejected() {
        assert!(PageSelection::parse("").is_err());
        assert!(PageSelection::parse("   ").is_err());
    }

    #[test]
    fn an_empty_part_is_rejected_rather_than_skipped() {
        assert!(PageSelection::parse("1,,3").is_err(), "a stray comma is a typo, not an empty selection");
        assert!(PageSelection::parse("1,").is_err());
    }

    #[test]
    fn a_bare_dash_is_rejected() {
        assert!(PageSelection::parse("-").is_err());
    }

    #[test]
    fn a_second_dash_is_rejected_rather_than_silently_absorbed() {
        assert!(PageSelection::parse("1-2-3").is_err());
    }

    #[test]
    fn non_numeric_parts_are_rejected() {
        for text in ["a", "1a", "1-a", "a-1", "+1", "1.5", "١"] {
            assert!(PageSelection::parse(text).is_err(), "'{text}' must not parse as a page selection");
        }
    }

    #[test]
    fn a_page_number_too_large_for_usize_is_rejected_rather_than_wrapping() {
        let error = PageSelection::parse("99999999999999999999999999").expect_err("an overlarge page number must not parse");
        assert!(matches!(error, Error::Unsupported(_)), "got {error:?}");
    }

    //---------------------------------------------------------------------
    // Resolution against a document
    //---------------------------------------------------------------------

    #[test]
    fn a_page_past_the_end_reports_index_out_of_bounds_with_the_number_typed() {
        let error = resolve("7", 5).expect_err("page 7 of a 5-page document must fail");
        assert!(matches!(error, Error::IndexOutOfBounds { index: 7, page_count: 5 }), "got {error:?}");
    }

    #[test]
    fn a_range_whose_end_is_past_the_end_reports_the_end() {
        let error = resolve("4-9", 5).expect_err("4-9 of a 5-page document must fail");
        assert!(matches!(error, Error::IndexOutOfBounds { index: 9, page_count: 5 }), "got {error:?}");
    }

    #[test]
    fn an_open_ended_range_starting_past_the_end_is_rejected_rather_than_empty() {
        let error = resolve("9-", 5).expect_err("9- of a 5-page document must fail");
        assert!(matches!(error, Error::IndexOutOfBounds { index: 9, page_count: 5 }), "got {error:?}");
    }

    #[test]
    fn every_page_of_the_document_can_be_selected() {
        assert_eq!(resolve("1-", 3).unwrap(), vec![0, 1, 2]);
        assert_eq!(resolve("-3", 3).unwrap(), vec![0, 1, 2]);
    }

    //---------------------------------------------------------------------
    // Contiguous runs
    //---------------------------------------------------------------------

    #[test]
    fn contiguous_indices_form_one_half_open_run() {
        assert_eq!(contiguous_runs(&[0, 1, 2]), vec![(0, 3)]);
    }

    #[test]
    fn a_gap_starts_a_new_run() {
        assert_eq!(contiguous_runs(&[0, 1, 2, 6, 8, 9]), vec![(0, 3), (6, 7), (8, 10)]);
    }

    #[test]
    fn an_empty_selection_has_no_runs() {
        assert_eq!(contiguous_runs(&[]), Vec::new());
    }

    #[test]
    fn a_single_index_is_a_run_of_one() {
        assert_eq!(contiguous_runs(&[4]), vec![(4, 5)]);
    }

    #[test]
    fn runs_cover_exactly_the_indices_they_were_built_from() {
        let indices = resolve("9-,1,1-3,7", 10).unwrap();
        let covered: Vec<usize> = contiguous_runs(&indices).into_iter().flat_map(|(start, end)| start..end).collect();
        assert_eq!(covered, indices, "the runs must name the same pages as the selection, in the same order");
    }
}
