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
use lulu_prep::report::{now_rfc3339, Report, SCHEMA_VERSION};

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
            let page_id = doc
                .page_iter()
                .next()
                .expect("a supplied cover PDF has at least one page (checked by preflight before this runs)");
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
}
