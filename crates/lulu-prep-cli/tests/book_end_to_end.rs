//! End-to-end `book` test (task 12.4): a raw, non-conformant fixture PDF
//! through `commands::run_book`, all the way to interior plus cover,
//! asserting the cover canvas is exactly what an independent
//! `cover::cover_geometry` call computes for the interior's *final* (padded)
//! page count — not the fixture's original, non-conformant count.

use lulu_prep::cover::cover_geometry;
use lulu_prep::normalize::{FitMode, NormalizeOptions};
use lulu_prep::pipeline::PipelineOptions;
use lulu_prep_cli::commands::{run_book, CoverSource};

fn sku() -> &'static lulu_prep::catalog::CatalogEntry {
    lulu_prep::catalog::lookup("0600X0900.BW.STD.PB.060UW444.MXX").unwrap()
}

fn fixture_bytes(name: &str) -> Vec<u8> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(format!("../lulu-prep/tests/fixtures/{name}"));
    std::fs::read(&path)
        .unwrap_or_else(|e| panic!("could not read fixture {}: {e}", path.display()))
}

#[test]
fn book_takes_a_raw_one_page_fixture_to_a_matching_interior_and_cover() {
    // no_bleed.pdf is a single page, no bleed, nowhere near the product's
    // 32-page minimum — book must normalize it to a conformant page count,
    // then build the cover for *that* count, not the original 1.
    let bytes = fixture_bytes("no_bleed.pdf");
    let options = NormalizeOptions {
        fit_mode: FitMode::Center,
        apply_gutter: false,
        split_spreads: false,
    };

    let outcome = run_book(
        &bytes,
        sku(),
        options,
        &PipelineOptions::new(),
        CoverSource::Template,
    )
    .unwrap();

    let final_page_count = outcome.interior.report.page_count.unwrap();
    assert_ne!(
        final_page_count, 1,
        "the raw fixture's 1 page must have been padded to the product minimum"
    );
    assert_eq!(final_page_count, 32);

    let independent_geometry = cover_geometry(sku(), final_page_count).unwrap();
    assert_eq!(outcome.cover.geometry, independent_geometry);
    assert_eq!(outcome.cover.geometry.page_count, final_page_count);

    // Both output PDFs must themselves be well-formed and readable.
    assert!(lulu_prep::pdf::load_from_bytes(&outcome.interior.output_bytes).is_ok());
    assert!(lulu_prep::pdf::load_from_bytes(&outcome.cover.output_bytes).is_ok());

    // The interior itself must actually carry the final page count.
    let interior_doc = lulu_prep::pdf::load_from_bytes(&outcome.interior.output_bytes).unwrap();
    assert_eq!(interior_doc.get_pages().len() as u32, final_page_count);
}
