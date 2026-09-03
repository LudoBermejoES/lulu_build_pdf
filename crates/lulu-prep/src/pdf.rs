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
pub fn apply_deterministic_identity(doc: &mut Document, doc_id: [u8; 16], creation_date_pdf: &str) {
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
            if let Ok(info_dict) = doc.get_object_mut(info_id).and_then(Object::as_dict_mut) {
                info_dict.set("CreationDate", date_object.clone());
                info_dict.set("ModDate", date_object);
            }
        }
        None => {
            let mut info_dict = Dictionary::new();
            info_dict.set("CreationDate", date_object.clone());
            info_dict.set("ModDate", date_object);
            let info_id = doc.add_object(Object::Dictionary(info_dict));
            doc.trailer.set("Info", Object::Reference(info_id));
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum PageGeometryError {
    #[error("page {0:?} has no MediaBox, and none of its ancestors in the Pages tree do either")]
    NoMediaBox(ObjectId),
    #[error("could not read page dictionary: {0}")]
    Pdf(#[from] lopdf::Error),
}

fn as_rect_points(object: &Object) -> Option<[f64; 4]> {
    let array = object.as_array().ok()?;
    if array.len() != 4 {
        return None;
    }
    let mut out = [0.0; 4];
    for (i, o) in array.iter().enumerate() {
        out[i] = match o {
            Object::Integer(n) => *n as f64,
            Object::Real(n) => *n as f64,
            _ => return None,
        };
    }
    Some(out)
}

/// Walk a page dictionary's `/Parent` chain looking up `key`, per the PDF
/// spec's inheritable page attributes (`MediaBox`, `CropBox`, `Resources`,
/// `Rotate` may be set on any ancestor `Pages` node and inherited down).
fn get_inherited<'a>(
    doc: &'a Document,
    mut dict: &'a Dictionary,
    key: &[u8],
) -> Option<&'a Object> {
    loop {
        if let Ok(value) = dict.get(key) {
            return Some(value);
        }
        let parent_ref = dict.get(b"Parent").ok()?.as_reference().ok()?;
        dict = doc.get_dictionary(parent_ref).ok()?;
    }
}

fn box_points(doc: &Document, dict: &Dictionary, key: &[u8], inherited: bool) -> Option<[f64; 4]> {
    let object = if inherited {
        get_inherited(doc, dict, key)?
    } else {
        dict.get(key).ok()?
    };
    as_rect_points(object)
}

/// Rotation in degrees clockwise, from the page's (inherited) `/Rotate` entry.
/// Defaults to 0. Values are normalized into `{0, 90, 180, 270}`.
pub fn rotation_degrees(doc: &Document, page_id: ObjectId) -> Result<i64, PageGeometryError> {
    let dict = doc.get_dictionary(page_id)?;
    let raw = get_inherited(doc, dict, b"Rotate")
        .and_then(|o| o.as_i64().ok())
        .unwrap_or(0);
    Ok(((raw % 360) + 360) % 360)
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
pub fn deep_copy_object(
    dest: &mut Document,
    src: &Document,
    object: &Object,
    memo: &mut std::collections::HashMap<ObjectId, ObjectId>,
) -> Object {
    match object {
        Object::Reference(src_id) => {
            if let Some(&dest_id) = memo.get(src_id) {
                return Object::Reference(dest_id);
            }
            let Ok(referenced) = src.get_object(*src_id) else {
                return Object::Null;
            };
            // Reserve the new id before recursing, so a cycle back to this
            // object resolves to the right (already-reserved) id rather than
            // recursing forever.
            let dest_id = dest.new_object_id();
            memo.insert(*src_id, dest_id);
            let copied = deep_copy_object(dest, src, referenced, memo);
            dest.objects.insert(dest_id, copied);
            Object::Reference(dest_id)
        }
        Object::Array(items) => Object::Array(
            items
                .iter()
                .map(|o| deep_copy_object(dest, src, o, memo))
                .collect(),
        ),
        Object::Dictionary(dict) => {
            let mut new_dict = Dictionary::new();
            for (k, v) in dict.as_hashmap() {
                new_dict.set(k.clone(), deep_copy_object(dest, src, v, memo));
            }
            Object::Dictionary(new_dict)
        }
        Object::Stream(stream) => {
            // get_plain_content() decompresses per the stream's own Filter;
            // the copied dict must drop Filter/DecodeParms (and the old
            // Length, which no longer matches) since the new stream holds
            // raw, undecoded bytes — Stream::new sets Length itself.
            let mut new_dict = Dictionary::new();
            for (k, v) in stream.dict.as_hashmap() {
                if matches!(k.as_slice(), b"Filter" | b"DecodeParms" | b"Length") {
                    continue;
                }
                new_dict.set(k.clone(), deep_copy_object(dest, src, v, memo));
            }
            let content = stream
                .get_plain_content()
                .unwrap_or_else(|_| stream.content.clone());
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
pub fn copy_page(
    dest: &mut Document,
    src: &Document,
    src_page_id: ObjectId,
) -> Result<ObjectId, PageGeometryError> {
    let src_dict = src.get_dictionary(src_page_id)?;
    let mut memo = std::collections::HashMap::new();
    let mut new_dict = Dictionary::new();
    for (k, v) in src_dict.as_hashmap() {
        if k == b"Parent" {
            continue;
        }
        new_dict.set(k.clone(), deep_copy_object(dest, src, v, &mut memo));
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

    #[test]
    fn deterministic_identity_produces_byte_identical_output_across_two_runs() {
        let fixed_id = [0x42u8; 16];
        let fixed_date = "D:20260101000000Z";

        let render = || {
            let (mut doc, _page_id) = doc_with_page(dictionary! {});
            apply_deterministic_identity(&mut doc, fixed_id, fixed_date);
            let mut bytes = Vec::new();
            doc.save_to(&mut bytes).unwrap();
            bytes
        };

        assert_eq!(render(), render());
    }

    #[test]
    fn deterministic_identity_sets_trailer_id_and_info_dates() {
        let (mut doc, _page_id) = doc_with_page(dictionary! {});
        apply_deterministic_identity(&mut doc, [0xAB; 16], "D:20260101000000Z");

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

        apply_deterministic_identity(&mut doc, [0x01; 16], "D:20260101000000Z");

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
