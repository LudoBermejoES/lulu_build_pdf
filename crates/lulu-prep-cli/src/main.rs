use camino::{Utf8Path, Utf8PathBuf};
use clap::{Parser, Subcommand, ValueEnum};
use std::io::Write as _;
use std::path::PathBuf;

use lulu_prep::catalog::{Binding, CatalogEntry};
use lulu_prep::external_tools::{GhostscriptFlattenOptions, GHOSTSCRIPT, QPDF};
use lulu_prep::normalize::{FitMode, NormalizeOptions};
use lulu_prep::pipeline::PipelineOptions;
use lulu_prep::report::Report;

use lulu_prep_cli::commands::{self, BookCommandError, CoverCommandError, CoverSource};
use lulu_prep_cli::config::{self, ConfigFile, EnvVars, Flags};
use lulu_prep_cli::exit_code::{exit_code_for_report, ExitCode};
use lulu_prep_cli::output_paths::{self, OutputRole};
use lulu_prep_cli::product_selection::{
    resolve_product, ComponentFilter, ProductSelector, SelectionError,
};
use lulu_prep_cli::products_command::format_products_table;
use lulu_prep_cli::spine_command::format_spine_report;

#[derive(Copy, Clone, Debug, ValueEnum)]
enum CliFitMode {
    Center,
    ScaleToBleed,
    StretchMargins,
}

impl From<CliFitMode> for FitMode {
    fn from(mode: CliFitMode) -> Self {
        match mode {
            CliFitMode::Center => FitMode::Center,
            CliFitMode::ScaleToBleed => FitMode::ScaleToBleed,
            CliFitMode::StretchMargins => FitMode::StretchMargins,
        }
    }
}

#[derive(Parser, Debug)]
#[command(
    name = "lulu-prep",
    about = "Prepare an arbitrary PDF for print submission to Lulu"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,

    /// pod_package_id, dotted or legacy form.
    #[arg(long, global = true)]
    sku: Option<String>,

    /// Trim size in inches, e.g. "6x9" (component selection; no default — matches any trim if omitted).
    #[arg(long, global = true)]
    trim: Option<String>,
    /// One of perfect, coil, saddle-stitch, case-wrap, linen-wrap, wire-o (component selection; no default).
    #[arg(long, global = true)]
    binding: Option<String>,
    /// "bw" or "fc" (component selection; no default).
    #[arg(long, global = true)]
    ink: Option<String>,
    /// "standard" or "premium", substring match (component selection; no default).
    #[arg(long, global = true)]
    quality: Option<String>,
    /// Paper description, substring match (component selection; no default).
    #[arg(long, global = true)]
    paper: Option<String>,
    /// Lamination description, substring match (component selection; no default).
    #[arg(long, global = true)]
    lamination: Option<String>,

    /// How rotation-baked interior/cover content is placed on the required page. Default: center.
    #[arg(long, global = true, value_enum)]
    fit_mode: Option<CliFitMode>,
    /// Directory default output paths are written into. Default: the current directory (".").
    #[arg(long, global = true)]
    output_dir: Option<String>,
    /// Promote warnings to a non-zero exit code. Default: false.
    #[arg(long, global = true)]
    strict: bool,
    /// Disable colour in text output (also respects the NO_COLOR environment variable). Default: false.
    #[arg(long, global = true)]
    no_color: bool,
    /// Minimum gutter width to advise, in inches. Default: 0.0 in.
    #[arg(long, global = true)]
    gutter_floor_in: Option<f64>,

    /// Print the effective configuration (value + source for every setting) and exit.
    #[arg(long, global = true)]
    print_config: bool,

    /// Emit the report as JSON instead of human-readable text.
    #[arg(long, global = true)]
    json: bool,
    /// Write the report to this path instead of stdout.
    #[arg(long, global = true)]
    report_out: Option<PathBuf>,

    /// Overwrite an existing output file.
    #[arg(long, global = true)]
    force: bool,
    /// Perform all analysis and print intended output paths, writing nothing.
    #[arg(long, global = true)]
    dry_run: bool,

    /// Fixed 32-hex-character document identifier for byte-identical repeat runs.
    #[arg(long, global = true)]
    doc_id: Option<String>,
    /// Fixed PDF creation date (`D:YYYYMMDDHHmmSSZ`) for byte-identical repeat runs.
    #[arg(long, global = true)]
    creation_date: Option<String>,

    /// Apply the gutter shift (a source already laid out with its own gutter
    /// would otherwise be double-shifted). Default: false.
    #[arg(long, global = true)]
    gutter: bool,
    /// Split each page down its vertical centre into two pages (left then
    /// right) before geometry — for a source imposed as two-up spreads.
    /// Never inferred from aspect ratio; a landscape source without this
    /// flag is reported, not split. Default: false.
    #[arg(long, global = true)]
    split_spreads: bool,
    /// Flatten the output through Ghostscript after normalizing. Default: false.
    #[arg(long, global = true)]
    flatten: bool,
    /// Path to the qpdf binary. Default: discovered on PATH.
    #[arg(long, global = true)]
    qpdf_path: Option<PathBuf>,
    /// Path to the Ghostscript binary. Default: discovered on PATH.
    #[arg(long, global = true)]
    gs_path: Option<PathBuf>,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Preflight an existing PDF against a product; writes no PDF.
    Check {
        /// Path to the PDF to check.
        input: PathBuf,
    },
    /// Normalize an interior PDF for a product.
    Interior {
        /// Path to the interior PDF to normalize.
        input: PathBuf,
        /// Output path. Default: "<input stem>-interior.pdf" in --output-dir.
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
    /// Generate a cover template, or fit supplied cover artwork.
    Cover {
        /// Existing single-page cover artwork to fit; omit to generate a design-aid template.
        #[arg(long)]
        supplied: Option<PathBuf>,
        /// Interior page count this cover is for (required; no default).
        #[arg(long)]
        pages: u32,
        /// Output path. Default: "<sku or supplied-file stem>-cover.pdf" in --output-dir.
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
    /// Normalize an interior and build its matching cover in one pass.
    Book {
        /// Path to the interior PDF to normalize.
        input: PathBuf,
        /// Existing single-page cover artwork to fit; omit to generate a design-aid template.
        #[arg(long)]
        supplied_cover: Option<PathBuf>,
        /// Interior output path. Default: "<input stem>-interior.pdf" in --output-dir.
        #[arg(long)]
        interior_output: Option<PathBuf>,
        /// Cover output path. Default: "<input stem>-cover.pdf" in --output-dir.
        #[arg(long)]
        cover_output: Option<PathBuf>,
    },
    /// Search and describe the embedded catalog, using the top-level component flags as filters.
    Products,
    /// Print the spine width and cover canvas for a product and page count, with no PDF input.
    Spine {
        /// Page count to compute spine width and cover canvas for (required; no default).
        #[arg(long)]
        pages: u32,
    },
}

fn parse_binding(s: &str) -> Option<Binding> {
    match s.to_lowercase().replace(['_', ' '], "-").as_str() {
        "perfect" => Some(Binding::Perfect),
        "coil" => Some(Binding::Coil),
        "saddle-stitch" => Some(Binding::SaddleStitch),
        "case-wrap" => Some(Binding::CaseWrap),
        "linen-wrap" => Some(Binding::LinenWrap),
        "wire-o" => Some(Binding::WireO),
        _ => None,
    }
}

fn parse_trim(s: &str) -> Option<(f64, f64)> {
    let (w, h) = s.split_once('x')?;
    Some((w.trim().parse().ok()?, h.trim().parse().ok()?))
}

fn build_selector(cli: &Cli) -> Result<ProductSelector, String> {
    if let Some(sku) = &cli.sku {
        return Ok(ProductSelector::Sku(sku.clone()));
    }
    let trim_in = match &cli.trim {
        Some(s) => Some(
            parse_trim(s).ok_or_else(|| format!("invalid --trim '{s}', expected e.g. '6x9'"))?,
        ),
        None => None,
    };
    let binding = match &cli.binding {
        Some(s) => Some(parse_binding(s).ok_or_else(|| format!("invalid --binding '{s}'"))?),
        None => None,
    };
    Ok(ProductSelector::Components(ComponentFilter {
        trim_in,
        binding,
        ink: cli.ink.clone(),
        quality: cli.quality.clone(),
        paper: cli.paper.clone(),
        lamination: cli.lamination.clone(),
    }))
}

fn load_config_file(path: &Utf8Path) -> Option<ConfigFile> {
    let text = std::fs::read_to_string(path).ok()?;
    match ConfigFile::parse(&text) {
        Ok(file) => Some(file),
        Err(e) => {
            eprintln!("warning: ignoring {path}: {e}");
            None
        }
    }
}

fn user_config_path() -> Option<Utf8PathBuf> {
    let base = std::env::var("XDG_CONFIG_HOME")
        .ok()
        .or_else(|| std::env::var("HOME").ok().map(|h| format!("{h}/.config")))?;
    Some(
        Utf8PathBuf::from(base)
            .join("lulu-prep")
            .join("config.toml"),
    )
}

fn build_effective_config(cli: &Cli) -> config::EffectiveConfig {
    let flags = Flags {
        fit_mode: cli.fit_mode.map(Into::into),
        output_dir: cli.output_dir.clone(),
        strict: cli.strict.then_some(true),
        no_color: cli.no_color.then_some(true),
        gutter_floor_in: cli.gutter_floor_in,
    };
    let env = EnvVars::from_process();
    let project = load_config_file(Utf8Path::new("lulu-prep.toml"));
    let user = user_config_path().and_then(|p| load_config_file(&p));
    config::resolve_config(&flags, &env, project.as_ref(), user.as_ref())
}

fn parse_doc_id(hex: &str) -> Result<[u8; 16], String> {
    if hex.len() != 32 {
        return Err(format!(
            "--doc-id must be exactly 32 hex characters, got {}",
            hex.len()
        ));
    }
    let mut out = [0u8; 16];
    for (i, chunk) in out.iter_mut().enumerate() {
        *chunk = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16)
            .map_err(|_| format!("--doc-id is not valid hex: '{hex}'"))?;
    }
    Ok(out)
}

/// Applies `--doc-id`/`--creation-date` to `bytes` if both were supplied, for
/// byte-identical repeat runs (`specs/cli/spec.md`, "Deterministic PDF identity").
fn apply_determinism(cli: &Cli, bytes: Vec<u8>) -> Result<Vec<u8>, String> {
    let (Some(doc_id_hex), Some(creation_date)) = (&cli.doc_id, &cli.creation_date) else {
        return Ok(bytes);
    };
    let doc_id = parse_doc_id(doc_id_hex)?;
    let mut doc = lulu_prep::pdf::load_from_bytes(&bytes).map_err(|e| e.to_string())?;
    lulu_prep::pdf::apply_deterministic_identity(&mut doc, doc_id, creation_date);
    let mut out = Vec::new();
    doc.save_to(&mut out).map_err(|e| e.to_string())?;
    Ok(out)
}

fn print_report(cli: &Cli, report: &Report) -> std::io::Result<()> {
    let text = if cli.json {
        report.to_json().expect("Report always serializes")
    } else {
        report.to_text()
    };
    match &cli.report_out {
        Some(path) => std::fs::write(path, text)?,
        None => {
            let stdout = std::io::stdout();
            let mut handle = stdout.lock();
            writeln!(handle, "{text}")?;
        }
    }
    Ok(())
}

fn write_output(
    cli: &Cli,
    input: &Utf8Path,
    role: OutputRole,
    output_dir: &Utf8Path,
    explicit: Option<&PathBuf>,
    bytes: &[u8],
) -> Result<Utf8PathBuf, ExitCode> {
    let path = match explicit {
        Some(p) => Utf8PathBuf::from_path_buf(p.clone()).expect("output path must be valid UTF-8"),
        None => output_paths::default_output_path(input, role, output_dir),
    };
    if let Err(refused) = output_paths::check_overwrite(&path, cli.force, |p| p.exists()) {
        eprintln!("{refused}");
        return Err(ExitCode::InvalidUsage);
    }
    if cli.dry_run {
        eprintln!("(dry run) would write {path}");
        return Ok(path);
    }
    if let Err(e) = std::fs::write(&path, bytes) {
        eprintln!("could not write {path}: {e}");
        return Err(ExitCode::IoOrParse);
    }
    eprintln!("wrote {path}");
    Ok(path)
}

fn resolve_input_utf8(path: &std::path::Path) -> Utf8PathBuf {
    Utf8PathBuf::from_path_buf(path.to_path_buf()).expect("input path must be valid UTF-8")
}

fn pipeline_options(cli: &Cli) -> PipelineOptions {
    PipelineOptions {
        qpdf_path: cli.qpdf_path.clone(),
        gs_path: cli.gs_path.clone(),
        flatten: cli.flatten.then_some(GhostscriptFlattenOptions {
            target_color_space: None,
            icc_profile_path: None,
        }),
        ..PipelineOptions::new()
    }
}

fn main() {
    let cli = Cli::parse();

    if cli.print_config {
        for line in build_effective_config(&cli).display_lines() {
            println!("{line}");
        }
        std::process::exit(ExitCode::Clean.as_i32());
    }

    let effective = build_effective_config(&cli);
    let fit_mode: FitMode = effective.fit_mode.value;
    let output_dir = Utf8PathBuf::from(effective.output_dir.value.clone());
    let strict = effective.strict.value;

    let code = run(&cli, fit_mode, &output_dir, strict);
    std::process::exit(code.as_i32());
}

fn run(cli: &Cli, fit_mode: FitMode, output_dir: &Utf8Path, strict: bool) -> ExitCode {
    match &cli.command {
        Command::Products => {
            let filter = ComponentFilter {
                trim_in: cli.trim.as_deref().and_then(parse_trim),
                binding: cli.binding.as_deref().and_then(parse_binding),
                ink: cli.ink.clone(),
                quality: cli.quality.clone(),
                paper: cli.paper.clone(),
                lamination: cli.lamination.clone(),
            };
            let entries = lulu_prep_cli::product_selection::search_catalog(&filter);
            println!("{}", format_products_table(&entries));
            ExitCode::Clean
        }
        Command::Spine { pages } => {
            let product = match resolve(cli) {
                Ok(p) => p,
                Err(code) => return code,
            };
            match format_spine_report(product, *pages) {
                Ok(text) => {
                    println!("{text}");
                    ExitCode::Clean
                }
                Err(e) => {
                    eprintln!("{e}");
                    ExitCode::InvalidUsage
                }
            }
        }
        Command::Check { input } => {
            let product = match resolve(cli) {
                Ok(p) => p,
                Err(code) => return code,
            };
            let bytes = match std::fs::read(input) {
                Ok(b) => b,
                Err(e) => {
                    eprintln!("could not read {}: {e}", input.display());
                    return ExitCode::IoOrParse;
                }
            };
            let outcome = commands::run_check(&bytes, product);
            if print_report(cli, &outcome.report).is_err() {
                return ExitCode::IoOrParse;
            }
            exit_code_for_report(&outcome.report, strict)
        }
        Command::Interior { input, output } => {
            let product = match resolve(cli) {
                Ok(p) => p,
                Err(code) => return code,
            };
            let bytes = match std::fs::read(input) {
                Ok(b) => b,
                Err(e) => {
                    eprintln!("could not read {}: {e}", input.display());
                    return ExitCode::IoOrParse;
                }
            };
            let options = NormalizeOptions {
                fit_mode,
                apply_gutter: cli.gutter,
                split_spreads: cli.split_spreads,
            };
            let outcome =
                match commands::run_interior(&bytes, product, options, &pipeline_options(cli)) {
                    Ok(o) => o,
                    Err(e) => return report_pipeline_error(&e),
                };
            let output_bytes = match apply_determinism(cli, outcome.output_bytes) {
                Ok(b) => b,
                Err(e) => {
                    eprintln!("{e}");
                    return ExitCode::InvalidUsage;
                }
            };
            if print_report(cli, &outcome.report).is_err() {
                return ExitCode::IoOrParse;
            }
            let input_utf8 = resolve_input_utf8(input);
            if write_output(
                cli,
                &input_utf8,
                OutputRole::Interior,
                output_dir,
                output.as_ref(),
                &output_bytes,
            )
            .is_err()
            {
                return ExitCode::InvalidUsage;
            }
            exit_code_for_report(&outcome.report, strict)
        }
        Command::Cover {
            supplied,
            pages,
            output,
        } => {
            let product = match resolve(cli) {
                Ok(p) => p,
                Err(code) => return code,
            };
            let supplied_bytes;
            let source = match supplied {
                Some(path) => {
                    supplied_bytes = match std::fs::read(path) {
                        Ok(b) => b,
                        Err(e) => {
                            eprintln!("could not read {}: {e}", path.display());
                            return ExitCode::IoOrParse;
                        }
                    };
                    CoverSource::Supplied {
                        bytes: &supplied_bytes,
                        fit_mode,
                    }
                }
                None => CoverSource::Template,
            };
            let outcome = match commands::run_cover(product, *pages, source) {
                Ok(o) => o,
                Err(e) => return report_cover_error(&e),
            };
            let output_bytes = match apply_determinism(cli, outcome.output_bytes) {
                Ok(b) => b,
                Err(e) => {
                    eprintln!("{e}");
                    return ExitCode::InvalidUsage;
                }
            };
            if print_report(cli, &outcome.report).is_err() {
                return ExitCode::IoOrParse;
            }
            let input_name = supplied
                .clone()
                .unwrap_or_else(|| PathBuf::from(&product.sku));
            let input_utf8 = resolve_input_utf8(&input_name);
            if write_output(
                cli,
                &input_utf8,
                OutputRole::Cover,
                output_dir,
                output.as_ref(),
                &output_bytes,
            )
            .is_err()
            {
                return ExitCode::InvalidUsage;
            }
            exit_code_for_report(&outcome.report, strict)
        }
        Command::Book {
            input,
            supplied_cover,
            interior_output,
            cover_output,
        } => {
            let product = match resolve(cli) {
                Ok(p) => p,
                Err(code) => return code,
            };
            let bytes = match std::fs::read(input) {
                Ok(b) => b,
                Err(e) => {
                    eprintln!("could not read {}: {e}", input.display());
                    return ExitCode::IoOrParse;
                }
            };
            let supplied_bytes;
            let cover_source = match supplied_cover {
                Some(path) => {
                    supplied_bytes = match std::fs::read(path) {
                        Ok(b) => b,
                        Err(e) => {
                            eprintln!("could not read {}: {e}", path.display());
                            return ExitCode::IoOrParse;
                        }
                    };
                    CoverSource::Supplied {
                        bytes: &supplied_bytes,
                        fit_mode,
                    }
                }
                None => CoverSource::Template,
            };
            let options = NormalizeOptions {
                fit_mode,
                apply_gutter: cli.gutter,
                split_spreads: cli.split_spreads,
            };
            let outcome = match commands::run_book(
                &bytes,
                product,
                options,
                &pipeline_options(cli),
                cover_source,
            ) {
                Ok(o) => o,
                Err(e) => return report_book_error(&e),
            };

            let interior_bytes = match apply_determinism(cli, outcome.interior.output_bytes) {
                Ok(b) => b,
                Err(e) => {
                    eprintln!("{e}");
                    return ExitCode::InvalidUsage;
                }
            };
            let cover_bytes = match apply_determinism(cli, outcome.cover.output_bytes) {
                Ok(b) => b,
                Err(e) => {
                    eprintln!("{e}");
                    return ExitCode::InvalidUsage;
                }
            };

            if print_report(cli, &outcome.interior.report).is_err() {
                return ExitCode::IoOrParse;
            }
            if print_report(cli, &outcome.cover.report).is_err() {
                return ExitCode::IoOrParse;
            }

            let input_utf8 = resolve_input_utf8(input);
            if write_output(
                cli,
                &input_utf8,
                OutputRole::Interior,
                output_dir,
                interior_output.as_ref(),
                &interior_bytes,
            )
            .is_err()
            {
                return ExitCode::InvalidUsage;
            }
            if write_output(
                cli,
                &input_utf8,
                OutputRole::Cover,
                output_dir,
                cover_output.as_ref(),
                &cover_bytes,
            )
            .is_err()
            {
                return ExitCode::InvalidUsage;
            }

            let interior_code = exit_code_for_report(&outcome.interior.report, strict);
            let cover_code = exit_code_for_report(&outcome.cover.report, strict);
            if interior_code == ExitCode::BlockingFindings
                || cover_code == ExitCode::BlockingFindings
            {
                ExitCode::BlockingFindings
            } else {
                ExitCode::Clean
            }
        }
    }
}

fn resolve(cli: &Cli) -> Result<&'static CatalogEntry, ExitCode> {
    let selector = match build_selector(cli) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("{e}");
            return Err(ExitCode::InvalidUsage);
        }
    };
    match resolve_product(&selector) {
        Ok(entry) => Ok(entry),
        Err(SelectionError::NoComponentsGiven) => {
            eprintln!("no product selector given: pass --sku or at least one of --trim/--binding/--ink/--quality/--paper/--lamination");
            Err(ExitCode::InvalidUsage)
        }
        Err(e) => {
            eprintln!("{e}");
            Err(ExitCode::InvalidUsage)
        }
    }
}

fn report_pipeline_error(e: &lulu_prep::pipeline::PipelineError) -> ExitCode {
    eprintln!("{e}");
    match e {
        lulu_prep::pipeline::PipelineError::MissingTool { .. } => ExitCode::MissingToolOrCredential,
        lulu_prep::pipeline::PipelineError::Load(_) => ExitCode::IoOrParse,
        _ => ExitCode::IoOrParse,
    }
}

fn report_cover_error(e: &CoverCommandError) -> ExitCode {
    eprintln!("{e}");
    match e {
        CoverCommandError::Geometry(_) => ExitCode::InvalidUsage,
        CoverCommandError::Load(_) => ExitCode::IoOrParse,
        CoverCommandError::Fit(_) => ExitCode::IoOrParse,
        CoverCommandError::Structural(_) => ExitCode::IoOrParse,
        CoverCommandError::Save(_) => ExitCode::IoOrParse,
    }
}

fn report_book_error(e: &BookCommandError) -> ExitCode {
    match e {
        BookCommandError::Interior(inner) => report_pipeline_error(inner),
        BookCommandError::Cover(inner) => report_cover_error(inner),
    }
}

#[allow(dead_code)]
fn detected_tool_names() -> [&'static str; 2] {
    [QPDF.name, GHOSTSCRIPT.name]
}
