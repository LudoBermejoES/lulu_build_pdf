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

    println!("(empty_password/real_password encrypted fixtures are generated by tests/fixtures/generate.sh via qpdf, not this program)");
}
