//! Detection and invocation of the optional external binaries — Ghostscript
//! and qpdf — that add capability beyond what this crate does natively.
//! Neither is required: every stage here is additive, and its absence is
//! reported, not fatal, unless a caller explicitly requested that stage.
//!
//! Ghostscript is AGPL-licensed, so it is only ever invoked as a subprocess
//! here — never linked into this crate.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ToolVersion {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}

impl ToolVersion {
    pub fn parse(s: &str) -> Option<ToolVersion> {
        // Accepts the first "N.N[.N]" substring found anywhere in the string,
        // since version output varies ("qpdf version 11.9.0", "gs 10.03.1").
        let digits_or_dot = |c: char| c.is_ascii_digit() || c == '.';
        let candidate: String = s
            .chars()
            .skip_while(|c| !c.is_ascii_digit())
            .take_while(|&c| digits_or_dot(c))
            .collect();
        let mut parts = candidate.split('.');
        let major = parts.next()?.parse().ok()?;
        let minor = parts.next().unwrap_or("0").parse().ok()?;
        let patch = parts.next().unwrap_or("0").parse().ok()?;
        Some(ToolVersion {
            major,
            minor,
            patch,
        })
    }
}

impl std::fmt::Display for ToolVersion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

impl PartialOrd for ToolVersion {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for ToolVersion {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        (self.major, self.minor, self.patch).cmp(&(other.major, other.minor, other.patch))
    }
}

#[derive(Debug, Clone, Copy)]
pub struct ToolSpec {
    pub name: &'static str,
    pub version_arg: &'static str,
    pub minimum_version: ToolVersion,
}

pub const QPDF: ToolSpec = ToolSpec {
    name: "qpdf",
    version_arg: "--version",
    minimum_version: ToolVersion {
        major: 10,
        minor: 0,
        patch: 0,
    },
};
pub const GHOSTSCRIPT: ToolSpec = ToolSpec {
    name: "gs",
    version_arg: "--version",
    minimum_version: ToolVersion {
        major: 9,
        minor: 50,
        patch: 0,
    },
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DetectionOutcome {
    Available {
        path: PathBuf,
        version: ToolVersion,
    },
    NotFound,
    Unresponsive,
    /// Found and responsive, but its version is below `minimum`.
    BelowMinimumVersion {
        path: PathBuf,
        found: ToolVersion,
        minimum: ToolVersion,
    },
    /// Found and responsive, but its version output couldn't be parsed at all.
    UnparseableVersion {
        path: PathBuf,
    },
}

impl DetectionOutcome {
    pub fn is_available(&self) -> bool {
        matches!(self, DetectionOutcome::Available { .. })
    }

    /// A short, stated reason a stage powered by this tool can't run —
    /// `None` when the tool is available.
    pub fn unavailable_reason(&self) -> Option<String> {
        match self {
            DetectionOutcome::Available { .. } => None,
            DetectionOutcome::NotFound => Some("not found on PATH".to_string()),
            DetectionOutcome::Unresponsive => {
                Some("did not respond to a version check in time".to_string())
            }
            DetectionOutcome::BelowMinimumVersion { found, minimum, .. } => {
                Some(format!("version {found} is below the required {minimum}"))
            }
            DetectionOutcome::UnparseableVersion { .. } => {
                Some("responded, but its version output could not be parsed".to_string())
            }
        }
    }
}

/// Why [`run_with_timeout_bytes`] did not return a captured status/output.
#[derive(Debug)]
enum RunError {
    /// The command could not even be started (binary missing, not
    /// executable, permission denied, ...) — or an OS-level error occurred
    /// while polling it, which is rare enough to fold into the same "this
    /// child is unusable" bucket rather than growing a third variant.
    Unusable(std::io::Error),
    /// The child was still running after `timeout` and was killed.
    Timeout { elapsed: Duration },
}

/// Runs `cmd`, killing it if it hasn't exited within `timeout`. Stdout and
/// stderr are each drained by a dedicated reader thread for the child's
/// entire lifetime, started immediately after spawn and joined only after
/// the child has exited (or been killed) — reading only *after* `try_wait`
/// observes exit, as an earlier version of this function did, deadlocks
/// against a child that writes more than one OS pipe buffer's worth of
/// output (typically 64KB) before exiting, since the child blocks on its
/// own `write()` while nothing is reading the other end.
fn run_with_timeout_bytes(
    cmd: &mut Command,
    timeout: Duration,
) -> Result<(std::process::ExitStatus, Vec<u8>, Vec<u8>), RunError> {
    let mut child = cmd
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(RunError::Unusable)?;
    let start = Instant::now();

    let stdout_handle = child.stdout.take().map(|mut pipe| {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = pipe.read_to_end(&mut buf);
            buf
        })
    });
    let stderr_handle = child.stderr.take().map(|mut pipe| {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = pipe.read_to_end(&mut buf);
            buf
        })
    });

    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {
                if start.elapsed() > timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(RunError::Timeout {
                        elapsed: start.elapsed(),
                    });
                }
                std::thread::sleep(Duration::from_millis(15));
            }
            Err(e) => return Err(RunError::Unusable(e)),
        }
    };

    // The child has exited (or was killed above, which closes its ends of
    // the pipes), so each reader thread's `read_to_end` will return; join
    // rather than detach so no output is lost to a thread still catching up.
    let stdout = stdout_handle
        .and_then(|h| h.join().ok())
        .unwrap_or_default();
    let stderr = stderr_handle
        .and_then(|h| h.join().ok())
        .unwrap_or_default();
    Ok((status, stdout, stderr))
}

/// String-oriented convenience wrapper over [`run_with_timeout_bytes`], for
/// callers (the capability-detection probe) whose output is expected to be
/// text. Invalid UTF-8 is replaced lossily rather than discarding whatever
/// was captured.
fn run_with_timeout(
    cmd: &mut Command,
    timeout: Duration,
) -> Result<(std::process::ExitStatus, String, String), RunError> {
    let (status, stdout, stderr) = run_with_timeout_bytes(cmd, timeout)?;
    Ok((
        status,
        String::from_utf8_lossy(&stdout).into_owned(),
        String::from_utf8_lossy(&stderr).into_owned(),
    ))
}

/// Detects one external tool: resolved from `configured_path` if given
/// (an explicit path always wins over `PATH`), else looked up on `PATH`.
/// Probes it for its version under `timeout`; a binary that doesn't
/// respond in time, or whose version is unparseable or below
/// `spec.minimum_version`, is treated as unavailable rather than causing
/// the run to hang or panic.
pub fn detect(
    spec: &ToolSpec,
    configured_path: Option<&Path>,
    timeout: Duration,
) -> DetectionOutcome {
    let resolved: PathBuf = match configured_path {
        Some(p) => p.to_path_buf(),
        None => match which::which(spec.name) {
            Ok(p) => p,
            Err(_) => return DetectionOutcome::NotFound,
        },
    };
    if configured_path.is_some() && !resolved.exists() {
        return DetectionOutcome::NotFound;
    }

    let Ok((status, stdout, stderr)) =
        run_with_timeout(Command::new(&resolved).arg(spec.version_arg), timeout)
    else {
        return DetectionOutcome::Unresponsive;
    };
    if !status.success() && stdout.is_empty() && stderr.is_empty() {
        return DetectionOutcome::Unresponsive;
    }
    let combined = format!("{stdout}{stderr}");
    let Some(version) = ToolVersion::parse(&combined) else {
        return DetectionOutcome::UnparseableVersion { path: resolved };
    };
    if version < spec.minimum_version {
        return DetectionOutcome::BelowMinimumVersion {
            path: resolved,
            found: version,
            minimum: spec.minimum_version,
        };
    }
    DetectionOutcome::Available {
        path: resolved,
        version,
    }
}

/// Converts a [`DetectionOutcome`] into a [`crate::report::DetectedTool`]
/// entry for the run report, whether or not the tool was actually available.
pub fn to_report_entry(spec: &ToolSpec, outcome: &DetectionOutcome) -> crate::report::DetectedTool {
    match outcome {
        DetectionOutcome::Available { path, version } => crate::report::DetectedTool {
            name: spec.name.to_string(),
            path: Some(path.display().to_string()),
            version: Some(version.to_string()),
        },
        _ => crate::report::DetectedTool {
            name: spec.name.to_string(),
            path: None,
            version: None,
        },
    }
}

#[derive(Debug, thiserror::Error)]
pub enum RepairError {
    #[error("qpdf could not run: {0}")]
    ToolUnavailable(String),
    #[error("qpdf could not repair this file: {stderr}")]
    RepairFailed { stderr: String },
    #[error("qpdf exited successfully but produced no output")]
    NoOutput,
    #[error("qpdf did not finish repairing this file within {elapsed:?} and was terminated")]
    Timeout { elapsed: Duration },
}

#[derive(Debug, Clone, Copy, Default)]
pub struct QpdfRepairOptions {
    pub linearize: bool,
}

/// Rebuilds a damaged cross-reference table and removes encryption (an
/// empty-password file decrypts silently; one needing a real password fails
/// with qpdf's own diagnostic, same as it would unrepaired), optionally
/// linearizing, via `qpdf --decrypt [--linearize] <tmpfile> -` — qpdf's
/// `infile` does not support stdin ("reading from stdin is not supported",
/// per `qpdf --help=usage`), only `outfile` may be `-` for stdout, so the
/// input is written to a temporary file first.
///
/// `timeout` bounds the qpdf invocation itself (not the temp-file setup):
/// a qpdf process that hangs on hostile input is killed and reported as
/// [`RepairError::Timeout`] rather than left to run forever. The temporary
/// input file is always cleaned up on return, timeout included, since it is
/// a [`tempfile::NamedTempFile`] deleted on drop.
pub fn repair_with_qpdf(
    qpdf_path: &Path,
    input_bytes: &[u8],
    options: QpdfRepairOptions,
    timeout: Duration,
) -> Result<Vec<u8>, RepairError> {
    let mut input_file = tempfile::Builder::new()
        .suffix(".pdf")
        .tempfile()
        .map_err(|e| RepairError::ToolUnavailable(e.to_string()))?;
    input_file
        .write_all(input_bytes)
        .map_err(|e| RepairError::ToolUnavailable(e.to_string()))?;
    input_file
        .flush()
        .map_err(|e| RepairError::ToolUnavailable(e.to_string()))?;

    let mut cmd = Command::new(qpdf_path);
    // Repair (reconstructing a broken xref) legitimately produces warnings;
    // qpdf's default exit code (3) for "succeeded with warnings" would
    // otherwise look identical to a hard failure (exit 2) to a status check.
    cmd.arg("--warning-exit-0");
    cmd.arg("--decrypt");
    if options.linearize {
        cmd.arg("--linearize");
    }
    cmd.arg(input_file.path()).arg("-");

    let (status, stdout, stderr) = match run_with_timeout_bytes(&mut cmd, timeout) {
        Ok(v) => v,
        Err(RunError::Timeout { elapsed }) => return Err(RepairError::Timeout { elapsed }),
        Err(RunError::Unusable(e)) => return Err(RepairError::ToolUnavailable(e.to_string())),
    };
    if !status.success() {
        return Err(RepairError::RepairFailed {
            stderr: String::from_utf8_lossy(&stderr).to_string(),
        });
    }
    if stdout.is_empty() {
        return Err(RepairError::NoOutput);
    }
    Ok(stdout)
}

/// Why loading a PDF failed when qpdf repair was in play — as opposed to
/// [`crate::pdf::LoadError`] alone, this distinguishes "native parsing
/// failed and repair was never attempted" (no `qpdf_path` was supplied)
/// from "native parsing failed, qpdf repair was attempted, and it also
/// failed" — the latter carries qpdf's own diagnostics rather than
/// discarding them in favour of the original parse error.
#[derive(Debug, thiserror::Error)]
pub enum RepairOrLoadError {
    #[error(transparent)]
    ParseFailed(#[from] crate::pdf::LoadError),
    #[error("could not parse this file ({native_err}); repairing it with qpdf was attempted and also failed: {qpdf_diagnostics}")]
    RepairAttemptedAndFailed {
        native_err: String,
        qpdf_diagnostics: String,
    },
}

/// Loads bytes as a PDF, attempting qpdf repair (when `qpdf_path` is given)
/// if native parsing fails, then re-parsing the repaired bytes rather than
/// bytes reused from the pipeline that produced this function's bytes-only
/// sibling, [`repair_bytes_if_needed`], which this delegates to — the two
/// callers just want a different final shape (a parsed [`lopdf::Document`]
/// here; raw bytes there, for a caller about to do its own parsing).
pub fn load_with_optional_repair(
    bytes: &[u8],
    qpdf_path: Option<&Path>,
    timeout: Duration,
) -> Result<(lopdf::Document, bool), RepairOrLoadError> {
    let (bytes, was_repaired) = repair_bytes_if_needed(bytes, qpdf_path, timeout)?;
    let doc = crate::pdf::load_from_bytes(&bytes)?;
    Ok((doc, was_repaired))
}

/// Loads bytes as a PDF only far enough to decide whether they need qpdf
/// repair (when `qpdf_path` is given) before handing them onward, returning
/// bytes rather than a parsed [`lopdf::Document`] — for a caller (the
/// pipeline) that is about to hand the bytes to another function which does
/// its own parsing, such as [`crate::normalize::normalize_interior`]. When
/// native parsing fails and no qpdf path is given, the original parse error
/// is returned — the caller (preflight) turns that into a blocking finding
/// naming qpdf as the remedy. `timeout` is passed through to
/// [`repair_with_qpdf`].
pub fn repair_bytes_if_needed(
    bytes: &[u8],
    qpdf_path: Option<&Path>,
    timeout: Duration,
) -> Result<(Vec<u8>, bool), RepairOrLoadError> {
    let native_err = match crate::pdf::load_from_bytes(bytes) {
        Ok(_) => return Ok((bytes.to_vec(), false)),
        Err(e) => e,
    };
    let Some(qpdf_path) = qpdf_path else {
        return Err(RepairOrLoadError::ParseFailed(native_err));
    };
    let repaired = repair_with_qpdf(qpdf_path, bytes, QpdfRepairOptions::default(), timeout)
        .map_err(|repair_err| RepairOrLoadError::RepairAttemptedAndFailed {
            native_err: native_err.to_string(),
            qpdf_diagnostics: repair_err.to_string(),
        })?;
    // Confirm the repair actually produced something lopdf can read before
    // handing it onward — a caller downstream re-parsing silently-still-
    // broken bytes would fail more confusingly there.
    if let Err(reparse_err) = crate::pdf::load_from_bytes(&repaired) {
        return Err(RepairOrLoadError::RepairAttemptedAndFailed {
            native_err: native_err.to_string(),
            qpdf_diagnostics: format!(
                "qpdf exited successfully but its output still does not parse: {reparse_err}"
            ),
        });
    }
    Ok((repaired, true))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorSpaceTarget {
    Cmyk,
}

/// Options for the Ghostscript flatten/colour-convert stage. Off by
/// default; the caller opts in explicitly. CMYK conversion always requires
/// an ICC profile — there is no unspecified default conversion.
#[derive(Debug, Clone, Default)]
pub struct GhostscriptFlattenOptions {
    pub target_color_space: Option<ColorSpaceTarget>,
    pub icc_profile_path: Option<PathBuf>,
}

#[derive(Debug, thiserror::Error)]
pub enum GhostscriptError {
    #[error("Ghostscript could not run: {0}")]
    ToolUnavailable(String),
    #[error("Ghostscript failed: {stderr}")]
    Failed { stderr: String },
    #[error("CMYK conversion requires an ICC profile; none was supplied")]
    MissingIccProfile,
    #[error("Ghostscript exited successfully but produced no output")]
    NoOutput,
    #[error("Ghostscript's output geometry does not match the input: {0}")]
    GeometryChanged(String),
    #[error("Ghostscript did not finish within {elapsed:?} and was terminated")]
    Timeout { elapsed: Duration },
}

/// Builds the Ghostscript argument list for the flatten/colour-convert
/// stage: flattens transparency and optional content (implicit in
/// `pdfwrite`'s output), preserves page geometry, embeds and subsets fonts,
/// and never downsamples images. Pure and independently testable — no
/// process is started here.
fn build_ghostscript_args(
    input_path: &Path,
    output_path: &Path,
    options: &GhostscriptFlattenOptions,
) -> Result<Vec<String>, GhostscriptError> {
    let mut args: Vec<String> = [
        "-dNOPAUSE",
        "-dBATCH",
        "-dSAFER",
        "-sDEVICE=pdfwrite",
        "-dEmbedAllFonts=true",
        "-dSubsetFonts=true",
        "-dDownsampleColorImages=false",
        "-dDownsampleGrayImages=false",
        "-dDownsampleMonoImages=false",
        "-dAutoRotatePages=/None",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();

    if let Some(ColorSpaceTarget::Cmyk) = options.target_color_space {
        let Some(profile) = &options.icc_profile_path else {
            return Err(GhostscriptError::MissingIccProfile);
        };
        args.push("-sProcessColorModel=DeviceCMYK".to_string());
        args.push("-sColorConversionStrategy=CMYK".to_string());
        args.push(format!("-sOutputICCProfile={}", profile.display()));
    }

    args.push(format!("-sOutputFile={}", output_path.display()));
    args.push(input_path.display().to_string());
    Ok(args)
}

/// Runs the flatten/colour-convert stage. Writes to its own temporary
/// output file — never the caller's real destination — so a failure here
/// can never leave a partial or corrupt file at a path the caller cares
/// about: the caller only persists the returned bytes after `Ok`, and
/// nothing is written anywhere on `Err`.
///
/// `timeout` bounds the Ghostscript invocation: a process that hangs on
/// hostile input is killed and reported as [`GhostscriptError::Timeout`]
/// rather than left to run forever. Both temporary files are cleaned up on
/// return, timeout included, since they are [`tempfile::NamedTempFile`]s
/// deleted on drop — so a timeout leaves the pre-stage bytes the caller
/// already has untouched and no partial output file behind.
pub fn flatten_with_ghostscript(
    gs_path: &Path,
    input_bytes: &[u8],
    options: &GhostscriptFlattenOptions,
    timeout: Duration,
) -> Result<(Vec<u8>, Vec<String>), GhostscriptError> {
    let mut input_file = tempfile::Builder::new()
        .suffix(".pdf")
        .tempfile()
        .map_err(|e| GhostscriptError::ToolUnavailable(e.to_string()))?;
    input_file
        .write_all(input_bytes)
        .map_err(|e| GhostscriptError::ToolUnavailable(e.to_string()))?;
    input_file
        .flush()
        .map_err(|e| GhostscriptError::ToolUnavailable(e.to_string()))?;
    let output_file = tempfile::Builder::new()
        .suffix(".pdf")
        .tempfile()
        .map_err(|e| GhostscriptError::ToolUnavailable(e.to_string()))?;

    let args = build_ghostscript_args(input_file.path(), output_file.path(), options)?;
    let mut cmd = Command::new(gs_path);
    cmd.args(&args);
    let (status, _stdout, stderr) = match run_with_timeout_bytes(&mut cmd, timeout) {
        Ok(v) => v,
        Err(RunError::Timeout { elapsed }) => return Err(GhostscriptError::Timeout { elapsed }),
        Err(RunError::Unusable(e)) => return Err(GhostscriptError::ToolUnavailable(e.to_string())),
    };
    if !status.success() {
        return Err(GhostscriptError::Failed {
            stderr: String::from_utf8_lossy(&stderr).to_string(),
        });
    }
    let bytes = std::fs::read(output_file.path()).map_err(|_| GhostscriptError::NoOutput)?;
    if bytes.is_empty() {
        return Err(GhostscriptError::NoOutput);
    }
    Ok((bytes, args))
}

/// Reads a PDF rectangle array (`[x0 y0 x1 y1]`) as exactly four numbers,
/// dereferencing each element against `doc` in case it is an indirect
/// reference. Returns `None` — "unreadable", never "nothing to compare" —
/// if the array does not have exactly four elements, or if any element
/// does not dereference to an `Integer` or `Real`. A caller comparing two
/// boxes must treat `None` on either side as a failure: a box array whose
/// elements are all unresolvable indirect references produces zero parsed
/// numbers on both sides under a naive `filter_map`, and `0 == 0` with
/// `.all()` over an empty iterator is vacuously `true` — which is exactly
/// the "compared nothing, called it a match" bug this type exists to rule
/// out at the type level.
fn box_as_four_numbers(doc: &lopdf::Document, arr: &[lopdf::Object]) -> Option<[f64; 4]> {
    if arr.len() != 4 {
        return None;
    }
    let mut out = [0.0f64; 4];
    for (i, obj) in arr.iter().enumerate() {
        let (_, resolved) = doc.dereference(obj).ok()?;
        out[i] = resolved.as_float().ok()? as f64;
    }
    Some(out)
}

/// Confirms the Ghostscript stage didn't alter what normalization already
/// decided: every page's `MediaBox` and `TrimBox` must be byte-identical
/// (within floating-point tolerance) to `before`, and the page count must
/// be unchanged. Run this after every Ghostscript invocation; a mismatch
/// fails the run rather than shipping silently-wrong geometry.
pub fn assert_geometry_preserved(
    before: &lopdf::Document,
    after: &lopdf::Document,
) -> Result<(), GhostscriptError> {
    let before_pages = before.get_pages();
    let after_pages = after.get_pages();
    if before_pages.len() != after_pages.len() {
        return Err(GhostscriptError::GeometryChanged(format!(
            "page count changed from {} to {}",
            before_pages.len(),
            after_pages.len()
        )));
    }
    for (page_number, before_id) in before_pages.values().enumerate() {
        let Some(after_id) = after_pages.get(&(page_number as u32 + 1)) else {
            return Err(GhostscriptError::GeometryChanged(format!(
                "page {} is missing from the output",
                page_number + 1
            )));
        };
        for box_key in [&b"MediaBox"[..], b"TrimBox"] {
            let before_box = before
                .get_dictionary(*before_id)
                .ok()
                .and_then(|d| d.get(box_key).ok())
                .and_then(|o| o.as_array().ok());
            let after_box = after
                .get_dictionary(*after_id)
                .ok()
                .and_then(|d| d.get(box_key).ok())
                .and_then(|o| o.as_array().ok());
            match (before_box, after_box) {
                (Some(b), Some(a)) => {
                    let bv = box_as_four_numbers(before, b);
                    let av = box_as_four_numbers(after, a);
                    match (bv, av) {
                        (Some(bv), Some(av)) => {
                            let matches =
                                bv.iter().zip(av.iter()).all(|(x, y)| (x - y).abs() < 0.01);
                            if !matches {
                                return Err(GhostscriptError::GeometryChanged(format!(
                                    "page {}'s {} changed: {bv:?} -> {av:?}",
                                    page_number + 1,
                                    String::from_utf8_lossy(box_key)
                                )));
                            }
                        }
                        _ => {
                            return Err(GhostscriptError::GeometryChanged(format!(
                                "page {}'s {} could not be read as four numbers on both sides ({bv:?} -> {av:?})",
                                page_number + 1,
                                String::from_utf8_lossy(box_key)
                            )));
                        }
                    }
                }
                (None, None) => {}
                _ => {
                    return Err(GhostscriptError::GeometryChanged(format!(
                        "page {}'s {} presence changed",
                        page_number + 1,
                        String::from_utf8_lossy(box_key)
                    )));
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use lopdf::dictionary;

    /// A generous bound for tests that expect a real tool invocation to
    /// succeed promptly — not the timeout under test.
    const TEST_TIMEOUT: Duration = Duration::from_secs(30);

    #[test]
    fn version_parses_common_formats() {
        assert_eq!(
            ToolVersion::parse("qpdf version 11.9.0"),
            Some(ToolVersion {
                major: 11,
                minor: 9,
                patch: 0
            })
        );
        assert_eq!(
            ToolVersion::parse("GPL Ghostscript 10.03.1"),
            Some(ToolVersion {
                major: 10,
                minor: 3,
                patch: 1
            })
        );
        assert_eq!(
            ToolVersion::parse("12.3.2"),
            Some(ToolVersion {
                major: 12,
                minor: 3,
                patch: 2
            })
        );
        assert_eq!(
            ToolVersion::parse("v9.50"),
            Some(ToolVersion {
                major: 9,
                minor: 50,
                patch: 0
            })
        );
    }

    #[test]
    fn version_ordering_is_numeric_not_lexical() {
        let v9 = ToolVersion::parse("9.9.0").unwrap();
        let v10 = ToolVersion::parse("10.0.0").unwrap();
        assert!(v10 > v9, "10.0.0 must sort after 9.9.0 numerically");
    }

    #[test]
    fn unparseable_version_string_returns_none() {
        assert_eq!(ToolVersion::parse("no version info here"), None);
    }

    #[test]
    fn detects_a_real_installed_binary() {
        // qpdf is a hard dependency of this test suite's own fixture
        // generation script (tests/fixtures/generate.sh) and is expected on
        // the development machine; skip gracefully if genuinely absent.
        let outcome = detect(&QPDF, None, Duration::from_secs(5));
        match outcome {
            DetectionOutcome::Available { version, .. } => assert!(version.major >= 10),
            DetectionOutcome::NotFound => {
                eprintln!("qpdf not installed on this machine; skipping assertion")
            }
            other => panic!("expected Available or NotFound, got {other:?}"),
        }
    }

    #[test]
    fn absent_binary_is_not_found() {
        let spec = ToolSpec {
            name: "definitely-not-a-real-binary-xyz123",
            version_arg: "--version",
            minimum_version: ToolVersion {
                major: 0,
                minor: 0,
                patch: 0,
            },
        };
        assert_eq!(
            detect(&spec, None, Duration::from_secs(2)),
            DetectionOutcome::NotFound
        );
    }

    #[test]
    fn configured_path_overrides_path_lookup() {
        // A configured path that doesn't exist must be reported as not found,
        // even if a same-named binary exists on PATH.
        let outcome = detect(
            &QPDF,
            Some(Path::new("/definitely/not/a/real/path/qpdf")),
            Duration::from_secs(2),
        );
        assert_eq!(outcome, DetectionOutcome::NotFound);
    }

    #[test]
    fn below_minimum_version_is_reported_as_unusable() {
        let spec = ToolSpec {
            name: "qpdf",
            version_arg: "--version",
            minimum_version: ToolVersion {
                major: 999,
                minor: 0,
                patch: 0,
            },
        };
        let outcome = detect(&spec, None, Duration::from_secs(5));
        match outcome {
            DetectionOutcome::BelowMinimumVersion { minimum, .. } => assert_eq!(minimum.major, 999),
            DetectionOutcome::NotFound => eprintln!("qpdf not installed; skipping assertion"),
            other => panic!("expected BelowMinimumVersion or NotFound, got {other:?}"),
        }
    }

    #[test]
    fn unresponsive_binary_times_out_rather_than_hanging() {
        // `sleep 5` never prints a version string and outlives a short timeout.
        let spec = ToolSpec {
            name: "sleep",
            version_arg: "5",
            minimum_version: ToolVersion {
                major: 0,
                minor: 0,
                patch: 0,
            },
        };
        let start = Instant::now();
        let outcome = detect(&spec, None, Duration::from_millis(200));
        assert!(
            start.elapsed() < Duration::from_secs(2),
            "must not wait for the full sleep duration"
        );
        match outcome {
            DetectionOutcome::Unresponsive | DetectionOutcome::NotFound => {}
            other => {
                panic!("expected Unresponsive (or NotFound if `sleep` truly absent), got {other:?}")
            }
        }
    }

    #[test]
    fn unavailable_reason_is_none_only_when_available() {
        assert!(DetectionOutcome::NotFound.unavailable_reason().is_some());
        assert!(DetectionOutcome::Unresponsive
            .unavailable_reason()
            .is_some());
        let available = DetectionOutcome::Available {
            path: PathBuf::from("/usr/bin/qpdf"),
            version: ToolVersion {
                major: 11,
                minor: 0,
                patch: 0,
            },
        };
        assert!(available.unavailable_reason().is_none());
        assert!(available.is_available());
    }

    #[test]
    fn report_entry_omits_path_and_version_when_unavailable() {
        let entry = to_report_entry(&QPDF, &DetectionOutcome::NotFound);
        assert_eq!(entry.name, "qpdf");
        assert!(entry.path.is_none());
        assert!(entry.version.is_none());
    }

    // --- qpdf repair ---

    /// A minimal, valid one-page PDF with a hand-computed, correct xref table.
    fn minimal_valid_pdf() -> Vec<u8> {
        let mut buf = b"%PDF-1.4\n".to_vec();
        let mut offsets = vec![0usize];
        for obj in [
            &b"1 0 obj\n<</Type/Pages/Kids[2 0 R]/Count 1>>\nendobj\n"[..],
            b"2 0 obj\n<</Type/Page/Parent 1 0 R/Resources<<>>/MediaBox[0 0 450 666]>>\nendobj\n",
            b"3 0 obj\n<</Type/Catalog/Pages 1 0 R>>\nendobj\n",
        ] {
            offsets.push(buf.len());
            buf.extend_from_slice(obj);
        }
        let xref_offset = buf.len();
        let mut xref = b"xref\n0 4\n0000000000 65535 f \n".to_vec();
        for off in &offsets[1..] {
            xref.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
        }
        buf.extend_from_slice(&xref);
        buf.extend_from_slice(
            format!("trailer\n<</Root 3 0 R/Size 4>>\nstartxref\n{xref_offset}\n%%EOF\n")
                .as_bytes(),
        );
        buf
    }

    /// The same PDF with its `startxref` offset corrupted, so a strict
    /// parser can't locate the (otherwise intact) xref table — the classic
    /// "damaged file" case qpdf's repair pass is built for.
    fn pdf_with_broken_startxref() -> Vec<u8> {
        let good = minimal_valid_pdf();
        let text = String::from_utf8(good).unwrap();
        let broken = text.replacen("startxref\n", "startxref\n999999\n", 1);
        broken.into_bytes()
    }

    fn qpdf_path_or_skip() -> Option<PathBuf> {
        match detect(&QPDF, None, Duration::from_secs(5)) {
            DetectionOutcome::Available { path, .. } => Some(path),
            _ => {
                eprintln!("qpdf not installed on this machine; skipping repair test");
                None
            }
        }
    }

    #[test]
    fn qpdf_repairs_a_broken_startxref() {
        let Some(qpdf_path) = qpdf_path_or_skip() else {
            return;
        };
        let broken = pdf_with_broken_startxref();

        // Confirm the premise: native parsing genuinely fails on this input.
        assert!(
            crate::pdf::load_from_bytes(&broken).is_err(),
            "fixture must actually be broken for lopdf"
        );

        let repaired = repair_with_qpdf(
            &qpdf_path,
            &broken,
            QpdfRepairOptions::default(),
            TEST_TIMEOUT,
        )
        .expect("qpdf repair");
        let doc = crate::pdf::load_from_bytes(&repaired).expect("repaired file must parse");
        assert_eq!(doc.get_pages().len(), 1);
    }

    #[test]
    fn load_with_optional_repair_recovers_a_broken_file_when_qpdf_is_available() {
        let Some(qpdf_path) = qpdf_path_or_skip() else {
            return;
        };
        let broken = pdf_with_broken_startxref();

        let (doc, repaired) = load_with_optional_repair(&broken, Some(&qpdf_path), TEST_TIMEOUT)
            .expect("should recover via repair");
        assert!(repaired);
        assert_eq!(doc.get_pages().len(), 1);
    }

    #[test]
    fn repair_bytes_if_needed_returns_parseable_bytes_for_a_broken_file() {
        let Some(qpdf_path) = qpdf_path_or_skip() else {
            return;
        };
        let broken = pdf_with_broken_startxref();
        let (bytes, repaired) = repair_bytes_if_needed(&broken, Some(&qpdf_path), TEST_TIMEOUT)
            .expect("should recover via repair");
        assert!(repaired);
        let doc = crate::pdf::load_from_bytes(&bytes).expect("repaired bytes must parse");
        assert_eq!(doc.get_pages().len(), 1);
    }

    #[test]
    fn repair_bytes_if_needed_passes_through_a_healthy_file_unchanged() {
        let good = minimal_valid_pdf();
        let (bytes, repaired) = repair_bytes_if_needed(&good, None, TEST_TIMEOUT).unwrap();
        assert!(!repaired);
        assert_eq!(bytes, good);
    }

    #[test]
    fn load_with_optional_repair_passes_through_a_healthy_file_unrepaired() {
        let good = minimal_valid_pdf();
        let (doc, repaired) =
            load_with_optional_repair(&good, None, TEST_TIMEOUT).expect("healthy file loads");
        assert!(!repaired);
        assert_eq!(doc.get_pages().len(), 1);
    }

    #[test]
    fn load_with_optional_repair_without_qpdf_surfaces_the_original_parse_error() {
        let broken = pdf_with_broken_startxref();
        let err = load_with_optional_repair(&broken, None, TEST_TIMEOUT).unwrap_err();
        assert!(
            matches!(err, RepairOrLoadError::ParseFailed(_)),
            "repair was never attempted (no qpdf_path given), so the error must say so, not \
             claim a repair attempt that never happened: {err}"
        );
    }

    #[test]
    fn repair_with_a_nonexistent_qpdf_path_reports_tool_unavailable() {
        let broken = pdf_with_broken_startxref();
        let err = repair_with_qpdf(
            Path::new("/definitely/not/a/real/qpdf/binary"),
            &broken,
            QpdfRepairOptions::default(),
            TEST_TIMEOUT,
        )
        .unwrap_err();
        assert!(matches!(err, RepairError::ToolUnavailable(_)));
    }

    #[test]
    fn a_failed_repair_reports_qpdf_own_diagnostics_not_just_the_original_error() {
        // Not a PDF at all — native parsing fails, and qpdf's own repair
        // attempt fails too (there is no xref to reconstruct: "unable to
        // find trailer dictionary while recovering damaged file"), so the
        // caller must see qpdf's own diagnostic, not a bare suggestion to
        // "try qpdf" for a tool that already ran and already explained why
        // it couldn't help.
        let Some(qpdf_path) = qpdf_path_or_skip() else {
            return;
        };
        let garbage = b"this is not a PDF file at all, just some plain bytes".to_vec();
        assert!(
            crate::pdf::load_from_bytes(&garbage).is_err(),
            "fixture must actually fail native parsing"
        );

        let err = repair_bytes_if_needed(&garbage, Some(&qpdf_path), TEST_TIMEOUT).unwrap_err();
        match err {
            RepairOrLoadError::RepairAttemptedAndFailed {
                qpdf_diagnostics, ..
            } => {
                assert!(
                    !qpdf_diagnostics.trim().is_empty(),
                    "must carry qpdf's own diagnostics, not an empty string"
                );
            }
            other => panic!(
                "expected RepairAttemptedAndFailed carrying qpdf's own diagnostics, got: {other}"
            ),
        }
    }

    // --- bounded timeout and concurrent draining (repair_with_qpdf) ---

    /// Writes an executable shell script standing in for a "tool" that
    /// ignores whatever arguments it's called with, for exercising
    /// [`run_with_timeout_bytes`] without depending on real qpdf/Ghostscript
    /// behaviour.
    #[cfg(unix)]
    fn fake_shell_tool(script_body: &str) -> tempfile::TempPath {
        use std::os::unix::fs::PermissionsExt;
        let mut file = tempfile::Builder::new()
            .prefix("fake-tool-")
            .tempfile()
            .expect("create fake tool script");
        file.write_all(format!("#!/bin/sh\n{script_body}\n").as_bytes())
            .expect("write fake tool script");
        file.flush().expect("flush fake tool script");
        let path = file.into_temp_path();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
            .expect("make fake tool script executable");
        path
    }

    #[cfg(unix)]
    #[test]
    fn qpdf_repair_times_out_rather_than_hanging_forever() {
        let script = fake_shell_tool("sleep 5");
        let broken = pdf_with_broken_startxref();
        let start = Instant::now();
        let err = repair_with_qpdf(
            &script,
            &broken,
            QpdfRepairOptions::default(),
            Duration::from_millis(150),
        )
        .unwrap_err();
        assert!(
            start.elapsed() < Duration::from_secs(2),
            "must not wait for the full sleep duration"
        );
        assert!(matches!(err, RepairError::Timeout { .. }), "{err}");
    }

    #[cfg(unix)]
    #[test]
    fn qpdf_repair_captures_output_larger_than_a_pipe_buffer_without_deadlock() {
        // Writes 200KB to stdout — well past a typical 64KB OS pipe buffer —
        // and exits successfully. With the old "read only after try_wait
        // observes exit" approach this would deadlock: the child blocks on
        // its own `write()` once the pipe fills, and the parent's loop only
        // calls `try_wait`, never reads, so the child never gets to exit.
        let script = fake_shell_tool("head -c 200000 /dev/zero");
        let broken = pdf_with_broken_startxref();
        let start = Instant::now();
        let repaired =
            repair_with_qpdf(&script, &broken, QpdfRepairOptions::default(), TEST_TIMEOUT)
                .expect("large output must not deadlock the read");
        assert_eq!(repaired.len(), 200_000);
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "draining concurrently must not be slow: {:?}",
            start.elapsed()
        );
    }

    // --- bounded timeout and concurrent draining (flatten_with_ghostscript) ---

    #[cfg(unix)]
    #[test]
    fn ghostscript_flatten_times_out_rather_than_hanging_forever() {
        let script = fake_shell_tool("sleep 5");
        let start = Instant::now();
        let err = flatten_with_ghostscript(
            &script,
            b"whatever",
            &GhostscriptFlattenOptions::default(),
            Duration::from_millis(150),
        )
        .unwrap_err();
        assert!(
            start.elapsed() < Duration::from_secs(2),
            "must not wait for the full sleep duration"
        );
        assert!(matches!(err, GhostscriptError::Timeout { .. }), "{err}");
    }

    #[cfg(unix)]
    #[test]
    fn ghostscript_flatten_captures_large_stderr_without_deadlock() {
        // Writes 200KB of diagnostics to stderr and exits non-zero — the
        // same deadlock risk as the qpdf stdout case above, on the other
        // pipe.
        let script = fake_shell_tool("head -c 200000 /dev/zero 1>&2; exit 1");
        let start = Instant::now();
        let err = flatten_with_ghostscript(
            &script,
            b"whatever",
            &GhostscriptFlattenOptions::default(),
            TEST_TIMEOUT,
        )
        .unwrap_err();
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "draining concurrently must not be slow: {:?}",
            start.elapsed()
        );
        match err {
            GhostscriptError::Failed { stderr } => assert_eq!(stderr.len(), 200_000),
            other => panic!("expected Failed with the full stderr captured, got: {other}"),
        }
    }

    // --- Ghostscript argument construction ---

    #[test]
    fn ghostscript_args_preserve_geometry_and_embed_fonts() {
        let args = build_ghostscript_args(
            Path::new("/in.pdf"),
            Path::new("/out.pdf"),
            &GhostscriptFlattenOptions::default(),
        )
        .unwrap();
        assert!(args.contains(&"-dEmbedAllFonts=true".to_string()));
        assert!(args.contains(&"-dDownsampleColorImages=false".to_string()));
        assert!(args.contains(&"-dAutoRotatePages=/None".to_string()));
        assert_eq!(args.last().unwrap(), "/in.pdf");
        assert!(args.iter().any(|a| a == "-sOutputFile=/out.pdf"));
    }

    #[test]
    fn cmyk_conversion_without_a_profile_is_refused() {
        let options = GhostscriptFlattenOptions {
            target_color_space: Some(ColorSpaceTarget::Cmyk),
            icc_profile_path: None,
        };
        let err = build_ghostscript_args(Path::new("/in.pdf"), Path::new("/out.pdf"), &options)
            .unwrap_err();
        assert!(matches!(err, GhostscriptError::MissingIccProfile));
    }

    #[test]
    fn cmyk_conversion_with_a_profile_names_it_in_the_arguments() {
        let options = GhostscriptFlattenOptions {
            target_color_space: Some(ColorSpaceTarget::Cmyk),
            icc_profile_path: Some(PathBuf::from("/profiles/GRACoL2006.icc")),
        };
        let args =
            build_ghostscript_args(Path::new("/in.pdf"), Path::new("/out.pdf"), &options).unwrap();
        assert!(args.iter().any(|a| a.contains("GRACoL2006.icc")));
        assert!(args.contains(&"-sProcessColorModel=DeviceCMYK".to_string()));
    }

    #[test]
    fn flatten_with_a_nonexistent_ghostscript_path_reports_tool_unavailable() {
        let err = flatten_with_ghostscript(
            Path::new("/definitely/not/a/real/gs/binary"),
            b"whatever",
            &GhostscriptFlattenOptions::default(),
            TEST_TIMEOUT,
        )
        .unwrap_err();
        assert!(matches!(err, GhostscriptError::ToolUnavailable(_)));
    }

    #[test]
    fn ghostscript_is_reported_unavailable_on_this_test_machine_or_detected_if_present() {
        // Documents the actual state rather than asserting either way: this
        // suite is designed to pass whether or not Ghostscript happens to be
        // installed on the machine running it (see task 9.12's spirit).
        let outcome = detect(&GHOSTSCRIPT, None, Duration::from_secs(5));
        match outcome {
            DetectionOutcome::Available { version, .. } => {
                eprintln!("Ghostscript {version} detected")
            }
            other => eprintln!("Ghostscript unavailable: {other:?}"),
        }
    }

    // --- geometry-preservation assertion (synthetic before/after) ---

    fn doc_with_page_box(width: f64, height: f64) -> lopdf::Document {
        let mut doc = lopdf::Document::with_version("1.7");
        let pages_id = doc.new_object_id();
        let page_id = doc.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => lopdf::Object::Reference(pages_id),
            "MediaBox" => lopdf::Object::Array(vec![0.0.into(), 0.0.into(), width.into(), height.into()]),
            "TrimBox" => lopdf::Object::Array(vec![9.0.into(), 9.0.into(), (width - 9.0).into(), (height - 9.0).into()]),
        });
        let pages = dictionary! { "Type" => "Pages", "Kids" => vec![lopdf::Object::Reference(page_id)], "Count" => 1 };
        doc.objects
            .insert(pages_id, lopdf::Object::Dictionary(pages));
        let catalog_id = doc.add_object(
            dictionary! { "Type" => "Catalog", "Pages" => lopdf::Object::Reference(pages_id) },
        );
        doc.trailer
            .set("Root", lopdf::Object::Reference(catalog_id));
        doc
    }

    #[test]
    fn identical_geometry_passes() {
        let before = doc_with_page_box(450.0, 666.0);
        let after = doc_with_page_box(450.0, 666.0);
        assert!(assert_geometry_preserved(&before, &after).is_ok());
    }

    #[test]
    fn changed_media_box_is_caught() {
        let before = doc_with_page_box(450.0, 666.0);
        let after = doc_with_page_box(451.0, 666.0);
        let err = assert_geometry_preserved(&before, &after).unwrap_err();
        assert!(matches!(err, GhostscriptError::GeometryChanged(_)));
    }

    #[test]
    fn changed_page_count_is_caught() {
        let before = doc_with_page_box(450.0, 666.0);
        let mut after = lopdf::Document::with_version("1.7");
        let pages_id = after.new_object_id();
        let pages =
            dictionary! { "Type" => "Pages", "Kids" => Vec::<lopdf::Object>::new(), "Count" => 0 };
        after
            .objects
            .insert(pages_id, lopdf::Object::Dictionary(pages));
        let catalog_id = after.add_object(
            dictionary! { "Type" => "Catalog", "Pages" => lopdf::Object::Reference(pages_id) },
        );
        after
            .trailer
            .set("Root", lopdf::Object::Reference(catalog_id));

        let err = assert_geometry_preserved(&before, &after).unwrap_err();
        assert!(matches!(err, GhostscriptError::GeometryChanged(_)));
    }

    #[test]
    fn tiny_floating_point_noise_is_tolerated() {
        let before = doc_with_page_box(450.0, 666.0);
        let after = doc_with_page_box(450.001, 666.0);
        assert!(assert_geometry_preserved(&before, &after).is_ok());
    }

    /// A document whose page's `MediaBox` is four elements, each of which is
    /// an indirect reference to an object that either resolves to a number
    /// (`resolvable = true`) or points nowhere at all (`resolvable =
    /// false`, the broken-indirect-element case this test module exists to
    /// catch).
    fn doc_with_indirect_media_box(width: f64, height: f64, resolvable: bool) -> lopdf::Document {
        let mut doc = lopdf::Document::with_version("1.7");
        let pages_id = doc.new_object_id();
        let coords = [0.0, 0.0, width, height];
        let media_box: Vec<lopdf::Object> = coords
            .iter()
            .map(|&v| {
                if resolvable {
                    lopdf::Object::Reference(doc.add_object(lopdf::Object::Real(v as f32)))
                } else {
                    // A reference to an object ID never inserted anywhere
                    // in this document — dereferencing it fails, the same
                    // shape as a box element a lenient viewer might guess
                    // at but this tool must not.
                    lopdf::Object::Reference((9999, 0))
                }
            })
            .collect();
        let page_id = doc.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => lopdf::Object::Reference(pages_id),
            "MediaBox" => lopdf::Object::Array(media_box),
        });
        let pages = dictionary! { "Type" => "Pages", "Kids" => vec![lopdf::Object::Reference(page_id)], "Count" => 1 };
        doc.objects
            .insert(pages_id, lopdf::Object::Dictionary(pages));
        let catalog_id = doc.add_object(
            dictionary! { "Type" => "Catalog", "Pages" => lopdf::Object::Reference(pages_id) },
        );
        doc.trailer
            .set("Root", lopdf::Object::Reference(catalog_id));
        doc
    }

    #[test]
    fn indirect_box_elements_that_resolve_are_compared_correctly() {
        let before = doc_with_indirect_media_box(450.0, 666.0, true);
        let after = doc_with_indirect_media_box(450.0, 666.0, true);
        assert!(assert_geometry_preserved(&before, &after).is_ok());
    }

    #[test]
    fn vacuous_empty_box_comparison_is_rejected() {
        // Both sides' MediaBox is four indirect references that resolve to
        // nothing. The old comparison built each side's numbers with
        // `.filter_map(|o| o.as_float().ok())`: every element fails to
        // parse (it's a `Reference`, not a number, and was never
        // dereferenced), so both sides collect to an empty `Vec<f64>`,
        // `0 == 0` holds, and `.all()` over an empty iterator is vacuously
        // `true` — the check passed having compared nothing. It must fail
        // instead, even when — especially when — both sides look identical
        // because both are equally unreadable.
        let doc = doc_with_indirect_media_box(450.0, 666.0, false);
        let err = assert_geometry_preserved(&doc, &doc).unwrap_err();
        assert!(matches!(err, GhostscriptError::GeometryChanged(_)), "{err}");
    }

    #[test]
    fn a_box_array_with_the_wrong_number_of_elements_is_rejected() {
        let mut doc = lopdf::Document::with_version("1.7");
        let pages_id = doc.new_object_id();
        let page_id = doc.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => lopdf::Object::Reference(pages_id),
            // Five numbers, not four — malformed, but must be caught
            // rather than silently zipped against a well-formed box.
            "MediaBox" => lopdf::Object::Array(vec![0.0.into(), 0.0.into(), 450.0.into(), 666.0.into(), 1.0.into()]),
        });
        let pages = dictionary! { "Type" => "Pages", "Kids" => vec![lopdf::Object::Reference(page_id)], "Count" => 1 };
        doc.objects
            .insert(pages_id, lopdf::Object::Dictionary(pages));
        let catalog_id = doc.add_object(
            dictionary! { "Type" => "Catalog", "Pages" => lopdf::Object::Reference(pages_id) },
        );
        doc.trailer
            .set("Root", lopdf::Object::Reference(catalog_id));

        let well_formed = doc_with_page_box(450.0, 666.0);
        let err = assert_geometry_preserved(&doc, &well_formed).unwrap_err();
        assert!(matches!(err, GhostscriptError::GeometryChanged(_)), "{err}");
    }
}
