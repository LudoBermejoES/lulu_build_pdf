//! Property tests (task 12.3): whatever the input page size, rotation, and
//! page count, `normalize_interior`'s output — whenever it succeeds — always
//! has uniform pages at the product's required size, a page count the
//! product's rules call conformant, and the exact box entries
//! `normalize::page_boxes` computes.

use lopdf::{dictionary, Object};
use lulu_prep::catalog::CatalogEntry;
use lulu_prep::geometry::{required_page_size, PageCountRules};
use lulu_prep::normalize::{normalize_interior, page_boxes, FitMode, NormalizeOptions};
use lulu_prep::units::Length;
use proptest::prelude::*;

fn sku() -> &'static CatalogEntry {
    lulu_prep::catalog::lookup("0600X0900.BW.STD.PB.060UW444.MXX").unwrap()
}

/// A document of `page_count` pages, each `width`x`height` points with
/// `rotation` degrees of `/Rotate`, and a trivial empty content stream.
fn build_doc(page_count: u32, width: f64, height: f64, rotation: i64) -> Vec<u8> {
    let mut doc = lopdf::Document::with_version("1.7");
    let pages_id = doc.new_object_id();
    let mut kids = Vec::new();
    for _ in 0..page_count {
        let contents = doc.add_object(lopdf::Stream::new(dictionary! {}, Vec::new()));
        let page_id = doc.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => Object::Reference(pages_id),
            "MediaBox" => Object::Array(vec![0.into(), 0.into(), width.into(), height.into()]),
            "Rotate" => rotation,
            "Contents" => Object::Reference(contents),
        });
        kids.push(Object::Reference(page_id));
    }
    doc.objects.insert(
        pages_id,
        Object::Dictionary(
            dictionary! { "Type" => "Pages", "Kids" => kids, "Count" => page_count as i64 },
        ),
    );
    let catalog_id =
        doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => Object::Reference(pages_id) });
    doc.trailer.set("Root", Object::Reference(catalog_id));
    let mut bytes = Vec::new();
    doc.save_to(&mut bytes).unwrap();
    bytes
}

const BOX_TOLERANCE: Length = Length::from_points(0.01);

proptest! {
    #![proptest_config(ProptestConfig { cases: 64, ..ProptestConfig::default() })]

    #[test]
    fn normalized_output_is_always_uniform_conformant_and_correctly_boxed(
        page_count in 1u32..24,
        width in 100.0f64..1000.0,
        height in 100.0f64..1000.0,
        rotation in prop_oneof![Just(0i64), Just(90), Just(180), Just(270)],
    ) {
        let bytes = build_doc(page_count, width, height, rotation);
        let options = NormalizeOptions { fit_mode: FitMode::Center, apply_gutter: false, split_spreads: false };

        // Above the product's maximum is a legitimate refusal, not a property
        // violation — only inputs that produce an Ok output are checked.
        let Ok(outcome) = normalize_interior(&bytes, sku(), options) else {
            return Ok(());
        };

        let doc = lulu_prep::pdf::load_from_bytes(&outcome.output_bytes).unwrap();
        let page_ids: Vec<_> = doc.page_iter().collect();
        let required = required_page_size(sku().trim_size);
        let rules = PageCountRules::from_catalog_entry(sku());
        let boxes = page_boxes(required);

        // Conformant page count.
        prop_assert_eq!(rules.next_conformant(page_ids.len() as u32), Ok(page_ids.len() as u32));
        prop_assert_eq!(outcome.final_page_count, page_ids.len() as u32);

        for &page_id in &page_ids {
            // Uniform, correctly-sized page (rotation baked in, so the
            // *effective* size always equals the required size).
            let size = lulu_prep::pdf::effective_page_size(&doc, page_id).unwrap();
            prop_assert!(size.approx_eq(required, BOX_TOLERANCE), "page size {size:?} != required {required:?}");

            // No leftover /Rotate — baking means the raw MediaBox is already
            // in reading orientation.
            let dict = doc.get_dictionary(page_id).unwrap();
            prop_assert_eq!(dict.get(b"Rotate").ok().and_then(|o| o.as_i64().ok()).unwrap_or(0), 0);

            // Exact box entries page_boxes() computes.
            let media_box = lulu_prep::pdf::own_box_rect(&doc, page_id).unwrap();
            prop_assert!((media_box.width() - boxes.media_bleed_box.width()).abs() <= BOX_TOLERANCE);
            prop_assert!((media_box.height() - boxes.media_bleed_box.height()).abs() <= BOX_TOLERANCE);

            let trim_box_array = dict.get(b"TrimBox").unwrap().as_array().unwrap();
            let trim_w = trim_box_array[2].as_float().unwrap() - trim_box_array[0].as_float().unwrap();
            let trim_h = trim_box_array[3].as_float().unwrap() - trim_box_array[1].as_float().unwrap();
            prop_assert!((trim_w as f64 - boxes.trim_art_box.width().as_points()).abs() < 0.01);
            prop_assert!((trim_h as f64 - boxes.trim_art_box.height().as_points()).abs() < 0.01);
        }
    }
}
