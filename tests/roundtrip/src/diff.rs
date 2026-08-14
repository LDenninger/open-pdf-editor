//! Structural comparison between two PDF byte buffers.
//!
//! This is the machinery behind the correctness promise: opening a document
//! and saving it unedited must produce a file that is structurally
//! identical to the original, even though the bytes themselves may differ
//! -- an incremental save appends rather than overwrites.
//!
//! # What "structurally identical" means here
//!
//! Two documents are structurally identical when the object graph reachable
//! from the catalog is **isomorphic**: same shape, same values, same stream
//! contents once decoded. Deliberately *not* part of the comparison, because
//! a conforming writer may change any of them without changing the document:
//!
//! - **object numbers** -- `qpdf --object-streams=generate` renumbers nearly
//!   every object; an incremental save appends objects under new numbers
//! - **compression** -- `/Filter`, `/DecodeParms` and `/Length` on a stream,
//!   since streams are compared by their decoded bytes
//! - **dictionary key order** -- keys are compared as a sorted set
//! - **string encoding** -- literal versus hexadecimal for the same bytes
//!
//! # How the comparison avoids object numbers
//!
//! Comparing the *set of reachable object ids*, which is what this engine
//! did originally, is wrong in both directions: it reports a difference for
//! every renumbering, and it reports none for a document whose objects were
//! rewritten in place. Both were observed -- a page could be deleted, the
//! pages reordered, a content stream replaced, a form value wiped, or an
//! object's body dropped while the reference to it remained, and the diff
//! called the result identical.
//!
//! Instead every reachable object gets a digest computed from its *contents*,
//! in which a reference contributes the digest its target had in the previous
//! round. Repeating that for [`REFINEMENT_ROUNDS`] rounds propagates each
//! object's identity outward through the graph, so two objects end with the
//! same digest only if the subgraphs around them agree to that depth. It is
//! the standard colour-refinement argument, and it has three properties this
//! job needs: object numbers never enter a digest, reference *cycles* -- the
//! `/Parent` back-edges every page tree has -- need no special handling
//! because nothing recurses, and the cost is linear in the object count per
//! round rather than exponential in the graph's depth.
//!
//! A reference whose target does not exist digests differently from every
//! present object, so dangling-reference corruption is visible rather than
//! silently skipped.

use std::collections::{HashMap, HashSet, VecDeque};

use lopdf::{Dictionary, Document, Object, ObjectId};
use sha2::{Digest, Sha256};

/// How many times each object's digest absorbs its neighbours' digests.
///
/// Each round widens the neighbourhood an object's digest summarises by one
/// reference. Four is chosen against the shape of a PDF rather than by
/// experiment: it is enough for a page's digest to depend on its resources'
/// resources -- page → `/Resources` → `/Font` → the font descriptor -- which
/// is the deepest chain a page-level edit normally disturbs. Raising it costs
/// one linear pass per round.
const REFINEMENT_ROUNDS: usize = 4;

/// How far up the `/Parent` chain an inheritable attribute is looked for.
///
/// Bounded because the buffers this engine is pointed at are malformed by
/// design, and a `/Parent` cycle must not hang the harness.
const PARENT_CHAIN_LIMIT: usize = 64;

/// Digest of an object's surroundings, independent of its object number.
type ObjectDigest = [u8; 32];

/// The digest a reference contributes when its target does not exist.
const DANGLING: &[u8] = b"dangling-reference";

/// A page's geometry, with inheritable attributes resolved up the page tree.
///
/// Extracted directly from PDF object structure -- independent of any
/// particular [`opdf_core::Document`] implementation, so this crate can diff
/// two byte buffers without one existing.
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
///
/// [`StructuralDiff::document_changed`] is the authoritative check; the page
/// fields exist to say *where* so a failure is diagnosable rather than a bare
/// "these differ".
#[derive(Clone, PartialEq, Debug, Default)]
pub struct StructuralDiff {
    /// Page counts before and after, present only if they differ.
    pub page_count_changed: Option<(usize, usize)>,
    /// Pages whose resolved geometry changed, matched by position.
    pub page_geometry_changes: Vec<PageGeometryChange>,
    /// Zero-based positions of pages whose content changed -- anything
    /// reachable from the page, including its content streams, resources,
    /// annotations, and form field values.
    pub pages_with_changed_content: Vec<usize>,
    /// Whether the object graph reachable from the catalog differs at all.
    ///
    /// True for a change anywhere, including outside the pages: outlines,
    /// the AcroForm dictionary, embedded files, document metadata.
    pub document_changed: bool,
    /// Count of references pointing at a non-existent object, before and
    /// after, present only if they differ.
    pub dangling_references_changed: Option<(usize, usize)>,
}

impl StructuralDiff {
    /// Whether the two documents are structurally identical by every check
    /// this type performs.
    pub fn is_empty(&self) -> bool {
        !self.document_changed
            && self.page_count_changed.is_none()
            && self.page_geometry_changes.is_empty()
            && self.pages_with_changed_content.is_empty()
            && self.dangling_references_changed.is_none()
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
        if !self.pages_with_changed_content.is_empty() {
            writeln!(formatter, "pages whose content changed: {:?}", self.pages_with_changed_content)?;
        }
        if let Some((before, after)) = self.dangling_references_changed {
            writeln!(formatter, "dangling references: {before} -> {after}")?;
        }
        if self.document_changed && self.page_count_changed.is_none() && self.page_geometry_changes.is_empty() && self.pages_with_changed_content.is_empty() {
            writeln!(formatter, "the object graph reachable from the catalog changed outside the pages")?;
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

//---------------------------------------------------------------------
// The comparison
//---------------------------------------------------------------------

/// Compare two PDF byte buffers structurally.
///
/// `before` and `after` do not need to be byte-identical -- an incremental
/// save appends new bytes even when nothing changed -- only structurally
/// identical in the sense given in the module documentation.
pub fn diff_bytes(before: &[u8], after: &[u8]) -> Result<StructuralDiff, DiffError> {
    let before_doc = Document::load_mem(before)?;
    let after_doc = Document::load_mem(after)?;

    let mut diff = StructuralDiff::default();

    //--- page count from the page tree itself, never from a derived vector:
    //--- a page whose geometry cannot be read must still be counted ---
    let before_page_ids: Vec<ObjectId> = before_doc.get_pages().into_values().collect();
    let after_page_ids: Vec<ObjectId> = after_doc.get_pages().into_values().collect();
    if before_page_ids.len() != after_page_ids.len() {
        diff.page_count_changed = Some((before_page_ids.len(), after_page_ids.len()));
    }

    for (index, (before_id, after_id)) in before_page_ids.iter().zip(after_page_ids.iter()).enumerate() {
        let before_geometry = page_geometry(&before_doc, *before_id);
        let after_geometry = page_geometry(&after_doc, *after_id);
        if before_geometry != after_geometry {
            diff.page_geometry_changes.push(PageGeometryChange {
                page_index: index,
                before: before_geometry,
                after: after_geometry,
            });
        }
    }

    //--- contents, compared through renumbering-invariant digests ---
    let before_digests = refine_object_digests(&before_doc);
    let after_digests = refine_object_digests(&after_doc);

    for (index, (before_id, after_id)) in before_page_ids.iter().zip(after_page_ids.iter()).enumerate() {
        if page_content_digest(&before_doc, *before_id, &before_digests) != page_content_digest(&after_doc, *after_id, &after_digests) {
            diff.pages_with_changed_content.push(index);
        }
    }

    diff.document_changed = catalog_digest(&before_doc, &before_digests) != catalog_digest(&after_doc, &after_digests);

    let before_dangling = count_dangling_references(&before_doc);
    let after_dangling = count_dangling_references(&after_doc);
    if before_dangling != after_dangling {
        diff.dangling_references_changed = Some((before_dangling, after_dangling));
    }

    Ok(diff)
}

//---------------------------------------------------------------------
// Page geometry, with inheritance
//---------------------------------------------------------------------

/// A page's geometry, resolving `/MediaBox` and `/Rotate` up the `/Parent`
/// chain when the page itself does not carry them.
///
/// Both are inheritable per ISO 32000-1 table 30, and most real documents put
/// `/MediaBox` on the root `/Pages` node rather than repeating it on every
/// page. Reading them only off the page dictionary reported "no geometry" for
/// such documents -- which, when the page count was derived from the list of
/// pages that *had* geometry, silently excused a deleted page as well.
fn page_geometry(document: &Document, page_id: ObjectId) -> PageGeometry {
    let media_box = inherited_attribute(document, page_id, b"MediaBox").and_then(|value| resolve_array(document, &value));

    let (width_pt, height_pt) = match media_box {
        // A /MediaBox is four numbers, but the buffers this engine is pointed
        // at are malformed by design: a short array yields a zero size rather
        // than panicking inside the harness whose purpose is to characterize
        // malformed input.
        Some(values) if values.len() >= 4 => ((as_f64(&values[2]) - as_f64(&values[0])).abs(), (as_f64(&values[3]) - as_f64(&values[1])).abs()),
        _ => (0.0, 0.0),
    };

    let rotation_degrees = inherited_attribute(document, page_id, b"Rotate")
        .and_then(|value| document.dereference(&value).ok().and_then(|(_, resolved)| resolved.as_i64().ok()))
        .unwrap_or(0);

    PageGeometry {
        width_pt,
        height_pt,
        rotation_degrees,
    }
}

/// Look `key` up on the page, then on each `/Parent` in turn.
fn inherited_attribute(document: &Document, page_id: ObjectId, key: &[u8]) -> Option<Object> {
    let mut current = page_id;
    let mut seen: HashSet<ObjectId> = HashSet::new();

    for _ in 0..PARENT_CHAIN_LIMIT {
        if !seen.insert(current) {
            return None; // a /Parent cycle in a malformed file
        }
        let dictionary = document.get_object(current).ok()?.as_dict().ok()?;
        if let Ok(value) = dictionary.get(key) {
            return Some(value.clone());
        }
        match dictionary.get(b"Parent") {
            Ok(Object::Reference(parent)) => current = *parent,
            _ => return None,
        }
    }
    None
}

fn resolve_array(document: &Document, value: &Object) -> Option<Vec<Object>> {
    document.dereference(value).ok()?.1.as_array().ok().cloned()
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

//---------------------------------------------------------------------
// Renumbering-invariant object digests
//---------------------------------------------------------------------

/// Digest every reachable object, refining each one's value by its
/// neighbours' for [`REFINEMENT_ROUNDS`] rounds.
///
/// See the module documentation for why this shape rather than a recursive
/// walk. The returned map covers every id reached from the catalog, including
/// ids that no object exists for.
fn refine_object_digests(document: &Document) -> HashMap<ObjectId, ObjectDigest> {
    let reachable = reachable_object_ids(document);
    let mut digests: HashMap<ObjectId, ObjectDigest> = reachable.iter().map(|id| (*id, [0_u8; 32])).collect();

    for _ in 0..REFINEMENT_ROUNDS {
        let mut next: HashMap<ObjectId, ObjectDigest> = HashMap::with_capacity(digests.len());
        for id in &reachable {
            let mut hasher = Sha256::new();
            match document.get_object(*id) {
                Ok(object) => feed_object(object, &digests, &mut hasher),
                //--- a reference into nothing: distinct from every real object ---
                Err(_) => hasher.update(DANGLING),
            }
            next.insert(*id, hasher.finalize().into());
        }
        digests = next;
    }

    digests
}

/// A page's digest with its `/Parent` edge excluded.
///
/// The refined digest of a page is not local: `/Parent` points back at the
/// page tree, which points at every other page, so after a few rounds a
/// change anywhere in the document reaches every page's digest. That is
/// harmless for deciding *whether* two documents differ — which is what
/// [`StructuralDiff::document_changed`] answers — but useless for saying
/// *which page*, and a diff that blames every page for one page's edit is
/// not worth reading.
///
/// Cutting the one upward edge leaves the page's own subtree: its content
/// streams, resources, and annotations, each still summarised by the refined
/// digests, which are themselves clean because nothing below a page points
/// back up.
fn page_content_digest(document: &Document, page_id: ObjectId, digests: &HashMap<ObjectId, ObjectDigest>) -> ObjectDigest {
    let mut hasher = Sha256::new();
    match document.get_object(page_id).and_then(|object| object.as_dict()) {
        Ok(dictionary) => feed_dictionary(dictionary, digests, &mut hasher, &[b"Parent"]),
        Err(_) => hasher.update(DANGLING),
    }
    hasher.finalize().into()
}

/// The digest of the whole document, taken at its catalog.
fn catalog_digest(document: &Document, digests: &HashMap<ObjectId, ObjectDigest>) -> Option<ObjectDigest> {
    match document.trailer.get(b"Root") {
        Ok(Object::Reference(id)) => digests.get(id).copied(),
        _ => None,
    }
}

/// Feed one object's canonical form into `hasher`.
///
/// Object numbers are never fed: a reference contributes the digest its
/// target carried in the previous round, which is what makes the result
/// invariant under renumbering.
fn feed_object(object: &Object, digests: &HashMap<ObjectId, ObjectDigest>, hasher: &mut Sha256) {
    match object {
        Object::Null => hasher.update(b"null"),
        Object::Boolean(value) => {
            hasher.update(b"bool");
            hasher.update([u8::from(*value)]);
        }
        Object::Integer(value) => {
            hasher.update(b"int");
            hasher.update(value.to_le_bytes());
        }
        Object::Real(value) => {
            hasher.update(b"real");
            hasher.update(value.to_le_bytes());
        }
        Object::Name(name) => {
            hasher.update(b"name");
            hasher.update(name);
        }
        //--- the encoding a string was written in is not a structural property ---
        Object::String(bytes, _format) => {
            hasher.update(b"string");
            hasher.update((bytes.len() as u64).to_le_bytes());
            hasher.update(bytes);
        }
        Object::Array(items) => {
            hasher.update(b"array");
            hasher.update((items.len() as u64).to_le_bytes());
            for item in items {
                feed_object(item, digests, hasher);
            }
        }
        Object::Dictionary(dictionary) => feed_dictionary(dictionary, digests, hasher, &[]),
        Object::Stream(stream) => {
            hasher.update(b"stream");
            //--- how the bytes are packed is not structure; what they say is ---
            feed_dictionary(&stream.dict, digests, hasher, &[b"Length", b"Filter", b"DecodeParms"]);
            match stream.decompressed_content() {
                Ok(content) => {
                    hasher.update(b"decoded");
                    hasher.update((content.len() as u64).to_le_bytes());
                    hasher.update(&content);
                }
                //--- undecodable: compare the raw bytes rather than ignore them ---
                Err(_) => {
                    hasher.update(b"raw");
                    hasher.update((stream.content.len() as u64).to_le_bytes());
                    hasher.update(&stream.content);
                }
            }
        }
        Object::Reference(id) => {
            hasher.update(b"ref");
            match digests.get(id) {
                Some(digest) => hasher.update(digest),
                None => hasher.update(DANGLING),
            }
        }
    }
}

/// Feed a dictionary with its keys in sorted order, skipping `excluded`.
fn feed_dictionary(dictionary: &Dictionary, digests: &HashMap<ObjectId, ObjectDigest>, hasher: &mut Sha256, excluded: &[&[u8]]) {
    let mut entries: Vec<(&[u8], &Object)> = dictionary
        .iter()
        .map(|(key, value)| (key.as_slice(), value))
        .filter(|(key, _)| !excluded.contains(key))
        .collect();
    //--- key order in the file is a writer's choice, not the document's ---
    entries.sort_unstable_by(|left, right| left.0.cmp(right.0));

    hasher.update(b"dict");
    hasher.update((entries.len() as u64).to_le_bytes());
    for (key, value) in entries {
        hasher.update((key.len() as u64).to_le_bytes());
        hasher.update(key);
        feed_object(value, digests, hasher);
    }
}

//---------------------------------------------------------------------
// Reachability
//---------------------------------------------------------------------

/// Every object id reachable from the trailer's `/Root`.
///
/// Ids are recorded whether or not an object exists for them, so a reference
/// into nothing is part of the graph rather than invisible to it.
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

/// How many reachable references point at an object that does not exist.
fn count_dangling_references(document: &Document) -> usize {
    reachable_object_ids(document).iter().filter(|id| document.get_object(**id).is_err()).count()
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
        // this diff engine cares about. This is the false-positive direction:
        // a digest that admitted an object number would fire on every object.
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
        assert!(
            diff.pages_with_changed_content.is_empty(),
            "renumbering objects must not read as a content change: {diff}"
        );
    }

    #[test]
    fn parsing_a_truncated_file_fails_cleanly() {
        let bytes = read_corpus_file("irs_f1040_truncated_10k.pdf");
        let result = diff_bytes(&bytes, &bytes);
        assert!(result.is_err(), "a truncated file must fail to parse, not panic");
    }

    //---------------------------------------------------------------------
    // The damage this engine used to call "identical"
    //---------------------------------------------------------------------
    //
    // Every case below was reproduced against the previous implementation,
    // which compared page count, direct-only geometry, and the *set* of
    // reachable object ids. Each one diffed empty. They are the reason the
    // engine was rewritten, so each gets a test naming the damage it stands
    // for rather than the mechanism that catches it.

    mod damage {
        use super::super::*;
        use lopdf::{Object, Stream, dictionary};

        /// A two-page document with the shapes real files have and the
        /// previous engine could not see past: geometry *inherited* from the
        /// root `/Pages` node rather than repeated on each page, distinct
        /// content streams, a shared font resource, and an AcroForm field
        /// with a value.
        struct Fixture {
            document: Document,
            pages_id: ObjectId,
            page_a: ObjectId,
            page_b: ObjectId,
            content_a: ObjectId,
            font_id: ObjectId,
            annot_id: ObjectId,
        }

        fn build_fixture() -> Fixture {
            let mut document = Document::with_version("1.7");

            let font_id = document.add_object(dictionary! {
                "Type" => "Font",
                "Subtype" => "Type1",
                "BaseFont" => "Helvetica",
            });
            let resources_id = document.add_object(dictionary! {
                "Font" => dictionary! { "F1" => font_id },
            });
            let annot_id = document.add_object(dictionary! {
                "Type" => "Annot",
                "Subtype" => "Widget",
                "FT" => "Tx",
                "T" => Object::string_literal("taxpayer_name"),
                "V" => Object::string_literal("original value"),
                "Rect" => vec![0.into(), 0.into(), 100.into(), 20.into()],
            });

            let content_a = document.add_object(Stream::new(dictionary! {}, b"0 0 1 rg 0 0 595 842 re f".to_vec()));
            let content_b = document.add_object(Stream::new(dictionary! {}, b"1 0 0 rg 0 0 595 842 re f".to_vec()));

            let pages_id = document.new_object_id();
            let page_a = document.add_object(dictionary! {
                "Type" => "Page",
                "Parent" => pages_id,
                "Contents" => content_a,
                "Resources" => resources_id,
                "Annots" => vec![annot_id.into()],
            });
            let page_b = document.add_object(dictionary! {
                "Type" => "Page",
                "Parent" => pages_id,
                "Contents" => content_b,
                "Resources" => resources_id,
            });

            //--- geometry lives here, not on the pages: the inheritance case ---
            document.objects.insert(
                pages_id,
                Object::Dictionary(dictionary! {
                    "Type" => "Pages",
                    "Kids" => vec![page_a.into(), page_b.into()],
                    "Count" => 2,
                    "MediaBox" => vec![0.into(), 0.into(), 595.into(), 842.into()],
                    "Rotate" => 90,
                }),
            );

            let catalog_id = document.add_object(dictionary! {
                "Type" => "Catalog",
                "Pages" => pages_id,
                "AcroForm" => dictionary! { "Fields" => vec![annot_id.into()] },
                //--- keeps a deleted page reachable, so removing it from /Kids
                //--- does not also remove it from the object graph ---
                "OpenAction" => vec![page_a.into(), "Fit".into()],
            });
            document.trailer.set("Root", catalog_id);

            Fixture {
                document,
                pages_id,
                page_a,
                page_b,
                content_a,
                font_id,
                annot_id,
            }
        }

        fn to_bytes(document: &mut Document) -> Vec<u8> {
            let mut bytes = Vec::new();
            document.save_to(&mut bytes).unwrap();
            bytes
        }

        /// Build the fixture, apply `damage`, and diff the two byte buffers.
        fn diff_after(damage: impl FnOnce(&mut Fixture)) -> StructuralDiff {
            let mut before = build_fixture();
            let mut after = build_fixture();
            damage(&mut after);
            diff_bytes(&to_bytes(&mut before.document), &to_bytes(&mut after.document)).unwrap()
        }

        /// The control. Without this, every test below would pass against an
        /// engine that simply called everything different.
        #[test]
        fn an_undamaged_document_diffs_empty() {
            let diff = diff_after(|_| {});
            assert!(diff.is_empty(), "two builds of the same document must diff empty: {diff}");
        }

        #[test]
        fn a_replaced_content_stream_is_not_identical() {
            let diff = diff_after(|fixture| {
                fixture.document.objects.insert(
                    fixture.content_a,
                    Object::Stream(Stream::new(dictionary! {}, b"0 1 0 rg 0 0 10 10 re f".to_vec())),
                );
            });
            assert!(!diff.is_empty(), "a page whose content stream was replaced is not structurally identical");
            assert_eq!(diff.pages_with_changed_content, vec![0], "the change must be localised to page one: {diff}");
        }

        #[test]
        fn reordered_pages_are_not_identical() {
            let diff = diff_after(|fixture| {
                let kids = vec![Object::Reference(fixture.page_b), Object::Reference(fixture.page_a)];
                if let Ok(pages) = fixture.document.get_object_mut(fixture.pages_id).and_then(Object::as_dict_mut) {
                    pages.set("Kids", kids);
                }
            });
            assert!(
                !diff.is_empty(),
                "reordering pages is the operation this editor exists to perform; it cannot diff as identical"
            );
        }

        #[test]
        fn a_deleted_page_is_not_identical() {
            let diff = diff_after(|fixture| {
                let kids = vec![Object::Reference(fixture.page_b)];
                if let Ok(pages) = fixture.document.get_object_mut(fixture.pages_id).and_then(Object::as_dict_mut) {
                    pages.set("Kids", kids);
                    pages.set("Count", 1);
                }
            });
            assert_eq!(
                diff.page_count_changed,
                Some((2, 1)),
                "a deleted page must be counted even though the page tree carries the geometry: {diff}"
            );
        }

        #[test]
        fn a_wiped_form_field_value_is_not_identical() {
            let diff = diff_after(|fixture| {
                if let Ok(annot) = fixture.document.get_object_mut(fixture.annot_id).and_then(Object::as_dict_mut) {
                    annot.set("V", Object::string_literal(""));
                }
            });
            assert!(
                !diff.is_empty(),
                "silently emptying a form field is exactly the damage this project promises not to do"
            );
            assert_eq!(diff.pages_with_changed_content, vec![0], "the field is on page one, via /Annots: {diff}");
        }

        #[test]
        fn a_dropped_inherited_rotation_is_not_identical() {
            let diff = diff_after(|fixture| {
                if let Ok(pages) = fixture.document.get_object_mut(fixture.pages_id).and_then(Object::as_dict_mut) {
                    pages.remove(b"Rotate");
                }
            });
            assert!(!diff.is_empty(), "dropping an inherited /Rotate turns every page upright: {diff}");
            assert_eq!(
                diff.page_geometry_changes.len(),
                2,
                "both pages inherited the rotation, so both changed: {diff}"
            );
            assert_eq!(diff.page_geometry_changes[0].before.rotation_degrees, 90);
            assert_eq!(diff.page_geometry_changes[0].after.rotation_degrees, 0);
        }

        #[test]
        fn a_deleted_object_body_behind_a_live_reference_is_not_identical() {
            let diff = diff_after(|fixture| {
                //--- the font object goes; /Resources still points at it ---
                fixture.document.objects.remove(&fixture.font_id);
            });
            assert!(!diff.is_empty(), "a dangling reference is the classic incremental-save corruption");
            assert_eq!(
                diff.dangling_references_changed,
                Some((0, 1)),
                "the reference into nothing must be reported, not skipped: {diff}"
            );
        }

        #[test]
        fn a_swapped_font_is_not_identical() {
            let diff = diff_after(|fixture| {
                if let Ok(font) = fixture.document.get_object_mut(fixture.font_id).and_then(Object::as_dict_mut) {
                    font.set("BaseFont", "Courier");
                }
            });
            assert!(!diff.is_empty(), "a document whose font was substituted does not render the same");
            assert_eq!(
                diff.pages_with_changed_content,
                vec![0, 1],
                "both pages share the font resource, so both changed: {diff}"
            );
        }
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
