//! End-to-end tests for the CLI-contract fixes in `openspec/changes/
//! harden-pdf-correctness/tasks.md` section 7 ("CLI contracts"), each
//! reproducing one documented gap against the real built binary rather than
//! only a pure unit — these are exactly the kind of bugs that only show up
//! end-to-end (`specs/cli/spec.md`).

use std::path::PathBuf;
use std::process::Command;

const SKU: &str = "0600X0900.BW.STD.PB.060UW444.MXX";
const SKU_GLOSS: &str = "0600X0900.BW.STD.PB.060UW444.GXX";
const LEGACY_SKU: &str = "0600X0900BWSTDPB060UW444MXX";

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_lulu-prep"))
}

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!("../lulu-prep/tests/fixtures/{name}"))
}

// --- 7.1: `book` emits one combined, parseable report -----------------

#[test]
fn book_json_on_stdout_is_one_parseable_document_with_interior_and_cover() {
    let dir = tempfile::tempdir().unwrap();
    let output = Command::new(bin())
        .args(["book", "--sku", SKU, "--json"])
        .args(["--output-dir", dir.path().to_str().unwrap()])
        .arg(fixture("correct_bleed.pdf"))
        .output()
        .unwrap();
    assert!(output.status.success() || output.status.code() == Some(1));

    let stdout = String::from_utf8(output.stdout).expect("stdout must be UTF-8");
    let value: serde_json::Value =
        serde_json::from_str(&stdout).expect("stdout must be exactly one parseable JSON document");
    assert!(
        value.get("interior").is_some(),
        "combined document must carry the interior report: {value}"
    );
    assert!(
        value.get("cover").is_some(),
        "combined document must carry the cover report: {value}"
    );
}

#[test]
fn book_report_out_is_not_truncated_by_the_second_write() {
    let dir = tempfile::tempdir().unwrap();
    let report_path = dir.path().join("report.json");
    let status = Command::new(bin())
        .args(["book", "--sku", SKU, "--json"])
        .args(["--report-out", report_path.to_str().unwrap()])
        .args(["--output-dir", dir.path().to_str().unwrap()])
        .arg(fixture("correct_bleed.pdf"))
        .status()
        .unwrap();
    assert!(status.code() == Some(0) || status.code() == Some(1));

    let text = std::fs::read_to_string(&report_path).unwrap();
    let value: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert!(value.get("interior").is_some());
    assert!(value.get("cover").is_some());
}

#[test]
fn book_text_report_leads_with_a_verdict_and_covers_both_documents() {
    let dir = tempfile::tempdir().unwrap();
    let output = Command::new(bin())
        .args(["book", "--sku", SKU])
        .args(["--output-dir", dir.path().to_str().unwrap()])
        .arg(fixture("correct_bleed.pdf"))
        .output()
        .unwrap();
    let stdout = String::from_utf8(output.stdout).unwrap();
    let first_line = stdout.lines().next().unwrap();
    assert!(
        first_line.starts_with("print-ready") || first_line.starts_with("not print-ready"),
        "first line must be the verdict: {first_line}"
    );
    assert!(stdout.contains("Interior:"));
    assert!(stdout.contains("Cover:"));
}

// --- 7.2: write_output's specific exit code is propagated --------------

#[test]
fn write_failure_to_a_nonexistent_directory_exits_three_not_two() {
    // The overwrite check itself passes (nothing exists at that path), but
    // the actual write fails because the parent directory doesn't exist —
    // this must surface as an I/O failure (3), not invalid usage (2).
    let dir = tempfile::tempdir().unwrap();
    let missing_output_dir = dir.path().join("does/not/exist");
    let status = Command::new(bin())
        .args(["interior", "--sku", SKU])
        .args(["--output-dir", missing_output_dir.to_str().unwrap()])
        .arg(fixture("correct_bleed.pdf"))
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(3));
}

// --- 7.3: unparseable config values are exit 2, never a silent fallback ---

#[test]
fn misspelled_fit_mode_env_var_exits_two_naming_accepted_values() {
    let dir = tempfile::tempdir().unwrap();
    let output = Command::new(bin())
        .env("LULU_PREP_FIT_MODE", "scaletobleed")
        .args(["interior", "--sku", SKU, "--dry-run"])
        .args(["--output-dir", dir.path().to_str().unwrap()])
        .arg(fixture("correct_bleed.pdf"))
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("LULU_PREP_FIT_MODE"), "{stderr}");
    assert!(stderr.contains("center"), "{stderr}");
    assert!(stderr.contains("scale-to-bleed"), "{stderr}");
    assert!(stderr.contains("stretch-margins"), "{stderr}");
}

#[test]
fn unparseable_strict_env_var_exits_two_rather_than_disabling_strict() {
    let dir = tempfile::tempdir().unwrap();
    let output = Command::new(bin())
        .env("LULU_PREP_STRICT", "yes-please")
        .args(["check", "--sku", SKU])
        .args(["--output-dir", dir.path().to_str().unwrap()])
        .arg(fixture("correct_bleed.pdf"))
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
}

#[test]
fn print_config_reports_config_errors_too() {
    let output = Command::new(bin())
        .env("LULU_PREP_FIT_MODE", "sideways")
        .args(["--print-config"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
}

// --- 7.4: --gutter-floor-in is actually read -----------------------------

#[test]
fn gutter_floor_in_produces_a_warning_when_the_applied_gutter_is_below_it() {
    // `check` only preflights and never normalizes, so it never applies (or
    // reports on) a gutter; `interior` is where a page-count-derived gutter
    // is actually computed and where --gutter-floor-in has something to
    // compare against.
    let dir = tempfile::tempdir().unwrap();
    let output = Command::new(bin())
        .args(["interior", "--sku", SKU, "--gutter-floor-in", "0.3"])
        .args(["--output-dir", dir.path().to_str().unwrap(), "--force"])
        .arg(fixture("correct_bleed.pdf"))
        .output()
        .unwrap();
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("gutter.below-configured-floor"), "{stdout}");
}

// --- 7.6: full SKU (not file_stem) names a template cover's default path --

#[test]
fn matte_and_gloss_cover_templates_do_not_collide_on_the_default_filename() {
    let dir = tempfile::tempdir().unwrap();

    let status_matte = Command::new(bin())
        .args(["cover", "--sku", SKU, "--pages", "32"])
        .args(["--output-dir", dir.path().to_str().unwrap()])
        .status()
        .unwrap();
    assert!(status_matte.code() == Some(0) || status_matte.code() == Some(1));

    let status_gloss = Command::new(bin())
        .args(["cover", "--sku", SKU_GLOSS, "--pages", "32"])
        .args(["--output-dir", dir.path().to_str().unwrap()])
        .status()
        .unwrap();
    assert!(status_gloss.code() == Some(0) || status_gloss.code() == Some(1));

    assert!(dir.path().join(format!("{SKU}-cover.pdf")).exists());
    assert!(dir.path().join(format!("{SKU_GLOSS}-cover.pdf")).exists());
}

// --- 7.7: --doc-id is validated bytewise, and partial pairs are reported --

#[test]
fn doc_id_with_a_multi_byte_character_is_rejected_cleanly_not_a_panic() {
    // 30 ASCII hex chars + one 2-byte UTF-8 character = 32 bytes but 31
    // actual characters — slicing by byte offset used to panic mid-char.
    let dir = tempfile::tempdir().unwrap();
    let doc_id = "0123456789abcdef0123456789abcd\u{e9}"; // 31 chars, 32 bytes
    assert_eq!(doc_id.len(), 32);
    let output = Command::new(bin())
        .args([
            "interior",
            "--sku",
            SKU,
            "--doc-id",
            doc_id,
            "--creation-date",
            "D:20260101000000Z",
        ])
        .args(["--output-dir", dir.path().to_str().unwrap(), "--dry-run"])
        .arg(fixture("correct_bleed.pdf"))
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("32"), "{stderr}");
    assert!(!stderr.is_empty());
}

#[test]
fn doc_id_without_creation_date_is_reported_not_silently_ignored() {
    let dir = tempfile::tempdir().unwrap();
    let output = Command::new(bin())
        .args([
            "interior",
            "--sku",
            SKU,
            "--doc-id",
            "0123456789abcdef0123456789abcdef",
        ])
        .args(["--output-dir", dir.path().to_str().unwrap(), "--dry-run"])
        .arg(fixture("correct_bleed.pdf"))
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("--creation-date") && stderr.contains("--doc-id"),
        "{stderr}"
    );
}

// --- 7.9: `products` validates --trim/--binding like every other command --

#[test]
fn products_rejects_an_unparseable_trim_rather_than_listing_the_whole_catalog() {
    let output = Command::new(bin())
        .args(["products", "--trim", "6*9"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("--trim"), "{stderr}");
}

#[test]
fn products_rejects_an_unparseable_binding() {
    let output = Command::new(bin())
        .args(["products", "--binding", "not-a-binding"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
}

// --- 7.10: legacy-SKU deprecation notice is surfaced ---------------------

#[test]
fn legacy_sku_form_surfaces_a_deprecation_warning() {
    let output = Command::new(bin())
        .args(["spine", "--sku", LEGACY_SKU, "--pages", "32"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0));
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("2027-02-01"), "{stderr}");
    assert!(stderr.contains(SKU), "{stderr}");
}

#[test]
fn dotted_sku_form_never_surfaces_a_deprecation_warning() {
    let output = Command::new(bin())
        .args(["spine", "--sku", SKU, "--pages", "32"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(0));
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(!stderr.contains("2027-02-01"), "{stderr}");
}

// --- Colour output never contains ANSI escapes, --no-color or not --------

#[test]
fn text_report_never_contains_ansi_escapes() {
    for no_color in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let mut cmd = Command::new(bin());
        cmd.args(["check", "--sku", SKU]);
        if no_color {
            cmd.arg("--no-color");
        }
        cmd.args(["--output-dir", dir.path().to_str().unwrap()]);
        cmd.arg(fixture("correct_bleed.pdf"));
        let output = cmd.output().unwrap();
        let stdout = String::from_utf8(output.stdout).unwrap();
        assert!(!stdout.contains('\u{1b}'), "ANSI escape found: {stdout:?}");
    }
}
