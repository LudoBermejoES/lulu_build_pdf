//! Snapshot tests over the preflight JSON report for every committed
//! fixture in `tests/fixtures/` (task 12.2). Each snapshot pins the exact
//! finding set a fixture produces today; a change in behavior shows up as a
//! snapshot diff to review, not a silent drift. Volatile fields (timestamp,
//! stage durations, tool versions) are stripped by `Report::normalized_for_diff`
//! before comparison.
//!
//! Regenerate fixtures with `cargo run -p lulu-prep --example generate_fixtures`.
//! Update snapshots by re-running with `INSTA_UPDATE=always` (no `cargo-insta`
//! binary is required; the runtime honors that env var on its own).

use lulu_prep::catalog::CatalogEntry;
use lulu_prep::preflight::preflight;

fn sku() -> &'static CatalogEntry {
    lulu_prep::catalog::lookup("0600X0900.BW.STD.PB.060UW444.MXX").unwrap()
}

fn fixture_bytes(name: &str) -> Vec<u8> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name);
    std::fs::read(&path)
        .unwrap_or_else(|e| panic!("could not read fixture {}: {e}", path.display()))
}

macro_rules! fixture_snapshot_test {
    ($test_name:ident, $file:literal) => {
        #[test]
        fn $test_name() {
            let bytes = fixture_bytes($file);
            let report = preflight(&bytes, Some(sku()));
            insta::assert_json_snapshot!(stringify!($test_name), report.normalized_for_diff());
        }
    };
}

fixture_snapshot_test!(no_bleed, "no_bleed.pdf");
fixture_snapshot_test!(correct_bleed, "correct_bleed.pdf");
fixture_snapshot_test!(mixed_sizes, "mixed_sizes.pdf");
fixture_snapshot_test!(rotated, "rotated.pdf");
fixture_snapshot_test!(unembedded_font, "unembedded_font.pdf");
fixture_snapshot_test!(low_resolution_image, "low_resolution_image.pdf");
fixture_snapshot_test!(nested_form_xobject_image, "nested_form_xobject_image.pdf");
fixture_snapshot_test!(live_transparency, "live_transparency.pdf");
fixture_snapshot_test!(optional_content_groups, "optional_content_groups.pdf");
fixture_snapshot_test!(two_up_spread, "two_up_spread.pdf");

#[test]
fn empty_password_encrypted() {
    let bytes = fixture_bytes("encrypted_empty_password.pdf");
    let report = preflight(&bytes, Some(sku()));
    insta::assert_json_snapshot!("empty_password_encrypted", report.normalized_for_diff());
}
