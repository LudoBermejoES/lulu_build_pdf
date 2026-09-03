//! Low-level PDF document helpers shared by preflight and normalization:
//! loading, inherited page-attribute resolution, and effective page geometry.

use crate::units::{Length, Rect, Size};
use lopdf::{Dictionary, Document, Object, ObjectId};

#[derive(Debug, thiserror::Error)]
pub enum LoadError {
    #[error("could not parse PDF: {0}")]
    Parse(#[from] lopdf::Error),
}

/// Load a document from raw bytes. Never panics on malformed input — a parse
/// failure comes back as an [`LoadError`] so a caller (preflight) can turn it
/// into a blocking finding rather than aborting without a report.
///
/// lopdf transparently tries an empty user password on load: if the file was
/// encrypted with one, loading succeeds fully decrypted and `is_encrypted()`
/// on the result is *false* (decrypting clears the trailer's `/Encrypt` entry).
/// Use [`was_ever_encrypted`] to detect "this file carries encryption" — Lulu
/// prohibits any encryption, empty password or not — rather than `is_encrypted()`
/// alone, which only reflects whether the file is *currently* still locked.
pub fn load_from_bytes(bytes: &[u8]) -> Result<Document, LoadError> {
    Ok(Document::load_mem(bytes)?)
}

/// Whether this document carries (or carried, before a transparent
/// empty-password decrypt on load) an encryption dictionary at all.
pub fn was_ever_encrypted(doc: &Document) -> bool {
    doc.is_encrypted() || doc.was_encrypted()
}

/// Overwrites `doc`'s trailer `/ID` and its `Info` dictionary's
/// `CreationDate`/`ModDate` with caller-supplied fixed values, so that two
/// runs over the same input with the same options produce byte-identical
/// output (`specs/cli/spec.md`, "Reproducibility") — nothing else in this
/// crate's write path ever embeds a wall-clock timestamp or random value,
/// but a caller-visible knob to pin these two fields is still worth having
/// since some inputs already carry their own (real, varying) values here.
///
/// `creation_date_pdf` must already be in PDF date-string form
/// (`D:YYYYMMDDHHmmSSZ`); this function does not validate or reformat it.
///
/// # Errors
///
/// Returns [`DeterministicIdentityError::InfoNotADictionary`] when the
/// trailer's `/Info` entry is present as an indirect reference but that
/// reference does not resolve to a dictionary (a dangling reference, or one
/// pointing at some other object type). The trailer `/ID` is still set in
/// that case — only the `Info` dates are skipped — since silently leaving
/// `CreationDate`/`ModDate` untouched would defeat the reproducibility
/// guarantee this function exists for (two runs over the same input could
/// then still differ, carrying whatever wall-clock value was already there,
/// with no indication anything was skipped).
pub fn apply_deterministic_identity(
    doc: &mut Document,
    doc_id: [u8; 16],
    creation_date_pdf: &str,
) -> Result<(), DeterministicIdentityError> {
    let id_string = Object::String(doc_id.to_vec(), lopdf::StringFormat::Hexadecimal);
    doc.trailer
        .set("ID", Object::Array(vec![id_string.clone(), id_string]));

    let date_object = Object::String(
        creation_date_pdf.as_bytes().to_vec(),
        lopdf::StringFormat::Literal,
    );
    match doc
        .trailer
        .get(b"Info")
        .ok()
        .and_then(|o| o.as_reference().ok())
    {
        Some(info_id) => {
            let info_dict = doc
                .get_object_mut(info_id)
                .ok()
                .and_then(|o| o.as_dict_mut().ok())
                .ok_or(DeterministicIdentityError::InfoNotADictionary(info_id))?;
            info_dict.set("CreationDate", date_object.clone());
            info_dict.set("ModDate", date_object);
        }
        None => {
            let mut info_dict = Dictionary::new();
            info_dict.set("CreationDate", date_object.clone());
            info_dict.set("ModDate", date_object);
            let info_id = doc.add_object(Object::Dictionary(info_dict));
            doc.trailer.set("Info", Object::Reference(info_id));
        }
    }
    Ok(())
}

#[derive(Debug, thiserror::Error)]
pub enum DeterministicIdentityError {
    #[error(
        "trailer's /Info {0:?} is a reference that does not resolve to a dictionary; \
         CreationDate/ModDate were not made deterministic"
    )]
    InfoNotADictionary(ObjectId),
}

#[derive(Debug, thiserror::Error)]
pub enum PageGeometryError {
    #[error("page {0:?} has no MediaBox, and none of its ancestors in the Pages tree do either")]
    NoMediaBox(ObjectId),
    #[error("could not read page dictionary: {0}")]
    Pdf(#[from] lopdf::Error),
    #[error(
        "page {0:?}'s /Resources (its own, or an inherited one from a Pages \
         ancestor) is a reference that does not resolve to a dictionary"
    )]
    ResourcesUnresolved(ObjectId),
    #[error(
        "copying page {0:?} encountered reference(s) to object(s) that do not \
         exist in the source document: {1:?}"
    )]
    DanglingReference(ObjectId, Vec<ObjectId>),
    #[error(
        "copying page {0:?} exceeded the deep-copy depth budget of {DEEP_COPY_DEPTH_BUDGET} \
         (a reference chain or nesting this deep is almost certainly hostile input, not \
         a legitimate document); some content may be missing from the copy"
    )]
    DeepCopyDepthExceeded(ObjectId),
}

/// Dereferences `object` one level if it is an [`Object::Reference`], or
/// returns it unchanged otherwise. The one place every PDF object access in
/// this module funnels through before inspecting an object's variant, so a
/// value written as `4 0 R` is handled identically to the same value written
/// inline — every legal PDF encoding of "a resource", "a box", "a rotation",
/// or "a number" is exactly one of these two shapes (`lopdf` does not nest
/// references, i.e. a reference never itself resolves to another reference).
fn resolve_object<'a>(doc: &'a Document, object: &'a Object) -> Option<&'a Object> {
    match object {
        Object::Reference(id) => doc.get_object(*id).ok(),
        other => Some(other),
    }
}

/// `object`, dereferenced through [`resolve_object`], as a dictionary — used
/// for `/Resources` and the `/Names` tree, both of which are legally either a
/// direct dictionary or an indirect reference to one.
fn resolve_dictionary<'a>(doc: &'a Document, object: &'a Object) -> Option<&'a Dictionary> {
    resolve_object(doc, object)?.as_dict().ok()
}

/// `object`, dereferenced through [`resolve_object`], as a number — used for
/// `/Rotate` and for the individual elements of a box array, both of which
/// are legally an `Integer`, a `Real`, or an indirect reference to either.
fn resolve_number(doc: &Document, object: &Object) -> Option<f64> {
    match resolve_object(doc, object)? {
        Object::Integer(n) => Some(*n as f64),
        Object::Real(n) => Some(*n as f64),
        _ => None,
    }
}

fn as_rect_points(doc: &Document, object: &Object) -> Option<[f64; 4]> {
    let array = resolve_object(doc, object)?.as_array().ok()?;
    if array.len() != 4 {
        return None;
    }
    let mut out = [0.0; 4];
    for (i, o) in array.iter().enumerate() {
        out[i] = resolve_number(doc, o)?;
    }
    Some(out)
}

/// Walk a page dictionary's `/Parent` chain looking up `key`, per the PDF
/// spec's inheritable page attributes (`MediaBox`, `CropBox`, `Resources`,
/// `Rotate` may be set on any ancestor `Pages` node and inherited down).
///
/// Bounded by a visited set of the `/Parent` references already followed, so
/// a cyclic `/Parent` chain (a page whose ancestry loops back on itself —
/// seen in the wild as a 372-byte hostile PDF) terminates and this returns
/// `None` instead of looping forever.
fn get_inherited<'a>(
    doc: &'a Document,
    mut dict: &'a Dictionary,
    key: &[u8],
) -> Option<&'a Object> {
    let mut visited = std::collections::HashSet::new();
    loop {
        if let Ok(value) = dict.get(key) {
            return Some(value);
        }
        let parent_ref = dict.get(b"Parent").ok()?.as_reference().ok()?;
        if !visited.insert(parent_ref) {
            return None;
        }
        dict = doc.get_dictionary(parent_ref).ok()?;
    }
}

/// Every ancestor `Pages` dictionary of `dict`, from its immediate `/Parent`
/// up to the tree's root, per the same cycle-bounded walk as
/// [`get_inherited`]. Ordered closest ancestor first.
fn ancestor_chain<'a>(doc: &'a Document, dict: &'a Dictionary) -> Vec<&'a Dictionary> {
    let mut chain = Vec::new();
    let mut visited = std::collections::HashSet::new();
    let mut current = dict;
    loop {
        let Ok(parent_ref) = current.get(b"Parent").and_then(Object::as_reference) else {
            break;
        };
        if !visited.insert(parent_ref) {
            break;
        }
        let Ok(parent_dict) = doc.get_dictionary(parent_ref) else {
            break;
        };
        chain.push(parent_dict);
        current = parent_dict;
    }
    chain
}

fn box_points(doc: &Document, dict: &Dictionary, key: &[u8], inherited: bool) -> Option<[f64; 4]> {
    let object = if inherited {
        get_inherited(doc, dict, key)?
    } else {
        dict.get(key).ok()?
    };
    as_rect_points(doc, object)
}

/// A page's `/Rotate` entry, resolved and classified. `/Rotate` is legally an
/// `Integer`, a `Real`, or an indirect reference to either, and the PDF spec
/// requires it to be a multiple of 90 — this distinguishes those cases rather
/// than collapsing all of them to `0` the way a bare `unwrap_or(0)` would, so
/// a caller that needs to raise a finding for "present but unreadable" or
/// "not a multiple of 90" can tell them apart from a page that legitimately
/// carries no `/Rotate` at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RotationOutcome {
    /// `/Rotate` is absent (interpreted as `0`, per spec default) or present
    /// and, within [`ROTATE_TOLERANCE_DEGREES`], at a multiple of 90 —
    /// normalized into `{0, 90, 180, 270}`.
    Normalized(i64),
    /// `/Rotate` is present but its value could not be resolved to a number
    /// at all — e.g. an indirect reference that doesn't resolve, or a
    /// non-numeric object such as a `Name`.
    Unreadable,
    /// `/Rotate` is present and numeric, but is not a multiple of 90 (beyond
    /// [`ROTATE_TOLERANCE_DEGREES`]). Carries the value rounded to the
    /// nearest integer degree, for use in a finding's message.
    NotAMultipleOf90(i64),
}

/// How far from an exact multiple of 90 a numeric `/Rotate` value may fall
/// and still be treated as that multiple (rather than
/// [`RotationOutcome::NotAMultipleOf90`]) — covers float round-trip noise
/// like `89.999999`, not a real oblique rotation.
const ROTATE_TOLERANCE_DEGREES: f64 = 0.5;

/// Resolves and classifies the page's (inherited) `/Rotate` entry — see
/// [`RotationOutcome`]. This is the accessor a caller that must raise a
/// finding for an unreadable or non-90-multiple rotation should use;
/// [`rotation_degrees`] is built on top of it for callers that only need the
/// geometric effect and are content to treat those two cases as unrotated.
pub fn rotation_outcome(
    doc: &Document,
    page_id: ObjectId,
) -> Result<RotationOutcome, PageGeometryError> {
    let dict = doc.get_dictionary(page_id)?;
    let Some(raw) = get_inherited(doc, dict, b"Rotate") else {
        return Ok(RotationOutcome::Normalized(0));
    };
    let Some(value) = resolve_number(doc, raw) else {
        return Ok(RotationOutcome::Unreadable);
    };
    let nearest_multiple = (value / 90.0).round() * 90.0;
    if (value - nearest_multiple).abs() > ROTATE_TOLERANCE_DEGREES {
        return Ok(RotationOutcome::NotAMultipleOf90(value.round() as i64));
    }
    let normalized = ((nearest_multiple as i64 % 360) + 360) % 360;
    Ok(RotationOutcome::Normalized(normalized))
}

/// Rotation in degrees clockwise, from the page's (inherited) `/Rotate`
/// entry, normalized into `{0, 90, 180, 270}`.
///
/// Built on [`rotation_outcome`], treating both
/// [`RotationOutcome::Unreadable`] and [`RotationOutcome::NotAMultipleOf90`]
/// as `0` — the conservative geometric choice for callers (such as
/// [`effective_page_size`]) that only need *some* rotation to apply and have
/// no way to surface a finding. A caller that needs to distinguish those
/// cases (to raise a finding rather than silently treat the page as
/// unrotated) should call [`rotation_outcome`] directly instead.
pub fn rotation_degrees(doc: &Document, page_id: ObjectId) -> Result<i64, PageGeometryError> {
    Ok(match rotation_outcome(doc, page_id)? {
        RotationOutcome::Normalized(n) => n,
        RotationOutcome::Unreadable | RotationOutcome::NotAMultipleOf90(_) => 0,
    })
}

/// The page's own box rect, in its own (unrotated) coordinate system —
/// following the PDF fallback chain `BleedBox -> CropBox -> MediaBox`
/// (`MediaBox` and `CropBox` are inheritable from the Pages tree; `BleedBox`
/// is not). This is where content is actually drawn, origin included — a
/// page's `MediaBox` need not start at `(0, 0)`.
pub fn own_box_rect(doc: &Document, page_id: ObjectId) -> Result<Rect, PageGeometryError> {
    let dict = doc.get_dictionary(page_id)?;
    let points = box_points(doc, dict, b"BleedBox", false)
        .or_else(|| box_points(doc, dict, b"CropBox", true))
        .or_else(|| box_points(doc, dict, b"MediaBox", true))
        .ok_or(PageGeometryError::NoMediaBox(page_id))?;
    let (x0, x1) = (points[0].min(points[2]), points[0].max(points[2]));
    let (y0, y1) = (points[1].min(points[3]), points[1].max(points[3]));
    Ok(Rect {
        x0: Length::from_points(x0),
        y0: Length::from_points(y0),
        x1: Length::from_points(x1),
        y1: Length::from_points(y1),
    })
}

/// The page's own box size — [`own_box_rect`]'s width and height, in its own
/// (unrotated) coordinate system. [`effective_page_size`] additionally swaps
/// width/height for a 90 or 270 degree rotation, since that's what a viewer
/// or printer would actually show.
pub fn own_box_size(doc: &Document, page_id: ObjectId) -> Result<Size, PageGeometryError> {
    let r = own_box_rect(doc, page_id)?;
    Ok(Size::new(r.width(), r.height()))
}

/// The page's effective print size: [`own_box_size`] with `/Rotate` applied —
/// a 90 or 270 degree rotation swaps width and height, since that's what a
/// viewer or printer would actually show.
pub fn effective_page_size(doc: &Document, page_id: ObjectId) -> Result<Size, PageGeometryError> {
    let size = own_box_size(doc, page_id)?;
    let rotation = rotation_degrees(doc, page_id)?;
    Ok(if rotation == 90 || rotation == 270 {
        Size::new(size.height, size.width)
    } else {
        size
    })
}

/// Resource category keys — each of these is, when present, itself a
/// dictionary mapping a local name (e.g. `/F1`) to a resource. Per the PDF
/// spec's page-attribute inheritance rules, a page's own entry under one of
/// these keys is merged with (not replacing wholesale) the same key
/// inherited from a `Pages` ancestor. `/ProcSet` (an array, not a
/// sub-dictionary) and any other non-dictionary entry are replaced wholesale
/// by the closer layer instead, since there is no meaningful per-name merge
/// for those.
const RESOURCE_CATEGORY_KEYS: &[&[u8]] = &[
    b"Font",
    b"XObject",
    b"ExtGState",
    b"ColorSpace",
    b"Pattern",
    b"Shading",
    b"Properties",
];

/// Merges `layer`'s entries into `merged`, in place. For a
/// [`RESOURCE_CATEGORY_KEYS`] key present as a (possibly indirect) dictionary
/// in both, entries are merged at the sub-dictionary level — a name already
/// present in `merged` is overwritten by `layer`'s value for that name, and a
/// name `layer` doesn't define is left untouched. Every other key is replaced
/// wholesale by `layer`'s value. Called with layers ordered farthest
/// (outermost `Pages` ancestor) first and closest (the page's own resources)
/// last, so that the page's own entries end up taking precedence per PDF's
/// inheritance rule, and a page defining its own `/Font` but not its own
/// `/XObject` still inherits the ancestor's `/XObject` untouched.
fn merge_resource_layer(doc: &Document, merged: &mut Dictionary, layer: &Dictionary) {
    for (key, value) in layer.iter() {
        let is_category = RESOURCE_CATEGORY_KEYS.contains(&key.as_slice());
        let sub_dict = is_category
            .then(|| resolve_dictionary(doc, value))
            .flatten();
        match sub_dict {
            Some(sub_dict) => {
                let mut combined = merged
                    .get(key)
                    .ok()
                    .and_then(|o| o.as_dict().ok())
                    .cloned()
                    .unwrap_or_default();
                for (sub_key, sub_value) in sub_dict.iter() {
                    combined.set(sub_key.clone(), sub_value.clone());
                }
                merged.set(key.clone(), Object::Dictionary(combined));
            }
            None => {
                merged.set(key.clone(), value.clone());
            }
        }
    }
}

/// A page's effective `/Resources`: its own, whether written as a direct
/// dictionary or an indirect reference, merged over whatever it inherits
/// from its `/Parent` chain (`/Resources` is one of the inheritable page
/// attributes, like `/MediaBox` and `/Rotate`) — see [`merge_resource_layer`]
/// for the merge rule. A page with no resolvable `/Resources` anywhere in
/// its ancestry (legitimate for a page that draws nothing) returns an empty
/// dictionary, not an error.
///
/// This is the one accessor every resource read in this crate should go
/// through instead of `lopdf::Document::get_page_resources` directly:
/// `get_page_resources` returns `(Option<&Dictionary>, Vec<ObjectId>)`, where
/// the direct dictionary and the inherited/indirect ids are kept separate and
/// unmerged — discarding the `Vec` (as `.ok().and_then(|(r, _)| r)` does) is
/// exactly the bug this function exists to close: a page whose `/Resources`
/// is `4 0 R`, or has none of its own and inherits one from its `Pages`
/// ancestor — both of which are what essentially every real PDF producer
/// emits — resolves to an empty dictionary instead of the fonts and images
/// the page's content stream actually references.
///
/// # Errors
///
/// Returns [`PageGeometryError::ResourcesUnresolved`] when the page's own
/// `/Resources`, or an inherited one from an ancestor, is present as an
/// indirect reference that does not resolve to a dictionary — a broken
/// reference is a defect in the document, not the "no resources" case, and
/// must not be silently treated the same way.
pub fn effective_page_resources(
    doc: &Document,
    page_id: ObjectId,
) -> Result<Dictionary, PageGeometryError> {
    let page_dict = doc.get_dictionary(page_id)?;
    let mut layers: Vec<&Dictionary> = ancestor_chain(doc, page_dict);
    layers.reverse(); // farthest ancestor first
    layers.push(page_dict); // page's own resources merge in last, so they win

    let mut merged = Dictionary::new();
    for layer_dict in layers {
        let Ok(resources_obj) = layer_dict.get(b"Resources") else {
            continue;
        };
        let resolved = resolve_dictionary(doc, resources_obj)
            .ok_or(PageGeometryError::ResourcesUnresolved(page_id))?;
        merge_resource_layer(doc, &mut merged, resolved);
    }
    Ok(merged)
}

/// The document catalog's dictionary, via the trailer's `/Root` entry.
fn catalog_dict(doc: &Document) -> Option<&Dictionary> {
    let root_ref = doc.trailer.get(b"Root").ok()?.as_reference().ok()?;
    doc.get_dictionary(root_ref).ok()
}

/// The document catalog's `/Names` tree (the root of the name dictionaries
/// holding, among others, `/JavaScript` and `/EmbeddedFiles` — the entries
/// Lulu prohibits), resolved whether the catalog writes `/Names` as a direct
/// dictionary or as an indirect reference to one. `None` when the catalog is
/// unreadable or carries no `/Names` entry at all (both legitimate: most
/// documents have no name dictionary).
pub fn catalog_names(doc: &Document) -> Option<&Dictionary> {
    let catalog = catalog_dict(doc)?;
    resolve_dictionary(doc, catalog.get(b"Names").ok()?)
}

/// Mutable counterpart to [`catalog_names`], for sanitization (which needs
/// to remove entries such as `/JavaScript`) — resolves and returns a mutable
/// reference to the *referenced* dictionary when `/Names` is an indirect
/// reference, rather than only handling the direct-dictionary case the way
/// `catalog.get_mut(b"Names")` alone would.
pub fn catalog_names_mut(doc: &mut Document) -> Option<&mut Dictionary> {
    enum NamesLocation {
        Indirect(ObjectId),
        Direct,
    }
    let root_ref = doc.trailer.get(b"Root").ok()?.as_reference().ok()?;
    let location = {
        let catalog = doc.get_dictionary(root_ref).ok()?;
        match catalog.get(b"Names").ok()? {
            Object::Reference(id) => NamesLocation::Indirect(*id),
            Object::Dictionary(_) => NamesLocation::Direct,
            _ => return None,
        }
    };
    match location {
        NamesLocation::Indirect(names_id) => doc.get_object_mut(names_id).ok()?.as_dict_mut().ok(),
        NamesLocation::Direct => doc
            .get_dictionary_mut(root_ref)
            .ok()?
            .get_mut(b"Names")
            .ok()?
            .as_dict_mut()
            .ok(),
    }
}

/// Recursion depth budget for [`deep_copy_object`]/[`deep_copy_object_reporting`].
/// A legitimate document's object graph — nested form XObjects, a font's
/// descriptor chain, and so on — never comes close to this; it exists to
/// bound a hostile file's reference chain (a ~1MB PDF with a 60,000-long
/// chain of single-reference indirect objects has been seen in practice) so
/// that copying it returns a truncated result instead of overflowing the
/// call stack, which is not even a catchable `panic` — it aborts the whole
/// process. A depth budget was chosen over rewriting this function around an
/// explicit worklist: it fixes the crash for every existing caller (all of
/// which call the unchanged-signature [`deep_copy_object`] below) without
/// changing that function's signature, whereas a worklist rewrite capable of
/// producing the same nested `Object` tree this one does would be a much
/// larger, more invasive change to this function's shape for comparatively
/// little additional safety margin at this depth.
const DEEP_COPY_DEPTH_BUDGET: usize = 4096;

/// Findings accumulated while copying that [`deep_copy_object`] (kept
/// side-effect-free in its return type, for compatibility with its existing
/// callers) discards, but that [`deep_copy_object_reporting`] surfaces to a
/// caller that wants to know about them — see [`DeepCopyReport`].
#[derive(Debug, Default)]
struct CopyBudget {
    dangling_references: Vec<ObjectId>,
    depth_budget_exceeded: bool,
}

/// Stack reserved for the scoped worker thread [`deep_copy_with_budget`]
/// runs the recursive copy on. [`DEEP_COPY_DEPTH_BUDGET`] levels of real
/// recursion, at an unoptimized debug build's frame size for
/// `deep_copy_object_inner` (which holds several locals per frame — a
/// `Dictionary` being built, iterator state, etc.), measured in practice to
/// overflow the ~2 MiB default stack Rust's own test harness gives each test
/// thread well before the depth budget was reached. Running on a thread with
/// an explicit, generous stack decouples "how deep the budget allows" from
/// "how much stack the calling thread happens to have" — the latter is not
/// this crate's to control (a caller may invoke this from a thread with a
/// small stack of its own), and guessing a depth budget small enough to be
/// safe on every possible caller stack would make the budget far more
/// conservative than the "a few thousand" that's actually reasonable for a
/// legitimate document.
const DEEP_COPY_STACK_SIZE: usize = 64 * 1024 * 1024;

/// Runs the recursive copy on a scoped worker thread with
/// [`DEEP_COPY_STACK_SIZE`] of stack — see that constant's doc comment for
/// why. The scope guarantees the worker is joined (and so `dest`/`src`
/// /`object`/`memo`'s borrows are still valid for the caller) before this
/// returns.
fn deep_copy_with_budget(
    dest: &mut Document,
    src: &Document,
    object: &Object,
    memo: &mut std::collections::HashMap<ObjectId, ObjectId>,
) -> (Object, CopyBudget) {
    std::thread::scope(|scope| {
        std::thread::Builder::new()
            .stack_size(DEEP_COPY_STACK_SIZE)
            .spawn_scoped(scope, move || {
                let mut budget = CopyBudget::default();
                let copied = deep_copy_object_inner(dest, src, object, memo, 0, &mut budget);
                (copied, budget)
            })
            .expect("spawning deep_copy_object's worker thread should not fail")
            .join()
            .expect("deep_copy_object's worker thread should not panic")
    })
}

/// Recursively copies `object` (and everything it references) from `src`
/// into `dest`, giving every copied object a fresh id in `dest` and
/// rewriting references accordingly. `memo` tracks objects already copied
/// (keyed by their id in `src`), both to avoid duplicate copies and to
/// correctly handle reference cycles (e.g. a page referencing its own
/// `Parent`, or two resources referencing each other).
///
/// This is how a page — with whatever fonts, images, and nested XObjects it
/// depends on — is moved from one `lopdf::Document` into another, since
/// `ObjectId`s are only meaningful within the document that assigned them.
///
/// A dangling reference (one that does not resolve to anything in `src`) is
/// silently substituted with `Object::Null`, and a subgraph deeper than
/// [`DEEP_COPY_DEPTH_BUDGET`] is truncated the same way, both for
/// compatibility with this function's existing callers, which use its return
/// value directly as an `Object` with no room for an out-of-band finding.
/// [`deep_copy_object_reporting`] performs the identical copy but also
/// returns a [`DeepCopyReport`] naming any dangling references or depth-limit
/// truncation encountered, for a caller (such as [`copy_page`], below) that
/// can turn those into a proper error instead.
pub fn deep_copy_object(
    dest: &mut Document,
    src: &Document,
    object: &Object,
    memo: &mut std::collections::HashMap<ObjectId, ObjectId>,
) -> Object {
    deep_copy_with_budget(dest, src, object, memo).0
}

/// Findings surfaced by [`deep_copy_object_reporting`] — see that function
/// and [`deep_copy_object`]'s doc comment for why these are not threaded
/// through `deep_copy_object`'s own return type.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct DeepCopyReport {
    /// Object ids referenced from the copied subgraph that did not resolve
    /// to anything in the source document. Each was substituted with
    /// `Object::Null` in the copy.
    pub dangling_references: Vec<ObjectId>,
    /// Whether [`DEEP_COPY_DEPTH_BUDGET`] was exhausted anywhere in the
    /// copied subgraph. When `true`, some branch of the copy was truncated
    /// to `Object::Null` rather than being fully copied.
    pub depth_budget_exceeded: bool,
}

/// Identical copy to [`deep_copy_object`], but returns a [`DeepCopyReport`]
/// alongside the copied object naming any dangling references or
/// depth-budget truncation encountered, so a caller can raise a finding
/// instead of silently accepting a `Null` substitution — see
/// [`PageGeometryError::DanglingReference`] and
/// [`PageGeometryError::DeepCopyDepthExceeded`], which [`copy_page`] uses
/// this function to detect and report.
pub fn deep_copy_object_reporting(
    dest: &mut Document,
    src: &Document,
    object: &Object,
    memo: &mut std::collections::HashMap<ObjectId, ObjectId>,
) -> (Object, DeepCopyReport) {
    let (copied, budget) = deep_copy_with_budget(dest, src, object, memo);
    (
        copied,
        DeepCopyReport {
            dangling_references: budget.dangling_references,
            depth_budget_exceeded: budget.depth_budget_exceeded,
        },
    )
}

fn deep_copy_object_inner(
    dest: &mut Document,
    src: &Document,
    object: &Object,
    memo: &mut std::collections::HashMap<ObjectId, ObjectId>,
    depth: usize,
    budget: &mut CopyBudget,
) -> Object {
    if depth > DEEP_COPY_DEPTH_BUDGET {
        budget.depth_budget_exceeded = true;
        return Object::Null;
    }
    match object {
        Object::Reference(src_id) => {
            if let Some(&dest_id) = memo.get(src_id) {
                return Object::Reference(dest_id);
            }
            let Ok(referenced) = src.get_object(*src_id) else {
                budget.dangling_references.push(*src_id);
                return Object::Null;
            };
            // Reserve the new id before recursing, so a cycle back to this
            // object resolves to the right (already-reserved) id rather than
            // recursing forever.
            let dest_id = dest.new_object_id();
            memo.insert(*src_id, dest_id);
            let copied = deep_copy_object_inner(dest, src, referenced, memo, depth + 1, budget);
            dest.objects.insert(dest_id, copied);
            Object::Reference(dest_id)
        }
        Object::Array(items) => Object::Array(
            items
                .iter()
                .map(|o| deep_copy_object_inner(dest, src, o, memo, depth + 1, budget))
                .collect(),
        ),
        Object::Dictionary(dict) => {
            let mut new_dict = Dictionary::new();
            for (k, v) in dict.as_hashmap() {
                new_dict.set(
                    k.clone(),
                    deep_copy_object_inner(dest, src, v, memo, depth + 1, budget),
                );
            }
            Object::Dictionary(new_dict)
        }
        Object::Stream(stream) => {
            // get_plain_content() decompresses per the stream's own Filter.
            // When that succeeds, the copied dict must drop Filter/DecodeParms
            // (and the old Length, which no longer matches either way) since
            // the new stream holds raw, undecoded bytes — Stream::new sets
            // Length itself. When it fails — as it does for filters lopdf's
            // decoder doesn't implement, e.g. DCTDecode/JPEG, CCITTFaxDecode,
            // JBIG2Decode, JPXDecode — the fallback content is the original,
            // *still-encoded* bytes, so Filter/DecodeParms must be kept
            // rather than stripped; stripping them while keeping encoded
            // bytes previously corrupted every such image (raw JPEG bytes in
            // a stream declaring no filter at all).
            let plain_content = stream.get_plain_content();
            let decoded = plain_content.is_ok();
            let content = plain_content.unwrap_or_else(|_| stream.content.clone());
            let mut new_dict = Dictionary::new();
            for (k, v) in stream.dict.as_hashmap() {
                if k.as_slice() == b"Length" {
                    continue;
                }
                if decoded && matches!(k.as_slice(), b"Filter" | b"DecodeParms") {
                    continue;
                }
                new_dict.set(
                    k.clone(),
                    deep_copy_object_inner(dest, src, v, memo, depth + 1, budget),
                );
            }
            let mut new_stream = lopdf::Stream::new(new_dict, content);
            new_stream.allows_compression = false;
            Object::Stream(new_stream)
        }
        other => other.clone(),
    }
}

/// Copies one page (and its `Contents`/`Resources` subgraph — fonts, images,
/// nested form XObjects) from `src` into `dest`, as a new, unlinked page
/// object — the caller is responsible for adding it to a Pages tree's
/// `/Kids` and bumping `/Count`. `/Parent` is not copied (it would pull in
/// the entire source Pages tree); the caller sets it after linking.
///
/// # Errors
///
/// Returns [`PageGeometryError::DanglingReference`] if any object reachable
/// from the page resolves nowhere in `src`, or
/// [`PageGeometryError::DeepCopyDepthExceeded`] if the object graph is deeper
/// than [`deep_copy_object`] is willing to follow — both are reported rather
/// than allowed to silently produce a page missing some of its content (see
/// [`deep_copy_object_reporting`]).
pub fn copy_page(
    dest: &mut Document,
    src: &Document,
    src_page_id: ObjectId,
) -> Result<ObjectId, PageGeometryError> {
    let src_dict = src.get_dictionary(src_page_id)?;
    let mut memo = std::collections::HashMap::new();
    let mut new_dict = Dictionary::new();
    let mut dangling_references = Vec::new();
    let mut depth_budget_exceeded = false;
    for (k, v) in src_dict.as_hashmap() {
        if k == b"Parent" {
            continue;
        }
        let (copied, report) = deep_copy_object_reporting(dest, src, v, &mut memo);
        dangling_references.extend(report.dangling_references);
        depth_budget_exceeded |= report.depth_budget_exceeded;
        new_dict.set(k.clone(), copied);
    }
    if depth_budget_exceeded {
        return Err(PageGeometryError::DeepCopyDepthExceeded(src_page_id));
    }
    if !dangling_references.is_empty() {
        return Err(PageGeometryError::DanglingReference(
            src_page_id,
            dangling_references,
        ));
    }
    Ok(dest.add_object(Object::Dictionary(new_dict)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use lopdf::{dictionary, Object};

    /// Builds a minimal one-page document with the given page dictionary entries
    /// merged in, wired into a valid catalog/pages tree.
    fn doc_with_page(page_entries: lopdf::Dictionary) -> (lopdf::Document, lopdf::ObjectId) {
        let mut doc = lopdf::Document::with_version("1.7");
        let pages_id = doc.new_object_id();
        let mut page_dict = dictionary! {
            "Type" => "Page",
            "Parent" => Object::Reference(pages_id),
        };
        page_dict.extend(&page_entries);
        let page_id = doc.add_object(Object::Dictionary(page_dict));
        let pages = dictionary! {
            "Type" => "Pages",
            "Kids" => vec![Object::Reference(page_id)],
            "Count" => 1,
        };
        doc.objects.insert(pages_id, Object::Dictionary(pages));
        let catalog_id = doc.add_object(dictionary! {
            "Type" => "Catalog",
            "Pages" => Object::Reference(pages_id),
        });
        doc.trailer.set("Root", Object::Reference(catalog_id));
        (doc, page_id)
    }

    fn mediabox(x0: f64, y0: f64, x1: f64, y1: f64) -> Object {
        Object::Array(vec![x0.into(), y0.into(), x1.into(), y1.into()])
    }

    #[test]
    fn media_box_only_page_resolves_to_media_box() {
        let (doc, page_id) = doc_with_page(dictionary! {
            "MediaBox" => mediabox(0.0, 0.0, 450.0, 666.0),
        });
        let size = effective_page_size(&doc, page_id).unwrap();
        assert_eq!(size.width.as_points(), 450.0);
        assert_eq!(size.height.as_points(), 666.0);
    }

    #[test]
    fn bleed_box_takes_priority_over_media_box() {
        let (doc, page_id) = doc_with_page(dictionary! {
            "MediaBox" => mediabox(0.0, 0.0, 500.0, 700.0),
            "BleedBox" => mediabox(0.0, 0.0, 450.0, 666.0),
        });
        let size = effective_page_size(&doc, page_id).unwrap();
        assert_eq!(size.width.as_points(), 450.0);
        assert_eq!(size.height.as_points(), 666.0);
    }

    #[test]
    fn crop_box_is_used_when_bleed_box_absent() {
        let (doc, page_id) = doc_with_page(dictionary! {
            "MediaBox" => mediabox(0.0, 0.0, 500.0, 700.0),
            "CropBox" => mediabox(0.0, 0.0, 450.0, 666.0),
        });
        let size = effective_page_size(&doc, page_id).unwrap();
        assert_eq!(size.width.as_points(), 450.0);
        assert_eq!(size.height.as_points(), 666.0);
    }

    #[test]
    fn media_box_is_inherited_from_the_pages_tree_when_absent_on_the_page() {
        let mut doc = lopdf::Document::with_version("1.7");
        let pages_id = doc.new_object_id();
        let page_dict = dictionary! {
            "Type" => "Page",
            "Parent" => Object::Reference(pages_id),
        };
        let page_id = doc.add_object(Object::Dictionary(page_dict));
        let pages = dictionary! {
            "Type" => "Pages",
            "Kids" => vec![Object::Reference(page_id)],
            "Count" => 1,
            "MediaBox" => mediabox(0.0, 0.0, 450.0, 666.0),
        };
        doc.objects.insert(pages_id, Object::Dictionary(pages));
        let catalog_id = doc.add_object(dictionary! {
            "Type" => "Catalog",
            "Pages" => Object::Reference(pages_id),
        });
        doc.trailer.set("Root", Object::Reference(catalog_id));

        let size = effective_page_size(&doc, page_id).unwrap();
        assert_eq!(size.width.as_points(), 450.0);
        assert_eq!(size.height.as_points(), 666.0);
    }

    #[test]
    fn rotate_90_swaps_width_and_height() {
        let (doc, page_id) = doc_with_page(dictionary! {
            "MediaBox" => mediabox(0.0, 0.0, 450.0, 666.0),
            "Rotate" => 90,
        });
        let size = effective_page_size(&doc, page_id).unwrap();
        assert_eq!(size.width.as_points(), 666.0);
        assert_eq!(size.height.as_points(), 450.0);
    }

    #[test]
    fn rotate_180_does_not_swap_width_and_height() {
        let (doc, page_id) = doc_with_page(dictionary! {
            "MediaBox" => mediabox(0.0, 0.0, 450.0, 666.0),
            "Rotate" => 180,
        });
        let size = effective_page_size(&doc, page_id).unwrap();
        assert_eq!(size.width.as_points(), 450.0);
        assert_eq!(size.height.as_points(), 666.0);
    }

    #[test]
    fn missing_media_box_is_an_error_not_a_panic() {
        let (doc, page_id) = doc_with_page(dictionary! {});
        assert!(effective_page_size(&doc, page_id).is_err());
    }

    #[test]
    fn round_trip_through_bytes_preserves_page_size() {
        let (mut doc, _) = doc_with_page(dictionary! {
            "MediaBox" => mediabox(0.0, 0.0, 450.0, 666.0),
        });
        let mut buf = Vec::new();
        doc.save_to(&mut buf).expect("save");
        let loaded = load_from_bytes(&buf).expect("load");
        let page_id = loaded.page_iter().next().expect("one page");
        let size = effective_page_size(&loaded, page_id).unwrap();
        assert_eq!(size.width.as_points(), 450.0);
        assert_eq!(size.height.as_points(), 666.0);
    }

    #[test]
    fn load_from_bytes_on_garbage_returns_an_error_not_a_panic() {
        let err = load_from_bytes(b"this is not a pdf");
        assert!(err.is_err());
    }

    // These fixtures are real qpdf-encrypted files (see tests/fixtures/generate.sh),
    // not round-tripped through lopdf's own writer: lopdf 0.44's writer silently
    // drops the /Encrypt trailer entry when saving an encrypted document (a bug
    // in the dependency, not our code — confirmed independently against a plain
    // classic-xref-table PDF, so it isn't specific to xref streams or object
    // streams either). Both fixtures share the same single 450x666 pt page.
    const EMPTY_PASSWORD_PDF: &[u8] =
        include_bytes!("../tests/fixtures/encrypted_empty_password.pdf");
    const REAL_PASSWORD_PDF: &[u8] =
        include_bytes!("../tests/fixtures/encrypted_real_password.pdf");

    #[test]
    fn loading_an_empty_password_pdf_transparently_decrypts_it() {
        // lopdf's loader auto-tries an empty password on load. On success it
        // clears the trailer's /Encrypt entry as part of decrypting — so
        // `is_encrypted()` reports false here even though the file *was*
        // encrypted. `was_encrypted()` is the one that stays true, and is
        // what a caller must check to still flag this as "carries an
        // encryption dictionary" per Lulu's prohibition on any encryption.
        let doc = load_from_bytes(EMPTY_PASSWORD_PDF).expect("load");
        assert!(!doc.is_encrypted());
        assert!(doc.was_encrypted());
        assert!(was_ever_encrypted(&doc));

        let page_id = doc.page_iter().next().expect("one page");
        let size = effective_page_size(&doc, page_id).unwrap();
        assert_eq!(size.width.as_points(), 450.0);
        assert_eq!(size.height.as_points(), 666.0);
    }

    #[test]
    fn loading_a_real_password_pdf_without_a_password_leaves_it_encrypted() {
        let doc = load_from_bytes(REAL_PASSWORD_PDF)
            .expect("load (structure parses; content stays encrypted)");
        assert!(doc.is_encrypted());
        assert!(!doc.was_encrypted());
        assert!(was_ever_encrypted(&doc));
    }

    // --- deep_copy_object / copy_page ---

    #[test]
    fn copy_page_preserves_content_bytes_and_box() {
        let mut src = Document::with_version("1.7");
        let content_id = src.add_object(lopdf::Stream::new(
            dictionary! {},
            b"1 0 0 RG 0 0 10 10 re S".to_vec(),
        ));
        let pages_id = src.new_object_id();
        let page_id = src.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => Object::Reference(pages_id),
            "MediaBox" => mediabox(0.0, 0.0, 450.0, 666.0),
            "Contents" => Object::Reference(content_id),
        });
        let pages = dictionary! { "Type" => "Pages", "Kids" => vec![Object::Reference(page_id)], "Count" => 1 };
        src.objects.insert(pages_id, Object::Dictionary(pages));
        let catalog_id = src.add_object(
            dictionary! { "Type" => "Catalog", "Pages" => Object::Reference(pages_id) },
        );
        src.trailer.set("Root", Object::Reference(catalog_id));

        let mut dest = Document::with_version("1.7");
        let new_page_id = copy_page(&mut dest, &src, page_id).unwrap();

        let new_page = dest.get_dictionary(new_page_id).unwrap();
        assert!(
            new_page.get(b"Parent").is_err(),
            "Parent must not be copied"
        );
        let content_ref = new_page.get(b"Contents").unwrap().as_reference().unwrap();
        let Object::Stream(new_stream) = dest.get_object(content_ref).unwrap() else {
            panic!()
        };
        assert_eq!(new_stream.content, b"1 0 0 RG 0 0 10 10 re S");
        assert!(new_stream.dict.get(b"Filter").is_err(), "an uncompressed source has no Filter to begin with, but confirms the key isn't spuriously added");
    }

    #[test]
    fn copy_page_deep_copies_an_embedded_font_and_strips_its_stale_filter() {
        let mut src = Document::with_version("1.7");
        // A Flate-compressed font file stream — exercises the Filter-stripping fix.
        let font_bytes = b"fake glyph data, repeated repeated repeated for compression".to_vec();
        let compressed = {
            use std::io::Write;
            let mut encoder =
                flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
            encoder.write_all(&font_bytes).unwrap();
            encoder.finish().unwrap()
        };
        let mut font_file_stream =
            lopdf::Stream::new(dictionary! { "Filter" => "FlateDecode" }, compressed);
        font_file_stream.start_position = Some(0);
        let font_file_id = src.add_object(Object::Stream(font_file_stream));
        let descriptor_id = src.add_object(dictionary! { "Type" => "FontDescriptor", "FontFile2" => Object::Reference(font_file_id) });
        let font_id = src.add_object(dictionary! { "Type" => "Font", "Subtype" => "TrueType", "FontDescriptor" => Object::Reference(descriptor_id) });
        let resources =
            dictionary! { "Font" => dictionary! { "F1" => Object::Reference(font_id) } };
        let content_id = src.add_object(lopdf::Stream::new(
            dictionary! {},
            b"BT /F1 12 Tf ET".to_vec(),
        ));
        let pages_id = src.new_object_id();
        let page_id = src.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => Object::Reference(pages_id),
            "MediaBox" => mediabox(0.0, 0.0, 450.0, 666.0),
            "Contents" => Object::Reference(content_id),
            "Resources" => resources,
        });
        let pages = dictionary! { "Type" => "Pages", "Kids" => vec![Object::Reference(page_id)], "Count" => 1 };
        src.objects.insert(pages_id, Object::Dictionary(pages));
        let catalog_id = src.add_object(
            dictionary! { "Type" => "Catalog", "Pages" => Object::Reference(pages_id) },
        );
        src.trailer.set("Root", Object::Reference(catalog_id));

        let mut dest = Document::with_version("1.7");
        let new_page_id = copy_page(&mut dest, &src, page_id).unwrap();

        let new_page = dest.get_dictionary(new_page_id).unwrap();
        let new_resources = new_page.get(b"Resources").unwrap().as_dict().unwrap();
        let new_font_ref = new_resources
            .get(b"Font")
            .unwrap()
            .as_dict()
            .unwrap()
            .get(b"F1")
            .unwrap()
            .as_reference()
            .unwrap();
        let new_font = dest.get_dictionary(new_font_ref).unwrap();
        let new_descriptor_ref = new_font
            .get(b"FontDescriptor")
            .unwrap()
            .as_reference()
            .unwrap();
        let new_descriptor = dest.get_dictionary(new_descriptor_ref).unwrap();
        let new_font_file_ref = new_descriptor
            .get(b"FontFile2")
            .unwrap()
            .as_reference()
            .unwrap();
        let Object::Stream(new_font_file) = dest.get_object(new_font_file_ref).unwrap() else {
            panic!()
        };

        assert_eq!(
            new_font_file.content, font_bytes,
            "font data must survive decompression + copy unchanged"
        );
        assert!(
            new_font_file.dict.get(b"Filter").is_err(),
            "stale Filter must be stripped since content is now raw"
        );
    }

    #[test]
    fn deep_copy_handles_reference_cycles_without_looping_forever() {
        let mut src = Document::with_version("1.7");
        let a_id = src.new_object_id();
        let b_id = src.new_object_id();
        src.objects.insert(
            a_id,
            Object::Dictionary(dictionary! { "Other" => Object::Reference(b_id) }),
        );
        src.objects.insert(
            b_id,
            Object::Dictionary(dictionary! { "Other" => Object::Reference(a_id) }),
        );

        let mut dest = Document::with_version("1.7");
        let mut memo = std::collections::HashMap::new();
        let copied = deep_copy_object(&mut dest, &src, &Object::Reference(a_id), &mut memo);

        let Object::Reference(new_a_id) = copied else {
            panic!()
        };
        let new_a = dest.get_dictionary(new_a_id).unwrap();
        let new_b_id = new_a.get(b"Other").unwrap().as_reference().unwrap();
        let new_b = dest.get_dictionary(new_b_id).unwrap();
        let back_to_a = new_b.get(b"Other").unwrap().as_reference().unwrap();
        assert_eq!(
            back_to_a, new_a_id,
            "the cycle must resolve back to the same copied object"
        );
    }

    // --- effective_page_resources (1.1) ---

    #[test]
    fn effective_page_resources_resolves_a_direct_dictionary() {
        let (doc, page_id) = doc_with_page(dictionary! {
            "Resources" => dictionary! { "Font" => dictionary! { "F1" => "whatever" } },
        });
        let resources = effective_page_resources(&doc, page_id).unwrap();
        assert!(resources.get(b"Font").unwrap().as_dict().is_ok());
    }

    #[test]
    fn effective_page_resources_resolves_an_indirect_reference() {
        let mut doc = lopdf::Document::with_version("1.7");
        let resources_id = doc.add_object(Object::Dictionary(
            dictionary! { "Font" => dictionary! { "F1" => "whatever" } },
        ));
        let pages_id = doc.new_object_id();
        let page_id = doc.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => Object::Reference(pages_id),
            "MediaBox" => mediabox(0.0, 0.0, 450.0, 666.0),
            "Resources" => Object::Reference(resources_id),
        });
        let pages = dictionary! { "Type" => "Pages", "Kids" => vec![Object::Reference(page_id)], "Count" => 1 };
        doc.objects.insert(pages_id, Object::Dictionary(pages));
        let catalog_id = doc.add_object(
            dictionary! { "Type" => "Catalog", "Pages" => Object::Reference(pages_id) },
        );
        doc.trailer.set("Root", Object::Reference(catalog_id));

        let resources = effective_page_resources(&doc, page_id).unwrap();
        let font_dict = resources.get(b"Font").unwrap().as_dict().unwrap();
        assert!(font_dict.get(b"F1").is_ok());
    }

    #[test]
    fn effective_page_resources_resolves_an_inherited_dictionary() {
        let mut doc = lopdf::Document::with_version("1.7");
        let pages_id = doc.new_object_id();
        let page_id = doc.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => Object::Reference(pages_id),
            "MediaBox" => mediabox(0.0, 0.0, 450.0, 666.0),
        });
        let pages = dictionary! {
            "Type" => "Pages",
            "Kids" => vec![Object::Reference(page_id)],
            "Count" => 1,
            "Resources" => dictionary! { "Font" => dictionary! { "F1" => "whatever" } },
        };
        doc.objects.insert(pages_id, Object::Dictionary(pages));
        let catalog_id = doc.add_object(
            dictionary! { "Type" => "Catalog", "Pages" => Object::Reference(pages_id) },
        );
        doc.trailer.set("Root", Object::Reference(catalog_id));

        let resources = effective_page_resources(&doc, page_id).unwrap();
        let font_dict = resources.get(b"Font").unwrap().as_dict().unwrap();
        assert!(font_dict.get(b"F1").is_ok());
    }

    #[test]
    fn effective_page_resources_merges_child_over_parent_at_the_sub_dictionary_level() {
        let mut doc = lopdf::Document::with_version("1.7");
        let pages_id = doc.new_object_id();
        let page_id = doc.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => Object::Reference(pages_id),
            "MediaBox" => mediabox(0.0, 0.0, 450.0, 666.0),
            // Page's own Font shadows the parent's F1 and adds F2; page has
            // no XObject of its own, so it must inherit the parent's.
            "Resources" => dictionary! {
                "Font" => dictionary! { "F1" => "page-own-f1", "F2" => "page-only-f2" },
            },
        });
        let pages = dictionary! {
            "Type" => "Pages",
            "Kids" => vec![Object::Reference(page_id)],
            "Count" => 1,
            "Resources" => dictionary! {
                "Font" => dictionary! { "F1" => "parent-f1" },
                "XObject" => dictionary! { "Im0" => "parent-image" },
            },
        };
        doc.objects.insert(pages_id, Object::Dictionary(pages));
        let catalog_id = doc.add_object(
            dictionary! { "Type" => "Catalog", "Pages" => Object::Reference(pages_id) },
        );
        doc.trailer.set("Root", Object::Reference(catalog_id));

        let resources = effective_page_resources(&doc, page_id).unwrap();
        let fonts = resources.get(b"Font").unwrap().as_dict().unwrap();
        assert_eq!(
            fonts.get(b"F1").unwrap().as_name().unwrap(),
            b"page-own-f1",
            "page's own entry must take precedence over the inherited one of the same name"
        );
        assert_eq!(
            fonts.get(b"F2").unwrap().as_name().unwrap(),
            b"page-only-f2"
        );
        let xobjects = resources.get(b"XObject").unwrap().as_dict().unwrap();
        assert_eq!(
            xobjects.get(b"Im0").unwrap().as_name().unwrap(),
            b"parent-image",
            "an inherited category the page doesn't redefine must still come through"
        );
    }

    #[test]
    fn effective_page_resources_with_no_resources_anywhere_is_an_empty_dictionary_not_an_error() {
        let (doc, page_id) = doc_with_page(dictionary! {
            "MediaBox" => mediabox(0.0, 0.0, 450.0, 666.0),
        });
        let resources = effective_page_resources(&doc, page_id).unwrap();
        assert!(resources.as_hashmap().is_empty());
    }

    #[test]
    fn effective_page_resources_on_a_dangling_indirect_reference_is_an_error() {
        let mut doc = lopdf::Document::with_version("1.7");
        let dangling_id = (999, 0);
        let pages_id = doc.new_object_id();
        let page_id = doc.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => Object::Reference(pages_id),
            "MediaBox" => mediabox(0.0, 0.0, 450.0, 666.0),
            "Resources" => Object::Reference(dangling_id),
        });
        let pages = dictionary! { "Type" => "Pages", "Kids" => vec![Object::Reference(page_id)], "Count" => 1 };
        doc.objects.insert(pages_id, Object::Dictionary(pages));
        let catalog_id = doc.add_object(
            dictionary! { "Type" => "Catalog", "Pages" => Object::Reference(pages_id) },
        );
        doc.trailer.set("Root", Object::Reference(catalog_id));

        let err = effective_page_resources(&doc, page_id).unwrap_err();
        assert!(matches!(err, PageGeometryError::ResourcesUnresolved(id) if id == page_id));
    }

    // --- rotation_outcome / rotation_degrees (1.3) ---

    #[test]
    fn rotation_outcome_absent_is_normalized_zero() {
        let (doc, page_id) = doc_with_page(dictionary! {
            "MediaBox" => mediabox(0.0, 0.0, 450.0, 666.0),
        });
        assert_eq!(
            rotation_outcome(&doc, page_id).unwrap(),
            RotationOutcome::Normalized(0)
        );
    }

    #[test]
    fn rotation_outcome_accepts_a_real_number() {
        let (doc, page_id) = doc_with_page(dictionary! {
            "MediaBox" => mediabox(0.0, 0.0, 450.0, 666.0),
            "Rotate" => Object::Real(90.0),
        });
        assert_eq!(
            rotation_outcome(&doc, page_id).unwrap(),
            RotationOutcome::Normalized(90)
        );
    }

    #[test]
    fn rotation_outcome_dereferences_an_indirect_rotate() {
        let mut doc = lopdf::Document::with_version("1.7");
        let rotate_id = doc.add_object(Object::Integer(270));
        let pages_id = doc.new_object_id();
        let page_id = doc.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => Object::Reference(pages_id),
            "MediaBox" => mediabox(0.0, 0.0, 450.0, 666.0),
            "Rotate" => Object::Reference(rotate_id),
        });
        let pages = dictionary! { "Type" => "Pages", "Kids" => vec![Object::Reference(page_id)], "Count" => 1 };
        doc.objects.insert(pages_id, Object::Dictionary(pages));
        let catalog_id = doc.add_object(
            dictionary! { "Type" => "Catalog", "Pages" => Object::Reference(pages_id) },
        );
        doc.trailer.set("Root", Object::Reference(catalog_id));

        assert_eq!(
            rotation_outcome(&doc, page_id).unwrap(),
            RotationOutcome::Normalized(270)
        );
    }

    #[test]
    fn rotation_outcome_dangling_reference_is_unreadable() {
        let (doc, page_id) = doc_with_page(dictionary! {
            "MediaBox" => mediabox(0.0, 0.0, 450.0, 666.0),
            "Rotate" => Object::Reference((999, 0)),
        });
        assert_eq!(
            rotation_outcome(&doc, page_id).unwrap(),
            RotationOutcome::Unreadable
        );
    }

    #[test]
    fn rotation_outcome_non_numeric_is_unreadable() {
        let (doc, page_id) = doc_with_page(dictionary! {
            "MediaBox" => mediabox(0.0, 0.0, 450.0, 666.0),
            "Rotate" => Object::Name(b"Bogus".to_vec()),
        });
        assert_eq!(
            rotation_outcome(&doc, page_id).unwrap(),
            RotationOutcome::Unreadable
        );
    }

    #[test]
    fn rotation_outcome_not_a_multiple_of_90_is_distinguished() {
        let (doc, page_id) = doc_with_page(dictionary! {
            "MediaBox" => mediabox(0.0, 0.0, 450.0, 666.0),
            "Rotate" => Object::Integer(45),
        });
        assert_eq!(
            rotation_outcome(&doc, page_id).unwrap(),
            RotationOutcome::NotAMultipleOf90(45)
        );
    }

    #[test]
    fn rotation_degrees_treats_unreadable_and_off_multiple_as_unrotated() {
        let (doc, unreadable_page) = doc_with_page(dictionary! {
            "MediaBox" => mediabox(0.0, 0.0, 450.0, 666.0),
            "Rotate" => Object::Reference((999, 0)),
        });
        assert_eq!(rotation_degrees(&doc, unreadable_page).unwrap(), 0);

        let (doc, oblique_page) = doc_with_page(dictionary! {
            "MediaBox" => mediabox(0.0, 0.0, 450.0, 666.0),
            "Rotate" => Object::Integer(45),
        });
        assert_eq!(rotation_degrees(&doc, oblique_page).unwrap(), 0);
    }

    // --- indirect box entries and elements (1.4) ---

    #[test]
    fn own_box_rect_dereferences_an_indirect_box_array() {
        let mut doc = lopdf::Document::with_version("1.7");
        let box_id = doc.add_object(mediabox(0.0, 0.0, 450.0, 666.0));
        let pages_id = doc.new_object_id();
        let page_id = doc.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => Object::Reference(pages_id),
            "MediaBox" => Object::Reference(box_id),
        });
        let pages = dictionary! { "Type" => "Pages", "Kids" => vec![Object::Reference(page_id)], "Count" => 1 };
        doc.objects.insert(pages_id, Object::Dictionary(pages));
        let catalog_id = doc.add_object(
            dictionary! { "Type" => "Catalog", "Pages" => Object::Reference(pages_id) },
        );
        doc.trailer.set("Root", Object::Reference(catalog_id));

        let rect = own_box_rect(&doc, page_id).unwrap();
        assert_eq!(rect.width().as_points(), 450.0);
        assert_eq!(rect.height().as_points(), 666.0);
    }

    #[test]
    fn own_box_rect_dereferences_indirect_array_elements() {
        let mut doc = lopdf::Document::with_version("1.7");
        let x1_id = doc.add_object(Object::Real(450.0));
        let y1_id = doc.add_object(Object::Integer(666));
        let box_array = Object::Array(vec![
            Object::Integer(0),
            Object::Integer(0),
            Object::Reference(x1_id),
            Object::Reference(y1_id),
        ]);
        let pages_id = doc.new_object_id();
        let page_id = doc.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => Object::Reference(pages_id),
            "MediaBox" => box_array,
        });
        let pages = dictionary! { "Type" => "Pages", "Kids" => vec![Object::Reference(page_id)], "Count" => 1 };
        doc.objects.insert(pages_id, Object::Dictionary(pages));
        let catalog_id = doc.add_object(
            dictionary! { "Type" => "Catalog", "Pages" => Object::Reference(pages_id) },
        );
        doc.trailer.set("Root", Object::Reference(catalog_id));

        let rect = own_box_rect(&doc, page_id).unwrap();
        assert_eq!(rect.width().as_points(), 450.0);
        assert_eq!(rect.height().as_points(), 666.0);
    }

    // --- catalog_names / catalog_names_mut (1.5) ---

    #[test]
    fn catalog_names_resolves_a_direct_dictionary() {
        let mut doc = lopdf::Document::with_version("1.7");
        let catalog_id = doc.add_object(dictionary! {
            "Type" => "Catalog",
            "Names" => dictionary! { "JavaScript" => dictionary! {} },
        });
        doc.trailer.set("Root", Object::Reference(catalog_id));

        let names = catalog_names(&doc).unwrap();
        assert!(names.get(b"JavaScript").is_ok());
    }

    #[test]
    fn catalog_names_resolves_an_indirect_reference() {
        let mut doc = lopdf::Document::with_version("1.7");
        let names_id = doc.add_object(Object::Dictionary(
            dictionary! { "JavaScript" => dictionary! {} },
        ));
        let catalog_id = doc.add_object(dictionary! {
            "Type" => "Catalog",
            "Names" => Object::Reference(names_id),
        });
        doc.trailer.set("Root", Object::Reference(catalog_id));

        let names = catalog_names(&doc).unwrap();
        assert!(names.get(b"JavaScript").is_ok());
    }

    #[test]
    fn catalog_names_is_none_when_absent() {
        let mut doc = lopdf::Document::with_version("1.7");
        let catalog_id = doc.add_object(dictionary! { "Type" => "Catalog" });
        doc.trailer.set("Root", Object::Reference(catalog_id));
        assert!(catalog_names(&doc).is_none());
    }

    #[test]
    fn catalog_names_mut_mutates_through_an_indirect_reference() {
        let mut doc = lopdf::Document::with_version("1.7");
        let names_id = doc.add_object(Object::Dictionary(
            dictionary! { "JavaScript" => dictionary! {}, "EmbeddedFiles" => dictionary! {} },
        ));
        let catalog_id = doc.add_object(dictionary! {
            "Type" => "Catalog",
            "Names" => Object::Reference(names_id),
        });
        doc.trailer.set("Root", Object::Reference(catalog_id));

        catalog_names_mut(&mut doc).unwrap().remove(b"JavaScript");

        let names = catalog_names(&doc).unwrap();
        assert!(names.get(b"JavaScript").is_err());
        assert!(
            names.get(b"EmbeddedFiles").is_ok(),
            "other entries must be untouched"
        );
    }

    #[test]
    fn catalog_names_mut_mutates_a_direct_dictionary() {
        let mut doc = lopdf::Document::with_version("1.7");
        let catalog_id = doc.add_object(dictionary! {
            "Type" => "Catalog",
            "Names" => dictionary! { "JavaScript" => dictionary! {} },
        });
        doc.trailer.set("Root", Object::Reference(catalog_id));

        catalog_names_mut(&mut doc).unwrap().remove(b"JavaScript");

        assert!(catalog_names(&doc).unwrap().get(b"JavaScript").is_err());
    }

    // --- /Parent cycle termination (1.6) ---

    #[test]
    fn a_parent_cycle_terminates_instead_of_looping_forever() {
        let mut doc = lopdf::Document::with_version("1.7");
        let a_id = doc.new_object_id();
        let b_id = doc.new_object_id();
        // a's Parent is b, b's Parent is a: a cycle with no MediaBox anywhere.
        doc.objects.insert(
            a_id,
            Object::Dictionary(
                dictionary! { "Type" => "Pages", "Parent" => Object::Reference(b_id) },
            ),
        );
        doc.objects.insert(
            b_id,
            Object::Dictionary(
                dictionary! { "Type" => "Pages", "Parent" => Object::Reference(a_id) },
            ),
        );
        let page_id = doc.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => Object::Reference(a_id),
        });

        // Must return promptly (an error, since no MediaBox is resolvable
        // anywhere in the cyclic ancestry) rather than hang.
        let result = own_box_rect(&doc, page_id);
        assert!(result.is_err());
    }

    #[test]
    fn effective_page_resources_terminates_on_a_parent_cycle() {
        let mut doc = lopdf::Document::with_version("1.7");
        let a_id = doc.new_object_id();
        let b_id = doc.new_object_id();
        doc.objects.insert(
            a_id,
            Object::Dictionary(
                dictionary! { "Type" => "Pages", "Parent" => Object::Reference(b_id) },
            ),
        );
        doc.objects.insert(
            b_id,
            Object::Dictionary(
                dictionary! { "Type" => "Pages", "Parent" => Object::Reference(a_id) },
            ),
        );
        let page_id = doc.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => Object::Reference(a_id),
        });

        // No /Resources anywhere in the cycle: must terminate with an empty
        // (not erroring) result rather than hang.
        let resources = effective_page_resources(&doc, page_id).unwrap();
        assert!(resources.as_hashmap().is_empty());
    }

    // --- deep-copy depth budget (1.7) ---

    /// Builds a chain of `length` indirect objects in `doc`, each a
    /// one-entry dictionary `{"Next" => Reference(next link)}`, terminated
    /// by an empty dictionary. Returns the id of the first (outermost) link.
    ///
    /// Deliberately *not* a chain of bare `Object::Reference`s pointing
    /// directly to one another: `lopdf::Document::get_object` iteratively
    /// (and non-recursively) unwinds a run of bare references itself before
    /// this crate's code ever sees more than one hop of it, capped at its
    /// own `DEREF_LIMIT` (128) — so a bare-reference chain never actually
    /// exercises `deep_copy_object`'s own recursion. Wrapping each link in a
    /// dictionary is what a real reference-chain PDF (and the hostile fixture
    /// this guards against) actually looks like, and is what makes each link
    /// cost one real frame of this crate's own recursion.
    fn build_dictionary_reference_chain(doc: &mut lopdf::Document, length: usize) -> ObjectId {
        let mut next_id = doc.add_object(Object::Dictionary(dictionary! {}));
        for _ in 0..length {
            let this_id = doc.new_object_id();
            doc.objects.insert(
                this_id,
                Object::Dictionary(dictionary! { "Next" => Object::Reference(next_id) }),
            );
            next_id = this_id;
        }
        next_id
    }

    #[test]
    fn deep_copy_object_reporting_truncates_a_long_reference_chain_instead_of_overflowing() {
        let mut src = Document::with_version("1.7");
        // Each link costs two depth increments (the Reference hop, then the
        // Dictionary it resolves to), so this comfortably exceeds the budget.
        let head = build_dictionary_reference_chain(&mut src, DEEP_COPY_DEPTH_BUDGET);

        let mut dest = Document::with_version("1.7");
        let mut memo = std::collections::HashMap::new();
        let (_copied, report) =
            deep_copy_object_reporting(&mut dest, &src, &Object::Reference(head), &mut memo);

        assert!(
            report.depth_budget_exceeded,
            "a chain longer than the budget must be reported as truncated"
        );
    }

    #[test]
    fn deep_copy_object_does_not_overflow_the_stack_on_a_long_reference_chain() {
        // Regression test for the crash itself: deep_copy_object (the
        // unchanged-signature, non-reporting entry point every existing
        // caller uses) must complete rather than abort the process, given a
        // ~60,000-link chain like the one found in a hostile ~1MB fixture.
        let mut src = Document::with_version("1.7");
        let head = build_dictionary_reference_chain(&mut src, 60_000);

        let mut dest = Document::with_version("1.7");
        let mut memo = std::collections::HashMap::new();
        // Must simply return, not crash.
        let _ = deep_copy_object(&mut dest, &src, &Object::Reference(head), &mut memo);
    }

    // --- stream filter preservation on decode failure (1.8) ---

    #[test]
    fn deep_copy_keeps_filter_and_bytes_verbatim_when_decoding_fails() {
        let mut src = Document::with_version("1.7");
        // DCTDecode (JPEG): lopdf's decode_filters doesn't implement this, so
        // get_plain_content() fails and the fallback path is exercised.
        let fake_jpeg_bytes =
            b"\xff\xd8\xff\xe0 not really a jpeg but undecoded either way".to_vec();
        let stream = lopdf::Stream::new(
            dictionary! { "Filter" => "DCTDecode", "Width" => 10, "Height" => 10 },
            fake_jpeg_bytes.clone(),
        );
        let stream_id = src.add_object(Object::Stream(stream));

        let mut dest = Document::with_version("1.7");
        let mut memo = std::collections::HashMap::new();
        let copied = deep_copy_object(&mut dest, &src, &Object::Reference(stream_id), &mut memo);

        let Object::Reference(new_id) = copied else {
            panic!("expected a reference")
        };
        let Object::Stream(new_stream) = dest.get_object(new_id).unwrap() else {
            panic!("expected a stream")
        };
        assert_eq!(
            new_stream.content, fake_jpeg_bytes,
            "undecodable content must be copied verbatim, not corrupted"
        );
        assert_eq!(
            new_stream.dict.get(b"Filter").unwrap().as_name().unwrap(),
            b"DCTDecode",
            "the original Filter must be kept when decoding failed, since the \
             bytes are still encoded"
        );
    }

    #[test]
    fn deep_copy_strips_filter_when_decoding_succeeds() {
        let mut src = Document::with_version("1.7");
        let plain = b"BT /F1 12 Tf ET".to_vec();
        let compressed = {
            use std::io::Write;
            let mut encoder =
                flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
            encoder.write_all(&plain).unwrap();
            encoder.finish().unwrap()
        };
        let stream = lopdf::Stream::new(dictionary! { "Filter" => "FlateDecode" }, compressed);
        let stream_id = src.add_object(Object::Stream(stream));

        let mut dest = Document::with_version("1.7");
        let mut memo = std::collections::HashMap::new();
        let copied = deep_copy_object(&mut dest, &src, &Object::Reference(stream_id), &mut memo);

        let Object::Reference(new_id) = copied else {
            panic!("expected a reference")
        };
        let Object::Stream(new_stream) = dest.get_object(new_id).unwrap() else {
            panic!("expected a stream")
        };
        assert_eq!(new_stream.content, plain);
        assert!(new_stream.dict.get(b"Filter").is_err());
    }

    // --- dangling reference reporting (1.10) ---

    #[test]
    fn deep_copy_object_reporting_names_a_dangling_reference() {
        let src = Document::with_version("1.7");
        let dangling_id = (42, 0);

        let mut dest = Document::with_version("1.7");
        let mut memo = std::collections::HashMap::new();
        let (copied, report) =
            deep_copy_object_reporting(&mut dest, &src, &Object::Reference(dangling_id), &mut memo);

        // A dangling reference at the top level is substituted directly
        // (there is no dest object to point a Reference at, since nothing
        // was resolved).
        assert_eq!(copied, Object::Null);
        assert_eq!(report.dangling_references, vec![dangling_id]);
    }

    #[test]
    fn deep_copy_object_silently_substitutes_null_for_a_dangling_reference() {
        // The plain, unchanged-signature entry point keeps its existing
        // (non-reporting) behaviour, for compatibility with its existing
        // callers outside this module.
        let src = Document::with_version("1.7");
        let dangling_id = (42, 0);

        let mut dest = Document::with_version("1.7");
        let mut memo = std::collections::HashMap::new();
        let copied = deep_copy_object(&mut dest, &src, &Object::Reference(dangling_id), &mut memo);

        assert_eq!(copied, Object::Null);
    }

    #[test]
    fn copy_page_reports_a_dangling_reference_as_an_error() {
        let mut src = Document::with_version("1.7");
        let pages_id = src.new_object_id();
        let page_id = src.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => Object::Reference(pages_id),
            "MediaBox" => mediabox(0.0, 0.0, 450.0, 666.0),
            "Contents" => Object::Reference((12345, 0)), // does not exist
        });
        let pages = dictionary! { "Type" => "Pages", "Kids" => vec![Object::Reference(page_id)], "Count" => 1 };
        src.objects.insert(pages_id, Object::Dictionary(pages));
        let catalog_id = src.add_object(
            dictionary! { "Type" => "Catalog", "Pages" => Object::Reference(pages_id) },
        );
        src.trailer.set("Root", Object::Reference(catalog_id));

        let mut dest = Document::with_version("1.7");
        let err = copy_page(&mut dest, &src, page_id).unwrap_err();
        assert!(matches!(
            err,
            PageGeometryError::DanglingReference(id, refs)
                if id == page_id && refs == vec![(12345, 0)]
        ));
    }

    // --- apply_deterministic_identity reports an unresolvable /Info (1.9) ---

    #[test]
    fn deterministic_identity_reports_when_info_is_not_a_dictionary() {
        let (mut doc, _page_id) = doc_with_page(dictionary! {});
        let not_a_dict_id = doc.add_object(Object::Integer(42));
        doc.trailer.set("Info", Object::Reference(not_a_dict_id));

        let err =
            apply_deterministic_identity(&mut doc, [0x01; 16], "D:20260101000000Z").unwrap_err();
        assert!(
            matches!(err, DeterministicIdentityError::InfoNotADictionary(id) if id == not_a_dict_id)
        );
    }

    #[test]
    fn deterministic_identity_reports_a_dangling_info_reference() {
        let (mut doc, _page_id) = doc_with_page(dictionary! {});
        let dangling_id = (999, 0);
        doc.trailer.set("Info", Object::Reference(dangling_id));

        let err =
            apply_deterministic_identity(&mut doc, [0x01; 16], "D:20260101000000Z").unwrap_err();
        assert!(
            matches!(err, DeterministicIdentityError::InfoNotADictionary(id) if id == dangling_id)
        );
    }

    #[test]
    fn deterministic_identity_still_sets_the_trailer_id_even_when_info_is_unresolvable() {
        let (mut doc, _page_id) = doc_with_page(dictionary! {});
        let not_a_dict_id = doc.add_object(Object::Integer(42));
        doc.trailer.set("Info", Object::Reference(not_a_dict_id));

        let _ = apply_deterministic_identity(&mut doc, [0x02; 16], "D:20260101000000Z");

        let id_array = doc.trailer.get(b"ID").unwrap().as_array().unwrap();
        assert_eq!(id_array.len(), 2);
    }

    #[test]
    fn deterministic_identity_produces_byte_identical_output_across_two_runs() {
        let fixed_id = [0x42u8; 16];
        let fixed_date = "D:20260101000000Z";

        let render = || {
            let (mut doc, _page_id) = doc_with_page(dictionary! {});
            apply_deterministic_identity(&mut doc, fixed_id, fixed_date).unwrap();
            let mut bytes = Vec::new();
            doc.save_to(&mut bytes).unwrap();
            bytes
        };

        assert_eq!(render(), render());
    }

    #[test]
    fn deterministic_identity_sets_trailer_id_and_info_dates() {
        let (mut doc, _page_id) = doc_with_page(dictionary! {});
        apply_deterministic_identity(&mut doc, [0xAB; 16], "D:20260101000000Z").unwrap();

        let id_array = doc.trailer.get(b"ID").unwrap().as_array().unwrap();
        assert_eq!(id_array.len(), 2);
        assert_eq!(id_array[0], id_array[1]);

        let info_id = doc.trailer.get(b"Info").unwrap().as_reference().unwrap();
        let info_dict = doc.get_dictionary(info_id).unwrap();
        assert_eq!(
            info_dict.get(b"CreationDate").unwrap().as_str().unwrap(),
            b"D:20260101000000Z"
        );
        assert_eq!(
            info_dict.get(b"ModDate").unwrap().as_str().unwrap(),
            b"D:20260101000000Z"
        );
    }

    #[test]
    fn deterministic_identity_overwrites_an_existing_info_dict_rather_than_duplicating_it() {
        let (mut doc, _page_id) = doc_with_page(dictionary! {});
        let info_id = doc.add_object(Object::Dictionary(
            dictionary! { "CreationDate" => Object::string_literal("D:20200101000000Z") },
        ));
        doc.trailer.set("Info", Object::Reference(info_id));

        apply_deterministic_identity(&mut doc, [0x01; 16], "D:20260101000000Z").unwrap();

        assert_eq!(
            doc.trailer.get(b"Info").unwrap().as_reference().unwrap(),
            info_id
        );
        let info_dict = doc.get_dictionary(info_id).unwrap();
        assert_eq!(
            info_dict.get(b"CreationDate").unwrap().as_str().unwrap(),
            b"D:20260101000000Z"
        );
    }
}
