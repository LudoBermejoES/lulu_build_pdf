//! Lulu-published geometry rules: bleed, safety margins, gutter, page count,
//! spine width, and cover canvas. Every dimension Lulu publishes lives here —
//! nothing else in this crate computes a length from a formula.

use crate::catalog::{Binding, CatalogEntry};
use crate::units::{Length, Size};

/// Bleed extends 0.125 in past the trim edge on every side.
pub fn bleed() -> Length {
    Length::from_inches(0.125)
}

/// The PDF page size a product requires: trim size outset by [`bleed`] on every side.
pub fn required_page_size(trim: Size) -> Size {
    trim.outset(bleed())
}

/// Interior safety margin: 0.500 in inside the trim edge, for every product.
pub fn interior_safety_margin() -> Length {
    Length::from_inches(0.5)
}

/// Cover safety margin: 0.750 in inside the trim edge for hardcover case wrap,
/// 0.250 in for every other binding.
pub fn cover_safety_margin(binding: Binding) -> Length {
    if binding == Binding::CaseWrap {
        Length::from_inches(0.75)
    } else {
        Length::from_inches(0.25)
    }
}

/// Lulu advises a 0.200 in gutter minimum in its PDF creation settings, separately
/// from the page-count-banded table below — [`GutterAllowance::below_advisory_floor`]
/// flags when the two disagree, without silently overriding the table.
const GUTTER_ADVISORY_FLOOR_IN: f64 = 0.2;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GutterAllowance {
    pub gutter: Length,
    pub total_margin: Length,
    pub below_advisory_floor: bool,
}

/// Inner-edge gutter allowance and total interior margin for a given page count,
/// per Lulu's published five-band table. Total over every page count >= 1.
pub fn gutter_allowance(page_count: u32) -> GutterAllowance {
    let (gutter_in, margin_in) = match page_count {
        0..=60 => (0.0, 0.5),
        61..=150 => (0.125, 0.625),
        151..=400 => (0.5, 1.0),
        401..=600 => (0.625, 1.125),
        _ => (0.75, 1.25),
    };
    GutterAllowance {
        gutter: Length::from_inches(gutter_in),
        total_margin: Length::from_inches(margin_in),
        below_advisory_floor: gutter_in < GUTTER_ADVISORY_FLOOR_IN,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum PageCountError {
    #[error("requested page count {requested} exceeds this product's maximum of {max}")]
    AboveMaximum { requested: u32, max: u32 },
    #[error("page-count rules with a divisibility multiple of 0 have no conformant count")]
    InvalidRules,
}

/// A product's page-count constraints: catalog minimum/maximum and the binding's
/// divisibility rule (multiple of 2 for coil/Wire-O, multiple of 4 otherwise).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PageCountRules {
    pub min: u32,
    pub max: u32,
    pub multiple: u32,
}

impl PageCountRules {
    pub fn from_catalog_entry(entry: &CatalogEntry) -> PageCountRules {
        PageCountRules {
            min: entry.min_page,
            max: entry.max_page,
            multiple: entry.binding.page_count_multiple(),
        }
    }

    /// The smallest page count >= `requested` that satisfies the product minimum
    /// and the binding's divisibility rule, or an error naming the maximum if no
    /// such count exists.
    pub fn next_conformant(&self, requested: u32) -> Result<u32, PageCountError> {
        if self.multiple == 0 {
            return Err(PageCountError::InvalidRules);
        }
        let raised = requested.max(self.min);
        let remainder = raised % self.multiple;
        let padded = if remainder == 0 {
            raised
        } else {
            match raised.checked_add(self.multiple - remainder) {
                Some(padded) => padded,
                // Overflowing past u32::MAX has the same practical outcome as
                // exceeding the product's maximum: no conformant count exists
                // that this type can represent.
                None => {
                    return Err(PageCountError::AboveMaximum {
                        requested,
                        max: self.max,
                    });
                }
            }
        };
        if padded > self.max {
            Err(PageCountError::AboveMaximum {
                requested,
                max: self.max,
            })
        } else {
            Ok(padded)
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SpineError {
    #[error("no spine width is defined below {minimum} pages (got {page_count})")]
    BelowHardcoverMinimum { page_count: u32, minimum: u32 },
    #[error("perfect-bound spine width requires the paper's interior PPI, which is missing for this product")]
    MissingPpi,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SpineWidth {
    Perfect(Length),
    Hardcover(Length),
    /// Saddle stitch, coil, and Wire-O have no printable spine.
    None,
}

/// Lulu's hardcover (case wrap / linen wrap) spine width table: `(page_count_upper_bound, inches)`.
/// Bands are inclusive of their upper bound; the first page in a book is 1, so a
/// page count is matched to the first band whose upper bound it does not exceed.
/// Transcribed from Lulu's "How is spine width calculated?" help article.
const HARDCOVER_SPINE_TABLE_IN: &[(u32, f64)] = &[
    (84, 0.25),
    (140, 0.5),
    (168, 0.625),
    (194, 0.6875),
    (222, 0.75),
    (250, 0.8125),
    (278, 0.875),
    (306, 0.9375),
    (334, 1.0),
    (360, 1.0625),
    (388, 1.125),
    (416, 1.1875),
    (444, 1.25),
    (472, 1.3125),
    (500, 1.375),
    (528, 1.4375),
    (556, 1.5),
    (582, 1.5625),
    (610, 1.625),
    (638, 1.6875),
    (666, 1.75),
    (694, 1.8125),
    (722, 1.875),
    (750, 1.9375),
    (778, 2.0),
    // Lulu's published table lists "779-800: 2.0625" and then a separate "800: 2.125"
    // row — an overlap at exactly 800. Hardcover products cap at 800 pages (see the
    // catalog), so we resolve it by treating 800 as its own exact band and 779-799
    // as the preceding one; anything at or above 800 gets the table's final width.
    (799, 2.0625),
    (u32::MAX, 2.125),
];
const HARDCOVER_SPINE_MINIMUM_PAGES: u32 = 24;

fn hardcover_spine_width(page_count: u32) -> Result<Length, SpineError> {
    if page_count < HARDCOVER_SPINE_MINIMUM_PAGES {
        return Err(SpineError::BelowHardcoverMinimum {
            page_count,
            minimum: HARDCOVER_SPINE_MINIMUM_PAGES,
        });
    }
    let inches = HARDCOVER_SPINE_TABLE_IN
        .iter()
        .find(|&&(upper, _)| page_count <= upper)
        .map(|&(_, inches)| inches)
        .expect("table's last band covers u32::MAX");
    Ok(Length::from_inches(inches))
}

/// Spine width for a product and its final interior page count.
///
/// Perfect binding: `page_count / interior_ppi + 0.06 in`, using the paper's PPI
/// bulk from the SKU (444 for standard papers, 460 for magazine/comic stock).
/// Hardcover (case wrap, linen wrap): [`HARDCOVER_SPINE_TABLE_IN`], not the
/// perfect-bound formula. Saddle stitch, coil, Wire-O: [`SpineWidth::None`].
pub fn spine_width(
    binding: Binding,
    page_count: u32,
    interior_ppi: Option<f64>,
) -> Result<SpineWidth, SpineError> {
    // `Binding::has_spine` is the one place "which bindings have a printable
    // spine at all" is decided; this function only chooses *how* to size one
    // for the bindings that do, so the spineless case can never drift out of
    // step with that decision.
    if !binding.has_spine() {
        return Ok(SpineWidth::None);
    }
    match binding {
        Binding::Perfect => {
            let ppi = interior_ppi.ok_or(SpineError::MissingPpi)?;
            let inches = page_count as f64 / ppi + 0.06;
            Ok(SpineWidth::Perfect(Length::from_inches(inches)))
        }
        Binding::CaseWrap | Binding::LinenWrap => {
            hardcover_spine_width(page_count).map(SpineWidth::Hardcover)
        }
        Binding::SaddleStitch | Binding::Coil | Binding::WireO => unreachable!(
            "has_spine() already returned false for these bindings and routed them through the early return above"
        ),
    }
}

/// A book's minimum spine width to reliably carry printed text, below which
/// Lulu's binding variance risks the text landing on the fold.
pub fn spine_too_narrow_for_text(spine: Length) -> bool {
    spine < Length::from_inches(0.125)
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CoverCanvas {
    pub width: Length,
    pub height: Length,
    pub spine_width: Length,
}

/// Perfect-bound cover canvas: `2 * trim_width + spine + 2 * bleed` wide,
/// `trim_height + 2 * bleed` tall.
pub fn perfect_cover_canvas(trim: Size, spine: Length) -> CoverCanvas {
    let b = bleed();
    CoverCanvas {
        width: trim.width * 2.0 + spine + b * 2.0,
        height: trim.height + b * 2.0,
        spine_width: spine,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::{self, Binding};
    use crate::units::{Length, Size};

    fn pt(p: f64) -> Length {
        Length::from_points(p)
    }

    #[test]
    fn six_by_nine_yields_450_by_666_points() {
        let trim = Size::new(Length::from_inches(6.0), Length::from_inches(9.0));
        let with_bleed = required_page_size(trim);
        assert!((with_bleed.width.as_points() - 450.0).abs() < 1e-9);
        assert!((with_bleed.height.as_points() - 666.0).abs() < 1e-9);
    }

    #[test]
    fn derived_bleed_size_agrees_with_the_whole_catalog() {
        let all = catalog::search(|_| true);
        assert!(!all.is_empty());
        for entry in all {
            let derived = required_page_size(entry.trim_size);
            assert!(
                derived.approx_eq(entry.bleed_size, Length::from_inches(0.01)),
                "SKU {}: derived {:?} vs catalog {:?}",
                entry.sku,
                derived,
                entry.bleed_size
            );
        }
    }

    #[test]
    fn safety_margins_match_lulu() {
        assert_eq!(interior_safety_margin().as_inches(), 0.5);
        assert_eq!(cover_safety_margin(Binding::Perfect).as_inches(), 0.25);
        assert_eq!(cover_safety_margin(Binding::LinenWrap).as_inches(), 0.25);
        assert_eq!(cover_safety_margin(Binding::CaseWrap).as_inches(), 0.75);
    }

    #[test]
    fn gutter_band_boundaries_resolve_unambiguously() {
        let cases: &[(u32, f64, f64)] = &[
            (60, 0.0, 0.5),
            (61, 0.125, 0.625),
            (150, 0.125, 0.625),
            (151, 0.5, 1.0),
            (400, 0.5, 1.0),
            (401, 0.625, 1.125),
            (600, 0.625, 1.125),
            (601, 0.75, 1.25),
        ];
        for &(pages, gutter_in, margin_in) in cases {
            let g = gutter_allowance(pages);
            assert!(
                (g.gutter.as_inches() - gutter_in).abs() < 1e-9,
                "pages={pages}"
            );
            assert!(
                (g.total_margin.as_inches() - margin_in).abs() < 1e-9,
                "pages={pages}"
            );
        }
    }

    #[test]
    fn thin_book_gutter_is_flagged_below_advisory_floor() {
        let g = gutter_allowance(40);
        assert_eq!(g.gutter.as_inches(), 0.0);
        assert!(g.below_advisory_floor);
    }

    #[test]
    fn mid_size_book_gutter_is_not_flagged() {
        let g = gutter_allowance(210);
        assert!(!g.below_advisory_floor);
    }

    #[test]
    fn page_count_pads_to_binding_multiple() {
        let rules = PageCountRules {
            min: 32,
            max: 800,
            multiple: 4,
        };
        assert_eq!(rules.next_conformant(205).unwrap(), 208);
    }

    #[test]
    fn page_count_raised_to_product_minimum() {
        let rules = PageCountRules {
            min: 32,
            max: 800,
            multiple: 4,
        };
        assert_eq!(rules.next_conformant(18).unwrap(), 32);
    }

    #[test]
    fn page_count_over_maximum_is_refused() {
        let rules = PageCountRules {
            min: 32,
            max: 800,
            multiple: 4,
        };
        let err = rules.next_conformant(812).unwrap_err();
        assert_eq!(
            err,
            PageCountError::AboveMaximum {
                requested: 812,
                max: 800
            }
        );
    }

    #[test]
    fn zero_multiple_is_an_error_not_a_panic() {
        let rules = PageCountRules {
            min: 0,
            max: 800,
            multiple: 0,
        };
        let err = rules.next_conformant(50).unwrap_err();
        assert_eq!(err, PageCountError::InvalidRules);
    }

    #[test]
    fn a_padding_amount_that_would_wrap_past_u32_max_errors_rather_than_overflowing() {
        // u32::MAX - 1, padded up to the next multiple of 4, wraps past u32::MAX.
        let rules = PageCountRules {
            min: 0,
            max: 10,
            multiple: 4,
        };
        let err = rules.next_conformant(u32::MAX - 1).unwrap_err();
        assert_eq!(
            err,
            PageCountError::AboveMaximum {
                requested: u32::MAX - 1,
                max: 10
            }
        );
    }

    #[test]
    fn saddle_stitch_pads_to_multiple_of_four() {
        let rules = PageCountRules {
            min: 4,
            max: 48,
            multiple: 4,
        };
        assert_eq!(rules.next_conformant(30).unwrap(), 32);
    }

    #[test]
    fn page_count_rules_from_catalog_entry() {
        let entry = catalog::lookup("0600X0900.BW.STD.PB.060UW444.MXX").unwrap();
        let rules = PageCountRules::from_catalog_entry(entry);
        assert_eq!(rules.min, 32);
        assert_eq!(rules.max, 800);
        assert_eq!(rules.multiple, 4);
    }

    #[test]
    fn perfect_bound_spine_at_444_ppi() {
        let spine = spine_width(Binding::Perfect, 210, Some(444.0)).unwrap();
        let SpineWidth::Perfect(w) = spine else {
            panic!("expected Perfect spine, got {spine:?}")
        };
        assert!(
            (w.as_inches() - 0.533).abs() < 0.001,
            "got {}",
            w.as_inches()
        );
    }

    #[test]
    fn perfect_bound_spine_at_460_ppi() {
        let spine = spine_width(Binding::Perfect, 210, Some(460.0)).unwrap();
        let SpineWidth::Perfect(w) = spine else {
            panic!("expected Perfect spine, got {spine:?}")
        };
        assert!(
            (w.as_inches() - 0.517).abs() < 0.001,
            "got {}",
            w.as_inches()
        );
    }

    #[test]
    fn hardcover_spine_comes_from_the_table_not_the_formula() {
        let spine = spine_width(Binding::CaseWrap, 210, None).unwrap();
        let SpineWidth::Hardcover(w) = spine else {
            panic!("expected Hardcover spine, got {spine:?}")
        };
        assert!((w.as_inches() - 0.750).abs() < 1e-9);
    }

    #[test]
    fn hardcover_below_the_tables_floor_is_undefined() {
        let err = spine_width(Binding::CaseWrap, 20, None).unwrap_err();
        assert_eq!(
            err,
            SpineError::BelowHardcoverMinimum {
                page_count: 20,
                minimum: 24
            }
        );
    }

    #[test]
    fn hardcover_at_800_pages_matches_the_tables_final_row() {
        let spine = spine_width(Binding::LinenWrap, 800, None).unwrap();
        let SpineWidth::Hardcover(w) = spine else {
            panic!("expected Hardcover spine, got {spine:?}")
        };
        assert!((w.as_inches() - 2.125).abs() < 1e-9);
    }

    #[test]
    fn spineless_bindings_return_none_spine() {
        for b in [Binding::SaddleStitch, Binding::Coil, Binding::WireO] {
            let spine = spine_width(b, 100, None).unwrap();
            assert_eq!(spine, SpineWidth::None);
        }
    }

    #[test]
    fn perfect_bound_cover_canvas_matches_lulus_published_example() {
        // Note: 210 is Lulu's own worked-example page count for this SKU's
        // cover-dimensions endpoint; it is not itself a conformant *final*
        // interior page count for perfect binding (not a multiple of 4).
        // This test verifies the raw spine/canvas formula in isolation —
        // crate::cover's higher-level cover_geometry() correctly refuses a
        // non-conformant count and is tested at 212 instead (see cover.rs).
        let entry = catalog::lookup("0600X0900.BW.STD.PB.060UW444.MXX").unwrap();
        let spine = spine_width(entry.binding, 210, entry.interior_ppi).unwrap();
        let SpineWidth::Perfect(spine_len) = spine else {
            panic!("expected Perfect spine")
        };
        let canvas = perfect_cover_canvas(entry.trim_size, spine_len);
        // Lulu's cover-dimensions worked example: 920 x 666 pt for this exact SKU at 210 pages.
        assert!(
            (canvas.width.as_points() - 920.0).abs() < 1.0,
            "got {}",
            canvas.width.as_points()
        );
        assert!(
            (canvas.height.as_points() - 666.0).abs() < 1.0,
            "got {}",
            canvas.height.as_points()
        );
    }

    #[test]
    fn spine_under_an_eighth_inch_is_too_narrow_for_text() {
        assert!(spine_too_narrow_for_text(pt(6.0)));
        assert!(!spine_too_narrow_for_text(Length::from_inches(0.2)));
    }
}
