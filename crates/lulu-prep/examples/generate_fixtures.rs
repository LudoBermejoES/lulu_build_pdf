//! Regenerates the committed test-corpus fixtures in `tests/fixtures/`
//! (`openspec/changes/prepare-pdf-for-lulu/tasks.md`, task 12.1). Each
//! fixture is built directly with `lopdf` so its structure is exact and
//! reviewable, rather than hand-computing byte offsets.
//!
//! `encrypted_empty_password.pdf` and `encrypted_real_password.pdf` are
//! generated separately by `tests/fixtures/generate.sh` (via qpdf, to work
//! around lopdf's writer dropping `/Encrypt` — see `src/pdf.rs`), not here.
//!
//! Run with `cargo run -p lulu-prep --example generate_fixtures`.

use lopdf::{dictionary, Dictionary, Document, Object, Stream};
use std::path::Path;

/// 6x9in trim, no bleed: exactly what a source file usually looks like
/// before this tool adds Lulu's 0.125in bleed on every side.
fn mediabox_no_bleed() -> Object {
    Object::Array(vec![0.into(), 0.into(), 432.into(), 648.into()])
}

/// 6x9in trim plus 0.125in bleed per side (450x666pt) — the size this
/// crate's `geometry::required_page_size` computes for that trim, and the
/// size Lulu's own file validation requires.
fn mediabox_with_bleed() -> Object {
    Object::Array(vec![0.into(), 0.into(), 450.into(), 666.into()])
}

fn empty_content(doc: &mut Document) -> Object {
    Object::Reference(doc.add_object(Stream::new(dictionary! {}, Vec::new())))
}

fn image_xobject(width: i64, height: i64) -> Dictionary {
    dictionary! {
        "Type" => "XObject",
        "Subtype" => "Image",
        "Width" => width,
        "Height" => height,
        "BitsPerComponent" => 8,
        "ColorSpace" => "DeviceRGB",
    }
}

/// One page at exactly the trim size, no bleed — the single most common
/// reason Lulu rejects a file: the source was never designed with bleed.
fn build_no_bleed() -> Document {
    let mut doc = Document::with_version("1.7");
    let contents = empty_content(&mut doc);
    let pages_id = doc.new_object_id();
    let page_id = doc.add_object(dictionary! {
        "Type" => "Page",
        "Parent" => Object::Reference(pages_id),
        "MediaBox" => mediabox_no_bleed(),
        "Contents" => contents,
    });
    doc.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! { "Type" => "Pages", "Kids" => vec![Object::Reference(page_id)], "Count" => 1 }),
    );
    let catalog_id =
        doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => Object::Reference(pages_id) });
    doc.trailer.set("Root", Object::Reference(catalog_id));
    doc
}

/// 32 pages (the product's minimum) at exactly the required bleed size,
/// with no fonts, images, or structure to flag — the print-ready baseline
/// every other fixture is a variation of.
fn build_correct_bleed() -> Document {
    let mut doc = Document::with_version("1.7");
    let pages_id = doc.new_object_id();
    let mut kids = Vec::new();
    for _ in 0..32 {
        let contents = empty_content(&mut doc);
        let page_id = doc.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => Object::Reference(pages_id),
            "MediaBox" => mediabox_with_bleed(),
            "Contents" => contents,
        });
        kids.push(Object::Reference(page_id));
    }
    doc.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! { "Type" => "Pages", "Kids" => kids, "Count" => 32 }),
    );
    let catalog_id =
        doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => Object::Reference(pages_id) });
    doc.trailer.set("Root", Object::Reference(catalog_id));
    doc
}

/// Three pages, one a different size from the other two — Lulu's file
/// validation rejects a mixed-size interior outright.
fn build_mixed_sizes() -> Document {
    let mut doc = Document::with_version("1.7");
    let pages_id = doc.new_object_id();
    let mut kids = Vec::new();
    for i in 0..3 {
        let contents = empty_content(&mut doc);
        let mediabox = if i == 1 {
            Object::Array(vec![0.into(), 0.into(), 500.into(), 700.into()])
        } else {
            mediabox_with_bleed()
        };
        let page_id = doc.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => Object::Reference(pages_id),
            "MediaBox" => mediabox,
            "Contents" => contents,
        });
        kids.push(Object::Reference(page_id));
    }
    doc.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! { "Type" => "Pages", "Kids" => kids, "Count" => 3 }),
    );
    let catalog_id =
        doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => Object::Reference(pages_id) });
    doc.trailer.set("Root", Object::Reference(catalog_id));
    doc
}

/// A correctly-bled page rotated 90 degrees — its visible (post-rotation)
/// size no longer matches the target until this tool bakes the rotation in.
fn build_rotated() -> Document {
    let mut doc = Document::with_version("1.7");
    let contents = empty_content(&mut doc);
    let pages_id = doc.new_object_id();
    let page_id = doc.add_object(dictionary! {
        "Type" => "Page",
        "Parent" => Object::Reference(pages_id),
        "MediaBox" => mediabox_with_bleed(),
        "Rotate" => 90,
        "Contents" => contents,
    });
    doc.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! { "Type" => "Pages", "Kids" => vec![Object::Reference(page_id)], "Count" => 1 }),
    );
    let catalog_id =
        doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => Object::Reference(pages_id) });
    doc.trailer.set("Root", Object::Reference(catalog_id));
    doc
}

/// A page referencing a standard Type1 font (Helvetica) with no
/// `FontDescriptor`/embedded font program — Lulu rejects any unembedded
/// font, standard 14 included.
fn build_unembedded_font() -> Document {
    let mut doc = Document::with_version("1.7");
    let font_id = doc.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type1",
        "BaseFont" => "Helvetica",
    });
    let resources = dictionary! { "Font" => dictionary! { "F1" => Object::Reference(font_id) } };
    let contents = empty_content(&mut doc);
    let pages_id = doc.new_object_id();
    let page_id = doc.add_object(dictionary! {
        "Type" => "Page",
        "Parent" => Object::Reference(pages_id),
        "MediaBox" => mediabox_with_bleed(),
        "Resources" => resources,
        "Contents" => contents,
    });
    doc.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! { "Type" => "Pages", "Kids" => vec![Object::Reference(page_id)], "Count" => 1 }),
    );
    let catalog_id =
        doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => Object::Reference(pages_id) });
    doc.trailer.set("Root", Object::Reference(catalog_id));
    doc
}

/// A 600x400px image drawn across 6x4in (432x288pt) — 100 ppi, below
/// Lulu's 300 ppi target.
fn build_low_resolution_image() -> Document {
    let mut doc = Document::with_version("1.7");
    let image_id = doc.add_object(Object::Stream(Stream::new(
        image_xobject(600, 400),
        vec![0u8; 4],
    )));
    let resources =
        dictionary! { "XObject" => dictionary! { "Im0" => Object::Reference(image_id) } };
    let content = doc.add_object(Stream::new(
        dictionary! {},
        b"q 432 0 0 288 0 0 cm /Im0 Do Q".to_vec(),
    ));
    let pages_id = doc.new_object_id();
    let page_id = doc.add_object(dictionary! {
        "Type" => "Page",
        "Parent" => Object::Reference(pages_id),
        "MediaBox" => mediabox_with_bleed(),
        "Resources" => resources,
        "Contents" => Object::Reference(content),
    });
    doc.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! { "Type" => "Pages", "Kids" => vec![Object::Reference(page_id)], "Count" => 1 }),
    );
    let catalog_id =
        doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => Object::Reference(pages_id) });
    doc.trailer.set("Root", Object::Reference(catalog_id));
    doc
}

/// A low-resolution image drawn inside a Form XObject rather than directly
/// on the page — exercises the content-stream walker's descent into forms
/// (`ctm_walk::walk_page_images`), composing the page's own `cm` with the
/// form's `/Matrix`.
fn build_nested_form_xobject_image() -> Document {
    let mut doc = Document::with_version("1.7");
    // 400x400px drawn at the form's native 1x1 unit square, then the form's
    // own Matrix scales by 72 (1in), then the page cm scales by 4 further
    // (4in) -> drawn at 4in -> 400/4 = 100 ppi.
    let image_id = doc.add_object(Object::Stream(Stream::new(
        image_xobject(400, 400),
        vec![0u8; 4],
    )));
    let form_resources =
        dictionary! { "XObject" => dictionary! { "Im0" => Object::Reference(image_id) } };
    let form_id = doc.add_object(Object::Stream(Stream::new(
        dictionary! {
            "Type" => "XObject",
            "Subtype" => "Form",
            "Matrix" => Object::Array(vec![72.0.into(), 0.0.into(), 0.0.into(), 72.0.into(), 0.0.into(), 0.0.into()]),
            "Resources" => form_resources,
        },
        b"/Im0 Do".to_vec(),
    )));
    let resources =
        dictionary! { "XObject" => dictionary! { "Fm0" => Object::Reference(form_id) } };
    let content = doc.add_object(Stream::new(
        dictionary! {},
        b"q 4 0 0 4 0 0 cm /Fm0 Do Q".to_vec(),
    ));
    let pages_id = doc.new_object_id();
    let page_id = doc.add_object(dictionary! {
        "Type" => "Page",
        "Parent" => Object::Reference(pages_id),
        "MediaBox" => mediabox_with_bleed(),
        "Resources" => resources,
        "Contents" => Object::Reference(content),
    });
    doc.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! { "Type" => "Pages", "Kids" => vec![Object::Reference(page_id)], "Count" => 1 }),
    );
    let catalog_id =
        doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => Object::Reference(pages_id) });
    doc.trailer.set("Root", Object::Reference(catalog_id));
    doc
}

/// A page with a soft-mask `ExtGState` — live transparency Lulu's own
/// normalizer flattens, which this tool flags rather than silently letting
/// through unflattened.
fn build_live_transparency() -> Document {
    let mut doc = Document::with_version("1.7");
    let resources = dictionary! { "ExtGState" => dictionary! { "GS0" => dictionary! { "SMask" => dictionary! { "Type" => "Mask" } } } };
    let contents = empty_content(&mut doc);
    let pages_id = doc.new_object_id();
    let page_id = doc.add_object(dictionary! {
        "Type" => "Page",
        "Parent" => Object::Reference(pages_id),
        "MediaBox" => mediabox_with_bleed(),
        "Resources" => resources,
        "Contents" => contents,
    });
    doc.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! { "Type" => "Pages", "Kids" => vec![Object::Reference(page_id)], "Count" => 1 }),
    );
    let catalog_id =
        doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => Object::Reference(pages_id) });
    doc.trailer.set("Root", Object::Reference(catalog_id));
    doc
}

/// A document declaring optional content (layers) via `/OCProperties` —
/// Lulu's file validation rejects layered PDFs; layers must be flattened.
fn build_optional_content_groups() -> Document {
    let mut doc = Document::with_version("1.7");
    let contents = empty_content(&mut doc);
    let pages_id = doc.new_object_id();
    let page_id = doc.add_object(dictionary! {
        "Type" => "Page",
        "Parent" => Object::Reference(pages_id),
        "MediaBox" => mediabox_with_bleed(),
        "Contents" => contents,
    });
    doc.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! { "Type" => "Pages", "Kids" => vec![Object::Reference(page_id)], "Count" => 1 }),
    );
    let catalog_id = doc.add_object(dictionary! {
        "Type" => "Catalog",
        "Pages" => Object::Reference(pages_id),
        "OCProperties" => dictionary! { "OCGs" => Vec::<Object>::new() },
    });
    doc.trailer.set("Root", Object::Reference(catalog_id));
    doc
}

/// A document whose catalog declares a two-up `PageLayout` — a common
/// export mistake (a spread meant for on-screen reading, not a single Lulu
/// page) this tool detects and forces to single-page.
fn build_two_up_spread() -> Document {
    let mut doc = Document::with_version("1.7");
    // Page itself is drawn at double width, as a real two-up spread export
    // would produce, in addition to the PageLayout metadata.
    let mediabox = Object::Array(vec![0.into(), 0.into(), 900.into(), 666.into()]);
    let contents = empty_content(&mut doc);
    let pages_id = doc.new_object_id();
    let page_id = doc.add_object(dictionary! {
        "Type" => "Page",
        "Parent" => Object::Reference(pages_id),
        "MediaBox" => mediabox,
        "Contents" => contents,
    });
    doc.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! { "Type" => "Pages", "Kids" => vec![Object::Reference(page_id)], "Count" => 1 }),
    );
    let catalog_id = doc.add_object(dictionary! {
        "Type" => "Catalog",
        "Pages" => Object::Reference(pages_id),
        "PageLayout" => "TwoPageLeft",
    });
    doc.trailer.set("Root", Object::Reference(catalog_id));
    doc
}

/// An unembedded standard-14 Type1 font dictionary — sufficient to be
/// referenced by name from a content stream; whether it's embedded is not
/// what the fixtures below that use this are testing.
fn font_dict(base_font: &str) -> Dictionary {
    dictionary! {
        "Type" => "Font",
        "Subtype" => "Type1",
        "BaseFont" => Object::Name(base_font.as_bytes().to_vec()),
    }
}

/// One page at the required bleed size whose `/Resources` is an *indirect*
/// reference to a dictionary holding a font, with content that actually
/// draws text through it — `openspec/changes/harden-pdf-correctness/
/// tasks.md` 9.1. Verified defect: nesting read only the page's *direct*
/// `/Resources`, so this exact shape silently dropped the font and the
/// glyphs it draws, normalizing to a blank page reported print-ready.
fn build_resources_indirect() -> Document {
    let mut doc = Document::with_version("1.7");
    let font_id = doc.add_object(font_dict("Helvetica"));
    let resources_id = doc.add_object(Object::Dictionary(
        dictionary! { "Font" => dictionary! { "F1" => Object::Reference(font_id) } },
    ));
    let content = doc.add_object(Stream::new(
        dictionary! {},
        b"BT /F1 24 Tf 36 300 Td (HELLO WORLD) Tj ET".to_vec(),
    ));
    let pages_id = doc.new_object_id();
    let page_id = doc.add_object(dictionary! {
        "Type" => "Page",
        "Parent" => Object::Reference(pages_id),
        "MediaBox" => mediabox_with_bleed(),
        "Resources" => Object::Reference(resources_id),
        "Contents" => Object::Reference(content),
    });
    doc.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! { "Type" => "Pages", "Kids" => vec![Object::Reference(page_id)], "Count" => 1 }),
    );
    let catalog_id =
        doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => Object::Reference(pages_id) });
    doc.trailer.set("Root", Object::Reference(catalog_id));
    doc
}

/// One page at the required bleed size with *no* `/Resources` of its own,
/// inheriting a font-bearing `/Resources` from its `Pages` ancestor, with
/// content that draws text through it — `tasks.md` 9.1's other half.
/// Verified defect: the same as [`build_resources_indirect`], but for the
/// inherited (rather than indirect) shape, which is what a page with no
/// per-page resources at all — also extremely common — looks like.
fn build_resources_inherited() -> Document {
    let mut doc = Document::with_version("1.7");
    let font_id = doc.add_object(font_dict("Helvetica"));
    let content = doc.add_object(Stream::new(
        dictionary! {},
        b"BT /F1 24 Tf 36 300 Td (HELLO WORLD) Tj ET".to_vec(),
    ));
    let pages_id = doc.new_object_id();
    let page_id = doc.add_object(dictionary! {
        "Type" => "Page",
        "Parent" => Object::Reference(pages_id),
        "MediaBox" => mediabox_with_bleed(),
        "Contents" => Object::Reference(content),
    });
    doc.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => vec![Object::Reference(page_id)],
            "Count" => 1,
            "Resources" => dictionary! { "Font" => dictionary! { "F1" => Object::Reference(font_id) } },
        }),
    );
    let catalog_id =
        doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => Object::Reference(pages_id) });
    doc.trailer.set("Root", Object::Reference(catalog_id));
    doc
}

/// A page whose own `/Font /F1` and whose `Pages` ancestor's `/Font /F1`
/// deliberately name *different* fonts under the same local name —
/// `tasks.md` 9.2. The page's own entry must win once resources are merged
/// (PDF inheritance: a page's own entry shadows an inherited one of the
/// same name), not the ancestor's.
fn build_resources_conflicting_key() -> Document {
    let mut doc = Document::with_version("1.7");
    let page_font_id = doc.add_object(font_dict("Helvetica"));
    let parent_font_id = doc.add_object(font_dict("Courier"));
    let content = doc.add_object(Stream::new(
        dictionary! {},
        b"BT /F1 24 Tf 36 300 Td (PAGE OWN FONT) Tj ET".to_vec(),
    ));
    let pages_id = doc.new_object_id();
    let page_id = doc.add_object(dictionary! {
        "Type" => "Page",
        "Parent" => Object::Reference(pages_id),
        "MediaBox" => mediabox_with_bleed(),
        "Resources" => dictionary! { "Font" => dictionary! { "F1" => Object::Reference(page_font_id) } },
        "Contents" => Object::Reference(content),
    });
    doc.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => vec![Object::Reference(page_id)],
            "Count" => 1,
            "Resources" => dictionary! { "Font" => dictionary! { "F1" => Object::Reference(parent_font_id) } },
        }),
    );
    let catalog_id =
        doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => Object::Reference(pages_id) });
    doc.trailer.set("Root", Object::Reference(catalog_id));
    doc
}

/// A correctly-bled, unrotated-box page carrying `/Rotate` as a *real*
/// number (`90.0`, not the integer `90`) — `tasks.md` 9.4. Verified defect:
/// `as_i64` only accepts `Object::Integer`, so this exact value read as `0`
/// (`unwrap_or(0)`), measuring the page's displayed 11x8.5in as its
/// unrotated 8.5x11in and baking no rotation at all.
fn build_rotate_real_number() -> Document {
    let mut doc = Document::with_version("1.7");
    let contents = empty_content(&mut doc);
    let pages_id = doc.new_object_id();
    let page_id = doc.add_object(dictionary! {
        "Type" => "Page",
        "Parent" => Object::Reference(pages_id),
        "MediaBox" => mediabox_with_bleed(),
        "Rotate" => Object::Real(90.0),
        "Contents" => contents,
    });
    doc.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! { "Type" => "Pages", "Kids" => vec![Object::Reference(page_id)], "Count" => 1 }),
    );
    let catalog_id =
        doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => Object::Reference(pages_id) });
    doc.trailer.set("Root", Object::Reference(catalog_id));
    doc
}

/// A correctly-bled, unrotated-box page carrying `/Rotate` as an *indirect*
/// reference to an integer object — `tasks.md` 9.4's other half. Same
/// verified defect as [`build_rotate_real_number`], for the indirect-
/// reference shape rather than the real-number shape.
fn build_rotate_indirect() -> Document {
    let mut doc = Document::with_version("1.7");
    let rotate_id = doc.add_object(Object::Integer(270));
    let contents = empty_content(&mut doc);
    let pages_id = doc.new_object_id();
    let page_id = doc.add_object(dictionary! {
        "Type" => "Page",
        "Parent" => Object::Reference(pages_id),
        "MediaBox" => mediabox_with_bleed(),
        "Rotate" => Object::Reference(rotate_id),
        "Contents" => contents,
    });
    doc.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! { "Type" => "Pages", "Kids" => vec![Object::Reference(page_id)], "Count" => 1 }),
    );
    let catalog_id =
        doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => Object::Reference(pages_id) });
    doc.trailer.set("Root", Object::Reference(catalog_id));
    doc
}

/// A 372-byte-class hostile shape: a page whose `/Parent` chain loops back
/// on itself, with no `/MediaBox`/`/Rotate` anywhere in the cycle to
/// terminate an unbounded walk early — `tasks.md` 9.5. Verified defect: the
/// inheritance walk had no visited-set, so a file shaped exactly like this
/// hung the tool forever.
fn build_parent_cycle() -> Document {
    let mut doc = Document::with_version("1.7");
    let a_id = doc.new_object_id();
    let b_id = doc.new_object_id();
    let page_id = doc.add_object(dictionary! {
        "Type" => "Page",
        "Parent" => Object::Reference(a_id),
    });
    doc.objects.insert(
        a_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Parent" => Object::Reference(b_id),
            "Kids" => vec![Object::Reference(page_id)],
            "Count" => 1,
        }),
    );
    doc.objects.insert(
        b_id,
        Object::Dictionary(dictionary! { "Type" => "Pages", "Parent" => Object::Reference(a_id) }),
    );
    let catalog_id =
        doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => Object::Reference(a_id) });
    doc.trailer.set("Root", Object::Reference(catalog_id));
    doc
}

/// A page whose `/MediaBox` is an indirect reference to an object that does
/// not exist in the file — `tasks.md` 9.6. Verified defect: an unresolvable
/// indirect box entry was silently skipped from every geometry check
/// instead of being named in a blocking finding.
fn build_mediabox_indirect_unresolved() -> Document {
    let mut doc = Document::with_version("1.7");
    let contents = empty_content(&mut doc);
    let dangling_ref = Object::Reference((9999, 0));
    let pages_id = doc.new_object_id();
    let page_id = doc.add_object(dictionary! {
        "Type" => "Page",
        "Parent" => Object::Reference(pages_id),
        "MediaBox" => dangling_ref,
        "Contents" => contents,
    });
    doc.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! { "Type" => "Pages", "Kids" => vec![Object::Reference(page_id)], "Count" => 1 }),
    );
    let catalog_id =
        doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => Object::Reference(pages_id) });
    doc.trailer.set("Root", Object::Reference(catalog_id));
    doc
}

/// A page whose `/MediaBox` resolves to zero width — `tasks.md` 9.7.
/// Verified defect: a degenerate box reached `FitMode::ScaleToBleed`'s scale
/// computation undivided, producing `inf`/`NaN` written straight into a
/// generated `cm` operator.
fn build_zero_dimension_page() -> Document {
    let mut doc = Document::with_version("1.7");
    let contents = empty_content(&mut doc);
    let pages_id = doc.new_object_id();
    let page_id = doc.add_object(dictionary! {
        "Type" => "Page",
        "Parent" => Object::Reference(pages_id),
        "MediaBox" => Object::Array(vec![0.into(), 0.into(), 0.into(), 648.into()]),
        "Contents" => contents,
    });
    doc.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! { "Type" => "Pages", "Kids" => vec![Object::Reference(page_id)], "Count" => 1 }),
    );
    let catalog_id =
        doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => Object::Reference(pages_id) });
    doc.trailer.set("Root", Object::Reference(catalog_id));
    doc
}

/// A document whose catalog `/Names` is an *indirect* reference to a tree
/// carrying a `/JavaScript` name tree with a real JavaScript action (its
/// `/JS` string carries a marker so a test can confirm the actual bytes are
/// gone from normalized output, not merely that a finding was raised) —
/// `tasks.md` 9.11.
fn build_names_indirect_javascript() -> Document {
    let mut doc = Document::with_version("1.7");
    let js_action_id = doc.add_object(dictionary! {
        "S" => "JavaScript",
        "JS" => Object::string_literal("app.alert('LULU_JS_MARKER');"),
    });
    let js_tree_id = doc.add_object(Object::Dictionary(
        dictionary! { "Names" => vec![Object::string_literal("Name1"), Object::Reference(js_action_id)] },
    ));
    let names_id = doc.add_object(Object::Dictionary(
        dictionary! { "JavaScript" => Object::Reference(js_tree_id) },
    ));
    let contents = empty_content(&mut doc);
    let pages_id = doc.new_object_id();
    let page_id = doc.add_object(dictionary! {
        "Type" => "Page",
        "Parent" => Object::Reference(pages_id),
        "MediaBox" => mediabox_with_bleed(),
        "Contents" => contents,
    });
    doc.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! { "Type" => "Pages", "Kids" => vec![Object::Reference(page_id)], "Count" => 1 }),
    );
    let catalog_id = doc.add_object(dictionary! {
        "Type" => "Catalog",
        "Pages" => Object::Reference(pages_id),
        "Names" => Object::Reference(names_id),
    });
    doc.trailer.set("Root", Object::Reference(catalog_id));
    doc
}

/// A low-resolution image drawn inside a form XObject whose own
/// `/Resources` is itself an *indirect* reference — nested at least one
/// level (page -> form -> image), combining task 1.2's fix (a form's own
/// indirect `/Resources`) with the CTM composition [`build_
/// nested_form_xobject_image`] already exercises — `tasks.md` 9.13. Same
/// 400x400px-drawn-at-4in -> 100 ppi geometry as that fixture.
fn build_nested_form_indirect_resources_image() -> Document {
    let mut doc = Document::with_version("1.7");
    let image_id = doc.add_object(Object::Stream(Stream::new(
        image_xobject(400, 400),
        vec![0u8; 4],
    )));
    let form_resources_id = doc.add_object(Object::Dictionary(
        dictionary! { "XObject" => dictionary! { "Im0" => Object::Reference(image_id) } },
    ));
    let form_id = doc.add_object(Object::Stream(Stream::new(
        dictionary! {
            "Type" => "XObject",
            "Subtype" => "Form",
            "Matrix" => Object::Array(vec![72.0.into(), 0.0.into(), 0.0.into(), 72.0.into(), 0.0.into(), 0.0.into()]),
            "Resources" => Object::Reference(form_resources_id),
        },
        b"/Im0 Do".to_vec(),
    )));
    let resources =
        dictionary! { "XObject" => dictionary! { "Fm0" => Object::Reference(form_id) } };
    let content = doc.add_object(Stream::new(
        dictionary! {},
        b"q 4 0 0 4 0 0 cm /Fm0 Do Q".to_vec(),
    ));
    let pages_id = doc.new_object_id();
    let page_id = doc.add_object(dictionary! {
        "Type" => "Page",
        "Parent" => Object::Reference(pages_id),
        "MediaBox" => mediabox_with_bleed(),
        "Resources" => resources,
        "Contents" => Object::Reference(content),
    });
    doc.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! { "Type" => "Pages", "Kids" => vec![Object::Reference(page_id)], "Count" => 1 }),
    );
    let catalog_id =
        doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => Object::Reference(pages_id) });
    doc.trailer.set("Root", Object::Reference(catalog_id));
    doc
}

/// A zero-page document: a valid `Pages` root with an empty `/Kids` and
/// `/Count 0` — `tasks.md` 9.10. Exercises the "supplied cover with no
/// pages" path end-to-end through the real CLI binary; the underlying
/// panic this guards against was already fixed and has in-memory coverage
/// (`crates/lulu-prep-cli/src/commands.rs`'s
/// `a_supplied_cover_with_no_pages_is_a_clean_error_not_a_panic`), but no
/// committed fixture exercised the real binary end-to-end.
fn build_zero_pages() -> Document {
    let mut doc = Document::with_version("1.7");
    let pages_id = doc.add_object(
        dictionary! { "Type" => "Pages", "Kids" => Vec::<Object>::new(), "Count" => 0 },
    );
    let catalog_id =
        doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => Object::Reference(pages_id) });
    doc.trailer.set("Root", Object::Reference(catalog_id));
    doc
}

/// A 100-page (a multiple of 4, so no padding is needed, and within the
/// 61-150 page band that gets a non-zero 0.125in gutter — needed so a
/// gutter-parity assertion has something to actually observe) interior
/// where the very first page object is referenced twice in `/Kids` — a
/// legal PDF trick for a repeated blank/divider page — `tasks.md` 9.14.
/// Verified defect: each `/Kids` occurrence of an aliased page was nested
/// (and gutter-shifted) *in place* on the same shared object, so transforms
/// compounded and only the last occurrence survived correctly.
fn build_aliased_page() -> Document {
    let mut doc = Document::with_version("1.7");
    let pages_id = doc.new_object_id();
    let mut kids = Vec::new();
    for _ in 0..99 {
        let contents = empty_content(&mut doc);
        let page_id = doc.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => Object::Reference(pages_id),
            "MediaBox" => mediabox_no_bleed(),
            "Contents" => contents,
        });
        kids.push(Object::Reference(page_id));
    }
    // 100th slot: the first page's object again, not a new one.
    kids.push(kids[0].clone());
    doc.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! { "Type" => "Pages", "Kids" => kids, "Count" => 100 }),
    );
    let catalog_id =
        doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => Object::Reference(pages_id) });
    doc.trailer.set("Root", Object::Reference(catalog_id));
    doc
}

fn write(doc: &mut Document, dir: &Path, name: &str) {
    let path = dir.join(name);
    let mut bytes = Vec::new();
    doc.save_to(&mut bytes)
        .unwrap_or_else(|e| panic!("failed to write {name}: {e}"));
    std::fs::write(&path, bytes)
        .unwrap_or_else(|e| panic!("failed to save {}: {e}", path.display()));
    println!("wrote {}", path.display());
}

fn main() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    std::fs::create_dir_all(&dir).unwrap();

    write(&mut build_no_bleed(), &dir, "no_bleed.pdf");
    write(&mut build_correct_bleed(), &dir, "correct_bleed.pdf");
    write(&mut build_mixed_sizes(), &dir, "mixed_sizes.pdf");
    write(&mut build_rotated(), &dir, "rotated.pdf");
    write(&mut build_unembedded_font(), &dir, "unembedded_font.pdf");
    write(
        &mut build_low_resolution_image(),
        &dir,
        "low_resolution_image.pdf",
    );
    write(
        &mut build_nested_form_xobject_image(),
        &dir,
        "nested_form_xobject_image.pdf",
    );
    write(
        &mut build_live_transparency(),
        &dir,
        "live_transparency.pdf",
    );
    write(
        &mut build_optional_content_groups(),
        &dir,
        "optional_content_groups.pdf",
    );
    write(&mut build_two_up_spread(), &dir, "two_up_spread.pdf");

    // --- harden-pdf-correctness test corpus (tasks.md group 9) ---
    write(
        &mut build_resources_indirect(),
        &dir,
        "resources_indirect.pdf",
    );
    write(
        &mut build_resources_inherited(),
        &dir,
        "resources_inherited.pdf",
    );
    write(
        &mut build_resources_conflicting_key(),
        &dir,
        "resources_conflicting_key.pdf",
    );
    write(
        &mut build_rotate_real_number(),
        &dir,
        "rotate_real_number.pdf",
    );
    write(&mut build_rotate_indirect(), &dir, "rotate_indirect.pdf");
    write(&mut build_parent_cycle(), &dir, "parent_cycle.pdf");
    write(
        &mut build_mediabox_indirect_unresolved(),
        &dir,
        "mediabox_indirect_unresolved.pdf",
    );
    write(
        &mut build_zero_dimension_page(),
        &dir,
        "zero_dimension_page.pdf",
    );
    write(
        &mut build_names_indirect_javascript(),
        &dir,
        "names_indirect_javascript.pdf",
    );
    write(
        &mut build_nested_form_indirect_resources_image(),
        &dir,
        "nested_form_indirect_resources_image.pdf",
    );
    write(&mut build_zero_pages(), &dir, "zero_pages.pdf");
    write(&mut build_aliased_page(), &dir, "aliased_page.pdf");

    println!("(empty_password/real_password encrypted fixtures are generated by tests/fixtures/generate.sh via qpdf, not this program)");
}
