//! Scratch tool: assembles a back+spine+front wrap cover from a supplied
//! front-cover PDF page, back-cover PDF page, and a spine JPEG image,
//! using the crate's own verified cover geometry. Not part of the public
//! CLI — kept as an example for this one-off batch-prep task.
//!
//! The supplied front/back pages are plain trim-size pages (no bleed);
//! this pads each with `geometry::bleed()` on its outer edge and on top
//! and bottom (never on the fold-facing edge), filling the new margin
//! with the caller-supplied solid colour, so the padded page exactly
//! matches the destination panel size and `assemble_three_panel_cover`
//! reports no size-mismatch findings.
//!
//! Usage: cover_builder <sku> <interior.pdf> <front.pdf> <back.pdf> \
//!   <back_r> <back_g> <back_b> <front_r> <front_g> <front_b> \
//!   <spine.jpg> <spine_px_w> <spine_px_h> <out.pdf>

use lopdf::{dictionary, Document, Object, ObjectId, Stream};
use lulu_prep::catalog;
use lulu_prep::cover::{apply_cover_structural_rules, assemble_three_panel_cover, cover_geometry_from_interior};
use lulu_prep::pdf::own_box_rect;
use lulu_prep::units::Rect;
use std::env;
use std::fs;

fn single_page_id(doc: &Document) -> ObjectId {
    *doc.get_pages().values().next().expect("document has no pages")
}

/// Pads a single-page document out to exactly `dest_rect`'s size: any
/// shortfall in width goes entirely on the outer edge (left for the back
/// cover, right for the front cover — never on the fold-facing edge), and
/// any shortfall in height is split evenly between top and bottom. Some
/// supplied cover pages already carry partial bleed of their own (this
/// varies per book), so the padding needed is derived from the artwork's
/// own measured size rather than assumed to be exactly one bleed constant.
fn pad_to_panel_size(doc: &mut Document, page_id: ObjectId, dest_rect: Rect, outer_side: OuterSide, color: (u8, u8, u8)) {
    let own = own_box_rect(doc, page_id).expect("own box rect");
    let pad_w = (dest_rect.width().as_points() - own.width().as_points()).max(0.0);
    let pad_h = (dest_rect.height().as_points() - own.height().as_points()).max(0.0);

    let (new_x0, new_x1) = match outer_side {
        OuterSide::Left => (own.x0.as_points() - pad_w, own.x1.as_points()),
        OuterSide::Right => (own.x0.as_points(), own.x1.as_points() + pad_w),
    };
    let new_y0 = own.y0.as_points() - pad_h / 2.0;
    let new_y1 = own.y1.as_points() + pad_h / 2.0;

    let fill = format!(
        "q {:.4} {:.4} {:.4} rg {:.4} {:.4} {:.4} {:.4} re f Q\n",
        color.0 as f64 / 255.0,
        color.1 as f64 / 255.0,
        color.2 as f64 / 255.0,
        new_x0,
        new_y0,
        new_x1 - new_x0,
        new_y1 - new_y0,
    );
    let fill_id = doc.add_object(Stream::new(dictionary! {}, fill.into_bytes()));

    let page_dict = doc.get_dictionary_mut(page_id).expect("page dict");
    page_dict.set(
        "MediaBox",
        vec![new_x0.into(), new_y0.into(), new_x1.into(), new_y1.into()],
    );
    // own_box_rect's fallback chain is BleedBox -> CropBox -> MediaBox; clear
    // the two boxes that would otherwise shadow the MediaBox we just set.
    page_dict.remove(b"BleedBox");
    page_dict.remove(b"CropBox");
    page_dict.remove(b"TrimBox");
    page_dict.remove(b"ArtBox");

    let existing_contents = page_dict.get(b"Contents").expect("page has Contents").clone();
    let mut contents_array = vec![Object::Reference(fill_id)];
    match existing_contents {
        Object::Array(arr) => contents_array.extend(arr),
        other @ Object::Reference(_) => contents_array.push(other),
        _ => panic!("unexpected Contents type"),
    }
    let page_dict = doc.get_dictionary_mut(page_id).expect("page dict");
    page_dict.set("Contents", Object::Array(contents_array));
}

#[derive(Clone, Copy)]
enum OuterSide {
    Left,
    Right,
}

fn build_spine_doc(width_pt: f64, height_pt: f64, jpeg_bytes: Vec<u8>, px_w: u32, px_h: u32) -> Document {
    let mut doc = Document::with_version("1.7");
    let image_dict = dictionary! {
        "Type" => "XObject",
        "Subtype" => "Image",
        "Width" => px_w as i64,
        "Height" => px_h as i64,
        "ColorSpace" => "DeviceRGB",
        "BitsPerComponent" => 8,
        "Filter" => "DCTDecode",
    };
    let image_id = doc.add_object(Stream::new(image_dict, jpeg_bytes));
    let content = format!("q {:.4} 0 0 {:.4} 0 0 cm /Im0 Do Q", width_pt, height_pt);
    let content_id = doc.add_object(Stream::new(dictionary! {}, content.into_bytes()));
    let resources = dictionary! { "XObject" => dictionary! { "Im0" => Object::Reference(image_id) } };
    let pages_id = doc.new_object_id();
    let page_id = doc.add_object(dictionary! {
        "Type" => "Page",
        "Parent" => Object::Reference(pages_id),
        "MediaBox" => vec![0.into(), 0.into(), width_pt.into(), height_pt.into()],
        "Contents" => Object::Reference(content_id),
        "Resources" => resources,
    });
    let pages = dictionary! { "Type" => "Pages", "Kids" => vec![Object::Reference(page_id)], "Count" => 1 };
    doc.objects.insert(pages_id, Object::Dictionary(pages));
    let catalog_id =
        doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => Object::Reference(pages_id) });
    doc.trailer.set("Root", Object::Reference(catalog_id));
    doc
}

fn main() {
    let args: Vec<String> = env::args().collect();
    if args.len() != 15 {
        eprintln!(
            "usage: cover_builder <sku> <interior.pdf> <front.pdf> <back.pdf> <back_r> <back_g> <back_b> <front_r> <front_g> <front_b> <spine.jpg> <spine_px_w> <spine_px_h> <out.pdf>"
        );
        std::process::exit(2);
    }
    let sku = &args[1];
    let interior_path = &args[2];
    let front_path = &args[3];
    let back_path = &args[4];
    let back_color = (
        args[5].parse::<u8>().unwrap(),
        args[6].parse::<u8>().unwrap(),
        args[7].parse::<u8>().unwrap(),
    );
    let front_color = (
        args[8].parse::<u8>().unwrap(),
        args[9].parse::<u8>().unwrap(),
        args[10].parse::<u8>().unwrap(),
    );
    let spine_jpg_path = &args[11];
    let spine_px_w: u32 = args[12].parse().expect("spine_px_w");
    let spine_px_h: u32 = args[13].parse().expect("spine_px_h");
    let out_path = &args[14];

    let entry = catalog::lookup(sku).unwrap_or_else(|e| panic!("sku {sku}: {e}"));
    let interior_doc = Document::load(interior_path).expect("load interior");
    let geo = cover_geometry_from_interior(entry, &interior_doc).expect("cover geometry");

    let mut front_doc = Document::load(front_path).expect("load front");
    let mut back_doc = Document::load(back_path).expect("load back");
    let front_page = single_page_id(&front_doc);
    let back_page = single_page_id(&back_doc);

    pad_to_panel_size(&mut back_doc, back_page, geo.back_panel, OuterSide::Left, back_color);
    pad_to_panel_size(&mut front_doc, front_page, geo.front_panel, OuterSide::Right, front_color);

    let spine_bytes = fs::read(spine_jpg_path).expect("read spine jpg");
    let spine_doc = build_spine_doc(
        geo.spine.width().as_points(),
        geo.spine.height().as_points(),
        spine_bytes,
        spine_px_w,
        spine_px_h,
    );
    let spine_page = single_page_id(&spine_doc);

    let (mut assembled, findings) = assemble_three_panel_cover(
        (&back_doc, back_page),
        (&spine_doc, spine_page),
        (&front_doc, front_page),
        &geo,
    )
    .expect("assemble three panel cover");

    for f in &findings {
        eprintln!("finding: {f:?}");
    }

    apply_cover_structural_rules(&mut assembled).expect("structural rules");

    let mut out_bytes = Vec::new();
    assembled
        .save_to(&mut out_bytes)
        .expect("serialize assembled cover");
    fs::write(out_path, &out_bytes).expect("write assembled cover");

    let report = lulu_prep::preflight::preflight_cover(&out_bytes, entry, geo.canvas);
    let blocking: Vec<_> = report
        .findings
        .iter()
        .filter(|f| f.severity == lulu_prep::report::Severity::Blocking)
        .collect();
    for f in &report.findings {
        eprintln!("preflight_cover: {:?} {} - {}", f.severity, f.code, f.message);
    }
    println!(
        "wrote {out_path} canvas={:.2}x{:.2}pt spine_width={:.4}pt page_count={} blocking={}",
        geo.canvas.width.as_points(),
        geo.canvas.height.as_points(),
        geo.spine.width().as_points(),
        geo.page_count,
        blocking.len(),
    );
    if !blocking.is_empty() {
        std::process::exit(1);
    }
}
