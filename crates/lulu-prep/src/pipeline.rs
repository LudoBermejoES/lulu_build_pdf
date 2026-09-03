//! Ties normalization and the optional external tools together into one
//! fixed stage order — repair, then normalization (geometry, gutter,
//! padding, sanitation, run as [`crate::normalize::normalize_interior`]'s
//! single combined step), then the optional Ghostscript flatten/colour
//! stage last — with a timed stage log and detected-tool list folded into
//! the run [`crate::report::Report`]. Delegated stages run last so nothing
//! external can change which rectangle is the trim.
//!
//! Spread splitting has no implementation yet (see the interior-normalization
//! spec's deferred scope) and is not represented as a stage here.

use crate::catalog::CatalogEntry;
use crate::external_tools::{self, GhostscriptFlattenOptions, GHOSTSCRIPT, QPDF};
use crate::normalize::NormalizeOptions;
use crate::report::StageLogEntry;
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// Where to look for the optional external binaries, and whether to run
/// the (off-by-default) Ghostscript stage.
#[derive(Debug, Clone, Default)]
pub struct PipelineOptions {
    pub qpdf_path: Option<PathBuf>,
    pub gs_path: Option<PathBuf>,
    /// `None` (the default) skips the Ghostscript stage entirely. `Some`
    /// requests it — and if Ghostscript isn't available, the run fails
    /// explicitly (see [`PipelineError::MissingTool`]) rather than silently
    /// skipping a stage the caller asked for.
    pub flatten: Option<GhostscriptFlattenOptions>,
    pub detection_timeout: Duration,
}

impl PipelineOptions {
    pub fn new() -> Self {
        PipelineOptions {
            detection_timeout: Duration::from_secs(5),
            ..Default::default()
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum PipelineError {
    #[error(transparent)]
    Load(#[from] crate::pdf::LoadError),
    #[error(transparent)]
    Normalize(#[from] crate::normalize::NormalizeInteriorError),
    #[error("the '{stage}' stage was requested but {tool} is not available: {reason}")]
    MissingTool {
        stage: &'static str,
        tool: &'static str,
        reason: String,
    },
    #[error(transparent)]
    Ghostscript(#[from] external_tools::GhostscriptError),
}

#[derive(Debug)]
pub struct PipelineOutcome {
    pub output_bytes: Vec<u8>,
    pub report: crate::report::Report,
}

/// Runs the full fixed pipeline over an interior PDF: detects qpdf and
/// Ghostscript (always, for the report, regardless of whether either is
/// used), repairs the input first if native parsing fails and qpdf is
/// available, normalizes, and — only if requested — flattens via
/// Ghostscript, asserting afterward that geometry was preserved.
pub fn run_pipeline(
    bytes: &[u8],
    product: &CatalogEntry,
    normalize_options: NormalizeOptions,
    pipeline_options: &PipelineOptions,
) -> Result<PipelineOutcome, PipelineError> {
    let mut stages = Vec::new();
    let mut detected_tools = Vec::new();

    let qpdf_outcome = external_tools::detect(
        &QPDF,
        pipeline_options.qpdf_path.as_deref(),
        pipeline_options.detection_timeout,
    );
    detected_tools.push(external_tools::to_report_entry(&QPDF, &qpdf_outcome));
    let gs_outcome = external_tools::detect(
        &GHOSTSCRIPT,
        pipeline_options.gs_path.as_deref(),
        pipeline_options.detection_timeout,
    );
    detected_tools.push(external_tools::to_report_entry(&GHOSTSCRIPT, &gs_outcome));

    let qpdf_path = match &qpdf_outcome {
        external_tools::DetectionOutcome::Available { path, .. } => Some(path.as_path()),
        _ => None,
    };

    let repair_start = Instant::now();
    let (repaired_bytes, was_repaired) = external_tools::repair_bytes_if_needed(bytes, qpdf_path)?;
    stages.push(StageLogEntry {
        name: "repair".to_string(),
        duration_ms: repair_start.elapsed().as_millis() as u64,
    });
    let _ = was_repaired; // recorded implicitly: normalize's own report reflects the (now-parseable) content either way

    let normalize_start = Instant::now();
    let normalize_outcome =
        crate::normalize::normalize_interior(&repaired_bytes, product, normalize_options)?;
    stages.push(StageLogEntry {
        name: "normalize".to_string(),
        duration_ms: normalize_start.elapsed().as_millis() as u64,
    });

    let mut report = normalize_outcome.report;
    let mut output_bytes = normalize_outcome.output_bytes;

    if let Some(flatten_options) = &pipeline_options.flatten {
        let external_tools::DetectionOutcome::Available { path: gs_path, .. } = &gs_outcome else {
            return Err(PipelineError::MissingTool {
                stage: "flatten",
                tool: "Ghostscript",
                reason: gs_outcome.unavailable_reason().unwrap_or_default(),
            });
        };

        let before_doc = crate::pdf::load_from_bytes(&output_bytes)?;
        let flatten_start = Instant::now();
        let (flattened_bytes, gs_args) =
            external_tools::flatten_with_ghostscript(gs_path, &output_bytes, flatten_options)?;
        let after_doc = crate::pdf::load_from_bytes(&flattened_bytes)?;
        external_tools::assert_geometry_preserved(&before_doc, &after_doc)?;

        stages.push(StageLogEntry {
            name: "flatten".to_string(),
            duration_ms: flatten_start.elapsed().as_millis() as u64,
        });
        report.findings.push(crate::report::Finding::new(
            "pipeline.ghostscript-invoked",
            crate::report::Severity::Info,
            format!("Ghostscript flatten stage ran: {}", gs_args.join(" ")),
        ));
        output_bytes = flattened_bytes;
    }

    report.detected_tools = detected_tools;
    report.stages = stages;

    Ok(PipelineOutcome {
        output_bytes,
        report,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use lopdf::dictionary;

    fn sku() -> &'static CatalogEntry {
        crate::catalog::lookup("0600X0900.BW.STD.PB.060UW444.MXX").unwrap()
    }

    fn doc_with_n_unbled_pages(n: usize) -> Vec<u8> {
        let mut doc = lopdf::Document::with_version("1.7");
        let pages_id = doc.new_object_id();
        let mut kids = Vec::new();
        for _ in 0..n {
            let content_id = doc.add_object(lopdf::Stream::new(dictionary! {}, Vec::new()));
            let page_id = doc.add_object(dictionary! {
                "Type" => "Page",
                "Parent" => lopdf::Object::Reference(pages_id),
                "MediaBox" => lopdf::Object::Array(vec![0.into(), 0.into(), 432.into(), 648.into()]),
                "Contents" => lopdf::Object::Reference(content_id),
            });
            kids.push(lopdf::Object::Reference(page_id));
        }
        let pages = dictionary! { "Type" => "Pages", "Kids" => kids, "Count" => n as i64 };
        doc.objects
            .insert(pages_id, lopdf::Object::Dictionary(pages));
        let catalog_id = doc.add_object(
            dictionary! { "Type" => "Catalog", "Pages" => lopdf::Object::Reference(pages_id) },
        );
        doc.trailer
            .set("Root", lopdf::Object::Reference(catalog_id));
        let mut buf = Vec::new();
        doc.save_to(&mut buf).unwrap();
        buf
    }

    #[test]
    fn pipeline_completes_and_reports_detected_tools() {
        let input = doc_with_n_unbled_pages(32);
        let outcome = run_pipeline(
            &input,
            sku(),
            NormalizeOptions::default(),
            &PipelineOptions::new(),
        )
        .unwrap();
        assert!(
            outcome.report.is_print_ready(),
            "{}",
            outcome.report.to_text()
        );
        // Both tools are always probed and recorded, whether or not present.
        assert_eq!(outcome.report.detected_tools.len(), 2);
        assert!(outcome
            .report
            .detected_tools
            .iter()
            .any(|t| t.name == "qpdf"));
        assert!(outcome.report.detected_tools.iter().any(|t| t.name == "gs"));
        assert!(outcome.report.stages.iter().any(|s| s.name == "repair"));
        assert!(outcome.report.stages.iter().any(|s| s.name == "normalize"));
    }

    #[test]
    fn requesting_flatten_without_ghostscript_fails_explicitly() {
        let input = doc_with_n_unbled_pages(32);
        let mut options = PipelineOptions::new();
        // Force gs "unavailable" regardless of the machine's real state, so
        // this test is deterministic everywhere.
        options.gs_path = Some(PathBuf::from("/definitely/not/a/real/gs/binary"));
        options.flatten = Some(GhostscriptFlattenOptions::default());

        let err = run_pipeline(&input, sku(), NormalizeOptions::default(), &options).unwrap_err();
        assert!(matches!(
            err,
            PipelineError::MissingTool {
                stage: "flatten",
                tool: "Ghostscript",
                ..
            }
        ));
    }

    #[test]
    fn pipeline_completes_fully_with_both_tools_forced_unavailable() {
        let input = doc_with_n_unbled_pages(32);
        let mut options = PipelineOptions::new();
        options.qpdf_path = Some(PathBuf::from("/definitely/not/a/real/qpdf/binary"));
        options.gs_path = Some(PathBuf::from("/definitely/not/a/real/gs/binary"));
        // flatten stays None (off by default): absence of a requested stage
        // must not block the run — only an explicitly *requested* stage
        // whose tool is missing does that (see the test above).

        let outcome = run_pipeline(&input, sku(), NormalizeOptions::default(), &options).unwrap();
        assert!(
            outcome.report.is_print_ready(),
            "{}",
            outcome.report.to_text()
        );
        for tool in &outcome.report.detected_tools {
            assert!(
                tool.path.is_none(),
                "{} should be reported unavailable",
                tool.name
            );
        }
    }

    #[test]
    fn pipeline_repairs_a_broken_input_when_qpdf_is_available() {
        let qpdf_path = match external_tools::detect(&QPDF, None, Duration::from_secs(5)) {
            external_tools::DetectionOutcome::Available { path, .. } => path,
            _ => {
                eprintln!("qpdf not installed; skipping");
                return;
            }
        };

        // Build a healthy file, then corrupt its startxref at the byte level
        // (save_to's output is a binary xref stream, not a plain-text
        // trailer, so this can't be done through a UTF-8 String).
        let good = doc_with_n_unbled_pages(32);
        let marker = b"startxref\n";
        let marker_pos = good
            .windows(marker.len())
            .position(|w| w == marker)
            .expect("save_to output must contain a startxref marker");
        let mut broken = good[..marker_pos + marker.len()].to_vec();
        broken.extend_from_slice(b"999999\n");
        broken.extend_from_slice(&good[marker_pos + marker.len()..]);
        assert!(
            crate::pdf::load_from_bytes(&broken).is_err(),
            "fixture must actually be broken"
        );

        let mut options = PipelineOptions::new();
        options.qpdf_path = Some(qpdf_path);
        let outcome = run_pipeline(&broken, sku(), NormalizeOptions::default(), &options).unwrap();
        assert!(
            outcome.report.is_print_ready(),
            "{}",
            outcome.report.to_text()
        );
        assert_eq!(outcome.report.page_count, Some(32));
    }
}
