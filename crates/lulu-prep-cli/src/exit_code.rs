//! Exit code mapping, per `specs/cli/spec.md`'s "Exit codes" requirement.

use lulu_prep::report::Report;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExitCode {
    Clean = 0,
    BlockingFindings = 1,
    InvalidUsage = 2,
    IoOrParse = 3,
    MissingToolOrCredential = 4,
}

impl ExitCode {
    pub fn as_i32(self) -> i32 {
        self as i32
    }
}

/// Maps a finished report to an exit code. Warnings never affect the result
/// unless `strict` is set, in which case they behave like blocking findings.
pub fn exit_code_for_report(report: &Report, strict: bool) -> ExitCode {
    let blocking = report.blocking_count() > 0 || (strict && report.warning_count() > 0);
    if blocking {
        ExitCode::BlockingFindings
    } else {
        ExitCode::Clean
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lulu_prep::report::{codes, Finding, Severity};

    fn report_with(findings: Vec<Finding>) -> Report {
        Report {
            schema_version: 1,
            input_digest: None,
            product_sku: None,
            page_count: None,
            catalog_fetch_date: None,
            tool_version: "test".to_string(),
            detected_tools: vec![],
            stages: vec![],
            findings,
            generated_at: "2026-01-01T00:00:00Z".to_string(),
        }
    }

    #[test]
    fn clean_report_exits_zero() {
        let report = report_with(vec![]);
        assert_eq!(exit_code_for_report(&report, false), ExitCode::Clean);
        assert_eq!(exit_code_for_report(&report, true), ExitCode::Clean);
    }

    #[test]
    fn blocking_finding_exits_one_regardless_of_strict() {
        let report = report_with(vec![Finding::new(
            codes::FONTS_NOT_EMBEDDED,
            Severity::Blocking,
            "x",
        )]);
        assert_eq!(
            exit_code_for_report(&report, false),
            ExitCode::BlockingFindings
        );
        assert_eq!(
            exit_code_for_report(&report, true),
            ExitCode::BlockingFindings
        );
    }

    #[test]
    fn warning_only_is_clean_unless_strict() {
        let report = report_with(vec![Finding::new(
            codes::FONTS_NOT_EMBEDDED,
            Severity::Warning,
            "x",
        )]);
        assert_eq!(exit_code_for_report(&report, false), ExitCode::Clean);
        assert_eq!(
            exit_code_for_report(&report, true),
            ExitCode::BlockingFindings
        );
    }

    #[test]
    fn info_only_never_affects_exit_code() {
        let report = report_with(vec![Finding::new(
            codes::FONTS_NOT_EMBEDDED,
            Severity::Info,
            "x",
        )]);
        assert_eq!(exit_code_for_report(&report, false), ExitCode::Clean);
        assert_eq!(exit_code_for_report(&report, true), ExitCode::Clean);
    }
}
