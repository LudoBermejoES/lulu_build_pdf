//! Ties normalization and the optional external tools together into one
//! fixed stage order — repair, then normalization (geometry, gutter,
//! padding, sanitation, and — when requested — spread splitting, run as
//! [`crate::normalize::normalize_interior`]'s single combined step), then
//! the optional Ghostscript flatten/colour stage last — with a timed stage
//! log and detected-tool list folded into the run [`crate::report::Report`].
//! Delegated stages run last so nothing external can change which rectangle
//! is the trim.
//!
//! Spread splitting is opt-in (see [`crate::normalize::NormalizeOptions::split_spreads`],
//! wired up by `normalize_interior` and exposed by the CLI's
//! `--split-spreads`) and is not represented as its own stage here — it
//! happens inside the single "normalize" stage this module logs.

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
    /// Bound on the `--version` capability-detection probe for each tool.
    /// Short, since a well-behaved binary answers almost instantly.
    pub detection_timeout: Duration,
    /// Bound on the qpdf repair and Ghostscript flatten invocations
    /// themselves — deliberately much longer than `detection_timeout`,
    /// since these stages do real work over the whole document (rebuilding
    /// a cross-reference table, or rewriting every page's content stream)
    /// rather than answering a `--version` probe. A hostile or pathological
    /// input can still make either tool hang; this bounds how long the run
    /// waits before treating that as a stage failure instead of hanging
    /// forever.
    pub external_tool_timeout: Duration,
}

impl PipelineOptions {
    pub fn new() -> Self {
        PipelineOptions {
            detection_timeout: Duration::from_secs(5),
            external_tool_timeout: Duration::from_secs(120),
            ..Default::default()
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum PipelineError {
    #[error(transparent)]
    Load(#[from] crate::pdf::LoadError),
    #[error(transparent)]
    Repair(#[from] external_tools::RepairOrLoadError),
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
    let (repaired_bytes, was_repaired) = external_tools::repair_bytes_if_needed(
        bytes,
        qpdf_path,
        pipeline_options.external_tool_timeout,
    )?;
    stages.push(StageLogEntry {
        name: "repair".to_string(),
        duration_ms: repair_start.elapsed().as_millis() as u64,
    });

    let normalize_start = Instant::now();
    let normalize_outcome =
        crate::normalize::normalize_interior(&repaired_bytes, product, normalize_options)?;
    stages.push(StageLogEntry {
        name: "normalize".to_string(),
        duration_ms: normalize_start.elapsed().as_millis() as u64,
    });

    let mut report = normalize_outcome.report;
    let mut output_bytes = normalize_outcome.output_bytes;

    // A reader of the report must be able to tell whether the findings
    // below describe the bytes the caller supplied or bytes qpdf rewrote
    // first — recorded as a finding (rather than silently discarded, as
    // `let _ = was_repaired;` used to do) since every other fact this
    // report states about the file lives in `findings`, not a side channel.
    if was_repaired {
        report.findings.push(crate::report::Finding::new(
            "pipeline.input-repaired",
            crate::report::Severity::Info,
            "the supplied file could not be parsed as-is and was structurally repaired by qpdf before analysis; every finding below describes the repaired bytes, not the original file".to_string(),
        ));
    }

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
        let (flattened_bytes, gs_args) = external_tools::flatten_with_ghostscript(
            gs_path,
            &output_bytes,
            flatten_options,
            pipeline_options.external_tool_timeout,
        )?;
        let after_doc = crate::pdf::load_from_bytes(&flattened_bytes)?;
        external_tools::assert_geometry_preserved(&before_doc, &after_doc)?;

        stages.push(StageLogEntry {
            name: "flatten".to_string(),
            duration_ms: flatten_start.elapsed().as_millis() as u64,
        });

        // Ghostscript can change exactly what preflight checks — it embeds
        // fonts, flattens transparency and optional content, and can
        // convert colour — so the findings preflight raised against
        // normalize's (pre-flatten) output no longer describe the file
        // that will actually ship. Keep this run's own process findings
        // (this module's and normalize's own, identified by their `code`
        // prefix — see `is_own_process_finding`) and replace every
        // preflight-derived finding with a fresh preflight of the
        // flattened bytes, so the report's conformance verdict describes
        // what was actually written.
        report.findings.retain(|f| is_own_process_finding(&f.code));
        let post_flatten_report = crate::preflight::preflight(&flattened_bytes, Some(product));
        report.findings.extend(post_flatten_report.findings);

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

/// Whether `code` names a finding this pipeline (or `normalize_interior`,
/// whose findings this module folds in verbatim) raised about its own
/// process — as opposed to a finding [`crate::preflight::preflight`] raised
/// by inspecting file content. By convention every finding either module
/// adds about what it *did* uses a `"normalize."` or `"pipeline."` code
/// prefix (the one exception, the gutter advisory, predates that
/// convention and is named explicitly below); every finding preflight adds
/// about what it *found* does not. This lets the flatten stage drop stale
/// pre-flatten preflight findings and replace them with a fresh preflight
/// of the bytes actually written, without discarding the process log.
fn is_own_process_finding(code: &str) -> bool {
    code.starts_with("normalize.")
        || code.starts_with("pipeline.")
        || code == crate::report::codes::GUTTER_BELOW_ADVISORY_FLOOR
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
        // The input was already healthy — no repair happened, so nothing
        // should claim otherwise.
        assert!(!outcome
            .report
            .findings
            .iter()
            .any(|f| f.code == "pipeline.input-repaired"));
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
        // A reader of the report must be able to tell the analysed bytes
        // were qpdf's repaired output, not the file as supplied.
        assert!(
            outcome
                .report
                .findings
                .iter()
                .any(|f| f.code == "pipeline.input-repaired"),
            "{}",
            outcome.report.to_text()
        );
    }

    #[test]
    fn own_process_findings_are_distinguished_from_preflight_findings() {
        // The flatten stage relies on this distinction (see
        // `is_own_process_finding`'s doc comment) to drop stale pre-flatten
        // preflight findings while keeping this pipeline's own process log.
        // Every code this module or `normalize_interior` actually uses for
        // its own findings must be recognised...
        for own_code in [
            "normalize.landscape-pages-observed",
            "normalize.annotations-removed",
            "normalize.acroform-removed",
            "normalize.javascript-removed",
            "normalize.embedded-files-removed",
            "normalize.scaled-to-bleed",
            "normalize.pages-padded",
            "pipeline.input-repaired",
            "pipeline.ghostscript-invoked",
            crate::report::codes::GUTTER_BELOW_ADVISORY_FLOOR,
        ] {
            assert!(
                is_own_process_finding(own_code),
                "{own_code} is one of this pipeline's own process findings and must survive a flatten stage"
            );
        }

        // ...and every code `preflight()` actually raises must NOT be, or a
        // stale finding from preflighting the pre-flatten bytes would
        // survive alongside (or instead of) a fresh finding from
        // preflighting the bytes actually written.
        for preflight_code in [
            "document.parse-failed",
            crate::report::codes::STRUCTURE_ENCRYPTED,
            crate::report::codes::GEOMETRY_MIXED_PAGE_SIZES,
            crate::report::codes::GEOMETRY_PAGE_SIZE_MISMATCH,
            crate::report::codes::FONTS_NOT_EMBEDDED,
            crate::report::codes::STRUCTURE_ANNOTATIONS,
            crate::report::codes::STRUCTURE_SPREAD_LAYOUT,
            crate::report::codes::IMAGE_LOW_RESOLUTION,
            crate::report::codes::IMAGE_EXCESSIVE_RESOLUTION,
            crate::report::codes::COLOUR_TOTAL_AREA_COVERAGE,
            crate::report::codes::COLOUR_UNSUPPORTED_SPACE,
            crate::report::codes::PAGE_COUNT_BELOW_MINIMUM,
            crate::report::codes::PAGE_COUNT_ABOVE_MAXIMUM,
            crate::report::codes::PAGE_COUNT_NOT_DIVISIBLE,
            "structure.live-transparency",
            "structure.optional-content",
        ] {
            assert!(
                !is_own_process_finding(preflight_code),
                "{preflight_code} is one of preflight's own findings and must be replaced, not kept, across a flatten stage"
            );
        }
    }

    #[test]
    fn flatten_stage_reports_a_fresh_preflight_of_the_flattened_bytes_not_the_pre_flatten_output() {
        let gs_path = match external_tools::detect(&GHOSTSCRIPT, None, Duration::from_secs(5)) {
            external_tools::DetectionOutcome::Available { path, .. } => path,
            _ => {
                eprintln!("Ghostscript not installed; skipping");
                return;
            }
        };
        let input = doc_with_n_unbled_pages(32);
        let mut options = PipelineOptions::new();
        options.gs_path = Some(gs_path);
        options.flatten = Some(GhostscriptFlattenOptions::default());

        let outcome = run_pipeline(&input, sku(), NormalizeOptions::default(), &options).unwrap();
        assert!(outcome
            .report
            .findings
            .iter()
            .any(|f| f.code == "pipeline.ghostscript-invoked"));

        // Once this pipeline's own process findings are set aside, what's
        // left must be exactly what preflighting the bytes actually
        // written says — not a stale preflight of the pre-flatten
        // intermediate.
        let expected = crate::preflight::preflight(&outcome.output_bytes, Some(sku())).findings;
        let actual: Vec<_> = outcome
            .report
            .findings
            .iter()
            .filter(|f| !is_own_process_finding(&f.code))
            .cloned()
            .collect();
        assert_eq!(actual, expected, "{}", outcome.report.to_text());
    }
}
