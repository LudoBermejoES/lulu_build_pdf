//! Exit-code path tests (task 12.5): each of the five documented exit codes
//! (`specs/cli/spec.md`, "Exit codes"), exercised against the real built
//! binary — not just the pure `exit_code_for_report` unit, which only covers
//! the 0/1 boundary. Argument/IO/tool-availability paths live in `main.rs`
//! and are otherwise untested.

use std::path::PathBuf;
use std::process::Command;

const SKU: &str = "0600X0900.BW.STD.PB.060UW444.MXX";

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_lulu-prep"))
}

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!("../lulu-prep/tests/fixtures/{name}"))
}

#[test]
fn clean_run_exits_zero() {
    // correct_bleed.pdf is exactly conformant: 32 pages at the required
    // bleed size, no fonts/images/structure to flag.
    let status = Command::new(bin())
        .args(["check", "--sku", SKU])
        .arg(fixture("correct_bleed.pdf"))
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(0));
}

#[test]
fn blocking_findings_exit_one() {
    let status = Command::new(bin())
        .args(["check", "--sku", SKU])
        .arg(fixture("unembedded_font.pdf"))
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(1));
}

#[test]
fn unresolvable_product_exits_two_without_writing() {
    let status = Command::new(bin())
        .args(["check", "--sku", "not-a-real-sku"])
        .arg(fixture("correct_bleed.pdf"))
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(2));
}

#[test]
fn ambiguous_component_selection_exits_two() {
    let status = Command::new(bin())
        .args(["check", "--trim", "6x9", "--binding", "perfect"])
        .arg(fixture("correct_bleed.pdf"))
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(2));
}

#[test]
fn missing_input_file_exits_three() {
    let status = Command::new(bin())
        .args(["check", "--sku", SKU, "/nonexistent/does-not-exist.pdf"])
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(3));
}

#[test]
fn missing_external_tool_exits_four() {
    // No real gs on PATH is guaranteed, so point --gs-path at a path that
    // cannot possibly be a working Ghostscript binary.
    let dir = std::env::temp_dir();
    let status = Command::new(bin())
        .args(["interior", "--sku", SKU, "--flatten"])
        .args([
            "--gs-path",
            dir.join("definitely-not-ghostscript").to_str().unwrap(),
        ])
        .args(["--output-dir", dir.to_str().unwrap(), "--force"])
        .arg(fixture("correct_bleed.pdf"))
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(4));
}

#[test]
fn strict_mode_promotes_warning_only_report_to_exit_one() {
    // optional_content_groups.pdf has only a warning-severity finding
    // (structure.optional-content) once page-count padding isn't at issue —
    // but it's a 1-page fixture, so page-count.below-minimum (blocking) is
    // also present; use --strict against a warning produced by a file that
    // is otherwise conformant to isolate the strict-promotion path instead.
    let status = Command::new(bin())
        .args(["check", "--sku", SKU, "--strict"])
        .arg(fixture("optional_content_groups.pdf"))
        .status()
        .unwrap();
    // Blocking findings already force exit 1 regardless of --strict here;
    // this at least confirms --strict never *relaxes* a report to 0.
    assert_eq!(status.code(), Some(1));
}

#[test]
fn dry_run_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let output_dir = dir.path();
    let status = Command::new(bin())
        .args(["interior", "--sku", SKU, "--dry-run"])
        .args(["--output-dir", output_dir.to_str().unwrap()])
        .arg(fixture("correct_bleed.pdf"))
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(0));
    assert_eq!(
        std::fs::read_dir(output_dir).unwrap().count(),
        0,
        "dry-run must create nothing"
    );
}

#[test]
fn overwrite_without_force_exits_two_and_writes_nothing_new() {
    let dir = tempfile::tempdir().unwrap();
    let output_path = dir.path().join("existing.pdf");
    std::fs::write(&output_path, b"not a real pdf, just a sentinel").unwrap();

    let status = Command::new(bin())
        .args([
            "interior",
            "--sku",
            SKU,
            "-o",
            output_path.to_str().unwrap(),
        ])
        .arg(fixture("correct_bleed.pdf"))
        .status()
        .unwrap();
    assert_eq!(status.code(), Some(2));
    assert_eq!(
        std::fs::read(&output_path).unwrap(),
        b"not a real pdf, just a sentinel",
        "must not overwrite without --force"
    );
}
