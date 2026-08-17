//! Turning `argv` into a validated [`Invocation`].
//!
//! Hand-rolled rather than delegated to a parsing crate — see `main.rs` for
//! why. Two rules make the scanner small enough to be obviously right:
//!
//! - An item is a flag if and only if it begins with `--`. `--force` is the
//!   only flag that takes no value; every other flag consumes either the rest
//!   of its own item after an `=`, or the item that follows it.
//! - A bare `--` ends the flags, so a path that begins with dashes can still
//!   be named.
//!
//! Paths stay `OsString` all the way through, so a file name that is not
//! valid UTF-8 is opened rather than rejected. Only flag names and the values
//! of `--pages`, `--degrees` and `--at` are required to be text.

use std::ffi::OsString;
use std::path::PathBuf;

use opdf_core::Rotation;

use crate::error::CliError;
use crate::range::PageSelection;

//---------------------------------------------------------------------
// What a command line asks for
//---------------------------------------------------------------------

/// Arguments of `opdf-cli merge`.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct MergeArgs {
    /// Where the concatenated document is written.
    pub output_path: PathBuf,
    /// The documents to concatenate, in the order they were named.
    pub input_paths: Vec<PathBuf>,
    /// Whether an existing output file may be overwritten.
    pub force: bool,
}

/// Arguments of `opdf-cli split`.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct SplitArgs {
    /// The document to split.
    pub input_path: PathBuf,
    /// 1-based number of the first page of the second half.
    pub at_page: usize,
    /// Where the pages before `at_page` are written.
    pub out_a_path: PathBuf,
    /// Where `at_page` and everything after it is written.
    pub out_b_path: PathBuf,
    /// Whether existing output files may be overwritten.
    pub force: bool,
}

/// Arguments of `opdf-cli rotate`.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct RotateArgs {
    /// The document to rotate pages in.
    pub input_path: PathBuf,
    /// Which pages to rotate.
    pub pages: PageSelection,
    /// The orientation those pages are set to — absolute, not a delta.
    pub rotation: Rotation,
    /// Where to write. `None` means in place, back over `input_path`.
    pub output_path: Option<PathBuf>,
    /// Whether an existing output file may be overwritten.
    pub force: bool,
}

/// Arguments of `opdf-cli extract`.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ExtractArgs {
    /// The document to copy pages out of. It is never modified.
    pub input_path: PathBuf,
    /// Which pages to copy.
    pub pages: PageSelection,
    /// Where the new document is written.
    pub output_path: PathBuf,
    /// Whether an existing output file may be overwritten.
    pub force: bool,
}

/// One fully validated command line.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Invocation {
    /// Print the help text and exit successfully.
    Help,
    /// Print the version and exit successfully.
    Version,
    /// Concatenate documents.
    Merge(MergeArgs),
    /// Split a document at a page boundary.
    Split(SplitArgs),
    /// Rotate a selection of pages.
    Rotate(RotateArgs),
    /// Copy a selection of pages into a new document.
    Extract(ExtractArgs),
}

//---------------------------------------------------------------------
// Parsing
//---------------------------------------------------------------------

/// The one flag that stands alone; every other flag takes a value.
const SWITCHES: [&str; 1] = ["force"];

/// Parse the arguments following the program name.
///
/// Every value is validated here, before any file is touched: a page range
/// that does not parse, a rotation that is not a quarter turn, and a missing
/// output path all fail while the disk is still untouched.
pub fn parse_args(items: &[OsString]) -> Result<Invocation, CliError> {
    //--- help and version win over everything, including a malformed rest of the line ---
    if let Some(invocation) = find_help_or_version(items) {
        return Ok(invocation);
    }

    let Some((subcommand, rest)) = items.split_first() else {
        return Err(CliError::Usage(
            "no subcommand given; run `opdf-cli --help` to see the four that exist".to_string(),
        ));
    };
    let subcommand = subcommand
        .to_str()
        .ok_or_else(|| CliError::Usage("the subcommand must be one of merge, split, rotate, extract".to_string()))?;

    //--- the flag set is per subcommand, and the scanner needs it: an unrecognized flag must be
    //--- named rather than silently swallowing the positional that follows it ---
    let value_flags: &[&str] = match subcommand {
        "merge" => &[],
        "split" => &["at", "out-a", "out-b"],
        "rotate" => &["pages", "degrees", "out"],
        "extract" => &["pages", "out"],
        other => {
            return Err(CliError::Usage(format!(
                "unknown subcommand '{other}'; expected merge, split, rotate, or extract"
            )));
        }
    };

    let mut line = CommandLine::scan(rest, value_flags, subcommand)?;
    let invocation = match subcommand {
        "merge" => Invocation::Merge(parse_merge(&mut line)?),
        "split" => Invocation::Split(parse_split(&mut line)?),
        "rotate" => Invocation::Rotate(parse_rotate(&mut line)?),
        "extract" => Invocation::Extract(parse_extract(&mut line)?),
        //--- every other name was rejected while choosing the flag set above ---
        other => return Err(CliError::Usage(format!("unknown subcommand '{other}'"))),
    };
    //--- anything still unclaimed belongs to no subcommand, so saying nothing would be silently ignoring it ---
    line.reject_leftovers()?;
    Ok(invocation)
}

/// Look for `--help`/`-h` or `--version`/`-V` anywhere on the line.
fn find_help_or_version(items: &[OsString]) -> Option<Invocation> {
    for item in items {
        match item.to_str() {
            Some("--help" | "-h") => return Some(Invocation::Help),
            Some("--version" | "-V") => return Some(Invocation::Version),
            //--- a path called "--help" is reachable after `--`, so stop looking there ---
            Some("--") => return None,
            _ => {}
        }
    }
    None
}

//---------------------------------------------------------------------
// The scanned line
//---------------------------------------------------------------------

/// Positional arguments and flags, separated but not yet interpreted.
struct CommandLine {
    positionals: Vec<PathBuf>,
    /// Flags in the order given. A `None` value marks a switch.
    flags: Vec<(String, Option<OsString>)>,
}

impl CommandLine {
    /// Split a subcommand's arguments into positionals and flags.
    ///
    /// `value_flags` names every flag this subcommand accepts a value for.
    /// A flag outside that set and outside [`SWITCHES`] is reported by name
    /// rather than assumed to take a value, which is what stops
    /// `merge --recursive out.pdf a.pdf` from quietly reading `out.pdf` as
    /// the value of a flag that does not exist.
    fn scan(items: &[OsString], value_flags: &[&str], subcommand: &str) -> Result<Self, CliError> {
        let mut positionals = Vec::new();
        let mut flags: Vec<(String, Option<OsString>)> = Vec::new();
        let mut index = 0;
        while index < items.len() {
            let item = &items[index];
            index += 1;

            //--- only a valid-UTF-8 item can be a flag, which is what keeps paths opaque ---
            let Some(text) = item.to_str() else {
                positionals.push(PathBuf::from(item));
                continue;
            };
            if text == "--" {
                positionals.extend(items[index..].iter().map(PathBuf::from));
                break;
            }
            let Some(flag) = text.strip_prefix("--") else {
                positionals.push(PathBuf::from(item));
                continue;
            };

            let (name, inline_value) = match flag.split_once('=') {
                Some((name, value)) => (name.to_string(), Some(OsString::from(value))),
                None => (flag.to_string(), None),
            };
            if flags.iter().any(|(seen, _)| *seen == name) {
                return Err(CliError::Usage(format!("--{name} was given twice")));
            }

            if SWITCHES.contains(&name.as_str()) {
                if inline_value.is_some() {
                    return Err(CliError::Usage(format!("--{name} takes no value")));
                }
                flags.push((name, None));
                continue;
            }
            if !value_flags.contains(&name.as_str()) {
                return Err(CliError::Usage(format!("unknown flag --{name} for `opdf-cli {subcommand}`")));
            }

            let value = match inline_value {
                Some(value) => value,
                None => {
                    //--- a flag must not eat the next flag: that turns a typo into a wrong operation ---
                    let next = items.get(index).filter(|next| !starts_a_flag(next));
                    let value = next.cloned().ok_or_else(|| CliError::Usage(format!("--{name} needs a value")))?;
                    index += 1;
                    value
                }
            };
            flags.push((name, Some(value)));
        }
        Ok(Self { positionals, flags })
    }

    /// Take the value of a flag that carries one.
    fn take_value(&mut self, name: &str) -> Result<Option<OsString>, CliError> {
        let Some(position) = self.flags.iter().position(|(seen, _)| seen == name) else {
            return Ok(None);
        };
        let (_, value) = self.flags.remove(position);
        value.map_or_else(|| Err(CliError::Usage(format!("--{name} needs a value"))), |value| Ok(Some(value)))
    }

    /// Take the value of a flag that must be present.
    fn require_value(&mut self, name: &str, subcommand: &str) -> Result<OsString, CliError> {
        self.take_value(name)?.ok_or_else(|| CliError::Usage(format!("{subcommand} needs --{name}")))
    }

    /// Take a flag that stands alone.
    fn take_switch(&mut self, name: &str) -> bool {
        match self.flags.iter().position(|(seen, _)| seen == name) {
            Some(position) => {
                self.flags.remove(position);
                true
            }
            None => false,
        }
    }

    /// Take exactly `count` positionals, or report how many were expected.
    fn take_positionals(&mut self, count: usize, expected: &str) -> Result<Vec<PathBuf>, CliError> {
        if self.positionals.len() < count {
            return Err(CliError::Usage(format!("expected {expected}")));
        }
        Ok(self.positionals.drain(..count).collect())
    }

    /// Take every remaining positional.
    fn take_remaining_positionals(&mut self) -> Vec<PathBuf> {
        std::mem::take(&mut self.positionals)
    }

    /// Fail if anything on the line went unclaimed.
    fn reject_leftovers(&self) -> Result<(), CliError> {
        if let Some((name, _)) = self.flags.first() {
            return Err(CliError::Usage(format!("unknown flag --{name} for this subcommand")));
        }
        if let Some(extra) = self.positionals.first() {
            return Err(CliError::Usage(format!("unexpected argument '{}'", extra.display())));
        }
        Ok(())
    }
}

/// Whether an item would be read as a flag rather than as a value.
fn starts_a_flag(item: &OsString) -> bool {
    item.to_str().is_some_and(|text| text.starts_with("--"))
}

//---------------------------------------------------------------------
// Typed flag values
//---------------------------------------------------------------------

/// Read a flag's value as text.
fn read_text(value: &OsString, name: &str) -> Result<String, CliError> {
    value
        .to_str()
        .map(str::to_string)
        .ok_or_else(|| CliError::Usage(format!("the value of --{name} must be text")))
}

/// Read `--pages`, keeping the library's own diagnosis of a bad range.
fn read_pages(value: &OsString) -> Result<PageSelection, CliError> {
    let text = read_text(value, "pages")?;
    PageSelection::parse(&text).map_err(|source| CliError::Argument {
        flag: "pages".to_string(),
        source,
    })
}

/// Read `--degrees` as an absolute page orientation.
fn read_rotation(value: &OsString) -> Result<Rotation, CliError> {
    let text = read_text(value, "degrees")?;
    let degrees: i32 = text
        .parse()
        .map_err(|_| CliError::Usage(format!("--degrees: '{text}' is not a whole number of degrees")))?;
    Rotation::from_degrees(degrees).map_err(|source| CliError::Argument {
        flag: "degrees".to_string(),
        source,
    })
}

/// Read `--at` as a 1-based page number.
fn read_page_number(value: &OsString) -> Result<usize, CliError> {
    let text = read_text(value, "at")?;
    let page_number: usize = text.parse().map_err(|_| CliError::Usage(format!("--at: '{text}' is not a page number")))?;
    if page_number == 0 {
        return Err(CliError::Usage("--at: page numbers are 1-based, so 0 is not a page".to_string()));
    }
    Ok(page_number)
}

//---------------------------------------------------------------------
// One parser per subcommand
//---------------------------------------------------------------------

/// Parse `merge OUT.pdf IN.pdf [IN.pdf ...]`.
fn parse_merge(line: &mut CommandLine) -> Result<MergeArgs, CliError> {
    let force = line.take_switch("force");
    let mut paths = line.take_positionals(1, "merge OUT.pdf IN1.pdf [IN2.pdf ...]")?;
    let output_path = paths.remove(0);
    let input_paths = line.take_remaining_positionals();
    if input_paths.is_empty() {
        return Err(CliError::Usage("merge needs at least one input document after the output path".to_string()));
    }
    Ok(MergeArgs {
        output_path,
        input_paths,
        force,
    })
}

/// Parse `split IN.pdf --at N --out-a A.pdf --out-b B.pdf`.
fn parse_split(line: &mut CommandLine) -> Result<SplitArgs, CliError> {
    let force = line.take_switch("force");
    let at_page = read_page_number(&line.require_value("at", "split")?)?;
    let out_a_path = PathBuf::from(line.require_value("out-a", "split")?);
    let out_b_path = PathBuf::from(line.require_value("out-b", "split")?);
    let mut paths = line.take_positionals(1, "split IN.pdf --at N --out-a A.pdf --out-b B.pdf")?;
    Ok(SplitArgs {
        input_path: paths.remove(0),
        at_page,
        out_a_path,
        out_b_path,
        force,
    })
}

/// Parse `rotate IN.pdf --pages RANGE --degrees D [--out OUT.pdf]`.
fn parse_rotate(line: &mut CommandLine) -> Result<RotateArgs, CliError> {
    let force = line.take_switch("force");
    let pages = read_pages(&line.require_value("pages", "rotate")?)?;
    let rotation = read_rotation(&line.require_value("degrees", "rotate")?)?;
    let output_path = line.take_value("out")?.map(PathBuf::from);
    let mut paths = line.take_positionals(1, "rotate IN.pdf --pages RANGE --degrees D [--out OUT.pdf]")?;
    Ok(RotateArgs {
        input_path: paths.remove(0),
        pages,
        rotation,
        output_path,
        force,
    })
}

/// Parse `extract IN.pdf --pages RANGE --out OUT.pdf`.
fn parse_extract(line: &mut CommandLine) -> Result<ExtractArgs, CliError> {
    let force = line.take_switch("force");
    let pages = read_pages(&line.require_value("pages", "extract")?)?;
    let output_path = PathBuf::from(line.require_value("out", "extract")?);
    let mut paths = line.take_positionals(1, "extract IN.pdf --pages RANGE --out OUT.pdf")?;
    Ok(ExtractArgs {
        input_path: paths.remove(0),
        pages,
        output_path,
        force,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(items: &[&str]) -> Result<Invocation, CliError> {
        let owned: Vec<OsString> = items.iter().map(OsString::from).collect();
        parse_args(&owned)
    }

    fn merge_args(items: &[&str]) -> MergeArgs {
        match parse(items).expect("must parse") {
            Invocation::Merge(args) => args,
            other => panic!("expected a merge, got {other:?}"),
        }
    }

    fn split_args(items: &[&str]) -> SplitArgs {
        match parse(items).expect("must parse") {
            Invocation::Split(args) => args,
            other => panic!("expected a split, got {other:?}"),
        }
    }

    fn rotate_args(items: &[&str]) -> RotateArgs {
        match parse(items).expect("must parse") {
            Invocation::Rotate(args) => args,
            other => panic!("expected a rotate, got {other:?}"),
        }
    }

    fn extract_args(items: &[&str]) -> ExtractArgs {
        match parse(items).expect("must parse") {
            Invocation::Extract(args) => args,
            other => panic!("expected an extract, got {other:?}"),
        }
    }

    //---------------------------------------------------------------------
    // merge
    //---------------------------------------------------------------------

    #[test]
    fn merge_takes_the_output_first_and_keeps_the_inputs_in_order() {
        let args = merge_args(&["merge", "out.pdf", "a.pdf", "b.pdf", "c.pdf"]);
        assert_eq!(args.output_path, PathBuf::from("out.pdf"));
        assert_eq!(
            args.input_paths,
            vec![PathBuf::from("a.pdf"), PathBuf::from("b.pdf"), PathBuf::from("c.pdf")],
            "argument order is page order, so it must survive parsing exactly"
        );
        assert!(!args.force, "--force must default to off");
    }

    #[test]
    fn merge_needs_at_least_one_input() {
        assert!(parse(&["merge", "out.pdf"]).is_err(), "an output with nothing to put in it is a misuse");
        assert!(parse(&["merge"]).is_err());
    }

    #[test]
    fn merge_accepts_force() {
        assert!(merge_args(&["merge", "--force", "out.pdf", "a.pdf"]).force);
        assert!(
            merge_args(&["merge", "out.pdf", "a.pdf", "--force"]).force,
            "a switch is positional-independent"
        );
    }

    //---------------------------------------------------------------------
    // split
    //---------------------------------------------------------------------

    #[test]
    fn split_reads_the_boundary_and_both_outputs() {
        let args = split_args(&["split", "in.pdf", "--at", "4", "--out-a", "a.pdf", "--out-b", "b.pdf"]);
        assert_eq!(args.input_path, PathBuf::from("in.pdf"));
        assert_eq!(
            args.at_page, 4,
            "--at is the 1-based number the user typed; the 0-based boundary is derived later"
        );
        assert_eq!(args.out_a_path, PathBuf::from("a.pdf"));
        assert_eq!(args.out_b_path, PathBuf::from("b.pdf"));
    }

    #[test]
    fn split_requires_every_one_of_its_flags() {
        assert!(parse(&["split", "in.pdf", "--out-a", "a.pdf", "--out-b", "b.pdf"]).is_err(), "missing --at");
        assert!(parse(&["split", "in.pdf", "--at", "2", "--out-b", "b.pdf"]).is_err(), "missing --out-a");
        assert!(parse(&["split", "in.pdf", "--at", "2", "--out-a", "a.pdf"]).is_err(), "missing --out-b");
        assert!(
            parse(&["split", "--at", "2", "--out-a", "a.pdf", "--out-b", "b.pdf"]).is_err(),
            "missing the input"
        );
    }

    #[test]
    fn split_rejects_a_boundary_that_is_not_a_page_number() {
        //--- page 0 does not exist under 1-based numbering, and neither does "two" ---
        assert!(parse(&["split", "in.pdf", "--at", "0", "--out-a", "a.pdf", "--out-b", "b.pdf"]).is_err());
        assert!(parse(&["split", "in.pdf", "--at", "two", "--out-a", "a.pdf", "--out-b", "b.pdf"]).is_err());
        assert!(parse(&["split", "in.pdf", "--at", "-1", "--out-a", "a.pdf", "--out-b", "b.pdf"]).is_err());
    }

    //---------------------------------------------------------------------
    // rotate
    //---------------------------------------------------------------------

    #[test]
    fn rotate_turns_degrees_into_a_rotation() {
        assert_eq!(
            rotate_args(&["rotate", "in.pdf", "--pages", "1", "--degrees", "90"]).rotation,
            Rotation::Quarter
        );
        assert_eq!(rotate_args(&["rotate", "in.pdf", "--pages", "1", "--degrees", "180"]).rotation, Rotation::Half);
        assert_eq!(
            rotate_args(&["rotate", "in.pdf", "--pages", "1", "--degrees", "270"]).rotation,
            Rotation::ThreeQuarter
        );
        assert_eq!(
            rotate_args(&["rotate", "in.pdf", "--pages", "1", "--degrees", "0"]).rotation,
            Rotation::None,
            "0 is how a page is set back to upright"
        );
    }

    #[test]
    fn rotate_normalizes_negative_and_overlarge_degrees_like_the_library_does() {
        assert_eq!(
            rotate_args(&["rotate", "in.pdf", "--pages", "1", "--degrees", "-90"]).rotation,
            Rotation::ThreeQuarter
        );
        assert_eq!(
            rotate_args(&["rotate", "in.pdf", "--pages", "1", "--degrees", "450"]).rotation,
            Rotation::Quarter
        );
    }

    #[test]
    fn rotate_rejects_a_rotation_that_is_not_a_quarter_turn() {
        let error = parse(&["rotate", "in.pdf", "--pages", "1", "--degrees", "45"]).expect_err("45 is not a page rotation");
        assert!(
            matches!(&error, CliError::Argument { flag, source: opdf_core::Error::Unsupported(_) } if flag == "degrees"),
            "got {error:?}"
        );
        assert_eq!(error.exit_code(), 2);
    }

    #[test]
    fn rotate_writes_in_place_when_no_output_is_named() {
        let args = rotate_args(&["rotate", "in.pdf", "--pages", "1", "--degrees", "90"]);
        assert_eq!(args.output_path, None, "no --out means the input is edited in place");

        let args = rotate_args(&["rotate", "in.pdf", "--pages", "1", "--degrees", "90", "--out", "out.pdf"]);
        assert_eq!(args.output_path, Some(PathBuf::from("out.pdf")));
    }

    #[test]
    fn rotate_requires_pages_and_degrees() {
        assert!(parse(&["rotate", "in.pdf", "--degrees", "90"]).is_err(), "missing --pages");
        assert!(parse(&["rotate", "in.pdf", "--pages", "1"]).is_err(), "missing --degrees");
    }

    //---------------------------------------------------------------------
    // extract
    //---------------------------------------------------------------------

    #[test]
    fn extract_reads_a_selection_and_an_output() {
        let args = extract_args(&["extract", "in.pdf", "--pages", "1-3,7", "--out", "out.pdf"]);
        assert_eq!(args.input_path, PathBuf::from("in.pdf"));
        assert_eq!(args.output_path, PathBuf::from("out.pdf"));
        assert_eq!(args.pages.resolve(10).expect("1-3,7 fits in 10 pages"), vec![0, 1, 2, 6]);
    }

    #[test]
    fn extract_requires_an_output() {
        assert!(parse(&["extract", "in.pdf", "--pages", "1"]).is_err(), "extract never edits in place");
    }

    /// The whole point of parsing the selection here: a bad range must be
    /// refused before a single file is touched, and it must keep the
    /// library's own diagnosis rather than being flattened to a string.
    #[test]
    fn an_inverted_page_range_is_refused_at_parse_time_with_invalid_range() {
        let error = parse(&["extract", "in.pdf", "--pages", "5-2", "--out", "out.pdf"]).expect_err("5-2 must not parse");
        assert!(
            matches!(&error, CliError::Argument { flag, source: opdf_core::Error::InvalidRange { start: 5, end: 2 } } if flag == "pages"),
            "got {error:?}"
        );
    }

    //---------------------------------------------------------------------
    // Shared syntax
    //---------------------------------------------------------------------

    #[test]
    fn a_flag_may_carry_its_value_after_an_equals_sign() {
        let args = extract_args(&["extract", "in.pdf", "--pages=2-3", "--out=out.pdf"]);
        assert_eq!(args.output_path, PathBuf::from("out.pdf"));
        assert_eq!(args.pages.resolve(5).expect("2-3 fits in 5 pages"), vec![1, 2]);
    }

    #[test]
    fn a_flag_with_no_value_is_a_usage_error_rather_than_swallowing_the_next_flag() {
        let error = parse(&["extract", "in.pdf", "--pages"]).expect_err("--pages needs a value");
        assert!(matches!(error, CliError::Usage(_)), "got {error:?}");
        assert!(
            parse(&["extract", "in.pdf", "--pages", "--out", "out.pdf"]).is_err(),
            "--out must not be consumed as the value of --pages"
        );
    }

    #[test]
    fn repeating_a_flag_is_a_usage_error_rather_than_a_silent_last_one_wins() {
        assert!(parse(&["extract", "in.pdf", "--pages", "1", "--pages", "2", "--out", "o.pdf"]).is_err());
        assert!(parse(&["merge", "--force", "--force", "o.pdf", "a.pdf"]).is_err());
    }

    #[test]
    fn an_unknown_flag_is_reported_rather_than_ignored() {
        let error = parse(&["merge", "--recursive", "out.pdf", "a.pdf"]).expect_err("--recursive does not exist");
        assert!(matches!(&error, CliError::Usage(message) if message.contains("--recursive")), "got {error:?}");
    }

    #[test]
    fn a_flag_that_belongs_to_another_subcommand_is_still_unknown() {
        assert!(parse(&["rotate", "in.pdf", "--pages", "1", "--degrees", "90", "--out-a", "a.pdf"]).is_err());
    }

    #[test]
    fn a_double_dash_ends_the_flags_so_a_dashed_path_can_be_named() {
        let args = merge_args(&["merge", "--", "--out.pdf", "--a.pdf"]);
        assert_eq!(args.output_path, PathBuf::from("--out.pdf"));
        assert_eq!(args.input_paths, vec![PathBuf::from("--a.pdf")]);
    }

    #[test]
    fn too_many_positionals_is_a_usage_error() {
        assert!(parse(&["extract", "in.pdf", "extra.pdf", "--pages", "1", "--out", "o.pdf"]).is_err());
        assert!(parse(&["split", "in.pdf", "extra.pdf", "--at", "2", "--out-a", "a.pdf", "--out-b", "b.pdf"]).is_err());
    }

    #[test]
    fn help_and_version_are_recognized_anywhere_on_the_line() {
        assert_eq!(parse(&["--help"]).expect("--help"), Invocation::Help);
        assert_eq!(parse(&["-h"]).expect("-h"), Invocation::Help);
        assert_eq!(parse(&["merge", "--help"]).expect("merge --help"), Invocation::Help);
        assert_eq!(parse(&["--version"]).expect("--version"), Invocation::Version);
        assert_eq!(parse(&["-V"]).expect("-V"), Invocation::Version);
    }

    #[test]
    fn no_arguments_at_all_is_a_usage_error_that_points_at_the_help() {
        let error = parse(&[]).expect_err("an empty command line does nothing");
        assert!(matches!(&error, CliError::Usage(message) if message.contains("--help")), "got {error:?}");
        assert_eq!(error.exit_code(), 2);
    }

    #[test]
    fn an_unknown_subcommand_is_reported_by_name() {
        let error = parse(&["shuffle", "in.pdf"]).expect_err("there is no shuffle");
        assert!(matches!(&error, CliError::Usage(message) if message.contains("shuffle")), "got {error:?}");
    }

    /// A file name is bytes on Unix, not text. Rejecting one would make the
    /// tool unusable on files it is perfectly able to read.
    #[cfg(unix)]
    #[test]
    fn a_path_that_is_not_valid_utf8_survives_parsing() {
        use std::os::unix::ffi::OsStringExt;

        let odd_name = OsString::from_vec(vec![b'i', 0xff, b'.', b'p', b'd', b'f']);
        let items = vec![OsString::from("merge"), OsString::from("out.pdf"), odd_name.clone()];
        match parse_args(&items).expect("a non-UTF-8 path is still a path") {
            Invocation::Merge(args) => assert_eq!(args.input_paths, vec![PathBuf::from(odd_name)]),
            other => panic!("expected a merge, got {other:?}"),
        }
    }
}
