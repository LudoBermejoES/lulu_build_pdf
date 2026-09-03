//! Configuration precedence: flags > environment > project config file >
//! user config file > built-in defaults (`specs/cli/spec.md`, "Configuration
//! precedence"). Every resolved value remembers which layer it came from so
//! the effective configuration can be printed with sources named.

use lulu_prep::normalize::FitMode;
use serde::Deserialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigSource {
    Flag,
    Env,
    ProjectFile,
    UserFile,
    Default,
}

impl ConfigSource {
    pub fn label(self) -> &'static str {
        match self {
            ConfigSource::Flag => "flag",
            ConfigSource::Env => "environment",
            ConfigSource::ProjectFile => "project config",
            ConfigSource::UserFile => "user config",
            ConfigSource::Default => "default",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Resolved<T> {
    pub value: T,
    pub source: ConfigSource,
}

/// Picks the highest-precedence `Some` among the four layers, falling back
/// to `default`. Each layer is optional because a given setting need not be
/// specified at every level.
fn resolve<T>(
    flag: Option<T>,
    env: Option<T>,
    project: Option<T>,
    user: Option<T>,
    default: T,
) -> Resolved<T> {
    if let Some(value) = flag {
        return Resolved {
            value,
            source: ConfigSource::Flag,
        };
    }
    if let Some(value) = env {
        return Resolved {
            value,
            source: ConfigSource::Env,
        };
    }
    if let Some(value) = project {
        return Resolved {
            value,
            source: ConfigSource::ProjectFile,
        };
    }
    if let Some(value) = user {
        return Resolved {
            value,
            source: ConfigSource::UserFile,
        };
    }
    Resolved {
        value: default,
        source: ConfigSource::Default,
    }
}

/// The subset of settings read from a `lulu-prep.toml` config file (project
/// or user). Every field is optional so a file only needs to state what it
/// overrides.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ConfigFile {
    pub fit_mode: Option<String>,
    pub output_dir: Option<String>,
    pub strict: Option<bool>,
    pub no_color: Option<bool>,
    pub gutter_floor_in: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigParseError(pub String);

impl std::fmt::Display for ConfigParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "invalid config file: {}", self.0)
    }
}

impl ConfigFile {
    pub fn parse(toml_text: &str) -> Result<ConfigFile, ConfigParseError> {
        toml::from_str(toml_text).map_err(|e| ConfigParseError(e.to_string()))
    }
}

fn parse_fit_mode(s: &str) -> Option<FitMode> {
    match s.to_lowercase().as_str() {
        "center" | "centre" => Some(FitMode::Center),
        "scale-to-bleed" | "scale_to_bleed" => Some(FitMode::ScaleToBleed),
        "stretch-margins" | "stretch_margins" => Some(FitMode::StretchMargins),
        _ => None,
    }
}

/// Every environment variable this CLI reads, gathered behind one function
/// so tests can construct it without touching the real process environment.
#[derive(Debug, Clone, Default)]
pub struct EnvVars {
    pub fit_mode: Option<String>,
    pub output_dir: Option<String>,
    pub strict: Option<String>,
    /// The de-facto `NO_COLOR` convention (any non-empty value disables
    /// colour), read independently of our own `LULU_PREP_NO_COLOR`.
    pub no_color: Option<String>,
    pub gutter_floor_in: Option<String>,
}

impl EnvVars {
    pub fn from_process() -> EnvVars {
        EnvVars {
            fit_mode: std::env::var("LULU_PREP_FIT_MODE").ok(),
            output_dir: std::env::var("LULU_PREP_OUTPUT_DIR").ok(),
            strict: std::env::var("LULU_PREP_STRICT").ok(),
            no_color: std::env::var("NO_COLOR")
                .ok()
                .or_else(|| std::env::var("LULU_PREP_NO_COLOR").ok()),
            gutter_floor_in: std::env::var("LULU_PREP_GUTTER_FLOOR_IN").ok(),
        }
    }
}

fn parse_bool_env(s: &str) -> Option<bool> {
    match s.to_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

/// Flags parsed from the command line — every field `None` means "not
/// passed", letting the precedence chain fall through to lower layers.
#[derive(Debug, Clone, Default)]
pub struct Flags {
    pub fit_mode: Option<FitMode>,
    pub output_dir: Option<String>,
    pub strict: Option<bool>,
    pub no_color: Option<bool>,
    pub gutter_floor_in: Option<f64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct EffectiveConfig {
    pub fit_mode: Resolved<FitMode>,
    pub output_dir: Resolved<String>,
    pub strict: Resolved<bool>,
    pub no_color: Resolved<bool>,
    pub gutter_floor_in: Resolved<f64>,
}

pub const DEFAULT_OUTPUT_DIR: &str = ".";
pub const DEFAULT_GUTTER_FLOOR_IN: f64 = 0.0;

/// Resolves the effective configuration across all four layers. `project`
/// and `user` are `None` when the corresponding config file doesn't exist —
/// a missing file is not an error, it just contributes nothing.
pub fn resolve_config(
    flags: &Flags,
    env: &EnvVars,
    project: Option<&ConfigFile>,
    user: Option<&ConfigFile>,
) -> EffectiveConfig {
    let project_fit_mode = project
        .and_then(|c| c.fit_mode.as_deref())
        .and_then(parse_fit_mode);
    let user_fit_mode = user
        .and_then(|c| c.fit_mode.as_deref())
        .and_then(parse_fit_mode);
    let env_fit_mode = env.fit_mode.as_deref().and_then(parse_fit_mode);

    let project_output_dir = project.and_then(|c| c.output_dir.clone());
    let user_output_dir = user.and_then(|c| c.output_dir.clone());

    let project_strict = project.and_then(|c| c.strict);
    let user_strict = user.and_then(|c| c.strict);
    let env_strict = env.strict.as_deref().and_then(parse_bool_env);

    let project_no_color = project.and_then(|c| c.no_color);
    let user_no_color = user.and_then(|c| c.no_color);
    let env_no_color = env.no_color.as_ref().map(|s| !s.is_empty());

    let project_gutter = project.and_then(|c| c.gutter_floor_in);
    let user_gutter = user.and_then(|c| c.gutter_floor_in);
    let env_gutter = env
        .gutter_floor_in
        .as_deref()
        .and_then(|s| s.parse::<f64>().ok());

    EffectiveConfig {
        fit_mode: resolve(
            flags.fit_mode,
            env_fit_mode,
            project_fit_mode,
            user_fit_mode,
            FitMode::default(),
        ),
        output_dir: resolve(
            flags.output_dir.clone(),
            env.output_dir.clone(),
            project_output_dir,
            user_output_dir,
            DEFAULT_OUTPUT_DIR.to_string(),
        ),
        strict: resolve(flags.strict, env_strict, project_strict, user_strict, false),
        no_color: resolve(
            flags.no_color,
            env_no_color,
            project_no_color,
            user_no_color,
            false,
        ),
        gutter_floor_in: resolve(
            flags.gutter_floor_in,
            env_gutter,
            project_gutter,
            user_gutter,
            DEFAULT_GUTTER_FLOOR_IN,
        ),
    }
}

fn fit_mode_label(mode: FitMode) -> &'static str {
    match mode {
        FitMode::Center => "center",
        FitMode::ScaleToBleed => "scale-to-bleed",
        FitMode::StretchMargins => "stretch-margins",
    }
}

impl EffectiveConfig {
    /// One printable line per setting: `name = value (source: <layer>)`.
    pub fn display_lines(&self) -> Vec<String> {
        vec![
            format!(
                "fit_mode = {} (source: {})",
                fit_mode_label(self.fit_mode.value),
                self.fit_mode.source.label()
            ),
            format!(
                "output_dir = {} (source: {})",
                self.output_dir.value,
                self.output_dir.source.label()
            ),
            format!(
                "strict = {} (source: {})",
                self.strict.value,
                self.strict.source.label()
            ),
            format!(
                "no_color = {} (source: {})",
                self.no_color.value,
                self.no_color.source.label()
            ),
            format!(
                "gutter_floor_in = {} (source: {})",
                self.gutter_floor_in.value,
                self.gutter_floor_in.source.label()
            ),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_defaults_when_nothing_set() {
        let config = resolve_config(&Flags::default(), &EnvVars::default(), None, None);
        assert_eq!(config.fit_mode.value, FitMode::Center);
        assert_eq!(config.fit_mode.source, ConfigSource::Default);
        assert_eq!(config.output_dir.value, DEFAULT_OUTPUT_DIR);
        assert!(!config.strict.value);
        assert!(!config.no_color.value);
    }

    #[test]
    fn flag_beats_configuration_file() {
        let project = ConfigFile {
            fit_mode: Some("scale-to-bleed".to_string()),
            ..Default::default()
        };
        let flags = Flags {
            fit_mode: Some(FitMode::StretchMargins),
            ..Default::default()
        };
        let config = resolve_config(&flags, &EnvVars::default(), Some(&project), None);
        assert_eq!(config.fit_mode.value, FitMode::StretchMargins);
        assert_eq!(config.fit_mode.source, ConfigSource::Flag);
    }

    #[test]
    fn env_beats_project_file_which_beats_user_file() {
        let project = ConfigFile {
            strict: Some(true),
            ..Default::default()
        };
        let user = ConfigFile {
            strict: Some(false),
            ..Default::default()
        };
        let config = resolve_config(
            &Flags::default(),
            &EnvVars::default(),
            Some(&project),
            Some(&user),
        );
        assert!(config.strict.value);
        assert_eq!(config.strict.source, ConfigSource::ProjectFile);

        let env = EnvVars {
            strict: Some("0".to_string()),
            ..Default::default()
        };
        let config = resolve_config(&Flags::default(), &env, Some(&project), Some(&user));
        assert!(!config.strict.value);
        assert_eq!(config.strict.source, ConfigSource::Env);
    }

    #[test]
    fn user_file_is_last_resort_before_defaults() {
        let user = ConfigFile {
            output_dir: Some("/home/me/out".to_string()),
            ..Default::default()
        };
        let config = resolve_config(&Flags::default(), &EnvVars::default(), None, Some(&user));
        assert_eq!(config.output_dir.value, "/home/me/out");
        assert_eq!(config.output_dir.source, ConfigSource::UserFile);
    }

    #[test]
    fn no_color_env_convention_is_respected() {
        let env = EnvVars {
            no_color: Some("1".to_string()),
            ..Default::default()
        };
        let config = resolve_config(&Flags::default(), &env, None, None);
        assert!(config.no_color.value);
        assert_eq!(config.no_color.source, ConfigSource::Env);
    }

    #[test]
    fn effective_configuration_is_printable_with_sources() {
        let flags = Flags {
            strict: Some(true),
            ..Default::default()
        };
        let config = resolve_config(&flags, &EnvVars::default(), None, None);
        let lines = config.display_lines();
        assert!(lines
            .iter()
            .any(|l| l.contains("strict = true (source: flag)")));
        assert!(lines
            .iter()
            .any(|l| l.contains("fit_mode = center (source: default)")));
    }

    #[test]
    fn config_file_parses_toml() {
        let file = ConfigFile::parse("fit_mode = \"scale-to-bleed\"\nstrict = true\n").unwrap();
        assert_eq!(file.fit_mode.as_deref(), Some("scale-to-bleed"));
        assert_eq!(file.strict, Some(true));
    }

    #[test]
    fn config_file_rejects_invalid_toml() {
        let err = ConfigFile::parse("this is not [ valid toml").unwrap_err();
        assert!(!err.0.is_empty());
    }
}
