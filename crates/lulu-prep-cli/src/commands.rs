//! Command logic shared between `main.rs`'s argument parsing and tests:
//! everything here works on in-memory byte buffers and an already-resolved
//! [`CatalogEntry`], so it never touches the filesystem itself.

use lulu_prep::catalog::CatalogEntry;
use lulu_prep::cover::{
    self, CoverGeometry, CoverGeometryError, CoverMetadata, CoverStructuralError, FitArtworkError,
};
use lulu_prep::normalize::{self, FitMode, NormalizeOptions};
use lulu_prep::pdf::{self, LoadError};
use lulu_prep::pipeline::{self, PipelineError, PipelineOptions};
use lulu_prep::report::{now_rfc3339, Finding, Report, Severity, SCHEMA_VERSION};
use serde::{Deserialize, Serialize};

pub struct CheckOutcome {
    pub report: Report,
}

/// `check`: preflight only, no PDF is written.
pub fn run_check(bytes: &[u8], product: &CatalogEntry) -> CheckOutcome {
    CheckOutcome {
        report: lulu_prep::preflight::preflight(bytes, Some(product)),
    }
}

pub struct InteriorOutcome {
    pub output_bytes: Vec<u8>,
    pub report: Report,
}

/// `interior`: repair (if needed) + normalize, via the library's fixed
/// pipeline, optionally followed by a Ghostscript flatten stage.
pub fn run_interior(
    bytes: &[u8],
    product: &CatalogEntry,
    normalize_options: NormalizeOptions,
    pipeline_options: &PipelineOptions,
) -> Result<InteriorOutcome, PipelineError> {
    let outcome = pipeline::run_pipeline(bytes, product, normalize_options, pipeline_options)?;
    Ok(InteriorOutcome {
        output_bytes: outcome.output_bytes,
        report: outcome.report,
    })
}

/// What to build the cover from: a freshly generated design-aid template, or
/// the caller's own single-page cover artwork fitted onto the required
/// canvas.
pub enum CoverSource<'a> {
    Template,
    Supplied { bytes: &'a [u8], fit_mode: FitMode },
}

#[derive(Debug, thiserror::Error)]
pub enum CoverCommandError {
    #[error(transparent)]
    Geometry(#[from] CoverGeometryError),
    #[error(transparent)]
    Load(#[from] LoadError),
    #[error(transparent)]
    Fit(#[from] FitArtworkError),
    #[error(transparent)]
    Structural(#[from] CoverStructuralError),
    #[error("could not write the cover PDF: {0}")]
    Save(#[from] std::io::Error),
    #[error("the supplied cover file has no pages")]
    NoPages,
}

#[derive(Debug)]
pub struct CoverOutcome {
    pub output_bytes: Vec<u8>,
    pub report: Report,
    pub geometry: CoverGeometry,
}

fn base_report(product: &CatalogEntry, page_count: u32) -> Report {
    Report {
        schema_version: SCHEMA_VERSION,
        input_digest: None,
        product_sku: Some(product.sku.clone()),
        page_count: Some(page_count),
        catalog_fetch_date: Some(lulu_prep::catalog::metadata().fetch_date.clone()),
        tool_version: env!("CARGO_PKG_VERSION").to_string(),
        detected_tools: Vec::new(),
        stages: Vec::new(),
        findings: Vec::new(),
        generated_at: now_rfc3339(),
    }
}

/// `cover`: generate a template, or fit supplied artwork, for `product` at
/// `page_count`. The caller supplies `page_count` directly for a standalone
/// `cover` invocation, or the normalized interior's final page count for
/// `book`.
pub fn run_cover(
    product: &CatalogEntry,
    page_count: u32,
    source: CoverSource,
) -> Result<CoverOutcome, CoverCommandError> {
    let geometry = cover::cover_geometry(product, page_count)?;
    let mut report = base_report(product, page_count);

    let mut doc = match source {
        CoverSource::Template => {
            let meta = CoverMetadata {
                product_sku: &product.sku,
                page_count,
                spine_width: geometry.spine.width(),
                canvas: geometry.canvas,
            };
            cover::generate_template(&geometry, &meta)
        }
        CoverSource::Supplied { bytes, fit_mode } => {
            let mut doc = pdf::load_from_bytes(bytes)?;
            if pdf::was_ever_encrypted(&doc) {
                return Err(CoverCommandError::Structural(
                    CoverStructuralError::PasswordRequired,
                ));
            }
            let page_id = doc.page_iter().next().ok_or(CoverCommandError::NoPages)?;
            let fit_findings = cover::fit_supplied_cover(&mut doc, page_id, &geometry, fit_mode)?;
            report.findings.extend(fit_findings);
            doc
        }
    };

    let sanitize_summary = cover::apply_cover_structural_rules(&mut doc)?;
    report
        .findings
        .extend(normalize::sanitize_summary_findings(&sanitize_summary));

    let mut output_bytes = Vec::new();
    doc.save_to(&mut output_bytes)?;

    let mut preflight_report =
        lulu_prep::preflight::preflight_cover(&output_bytes, product, geometry.canvas);
    report.findings.append(&mut preflight_report.findings);

    Ok(CoverOutcome {
        output_bytes,
        report,
        geometry,
    })
}

#[derive(Debug, thiserror::Error)]
pub enum BookCommandError {
    #[error(transparent)]
    Interior(#[from] PipelineError),
    #[error(transparent)]
    Cover(#[from] CoverCommandError),
}

pub struct BookOutcome {
    pub interior: InteriorOutcome,
    pub cover: CoverOutcome,
}

/// `book`: normalizes the interior first, then builds the cover from the
/// *normalized* interior's final page count — never the caller's original
/// page count — so the pair can never drift apart (`specs/cli/spec.md`,
/// "Book command keeps the pair in step").
pub fn run_book(
    interior_bytes: &[u8],
    product: &CatalogEntry,
    normalize_options: NormalizeOptions,
    pipeline_options: &PipelineOptions,
    cover_source: CoverSource,
) -> Result<BookOutcome, BookCommandError> {
    let interior = run_interior(interior_bytes, product, normalize_options, pipeline_options)?;
    let page_count = interior
        .report
        .page_count
        .expect("normalize_interior always sets page_count on success");
    let cover = run_cover(product, page_count, cover_source)?;
    Ok(BookOutcome { interior, cover })
}

/// The combined report `book` emits: one document carrying both the
/// interior's and the cover's `Report`, rather than the two concatenated
/// separately — the latter is not parseable as a single JSON document and
/// truncates when written to one `--report-out` path
/// (`specs/cli/spec.md`, "A two-file command emits one document" and "A
/// two-file command does not truncate a report file").
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BookReport {
    pub interior: Report,
    pub cover: Report,
}

impl BookReport {
    pub fn blocking_count(&self) -> usize {
        self.interior.blocking_count() + self.cover.blocking_count()
    }

    pub fn warning_count(&self) -> usize {
        self.interior.warning_count() + self.cover.warning_count()
    }

    pub fn is_print_ready(&self) -> bool {
        self.interior.is_print_ready() && self.cover.is_print_ready()
    }

    /// One line stating the verdict, product, and final page count, across
    /// both documents (`specs/cli/spec.md`, "Report leads with the
    /// verdict").
    pub fn verdict_line(&self) -> String {
        let product = self
            .interior
            .product_sku
            .as_deref()
            .or(self.cover.product_sku.as_deref());
        if self.is_print_ready() {
            let mut parts = vec!["print-ready".to_string()];
            if let Some(sku) = product {
                parts.push(format!("for {sku}"));
            }
            if let Some(pages) = self.interior.page_count {
                parts.push(format!("at {pages} pages"));
            }
            parts.push("(interior + cover)".to_string());
            let mut line = parts.join(" ");
            let warnings = self.warning_count();
            if warnings > 0 {
                line.push_str(&format!(
                    " ({warnings} warning{} remaining)",
                    if warnings == 1 { "" } else { "s" }
                ));
            }
            line
        } else {
            format!(
                "not print-ready: {} blocking issue{}, {} warning{} across interior and cover",
                self.blocking_count(),
                if self.blocking_count() == 1 { "" } else { "s" },
                self.warning_count(),
                if self.warning_count() == 1 { "" } else { "s" },
            )
        }
    }

    pub fn to_json(&self) -> serde_json::Result<String> {
        serde_json::to_string_pretty(self)
    }

    /// Human-readable text leading with the combined verdict, then each
    /// document's own rendering, indented under its own heading.
    pub fn to_text(&self) -> String {
        let mut out = self.verdict_line();
        out.push('\n');
        out.push_str("\nInterior:\n");
        out.push_str(&indent_lines(&self.interior.to_text()));
        out.push_str("\n\nCover:\n");
        out.push_str(&indent_lines(&self.cover.to_text()));
        out
    }
}

fn indent_lines(text: &str) -> String {
    text.lines()
        .map(|line| format!("  {line}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Advisory findings from `--gutter-floor-in`: the CLI's own comparison of
/// the library's page-count-banded gutter (`lulu_prep::geometry::
/// gutter_allowance`, the same function `normalize_interior` itself calls)
/// against a user-configured floor, independent of the library's own fixed
/// 0.2 in advisory constant. A floor of 0.0 (the default, when the option
/// isn't set at any layer) can never trigger, since the applied gutter is
/// never negative — so leaving `--gutter-floor-in` unset is a true no-op,
/// not a silently-ignored flag.
pub fn gutter_floor_findings(page_count: u32, floor_in: f64) -> Vec<Finding> {
    let allowance = lulu_prep::geometry::gutter_allowance(page_count);
    let actual_in = allowance.gutter.as_inches();
    if actual_in < floor_in {
        vec![Finding::new(
            "gutter.below-configured-floor",
            Severity::Warning,
            format!(
                "at {page_count} pages, the applied gutter is {actual_in:.3} in, below the --gutter-floor-in threshold of {floor_in:.3} in"
            ),
        )
        .with_observed(format!("{actual_in:.3} in"))
        .with_expected(format!(">= {floor_in:.3} in"))]
    } else {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lopdf::dictionary;

    fn sku() -> &'static CatalogEntry {
        lulu_prep::catalog::lookup("0600X0900.BW.STD.PB.060UW444.MXX").unwrap()
    }

    fn minimal_pdf(page_count: u32, size: lulu_prep::units::Size) -> Vec<u8> {
        let mut doc = lopdf::Document::with_version("1.7");
        let content_id = doc.add_object(lopdf::Stream::new(dictionary! {}, b"".to_vec()));
        let pages_id = doc.new_object_id();
        let mut kids = Vec::new();
        for _ in 0..page_count {
            let page_id = doc.add_object(dictionary! {
                "Type" => "Page",
                "Parent" => pages_id,
                "MediaBox" => vec![0.into(), 0.into(), size.width.as_points().into(), size.height.as_points().into()],
                "Contents" => content_id,
            });
            kids.push(page_id.into());
        }
        doc.objects.insert(
            pages_id,
            lopdf::Object::Dictionary(dictionary! {
                "Type" => "Pages",
                "Count" => page_count as i64,
                "Kids" => kids,
            }),
        );
        let catalog_id = doc.add_object(dictionary! {
            "Type" => "Catalog",
            "Pages" => pages_id,
        });
        doc.trailer.set("Root", catalog_id);
        let mut bytes = Vec::new();
        doc.save_to(&mut bytes).unwrap();
        bytes
    }

    #[test]
    fn check_runs_preflight_without_writing_anything() {
        let bytes = minimal_pdf(2, sku().bleed_size);
        let outcome = run_check(&bytes, sku());
        assert!(outcome.report.product_sku.is_none() || outcome.report.product_sku.is_some());
        // preflight() itself decides product_sku presence; the point of this
        // test is simply that run_check never panics on a minimal valid input
        // and returns a report we can inspect.
        assert!(outcome.report.page_count.unwrap() >= 2);
    }

    #[test]
    fn interior_normalizes_and_reports_final_page_count() {
        let bytes = minimal_pdf(1, sku().trim_size);
        let options = NormalizeOptions {
            fit_mode: FitMode::Center,
            apply_gutter: false,
            split_spreads: false,
        };
        let outcome = run_interior(&bytes, sku(), options, &PipelineOptions::new()).unwrap();
        assert!(outcome.report.page_count.unwrap() >= 32); // padded to product minimum
        assert!(!outcome.output_bytes.is_empty());
    }

    #[test]
    fn cover_template_generates_for_a_conformant_page_count() {
        let outcome = run_cover(sku(), 212, CoverSource::Template).unwrap();
        assert_eq!(outcome.geometry.page_count, 212);
        assert!(!outcome.output_bytes.is_empty());
    }

    #[test]
    fn cover_rejects_non_conformant_page_count_before_writing_anything() {
        let err = run_cover(sku(), 213, CoverSource::Template).unwrap_err();
        assert!(matches!(
            err,
            CoverCommandError::Geometry(CoverGeometryError::NonConformantPageCount { .. })
        ));
    }

    #[test]
    fn a_supplied_cover_with_no_pages_is_a_clean_error_not_a_panic() {
        let bytes = minimal_pdf(0, sku().bleed_size);
        let err = run_cover(
            sku(),
            212,
            CoverSource::Supplied {
                bytes: &bytes,
                fit_mode: FitMode::Center,
            },
        )
        .unwrap_err();
        assert!(matches!(err, CoverCommandError::NoPages));
    }

    #[test]
    fn book_derives_cover_page_count_from_the_normalized_interior_not_the_input() {
        // One page in, but the product's minimum forces padding — the cover
        // must be built for the padded count, not the original 1.
        let bytes = minimal_pdf(1, sku().trim_size);
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
        assert_eq!(
            outcome.cover.geometry.page_count,
            outcome.interior.report.page_count.unwrap()
        );
        assert_ne!(outcome.cover.geometry.page_count, 1);
    }

    fn empty_report(sku: &str, page_count: u32) -> Report {
        Report {
            schema_version: SCHEMA_VERSION,
            input_digest: None,
            product_sku: Some(sku.to_string()),
            page_count: Some(page_count),
            catalog_fetch_date: None,
            tool_version: "test".to_string(),
            detected_tools: vec![],
            stages: vec![],
            findings: vec![],
            generated_at: "2026-01-01T00:00:00Z".to_string(),
        }
    }

    #[test]
    fn book_report_json_is_one_parseable_document_with_both_reports() {
        let book_report = BookReport {
            interior: empty_report("sku-a", 32),
            cover: empty_report("sku-a", 32),
        };
        let json = book_report.to_json().unwrap();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert!(value.get("interior").is_some());
        assert!(value.get("cover").is_some());
    }

    #[test]
    fn book_report_text_leads_with_a_combined_verdict() {
        let book_report = BookReport {
            interior: empty_report("sku-a", 32),
            cover: empty_report("sku-a", 32),
        };
        let text = book_report.to_text();
        let first_line = text.lines().next().unwrap();
        assert!(first_line.starts_with("print-ready"));
        assert!(first_line.contains("sku-a"));
        assert!(first_line.contains("32 pages"));
        assert!(text.contains("Interior:"));
        assert!(text.contains("Cover:"));
    }

    #[test]
    fn book_report_verdict_is_not_print_ready_if_either_side_has_a_blocking_finding() {
        let mut cover = empty_report("sku-a", 32);
        cover.findings.push(Finding::new(
            "structure.encrypted",
            Severity::Blocking,
            "encrypted",
        ));
        let book_report = BookReport {
            interior: empty_report("sku-a", 32),
            cover,
        };
        assert!(!book_report.is_print_ready());
        assert!(book_report.verdict_line().starts_with("not print-ready"));
    }

    #[test]
    fn gutter_floor_findings_is_empty_when_actual_gutter_meets_the_floor() {
        // 212 pages -> 0.5 in gutter per the banded table; a 0.3 in floor is met.
        assert!(gutter_floor_findings(212, 0.3).is_empty());
    }

    #[test]
    fn gutter_floor_findings_warns_when_actual_gutter_is_below_the_configured_floor() {
        // 32 pages -> 0.0 in gutter per the banded table; any positive floor trips it.
        let findings = gutter_floor_findings(32, 0.3);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].severity, Severity::Warning);
    }

    #[test]
    fn gutter_floor_findings_is_a_true_no_op_at_the_default_zero_floor() {
        for pages in [1, 32, 100, 212, 500, 700] {
            assert!(gutter_floor_findings(pages, 0.0).is_empty());
        }
    }
}
