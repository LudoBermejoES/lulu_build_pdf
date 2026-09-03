//! Default output path derivation and overwrite protection
//! (`specs/cli/spec.md`, "Output paths and safety").

use camino::{Utf8Path, Utf8PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputRole {
    Interior,
    Cover,
}

impl OutputRole {
    fn suffix(self) -> &'static str {
        match self {
            OutputRole::Interior => "-interior",
            OutputRole::Cover => "-cover",
        }
    }
}

/// Derives `<stem><role-suffix>.pdf` inside `output_dir`, from an input
/// path's file stem (its extension, if any, is dropped — output is always a
/// PDF).
pub fn default_output_path(
    input: &Utf8Path,
    role: OutputRole,
    output_dir: &Utf8Path,
) -> Utf8PathBuf {
    let stem = input.file_stem().unwrap_or("output");
    output_dir.join(format!("{stem}{}.pdf", role.suffix()))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OverwriteRefused {
    pub path: Utf8PathBuf,
}

impl std::fmt::Display for OverwriteRefused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "refusing to overwrite existing file '{}' without --force",
            self.path
        )
    }
}

/// Refuses to proceed if `path` already exists and `force` is false. The
/// existence check is injected via `exists` so tests don't need real files.
pub fn check_overwrite(
    path: &Utf8Path,
    force: bool,
    exists: impl Fn(&Utf8Path) -> bool,
) -> Result<(), OverwriteRefused> {
    if !force && exists(path) {
        return Err(OverwriteRefused {
            path: path.to_owned(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_interior_path_uses_suffix_and_dir() {
        let input = Utf8Path::new("book.pdf");
        let path = default_output_path(input, OutputRole::Interior, Utf8Path::new("out"));
        assert_eq!(path, Utf8PathBuf::from("out/book-interior.pdf"));
    }

    #[test]
    fn default_cover_path_uses_suffix_and_dir() {
        let input = Utf8Path::new("/tmp/book.pdf");
        let path = default_output_path(input, OutputRole::Cover, Utf8Path::new("."));
        assert_eq!(path, Utf8PathBuf::from("./book-cover.pdf"));
    }

    #[test]
    fn input_with_no_extension_still_works() {
        let input = Utf8Path::new("manuscript");
        let path = default_output_path(input, OutputRole::Interior, Utf8Path::new("out"));
        assert_eq!(path, Utf8PathBuf::from("out/manuscript-interior.pdf"));
    }

    #[test]
    fn overwrite_without_force_is_refused_when_path_exists() {
        let path = Utf8Path::new("book-interior.pdf");
        let err = check_overwrite(path, false, |_| true).unwrap_err();
        assert_eq!(err.path, path);
    }

    #[test]
    fn overwrite_with_force_succeeds_even_if_path_exists() {
        let path = Utf8Path::new("book-interior.pdf");
        assert!(check_overwrite(path, true, |_| true).is_ok());
    }

    #[test]
    fn nonexistent_path_never_needs_force() {
        let path = Utf8Path::new("book-interior.pdf");
        assert!(check_overwrite(path, false, |_| false).is_ok());
    }
}
