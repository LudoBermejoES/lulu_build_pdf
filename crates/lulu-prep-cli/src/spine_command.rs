//! `lulu-prep spine` — spine width and cover canvas with no PDF input
//! (`specs/cli/spec.md`, "Spine query needs no PDF" scenario).

use lulu_prep::catalog::CatalogEntry;
use lulu_prep::cover::{cover_geometry, CoverGeometryError};

pub fn format_spine_report(
    entry: &CatalogEntry,
    page_count: u32,
) -> Result<String, CoverGeometryError> {
    let geometry = cover_geometry(entry, page_count)?;
    Ok(format!(
        "product: {}\npage_count: {}\nspine_width_in: {:.4}\ncover_canvas_in: {:.3}x{:.3}",
        entry.sku,
        page_count,
        geometry.spine.width().as_inches(),
        geometry.canvas.width.as_inches(),
        geometry.canvas.height.as_inches(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_spine_width_and_canvas_for_a_valid_product_and_page_count() {
        let entry = lulu_prep::catalog::lookup("0600X0900.BW.STD.PB.060UW444.MXX").unwrap();
        let report = format_spine_report(entry, 212).unwrap();
        assert!(report.contains("page_count: 212"));
        assert!(report.contains("spine_width_in:"));
        assert!(report.contains("cover_canvas_in:"));
    }

    #[test]
    fn non_conformant_page_count_is_an_error_not_a_guess() {
        let entry = lulu_prep::catalog::lookup("0600X0900.BW.STD.PB.060UW444.MXX").unwrap();
        let err = format_spine_report(entry, 213).unwrap_err();
        assert!(matches!(
            err,
            CoverGeometryError::NonConformantPageCount { .. }
        ));
    }
}
