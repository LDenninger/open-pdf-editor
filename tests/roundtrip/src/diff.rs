//! Structural comparison between two PDF byte buffers.
//!
//! This is the machinery behind the correctness promise: opening a document
//! and saving it unedited must produce a file that is structurally
//! identical to the original, even though the bytes themselves may differ
//! -- an incremental save appends rather than overwrites. "Structurally
//! identical" is defined here as: the same page count, the same geometry
//! (size and rotation) for every page in the same order, and the same set
//! of objects reachable from the document catalog.

use std::collections::{HashSet, VecDeque};

use lopdf::{Dictionary, Document, Object, ObjectId};

/// A page's geometry, extracted directly from PDF object structure --
/// independent of any particular [`opdf_core::Document`] implementation, so
/// this crate can diff two byte buffers without one existing.
#[derive(Clone, PartialEq, Debug)]
pub struct PageGeometry {
    /// Page width in PDF points, from `/MediaBox`.
    pub width_pt: f64,
    /// Page height in PDF points, from `/MediaBox`.
    pub height_pt: f64,
    /// The page's `/Rotate` entry in degrees, defaulting to 0 when absent.
    pub rotation_degrees: i64,
}

/// One page's geometry differing between two document revisions.
#[derive(Clone, PartialEq, Debug)]
pub struct PageGeometryChange {
    /// Zero-based position in document order.
    pub page_index: usize,
    /// Geometry in the first buffer.
    pub before: PageGeometry,
    /// Geometry in the second buffer.
    pub after: PageGeometry,
}

/// Everything that differs structurally between two PDF byte buffers.
///
/// An empty diff (`is_empty()` is `true`) is what the round-trip harness
/// asserts for an open-then-save-unchanged cycle.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct StructuralDiff {
    /// Page counts before and after, present only if they differ.
    pub page_count_changed: Option<(usize, usize)>,
    /// Pages whose size or rotation changed, matched by position.
    pub page_geometry_changes: Vec<PageGeometryChange>,
    /// Object identities reachable before but not after.
    pub objects_removed: Vec<ObjectId>,
    /// Object identities reachable after but not before.
    pub objects_added: Vec<ObjectId>,
}

impl StructuralDiff {
    /// Whether the two documents are structurally identical by every check
    /// this type performs.
    pub fn is_empty(&self) -> bool {
        self.page_count_changed.is_none() && self.page_geometry_changes.is_empty() && self.objects_removed.is_empty() && self.objects_added.is_empty()
    }
}

impl std::fmt::Display for StructuralDiff {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.is_empty() {
            return write!(formatter, "no structural differences");
        }
        if let Some((before, after)) = self.page_count_changed {
            writeln!(formatter, "page count changed: {before} -> {after}")?;
        }
        for change in &self.page_geometry_changes {
            writeln!(
                formatter,
                "page {} geometry changed: {:?} -> {:?}",
                change.page_index, change.before, change.after
            )?;
        }
        if !self.objects_removed.is_empty() {
            writeln!(
                formatter,
                "{} objects no longer reachable: {:?}",
                self.objects_removed.len(),
                self.objects_removed
            )?;
        }
        if !self.objects_added.is_empty() {
            writeln!(formatter, "{} newly reachable objects: {:?}", self.objects_added.len(), self.objects_added)?;
        }
        Ok(())
    }
}

/// Failure to parse one of the two buffers being diffed.
#[derive(Debug, thiserror::Error)]
pub enum DiffError {
    /// One of the two buffers is not a parseable PDF.
    #[error("failed to parse pdf: {0}")]
    Parse(#[from] lopdf::Error),
}

/// Compare two PDF byte buffers structurally.
///
/// `before` and `after` do not need to be byte-identical -- an incremental
/// save appends new bytes even when nothing changed -- only structurally
/// identical: same page count and geometry in the same order, and the same
/// set of objects reachable from the document catalog.
pub fn diff_bytes(before: &[u8], after: &[u8]) -> Result<StructuralDiff, DiffError> {
    let before_doc = Document::load_mem(before)?;
    let after_doc = Document::load_mem(after)?;

    let mut diff = StructuralDiff::default();

    //--- page count and geometry, matched by document order ---
    let before_pages = ordered_page_geometry(&before_doc);
    let after_pages = ordered_page_geometry(&after_doc);

    if before_pages.len() != after_pages.len() {
        diff.page_count_changed = Some((before_pages.len(), after_pages.len()));
    }

    for (index, (before_page, after_page)) in before_pages.iter().zip(after_pages.iter()).enumerate() {
        if before_page != after_page {
            diff.page_geometry_changes.push(PageGeometryChange {
                page_index: index,
                before: before_page.clone(),
                after: after_page.clone(),
            });
        }
    }

    //--- object graph reachable from the trailer's /Root ---
    let before_reachable = reachable_object_ids(&before_doc);
    let after_reachable = reachable_object_ids(&after_doc);

    diff.objects_removed = before_reachable.difference(&after_reachable).copied().collect();
    diff.objects_added = after_reachable.difference(&before_reachable).copied().collect();
    diff.objects_removed.sort_unstable();
    diff.objects_added.sort_unstable();

    Ok(diff)
}

fn ordered_page_geometry(document: &Document) -> Vec<PageGeometry> {
    // get_pages() returns a BTreeMap<page_number, ObjectId>, so this
    // iterates in page-number order, not object-id order.
    document
        .get_pages()
        .into_iter()
        .filter_map(|(_page_number, id)| page_geometry(document, id))
        .collect()
}

fn page_geometry(document: &Document, page_id: ObjectId) -> Option<PageGeometry> {
    let page_dict = document.get_object(page_id).ok()?.as_dict().ok()?;
    let media_box = resolve_array(document, page_dict, b"MediaBox")?;
    // A /MediaBox is four numbers, but the buffers this engine is pointed at
    // are malformed by design -- index through get() so a short array yields
    // "no geometry for this page" rather than panicking inside the harness
    // whose entire purpose is to characterize malformed input.
    if media_box.len() < 4 {
        return None;
    }
    let width_pt = (as_f64(&media_box[2]) - as_f64(&media_box[0])).abs();
    let height_pt = (as_f64(&media_box[3]) - as_f64(&media_box[1])).abs();
    let rotation_degrees = page_dict.get(b"Rotate").ok().and_then(|value| value.as_i64().ok()).unwrap_or(0);
    Some(PageGeometry {
        width_pt,
        height_pt,
        rotation_degrees,
    })
}

fn resolve_array(document: &Document, dict: &Dictionary, key: &[u8]) -> Option<Vec<Object>> {
    let value = dict.get(key).ok()?;
    let resolved = document.dereference(value).ok()?.1;
    resolved.as_array().ok().cloned()
}

fn as_f64(object: &Object) -> f64 {
    // lopdf models a PDF number as either Integer(i64) or Real(f32). Take the
    // integer path first, so an integral /MediaBox entry converts exactly
    // rather than being rounded through f32 on the way.
    object
        .as_i64()
        .map(|value| value as f64)
        .or_else(|_| object.as_f32().map(f64::from))
        .unwrap_or(0.0)
}

fn reachable_object_ids(document: &Document) -> HashSet<ObjectId> {
    let mut visited = HashSet::new();
    let mut queue = VecDeque::new();

    if let Ok(Object::Reference(id)) = document.trailer.get(b"Root") {
        queue.push_back(*id);
    }

    while let Some(id) = queue.pop_front() {
        if !visited.insert(id) {
            continue; // already visited -- guards against reference cycles in malformed files
        }
        if let Ok(object) = document.get_object(id) {
            collect_references(object, &mut queue);
        }
    }
    visited
}

fn collect_references(object: &Object, queue: &mut VecDeque<ObjectId>) {
    match object {
        Object::Reference(id) => queue.push_back(*id),
        Object::Array(items) => items.iter().for_each(|item| collect_references(item, queue)),
        Object::Dictionary(dict) => dict.iter().for_each(|(_key, value)| collect_references(value, queue)),
        Object::Stream(stream) => stream.dict.iter().for_each(|(_key, value)| collect_references(value, queue)),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::corpus::CorpusManifest;
    use std::path::Path;

    fn read_corpus_file(name: &str) -> Vec<u8> {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../corpus/files").join(name);
        std::fs::read(path).unwrap()
    }

    #[test]
    fn diffing_a_file_against_itself_is_empty() {
        let bytes = read_corpus_file("irs_f1040.pdf");
        let diff = diff_bytes(&bytes, &bytes).unwrap();
        assert!(diff.is_empty(), "a file must diff as identical to itself: {diff}");
    }

    #[test]
    fn diffing_two_different_real_files_is_not_empty() {
        let before = read_corpus_file("irs_f1040.pdf");
        let after = read_corpus_file("zh_wiki_monthly.pdf");
        let diff = diff_bytes(&before, &after).unwrap();
        assert!(!diff.is_empty(), "structurally different documents (2 pages vs 14) must not diff as identical");
        assert_eq!(diff.page_count_changed, Some((2, 14)));
    }

    #[test]
    fn diffing_object_stream_variant_against_its_source_is_empty() {
        // qpdf --object-streams=generate changes every byte's position and
        // most object numbers, but must not change the document structure
        // this diff engine cares about.
        let before = read_corpus_file("irs_f1040.pdf");
        let after = read_corpus_file("irs_f1040_object_streams.pdf");
        let diff = diff_bytes(&before, &after).unwrap();
        assert!(
            diff.page_count_changed.is_none(),
            "compressing into object streams must not change the page count: {diff}"
        );
        assert!(
            diff.page_geometry_changes.is_empty(),
            "compressing into object streams must not change page geometry: {diff}"
        );
    }

    #[test]
    fn parsing_a_truncated_file_fails_cleanly() {
        let bytes = read_corpus_file("irs_f1040_truncated_10k.pdf");
        let result = diff_bytes(&bytes, &bytes);
        assert!(result.is_err(), "a truncated file must fail to parse, not panic");
    }

    #[test]
    fn the_manifest_covers_every_file_this_module_reads() {
        // Guards against this test module silently reading a corpus file
        // that was never added to the manifest.
        let manifest_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../corpus/manifest.toml");
        let manifest = CorpusManifest::load(&manifest_path).unwrap();
        let known: Vec<&str> = manifest.checked_in().map(|entry| entry.file.as_str()).collect();
        for name in [
            "irs_f1040.pdf",
            "zh_wiki_monthly.pdf",
            "irs_f1040_object_streams.pdf",
            "irs_f1040_truncated_10k.pdf",
        ] {
            assert!(known.contains(&name), "{name} is read by diff.rs's tests but missing from manifest.toml");
        }
    }
}
