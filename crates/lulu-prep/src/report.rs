//! Findings and the run report: the shared vocabulary every check (preflight,
//! normalization, cover preparation, external tools, API verification) reports
//! through, in both human-readable and JSON form.

use serde::{Deserialize, Serialize};

pub const SCHEMA_VERSION: u32 = 1;

/// The current time as an RFC 3339 UTC timestamp, with no external time-zone
/// dependency: `std::time` gives Unix seconds; the calendar breakdown is
/// plain arithmetic (proleptic Gregorian, valid for any post-1970 date).
pub fn now_rfc3339() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let days = secs / 86_400;
    let time_of_day = secs % 86_400;
    let (hour, minute, second) = (
        time_of_day / 3600,
        (time_of_day / 60) % 60,
        time_of_day % 60,
    );

    // Civil-from-days algorithm (Howard Hinnant's public-domain date algorithms).
    let z = days as i64 + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { y + 1 } else { y };

    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

/// Stable, dotted-string finding codes. New checks (preflight, normalization,
/// external tools, API verification) add their own codes here as they land —
/// this registry exists so codes never drift between call sites, and so a
/// report can be diffed or a finding suppressed by code.
pub mod codes {
    pub const GEOMETRY_PAGE_SIZE_MISMATCH: &str = "geometry.page-size-mismatch";
    pub const GEOMETRY_MIXED_PAGE_SIZES: &str = "geometry.mixed-page-sizes";
    pub const FONTS_NOT_EMBEDDED: &str = "fonts.not-embedded";
    pub const IMAGE_LOW_RESOLUTION: &str = "image.low-resolution";
    pub const IMAGE_EXCESSIVE_RESOLUTION: &str = "image.excessive-resolution";
    pub const COLOUR_TOTAL_AREA_COVERAGE: &str = "colour.total-area-coverage";
    pub const COLOUR_UNSUPPORTED_SPACE: &str = "colour.unsupported-space";
    pub const STRUCTURE_ENCRYPTED: &str = "structure.encrypted";
    pub const STRUCTURE_ANNOTATIONS: &str = "structure.annotations";
    pub const STRUCTURE_SPREAD_LAYOUT: &str = "structure.spread-layout";
    pub const PAGE_COUNT_BELOW_MINIMUM: &str = "page-count.below-minimum";
    pub const PAGE_COUNT_ABOVE_MAXIMUM: &str = "page-count.above-maximum";
    pub const PAGE_COUNT_NOT_DIVISIBLE: &str = "page-count.not-divisible";
    pub const GUTTER_BELOW_ADVISORY_FLOOR: &str = "gutter.below-advisory-floor";
    pub const GEOMETRY_UNRESOLVABLE_RESOURCES: &str = "geometry.unresolvable-resources";
    pub const GEOMETRY_DEGENERATE: &str = "geometry.degenerate";
    pub const GUTTER_EXCEEDS_SAFE_AREA: &str = "gutter.exceeds-safe-area";
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    /// A condition Lulu rejects outright.
    Blocking,
    /// A condition Lulu accepts but that degrades print quality or relies on
    /// Lulu's own normalizer.
    Warning,
    /// An observation that needs no action.
    Info,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Finding {
    pub code: String,
    pub severity: Severity,
    pub message: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pages: Vec<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected: Option<String>,
    #[serde(default)]
    pub fixable: bool,
}

impl Finding {
    pub fn new(code: impl Into<String>, severity: Severity, message: impl Into<String>) -> Finding {
        Finding {
            code: code.into(),
            severity,
            message: message.into(),
            pages: Vec::new(),
            observed: None,
            expected: None,
            fixable: false,
        }
    }

    pub fn with_pages(mut self, pages: Vec<u32>) -> Finding {
        self.pages = pages;
        self
    }

    pub fn with_observed(mut self, observed: impl Into<String>) -> Finding {
        self.observed = Some(observed.into());
        self
    }

    pub fn with_expected(mut self, expected: impl Into<String>) -> Finding {
        self.expected = Some(expected.into());
        self
    }

    pub fn fixable(mut self, fixable: bool) -> Finding {
        self.fixable = fixable;
        self
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DetectedTool {
    pub name: String,
    pub path: Option<String>,
    pub version: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StageLogEntry {
    pub name: String,
    pub duration_ms: u64,
}

/// The full record of one run, in a form serializable to JSON and renderable
/// as human-readable text from the same data — the text form can never claim
/// something the JSON does not.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Report {
    pub schema_version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_digest: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub product_sku: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page_count: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub catalog_fetch_date: Option<String>,
    pub tool_version: String,
    #[serde(default)]
    pub detected_tools: Vec<DetectedTool>,
    #[serde(default)]
    pub stages: Vec<StageLogEntry>,
    pub findings: Vec<Finding>,
    pub generated_at: String,
}

impl Report {
    pub fn blocking_count(&self) -> usize {
        self.findings
            .iter()
            .filter(|f| f.severity == Severity::Blocking)
            .count()
    }

    pub fn warning_count(&self) -> usize {
        self.findings
            .iter()
            .filter(|f| f.severity == Severity::Warning)
            .count()
    }

    pub fn info_count(&self) -> usize {
        self.findings
            .iter()
            .filter(|f| f.severity == Severity::Info)
            .count()
    }

    pub fn is_print_ready(&self) -> bool {
        self.blocking_count() == 0
    }

    /// The report's one-line summary: readiness, and enough context (product,
    /// page count, or finding counts) to be useful on its own.
    pub fn verdict_line(&self) -> String {
        if self.is_print_ready() {
            let mut parts = vec!["print-ready".to_string()];
            if let Some(sku) = &self.product_sku {
                parts.push(format!("for {sku}"));
            }
            if let Some(pages) = self.page_count {
                parts.push(format!("at {pages} pages"));
            }
            let mut line = parts.join(" ");
            if self.warning_count() > 0 {
                line.push_str(&format!(
                    " ({} warning{} remaining)",
                    self.warning_count(),
                    if self.warning_count() == 1 { "" } else { "s" }
                ));
            }
            line
        } else {
            format!(
                "not print-ready: {} blocking issue{}, {} warning{}",
                self.blocking_count(),
                if self.blocking_count() == 1 { "" } else { "s" },
                self.warning_count(),
                if self.warning_count() == 1 { "" } else { "s" },
            )
        }
    }

    fn findings_by(&self, severity: Severity) -> Vec<&Finding> {
        self.findings
            .iter()
            .filter(|f| f.severity == severity)
            .collect()
    }

    fn render_section(out: &mut String, title: &str, findings: &[&Finding]) {
        if findings.is_empty() {
            return;
        }
        out.push('\n');
        out.push_str(title);
        out.push('\n');
        for f in findings {
            out.push_str(&format!("  [{}] {}\n", f.code, f.message));
            if !f.pages.is_empty() {
                out.push_str(&format!("    pages: {:?}\n", f.pages));
            }
            if let Some(observed) = &f.observed {
                out.push_str(&format!("    observed: {observed}\n"));
            }
            if let Some(expected) = &f.expected {
                out.push_str(&format!("    expected: {expected}\n"));
            }
        }
    }

    /// Human-readable text, grouped by severity (blocking, then warning, then
    /// info), leading with the one-line verdict. Contains no ANSI escapes.
    pub fn to_text(&self) -> String {
        let mut out = self.verdict_line();
        out.push('\n');
        Self::render_section(&mut out, "Blocking:", &self.findings_by(Severity::Blocking));
        Self::render_section(&mut out, "Warning:", &self.findings_by(Severity::Warning));
        Self::render_section(&mut out, "Info:", &self.findings_by(Severity::Info));
        out.trim_end().to_string()
    }

    pub fn to_json(&self) -> serde_json::Result<String> {
        serde_json::to_string_pretty(self)
    }

    /// A copy of this report's JSON with every volatile field masked out —
    /// `generated_at`, stage durations, and detected-tool versions — so two
    /// runs of the same input can be compared for a genuine difference.
    pub fn normalized_for_diff(&self) -> serde_json::Value {
        let mut value = serde_json::to_value(self).expect("Report always serializes");
        if let Some(obj) = value.as_object_mut() {
            obj.insert("generated_at".to_string(), serde_json::Value::Null);
            if let Some(stages) = obj.get_mut("stages").and_then(|s| s.as_array_mut()) {
                for stage in stages {
                    if let Some(stage_obj) = stage.as_object_mut() {
                        stage_obj.insert("duration_ms".to_string(), serde_json::Value::Null);
                    }
                }
            }
            if let Some(tools) = obj.get_mut("detected_tools").and_then(|t| t.as_array_mut()) {
                for tool in tools {
                    if let Some(tool_obj) = tool.as_object_mut() {
                        tool_obj.insert("version".to_string(), serde_json::Value::Null);
                    }
                }
            }
        }
        value
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn blocking(code: &str) -> Finding {
        Finding::new(code, Severity::Blocking, format!("blocking: {code}"))
    }

    fn warning(code: &str) -> Finding {
        Finding::new(code, Severity::Warning, format!("warning: {code}"))
    }

    #[test]
    fn finding_builder_carries_observed_and_expected() {
        let f = Finding::new(
            codes::GEOMETRY_PAGE_SIZE_MISMATCH,
            Severity::Blocking,
            "page too small",
        )
        .with_pages(vec![1, 2, 3])
        .with_observed("6.000 x 9.000 in")
        .with_expected("6.250 x 9.250 in")
        .fixable(true);
        assert_eq!(f.code, codes::GEOMETRY_PAGE_SIZE_MISMATCH);
        assert_eq!(f.severity, Severity::Blocking);
        assert_eq!(f.pages, vec![1, 2, 3]);
        assert_eq!(f.observed.as_deref(), Some("6.000 x 9.000 in"));
        assert_eq!(f.expected.as_deref(), Some("6.250 x 9.250 in"));
        assert!(f.fixable);
    }

    #[test]
    fn codes_are_stable_strings() {
        // The same defect, constructed twice, must carry an identical code —
        // that's what lets a report be diffed or a finding suppressed by code.
        let a = blocking(codes::STRUCTURE_ENCRYPTED);
        let b = blocking(codes::STRUCTURE_ENCRYPTED);
        assert_eq!(a.code, b.code);
    }

    fn sample_report(findings: Vec<Finding>) -> Report {
        Report {
            schema_version: 1,
            input_digest: Some("deadbeef".to_string()),
            product_sku: Some("0600X0900.BW.STD.PB.060UW444.MXX".to_string()),
            page_count: Some(210),
            catalog_fetch_date: Some("2026-09-03".to_string()),
            tool_version: "0.1.0".to_string(),
            detected_tools: vec![],
            stages: vec![],
            findings,
            generated_at: "2026-09-03T12:00:00Z".to_string(),
        }
    }

    #[test]
    fn verdict_line_reports_both_counts_when_not_print_ready() {
        let findings = vec![
            blocking(codes::STRUCTURE_ENCRYPTED),
            blocking(codes::FONTS_NOT_EMBEDDED),
            warning(codes::COLOUR_TOTAL_AREA_COVERAGE),
            warning(codes::COLOUR_TOTAL_AREA_COVERAGE),
            warning(codes::COLOUR_TOTAL_AREA_COVERAGE),
            warning(codes::COLOUR_TOTAL_AREA_COVERAGE),
            warning(codes::COLOUR_TOTAL_AREA_COVERAGE),
        ];
        let report = sample_report(findings);
        assert_eq!(report.blocking_count(), 2);
        assert_eq!(report.warning_count(), 5);
        assert!(!report.is_print_ready());

        let verdict = report.verdict_line();
        assert!(
            verdict.contains("not print-ready") || verdict.contains("not print ready"),
            "{verdict}"
        );
        assert!(verdict.contains('2'), "{verdict}");
        assert!(verdict.contains('5'), "{verdict}");
    }

    #[test]
    fn verdict_line_reports_readiness_with_product_and_page_count() {
        let report = sample_report(vec![warning(codes::COLOUR_TOTAL_AREA_COVERAGE)]);
        assert!(report.is_print_ready());
        let verdict = report.verdict_line();
        assert!(
            verdict.contains("print-ready") || verdict.contains("print ready"),
            "{verdict}"
        );
        assert!(
            verdict.contains("0600X0900.BW.STD.PB.060UW444.MXX"),
            "{verdict}"
        );
        assert!(verdict.contains("210"), "{verdict}");
    }

    #[test]
    fn text_report_groups_findings_by_severity_and_leads_with_verdict() {
        let findings = vec![
            warning(codes::COLOUR_TOTAL_AREA_COVERAGE),
            blocking(codes::STRUCTURE_ENCRYPTED),
        ];
        let report = sample_report(findings);
        let text = report.to_text();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[0], report.verdict_line());

        let blocking_pos = text.find("Blocking").expect("a Blocking section header");
        let warning_pos = text.find("Warning").expect("a Warning section header");
        assert!(
            blocking_pos < warning_pos,
            "blocking section must come before warning section"
        );

        // No ANSI escapes — readable without colour.
        assert!(!text.contains('\u{1b}'));
    }

    #[test]
    fn json_report_round_trips_and_carries_schema_version() {
        let report = sample_report(vec![blocking(codes::STRUCTURE_ENCRYPTED)]);
        let json = report.to_json().expect("serializes");
        let value: serde_json::Value = serde_json::from_str(&json).expect("parses as JSON");
        assert_eq!(value["schema_version"], 1);
        assert_eq!(value["findings"][0]["code"], codes::STRUCTURE_ENCRYPTED);

        let round_tripped: Report =
            serde_json::from_str(&json).expect("deserializes back to Report");
        assert_eq!(round_tripped.findings.len(), report.findings.len());
    }

    #[test]
    fn two_runs_differ_only_in_volatile_fields() {
        let mut run1 = sample_report(vec![blocking(codes::STRUCTURE_ENCRYPTED)]);
        let mut run2 = run1.clone();

        run1.generated_at = "2026-09-03T12:00:00Z".to_string();
        run2.generated_at = "2026-09-03T12:05:33Z".to_string();
        run1.stages.push(StageLogEntry {
            name: "repair".to_string(),
            duration_ms: 120,
        });
        run2.stages.push(StageLogEntry {
            name: "repair".to_string(),
            duration_ms: 340,
        });
        run1.detected_tools.push(DetectedTool {
            name: "qpdf".to_string(),
            path: Some("/usr/bin/qpdf".to_string()),
            version: Some("11.9.0".to_string()),
        });
        run2.detected_tools.push(DetectedTool {
            name: "qpdf".to_string(),
            path: Some("/usr/bin/qpdf".to_string()),
            version: Some("11.9.1".to_string()),
        });

        assert_eq!(run1.normalized_for_diff(), run2.normalized_for_diff());

        // A genuine difference in findings must still show up.
        run2.findings
            .push(warning(codes::COLOUR_TOTAL_AREA_COVERAGE));
        assert_ne!(run1.normalized_for_diff(), run2.normalized_for_diff());
    }
}
