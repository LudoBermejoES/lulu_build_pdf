//! End-to-end CLI tests for `openspec/changes/harden-pdf-correctness/
//! tasks.md` section 9 that specifically need the real built binary rather
//! than a library-level call — each reproduces one documented gap's
//! verification scenario against `env!("CARGO_BIN_EXE_lulu-prep")`, the
//! same pattern `exit_codes.rs`/`cli_contracts.rs` already use.

use std::path::PathBuf;
use std::process::Command;

const SKU: &str = "0600X0900.BW.STD.PB.060UW444.MXX";
const CASE_WRAP_SKU: &str = "0600X0900.BW.STD.CW.060UW444.MXX";

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_lulu-prep"))
}

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!("../lulu-prep/tests/fixtures/{name}"))
}

fn trim_box(doc: &lopdf::Document) -> Vec<f64> {
    let page_id = doc.page_iter().next().expect("one page");
    let page = doc.get_dictionary(page_id).unwrap();
    page.get(b"TrimBox")
        .unwrap()
        .as_array()
        .unwrap()
        .iter()
        .map(|o| o.as_float().unwrap() as f64)
        .collect()
}

// --- 9.3: `check` and `interior` agree on unembedded_font.pdf: both
// blocking, both exit 1, through the real binary (not only
// normalize::tests::check_and_interior_agree_on_the_unembedded_font_fixture,
// which calls `preflight`/`normalize_interior` directly). ---

#[test]
fn check_and_interior_both_report_blocking_and_exit_one_on_the_unembedded_font_fixture() {
    let check_status = Command::new(bin())
        .args(["check", "--sku", SKU])
        .arg(fixture("unembedded_font.pdf"))
        .status()
        .unwrap();
    assert_eq!(check_status.code(), Some(1));

    let dir = tempfile::tempdir().unwrap();
    let interior_output = Command::new(bin())
        .args(["interior", "--sku", SKU, "--force"])
        .args(["--output-dir", dir.path().to_str().unwrap()])
        .arg(fixture("unembedded_font.pdf"))
        .output()
        .unwrap();
    assert_eq!(
        interior_output.status.code(),
        Some(1),
        "`interior` must not report print-ready when `check` reports blocking"
    );
    let stdout = String::from_utf8(interior_output.stdout).unwrap();
    assert!(stdout.contains("fonts.not-embedded"), "{stdout}");
}

// --- 9.7: a zero-dimension page must be refused (a blocking finding, exit
// 1), and the written output must never carry a NaN/inf byte sequence. ---

#[test]
fn zero_dimension_page_fixture_is_refused_and_writes_no_nan_or_inf() {
    let dir = tempfile::tempdir().unwrap();
    let status = Command::new(bin())
        .args(["interior", "--sku", SKU, "--force"])
        .args(["--output-dir", dir.path().to_str().unwrap()])
        .arg(fixture("zero_dimension_page.pdf"))
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(1));

    let output_path = dir.path().join("zero_dimension_page-interior.pdf");
    let bytes = std::fs::read(&output_path)
        .unwrap_or_else(|e| panic!("expected output at {}: {e}", output_path.display()));
    let text = String::from_utf8_lossy(&bytes);
    assert!(!text.contains("NaN"), "output must not contain NaN");
    assert!(
        !text.contains(" inf ") && !text.contains(" -inf "),
        "output must not contain an inf cm operand"
    );
    assert!(
        lopdf::Document::load_mem(&bytes).is_ok(),
        "the output must still be a well-formed PDF"
    );
}

// --- 9.8: case-wrap cover trim geometry, through the real `cover` command
// end-to-end, inspecting the actual written PDF's TrimBox bytes (cover.rs's
// own test module already pins this exact 212-page/63pt-inset case via
// `generate_template` directly; this is the same numbers through the real
// CLI binary and file it actually writes). ---

#[test]
fn case_wrap_cover_command_writes_a_trim_box_inset_by_the_board_overhang() {
    let dir = tempfile::tempdir().unwrap();
    let status = Command::new(bin())
        .args(["cover", "--sku", CASE_WRAP_SKU, "--pages", "212"])
        .args(["--output-dir", dir.path().to_str().unwrap()])
        .status()
        .unwrap();
    assert!(status.code() == Some(0) || status.code() == Some(1));

    let output_path = dir.path().join(format!("{CASE_WRAP_SKU}-cover.pdf"));
    let bytes = std::fs::read(&output_path)
        .unwrap_or_else(|e| panic!("expected output at {}: {e}", output_path.display()));
    let doc = lopdf::Document::load_mem(&bytes).unwrap();
    let trim = trim_box(&doc);
    let close = |a: f64, b: f64| (a - b).abs() < 0.6;
    assert!(close(trim[0], 63.0), "{trim:?}");
    assert!(close(trim[1], 63.0), "{trim:?}");
    assert!(close(trim[2], 981.0), "{trim:?}");
    assert!(close(trim[3], 711.0), "{trim:?}");
}

// --- 9.10: a zero-page supplied cover must be a clean error (exit 3), not
// a panic, through the real `cover` command
// (`commands.rs`'s own `a_supplied_cover_with_no_pages_is_a_clean_error_not_a_panic`
// already covers this at the `run_cover` library level; this is the same
// scenario through the real CLI binary end-to-end, with a committed
// fixture). ---

#[test]
fn a_zero_page_supplied_cover_is_a_clean_error_not_a_panic_through_the_real_binary() {
    let dir = tempfile::tempdir().unwrap();
    let output = Command::new(bin())
        .args(["cover", "--sku", SKU, "--pages", "32"])
        .args(["--supplied"])
        .arg(fixture("zero_pages.pdf"))
        .args(["--output-dir", dir.path().to_str().unwrap()])
        .output()
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(3),
        "must exit cleanly (I/O-or-parse), not crash: {output:?}"
    );
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(!stderr.is_empty());
}

// --- 9.14: a page aliased twice in /Kids must be nested independently per
// occurrence through the real `interior` command
// (`normalize.rs`'s own `aliased_page_is_nested_independently_per_occurrence_end_to_end`
// already covers this at the `normalize_interior` library level, in-memory;
// this is the same scenario against a committed fixture through the real
// CLI binary end-to-end). ---

#[test]
fn aliased_page_fixture_is_nested_independently_per_occurrence_through_the_real_binary() {
    let dir = tempfile::tempdir().unwrap();
    let status = Command::new(bin())
        .args(["interior", "--sku", SKU, "--gutter", "--force"])
        .args(["--output-dir", dir.path().to_str().unwrap()])
        .arg(fixture("aliased_page.pdf"))
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(0));

    let output_path = dir.path().join("aliased_page-interior.pdf");
    let bytes = std::fs::read(&output_path).unwrap();
    let doc = lopdf::Document::load_mem(&bytes).unwrap();
    assert_eq!(
        doc.get_pages().len(),
        100,
        "the fixture is already a conformant page count; nothing should be padded"
    );

    let page1 = *doc.get_pages().get(&1).unwrap();
    let page100 = *doc.get_pages().get(&100).unwrap();
    assert_ne!(
        page1, page100,
        "the aliased page's two occurrences must be distinct output page objects"
    );

    let cm_x = |id: lopdf::ObjectId| -> f64 {
        let page = doc.get_dictionary(id).unwrap();
        let content_ref = page.get(b"Contents").unwrap().as_reference().unwrap();
        let lopdf::Object::Stream(stream) = doc.get_object(content_ref).unwrap() else {
            panic!("expected a stream")
        };
        let bytes = stream.get_plain_content().unwrap();
        let content = lopdf::content::Content::decode(&bytes).unwrap();
        let cm_op = content
            .operations
            .iter()
            .find(|op| op.operator == "cm")
            .unwrap();
        cm_op.operands[4].as_float().unwrap() as f64
    };
    // Both occurrences are nested from the *same* original page object, but
    // at different final positions (1: odd/recto, 100: even/verso), so each
    // must get its own, independent gutter parity: recto shifts toward +x
    // by the gutter, verso toward -x by the same amount, on top of the
    // shared 9pt centering offset (450-432 bleed / 2) — 18pt and 0pt
    // respectively for this fixture's 0.125in (9pt) gutter. If the two
    // occurrences shared a compounded transform instead of each getting its
    // own, these would not differ by exactly twice the gutter.
    let x1 = cm_x(page1);
    let x100 = cm_x(page100);
    assert!((x1 - 18.0).abs() < 1e-6, "page 1 (recto): {x1}");
    assert!((x100 - 0.0).abs() < 1e-6, "page 100 (verso): {x100}");
}
