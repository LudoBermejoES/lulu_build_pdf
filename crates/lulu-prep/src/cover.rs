//! Cover geometry, template generation, and fitting supplied artwork onto
//! the correct canvas — derived from the product and the *final* interior
//! page count, never invented for hardcover bindings.

use crate::catalog::{Binding, CatalogEntry};
use crate::geometry::{self, PageCountRules, SpineWidth};
use crate::units::{Length, Matrix, Rect, Size};
use lopdf::{dictionary, Dictionary};

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum CoverGeometryError {
    #[error("{page_count} pages is not a conformant count for this product; the next valid count is {next_valid}")]
    NonConformantPageCount { page_count: u32, next_valid: u32 },
    #[error("perfect-bound spine width requires the paper's interior PPI, which is missing for this product")]
    MissingPpi,
    #[error("no hardcover geometry is available for this product at {page_count} pages — download Lulu's cover template or enable API verification; a locally inferred estimate is never used for a final cover")]
    HardcoverGeometryUnavailable { page_count: u32 },
}

/// Fold x-positions and panel/spine rectangles for a wrap-style cover.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CoverGeometry {
    pub canvas: Size,
    pub back_panel: Rect,
    pub spine: Rect,
    pub front_panel: Rect,
    /// (back|spine fold, spine|front fold), as x-coordinates on the canvas.
    pub fold_positions: (Length, Length),
    pub safety_margin: Length,
    /// The hinge zone on each side of the spine — hardcover only.
    pub hinge_zones: Option<(Rect, Rect)>,
    /// The page count this geometry was built for, so a caller can confirm
    /// it matches the interior it's pairing this cover with.
    pub page_count: u32,
}

fn require_conformant_count(
    entry: &CatalogEntry,
    page_count: u32,
) -> Result<(), CoverGeometryError> {
    let rules = PageCountRules::from_catalog_entry(entry);
    match rules.next_conformant(page_count) {
        Ok(next_valid) if next_valid == page_count => Ok(()),
        Ok(next_valid) => Err(CoverGeometryError::NonConformantPageCount {
            page_count,
            next_valid,
        }),
        Err(_) => Err(CoverGeometryError::NonConformantPageCount {
            page_count,
            next_valid: entry.max_page,
        }),
    }
}

/// Flat (no-fold-verified-formula) wrap geometry: perfect binding uses its
/// real published spine formula; saddle stitch, coil, and Wire-O have no
/// spine at all ([`SpineWidth::None`]), so they get the same panel layout
/// with a zero-width spine rather than a flat cover with no fold markers.
fn perfect_geometry(
    entry: &CatalogEntry,
    page_count: u32,
) -> Result<CoverGeometry, CoverGeometryError> {
    let spine = geometry::spine_width(entry.binding, page_count, entry.interior_ppi)
        .map_err(|_| CoverGeometryError::MissingPpi)?;
    let spine_width = match spine {
        SpineWidth::Perfect(w) => w,
        SpineWidth::None => Length::ZERO,
        SpineWidth::Hardcover(_) => unreachable!(
            "hardcover bindings are routed to hardcover_geometry, not perfect_geometry"
        ),
    };
    let canvas = geometry::perfect_cover_canvas(entry.trim_size, spine_width);
    let bleed = geometry::bleed();

    let fold1 = bleed + entry.trim_size.width;
    let fold2 = fold1 + spine_width;

    Ok(CoverGeometry {
        canvas: Size::new(canvas.width, canvas.height),
        back_panel: Rect {
            x0: Length::ZERO,
            y0: Length::ZERO,
            x1: fold1,
            y1: canvas.height,
        },
        spine: Rect {
            x0: fold1,
            y0: Length::ZERO,
            x1: fold2,
            y1: canvas.height,
        },
        front_panel: Rect {
            x0: fold2,
            y0: Length::ZERO,
            x1: canvas.width,
            y1: canvas.height,
        },
        fold_positions: (fold1, fold2),
        safety_margin: geometry::cover_safety_margin(entry.binding),
        hinge_zones: None,
        page_count,
    })
}

/// Case wrap's per-side overhang beyond the trim, on all four edges —
/// verified live against Lulu's production `cover-dimensions` endpoint on
/// 2026-09-03, across two trim sizes (6x9in and A4) and five page counts
/// spanning the full 24-800 range: `canvas_width = 2*trim_width + spine +
/// 2*OVERHANG_IN`, `canvas_height = trim_height + 2*OVERHANG_IN`. 7 of 8
/// probes matched to the point; the eighth (400 pages, 6x9in) differed by
/// 0.5 pt, consistent with a rounding difference at that spine-table band
/// boundary rather than a formula error. See the `hardcover_case_wrap_*`
/// tests below for the exact recorded values. This is Lulu-confirmed, not
/// locally inferred — case wrap no longer needs [`HARDCOVER_TEMPLATE_TABLE`].
const CASE_WRAP_OVERHANG_IN: f64 = 0.875;

fn case_wrap_geometry(
    entry: &CatalogEntry,
    page_count: u32,
) -> Result<CoverGeometry, CoverGeometryError> {
    let spine = geometry::spine_width(entry.binding, page_count, entry.interior_ppi)
        .map_err(|_| CoverGeometryError::HardcoverGeometryUnavailable { page_count })?;
    let SpineWidth::Hardcover(spine_width) = spine else {
        return Err(CoverGeometryError::HardcoverGeometryUnavailable { page_count });
    };

    let overhang = Length::from_inches(CASE_WRAP_OVERHANG_IN);
    let canvas = Size::new(
        entry.trim_size.width * 2.0 + spine_width + overhang * 2.0,
        entry.trim_size.height + overhang * 2.0,
    );
    let fold1 = (canvas.width - spine_width) / 2.0;
    let fold2 = fold1 + spine_width;
    let hinge = Length::from_inches(0.25);

    Ok(CoverGeometry {
        canvas,
        back_panel: Rect {
            x0: Length::ZERO,
            y0: Length::ZERO,
            x1: fold1,
            y1: canvas.height,
        },
        spine: Rect {
            x0: fold1,
            y0: Length::ZERO,
            x1: fold2,
            y1: canvas.height,
        },
        front_panel: Rect {
            x0: fold2,
            y0: Length::ZERO,
            x1: canvas.width,
            y1: canvas.height,
        },
        fold_positions: (fold1, fold2),
        safety_margin: geometry::cover_safety_margin(entry.binding),
        hinge_zones: Some((
            Rect {
                x0: fold1,
                y0: Length::ZERO,
                x1: fold1 + hinge,
                y1: canvas.height,
            },
            Rect {
                x0: fold2 - hinge,
                y0: Length::ZERO,
                x1: fold2,
                y1: canvas.height,
            },
        )),
        page_count,
    })
}

/// Linen wrap (with dust jacket) canvas dimensions. **Not the same layout as
/// case wrap**: a live probe of `0600X0900.BW.STD.LW.060UC444.GBB` at 100
/// pages returned 1458 x 702 pt — nothing close to case wrap's ~1026 x 774
/// pt at the same page count. A dust jacket has front and back *flaps*
/// (typically ~2/3 the cover width each) folded in from a wider flat sheet,
/// a fundamentally different panel layout than the 3-panel back/spine/front
/// this crate models — reverse-engineering it from one data point would be
/// guessing, which [`CoverGeometryError::HardcoverGeometryUnavailable`]
/// exists specifically to avoid. Transcribed data belongs in
/// [`HARDCOVER_TEMPLATE_TABLE`] (currently empty) once a dust-jacket panel
/// model is designed; see `openspec/changes/prepare-pdf-for-lulu/design.md`
/// § Open Questions.
struct HardcoverTemplateEntry {
    sku: &'static str,
    page_count: u32,
    canvas: Size,
    hinge_width: Length,
}

/// Zero entries transcribed as of this writing (2026-09-03) — see the
/// "Hardcover template table coverage" resolution in
/// `openspec/changes/prepare-pdf-for-lulu/design.md` § Open Questions for
/// why (case wrap no longer needs this table; linen wrap's dust-jacket
/// panel model isn't designed yet). Update this count when entries land.
const HARDCOVER_TEMPLATE_TABLE: &[HardcoverTemplateEntry] = &[];

fn hardcover_geometry(
    entry: &CatalogEntry,
    page_count: u32,
) -> Result<CoverGeometry, CoverGeometryError> {
    hardcover_geometry_from_table(entry, page_count, HARDCOVER_TEMPLATE_TABLE)
}

fn hardcover_geometry_from_table(
    entry: &CatalogEntry,
    page_count: u32,
    table: &[HardcoverTemplateEntry],
) -> Result<CoverGeometry, CoverGeometryError> {
    let found = table
        .iter()
        .find(|row| row.sku == entry.sku && row.page_count == page_count);
    let Some(row) = found else {
        return Err(CoverGeometryError::HardcoverGeometryUnavailable { page_count });
    };

    let spine = geometry::spine_width(entry.binding, page_count, entry.interior_ppi).ok();
    let spine_width = match spine {
        Some(SpineWidth::Hardcover(w)) => w,
        _ => return Err(CoverGeometryError::HardcoverGeometryUnavailable { page_count }),
    };

    let fold1 = (row.canvas.width - spine_width) / 2.0;
    let fold2 = fold1 + spine_width;
    let hinge = row.hinge_width;

    Ok(CoverGeometry {
        canvas: row.canvas,
        back_panel: Rect {
            x0: Length::ZERO,
            y0: Length::ZERO,
            x1: fold1,
            y1: row.canvas.height,
        },
        spine: Rect {
            x0: fold1,
            y0: Length::ZERO,
            x1: fold2,
            y1: row.canvas.height,
        },
        front_panel: Rect {
            x0: fold2,
            y0: Length::ZERO,
            x1: row.canvas.width,
            y1: row.canvas.height,
        },
        fold_positions: (fold1, fold2),
        safety_margin: geometry::cover_safety_margin(entry.binding),
        hinge_zones: Some((
            Rect {
                x0: fold1,
                y0: Length::ZERO,
                x1: fold1 + hinge,
                y1: row.canvas.height,
            },
            Rect {
                x0: fold2 - hinge,
                y0: Length::ZERO,
                x1: fold2,
                y1: row.canvas.height,
            },
        )),
        page_count,
    })
}

/// Cover geometry for `entry` at its *final* interior page count (after
/// normalization padding). Refuses a page count that doesn't satisfy the
/// product's own rules, naming the next valid one.
pub fn cover_geometry(
    entry: &CatalogEntry,
    page_count: u32,
) -> Result<CoverGeometry, CoverGeometryError> {
    require_conformant_count(entry, page_count)?;
    match entry.binding {
        Binding::Perfect => perfect_geometry(entry, page_count),
        Binding::CaseWrap => case_wrap_geometry(entry, page_count),
        Binding::LinenWrap => hardcover_geometry(entry, page_count),
        Binding::SaddleStitch | Binding::Coil | Binding::WireO => {
            // These bindings have no spine (crate::geometry::SpineWidth::None);
            // the caller shouldn't be requesting wrap-cover geometry for one,
            // but treat it the same as a perfect-bound flat cover (spine width 0)
            // rather than panicking on an unexpected binding.
            perfect_geometry(entry, page_count)
        }
    }
}

/// Reads the final page count from a normalized interior document rather
/// than accepting it separately, so the cover's spine can never silently
/// disagree with the book it's paired with.
pub fn cover_geometry_from_interior(
    entry: &CatalogEntry,
    interior_doc: &lopdf::Document,
) -> Result<CoverGeometry, CoverGeometryError> {
    let page_count = interior_doc.get_pages().len() as u32;
    cover_geometry(entry, page_count)
}

/// A legend line-item shown on a generated template, or embedded as
/// document metadata on any cover this crate writes.
pub struct CoverMetadata<'a> {
    pub product_sku: &'a str,
    pub page_count: u32,
    pub spine_width: Length,
    pub canvas: Size,
}

const NOT_FOR_SUBMISSION_NOTICE: &str = "DESIGN AID ONLY -- NOT FOR SUBMISSION TO LULU";

fn rect_stroke_path(r: Rect) -> String {
    format!(
        "{:.2} {:.2} m {:.2} {:.2} l {:.2} {:.2} l {:.2} {:.2} l h S",
        r.x0.as_points(),
        r.y0.as_points(),
        r.x1.as_points(),
        r.y0.as_points(),
        r.x1.as_points(),
        r.y1.as_points(),
        r.x0.as_points(),
        r.y1.as_points(),
    )
}

fn vline_path(x: Length, y0: Length, y1: Length) -> String {
    format!(
        "{:.2} {:.2} m {:.2} {:.2} l S",
        x.as_points(),
        y0.as_points(),
        x.as_points(),
        y1.as_points()
    )
}

/// Escapes a string for use inside a PDF literal-string content operand.
fn pdf_escape(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('(', "\\(")
        .replace(')', "\\)")
}

/// Writes a blank cover template PDF: the exact canvas size, with
/// non-printing guides (trim, fold, safety margins, and — for hardcover —
/// hinge zones) in one named, removable optional content group, plus a
/// always-visible legend. Marked in both its legend text and its document
/// `Subject` metadata as a design aid, never itself submittable to Lulu.
pub fn generate_template(geo: &CoverGeometry, meta: &CoverMetadata) -> lopdf::Document {
    let mut doc = lopdf::Document::with_version("1.7");

    let ocg_id = doc.add_object(dictionary! {
        "Type" => "OCG",
        "Name" => lopdf::Object::String(b"Cover Guides (delete before printing)".to_vec(), lopdf::StringFormat::Literal),
    });

    let trim_rect = Rect::from_origin_size(geo.canvas).inset(geometry::bleed());
    let mut guide_ops = String::new();
    guide_ops.push_str("q 1 0 0 RG 0.5 w\n");
    guide_ops.push_str(&rect_stroke_path(trim_rect));
    guide_ops.push('\n');
    guide_ops.push_str(&vline_path(
        geo.fold_positions.0,
        Length::ZERO,
        geo.canvas.height,
    ));
    guide_ops.push('\n');
    guide_ops.push_str(&vline_path(
        geo.fold_positions.1,
        Length::ZERO,
        geo.canvas.height,
    ));
    guide_ops.push('\n');
    guide_ops.push_str("0 0 1 RG [3 3] 0 d\n");
    for panel in [geo.back_panel, geo.spine, geo.front_panel] {
        let safety = panel.inset(geo.safety_margin);
        guide_ops.push_str(&rect_stroke_path(safety));
        guide_ops.push('\n');
    }
    if let Some((left_hinge, right_hinge)) = geo.hinge_zones {
        guide_ops.push_str("0 0.6 0 RG [1 2] 0 d\n");
        guide_ops.push_str(&vline_path(left_hinge.x0, Length::ZERO, geo.canvas.height));
        guide_ops.push('\n');
        guide_ops.push_str(&vline_path(left_hinge.x1, Length::ZERO, geo.canvas.height));
        guide_ops.push('\n');
        guide_ops.push_str(&vline_path(right_hinge.x0, Length::ZERO, geo.canvas.height));
        guide_ops.push('\n');
        guide_ops.push_str(&vline_path(right_hinge.x1, Length::ZERO, geo.canvas.height));
        guide_ops.push('\n');
    }
    guide_ops.push_str("Q\n");

    let marked_guides = format!("/OC /MC0 BDC\n{guide_ops}EMC\n");

    let legend_lines = [
        NOT_FOR_SUBMISSION_NOTICE.to_string(),
        format!("Product: {}", meta.product_sku),
        format!("Page count: {}", meta.page_count),
        format!("Spine width: {:.3} in", meta.spine_width.as_inches()),
        format!(
            "Canvas: {:.3} x {:.3} in",
            meta.canvas.width.as_inches(),
            meta.canvas.height.as_inches()
        ),
    ];
    let mut legend = String::from("0 0 0 rg BT /F1 10 Tf\n");
    let legend_x = geo.back_panel.x0.as_points() + 6.0;
    let mut legend_y = geo.canvas.height.as_points() - 14.0;
    for line in legend_lines {
        legend.push_str(&format!(
            "1 0 0 1 {legend_x:.2} {legend_y:.2} Tm ({}) Tj\n",
            pdf_escape(&line)
        ));
        legend_y -= 12.0;
    }
    legend.push_str("ET\n");

    let content = format!("{marked_guides}{legend}");
    let content_id = doc.add_object(lopdf::Stream::new(dictionary! {}, content.into_bytes()));

    let font_id = doc.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type1",
        "BaseFont" => "Helvetica",
    });
    let properties = dictionary! { "MC0" => lopdf::Object::Reference(ocg_id) };
    let resources = dictionary! {
        "Font" => dictionary! { "F1" => lopdf::Object::Reference(font_id) },
        "Properties" => properties,
    };

    let pages_id = doc.new_object_id();
    let page_id = doc.add_object(dictionary! {
        "Type" => "Page",
        "Parent" => lopdf::Object::Reference(pages_id),
        "MediaBox" => rect_to_array(Rect::from_origin_size(geo.canvas)),
        "TrimBox" => rect_to_array(trim_rect),
        "Contents" => lopdf::Object::Reference(content_id),
        "Resources" => resources,
    });
    let pages = dictionary! { "Type" => "Pages", "Kids" => vec![lopdf::Object::Reference(page_id)], "Count" => 1 };
    doc.objects
        .insert(pages_id, lopdf::Object::Dictionary(pages));

    let ocg_config = dictionary! { "ON" => vec![lopdf::Object::Reference(ocg_id)], "Order" => vec![lopdf::Object::Reference(ocg_id)] };
    let oc_properties =
        dictionary! { "OCGs" => vec![lopdf::Object::Reference(ocg_id)], "D" => ocg_config };
    let catalog_id = doc.add_object(dictionary! {
        "Type" => "Catalog",
        "Pages" => lopdf::Object::Reference(pages_id),
        "OCProperties" => oc_properties,
    });
    doc.trailer
        .set("Root", lopdf::Object::Reference(catalog_id));

    let info_id = doc.add_object(dictionary! {
        "Subject" => lopdf::Object::String(NOT_FOR_SUBMISSION_NOTICE.as_bytes().to_vec(), lopdf::StringFormat::Literal),
        "Title" => lopdf::Object::String(b"Lulu cover template (design aid)".to_vec(), lopdf::StringFormat::Literal),
    });
    doc.trailer.set("Info", lopdf::Object::Reference(info_id));

    doc
}

fn rect_to_array(r: Rect) -> lopdf::Object {
    lopdf::Object::Array(
        r.as_pdf_array_points()
            .into_iter()
            .map(|v| lopdf::Object::Real(v as f32))
            .collect(),
    )
}

#[derive(Debug, thiserror::Error)]
pub enum FitArtworkError {
    #[error(transparent)]
    Nest(#[from] crate::normalize::NestError),
    #[error(transparent)]
    Geometry(#[from] crate::pdf::PageGeometryError),
}

/// Fits a supplied single-page cover onto `geo`'s canvas, in place. Content
/// already at the required size (within 0.5 pt) passes through unscaled
/// (`fit_mode` still applies for a genuine mismatch, offering the same
/// `center`/`scale-to-bleed`/`stretch-margins` choices as the interior).
/// Reports a blocking, non-stretching finding when the width is off by an
/// amount consistent with a spine computed for the wrong page count — this
/// runs regardless of `fit_mode`, since a caller opting into scaling should
/// still be told *why* the artwork didn't already fit.
pub fn fit_supplied_cover(
    doc: &mut lopdf::Document,
    page_id: lopdf::ObjectId,
    geo: &CoverGeometry,
    fit_mode: crate::normalize::FitMode,
) -> Result<Vec<crate::report::Finding>, FitArtworkError> {
    let original_size = crate::pdf::own_box_size(doc, page_id)?;
    crate::normalize::nest_page(doc, page_id, geo.canvas, fit_mode, Matrix::IDENTITY)?;

    let mut findings = Vec::new();
    let width_diff = (geo.canvas.width - original_size.width).as_points();
    if width_diff.abs() > 0.5 {
        let mut message = format!(
            "supplied cover is {:.1}pt {} than the required {:.1}pt width — consistent with a spine computed for a different page count",
            width_diff.abs(),
            if width_diff > 0.0 { "narrower" } else { "wider" },
            geo.canvas.width.as_points()
        );
        if let Some(ppi) = ppi_for_implied_page_count(geo) {
            let implied_spine_in = (original_size.width - geo.canvas.width
                + (geo.fold_positions.1 - geo.fold_positions.0))
                .as_inches();
            let implied_pages = ((implied_spine_in - 0.06) * ppi).round();
            if implied_pages > 0.0 {
                message.push_str(&format!(" (implies roughly {implied_pages:.0} pages)"));
            }
        }
        findings.push(
            crate::report::Finding::new(
                "cover.spine-mismatch",
                crate::report::Severity::Blocking,
                message,
            )
            .with_observed(format!("{:.1}pt", original_size.width.as_points()))
            .with_expected(format!("{:.1}pt", geo.canvas.width.as_points()))
            .fixable(false),
        );
    }
    Ok(findings)
}

/// The perfect-bound interior PPI implied by this geometry's spine, if the
/// spine was computed by the formula (not a hardcover table lookup) — used
/// to translate a width mismatch into an implied page count for the finding
/// message. `None` when the spine has no such formula (hardcover, or no
/// spine at all).
fn ppi_for_implied_page_count(geo: &CoverGeometry) -> Option<f64> {
    let spine_width = (geo.fold_positions.1 - geo.fold_positions.0).as_inches();
    if geo.hinge_zones.is_some() || spine_width <= 0.0 {
        return None;
    }
    let implied_ppi = geo.page_count as f64 / (spine_width - 0.06);
    if implied_ppi.is_finite() && implied_ppi > 0.0 {
        Some(implied_ppi)
    } else {
        None
    }
}

/// Copies a page's content and resources (fonts, images, nested XObjects —
/// [`crate::pdf::deep_copy_object`]) from `src` into `dest` as a Form
/// XObject sized to the page's own box, for [`assemble_three_panel_cover`].
fn copy_page_as_form(
    dest: &mut lopdf::Document,
    src: &lopdf::Document,
    src_page_id: lopdf::ObjectId,
) -> Result<lopdf::ObjectId, crate::pdf::PageGeometryError> {
    let own_rect = crate::pdf::own_box_rect(src, src_page_id)?;
    let content_bytes = src.get_page_content(src_page_id);
    let resources_dict = src
        .get_page_resources(src_page_id)
        .ok()
        .and_then(|(r, _)| r)
        .cloned()
        .unwrap_or_default();
    let mut memo = std::collections::HashMap::new();
    let copied_resources = crate::pdf::deep_copy_object(
        dest,
        src,
        &lopdf::Object::Dictionary(resources_dict),
        &mut memo,
    );
    let form_dict = dictionary! {
        "Type" => "XObject",
        "Subtype" => "Form",
        "BBox" => rect_to_array(own_rect),
        "Resources" => copied_resources,
    };
    Ok(dest.add_object(lopdf::Object::Stream(lopdf::Stream::new(
        form_dict,
        content_bytes,
    ))))
}

/// Assembles separately supplied back-cover, spine, and front-cover pages
/// into one wrap-format cover document, placing each at its computed panel
/// rectangle in left-to-right order. Each source may be a different
/// `Document`; fonts, images, and nested XObjects are deep-copied
/// ([`crate::pdf::deep_copy_object`]) into the assembled result.
pub fn assemble_three_panel_cover(
    back: (&lopdf::Document, lopdf::ObjectId),
    spine: (&lopdf::Document, lopdf::ObjectId),
    front: (&lopdf::Document, lopdf::ObjectId),
    geo: &CoverGeometry,
) -> Result<lopdf::Document, crate::pdf::PageGeometryError> {
    let mut dest = lopdf::Document::with_version("1.7");
    let mut content = String::new();
    let mut xobjects = Dictionary::new();

    for (name, (src, src_page_id), dest_rect) in [
        ("Bk", back, geo.back_panel),
        ("Sp", spine, geo.spine),
        ("Fr", front, geo.front_panel),
    ] {
        let own_rect = crate::pdf::own_box_rect(src, src_page_id)?;
        let own_size = Size::new(own_rect.width(), own_rect.height());
        let dest_size = Size::new(dest_rect.width(), dest_rect.height());
        let placement =
            crate::normalize::fit_placement(own_size, dest_size, crate::normalize::FitMode::Center);
        let to_origin = Matrix::translate(Length::ZERO - own_rect.x0, Length::ZERO - own_rect.y0);
        let to_panel_origin = Matrix::translate(dest_rect.x0, dest_rect.y0);
        let full = to_origin.then(placement.transform).then(to_panel_origin);

        let form_id = copy_page_as_form(&mut dest, src, src_page_id)?;
        xobjects.set(name, lopdf::Object::Reference(form_id));

        let cm = full.as_cm_operands();
        content.push_str(&format!(
            "q {:.4} {:.4} {:.4} {:.4} {:.4} {:.4} cm /{name} Do Q\n",
            cm[0], cm[1], cm[2], cm[3], cm[4], cm[5]
        ));
    }

    let content_id = dest.add_object(lopdf::Stream::new(dictionary! {}, content.into_bytes()));
    let resources = dictionary! { "XObject" => xobjects };
    let trim_rect = Rect::from_origin_size(geo.canvas).inset(geometry::bleed());

    let pages_id = dest.new_object_id();
    let page_id = dest.add_object(dictionary! {
        "Type" => "Page",
        "Parent" => lopdf::Object::Reference(pages_id),
        "MediaBox" => rect_to_array(Rect::from_origin_size(geo.canvas)),
        "TrimBox" => rect_to_array(trim_rect),
        "Contents" => lopdf::Object::Reference(content_id),
        "Resources" => resources,
    });
    let pages = dictionary! { "Type" => "Pages", "Kids" => vec![lopdf::Object::Reference(page_id)], "Count" => 1 };
    dest.objects
        .insert(pages_id, lopdf::Object::Dictionary(pages));
    let catalog_id = dest.add_object(
        dictionary! { "Type" => "Catalog", "Pages" => lopdf::Object::Reference(pages_id) },
    );
    dest.trailer
        .set("Root", lopdf::Object::Reference(catalog_id));

    Ok(dest)
}

#[derive(Debug, thiserror::Error)]
pub enum CoverStructuralError {
    #[error("this cover file is encrypted with a password; supply it and decrypt the file before preparing the cover")]
    PasswordRequired,
}

/// Applies Lulu's cover structural rules to an already-assembled or
/// already-fitted cover document: refuses an encrypted file, and strips
/// annotations/`AcroForm`/JavaScript/embedded files via
/// [`crate::normalize::sanitize_structure`] (the cover is always a single
/// page by construction, and its `TrimBox` is set when the page was built).
pub fn apply_cover_structural_rules(
    doc: &mut lopdf::Document,
) -> Result<crate::normalize::SanitizeSummary, CoverStructuralError> {
    if doc.is_encrypted() {
        return Err(CoverStructuralError::PasswordRequired);
    }
    Ok(crate::normalize::sanitize_structure(doc))
}

const PREVIEW_NOTICE: &str = "PROOF FOR REVIEW -- NOT FOR SUBMISSION TO LULU";

/// Extracts one wrap-cover panel (front or back) from `cover_doc`'s cover
/// page into a standalone, unlinked page in `dest`, sized to the interior's
/// page-with-bleed geometry (`required_size`) rather than the panel's own
/// (smaller, single-bleed) width — the panel is centred within it, and a
/// small proof stamp is added. The caller links the returned page into a
/// Pages tree (setting `/Parent` and `/Type`).
fn extract_panel_as_preview_page(
    dest: &mut lopdf::Document,
    cover_doc: &lopdf::Document,
    cover_page_id: lopdf::ObjectId,
    panel_rect: Rect,
    required_size: Size,
) -> Result<lopdf::ObjectId, crate::pdf::PageGeometryError> {
    let panel_size = Size::new(panel_rect.width(), panel_rect.height());
    let placement = crate::normalize::fit_placement(
        panel_size,
        required_size,
        crate::normalize::FitMode::Center,
    );
    let to_origin = Matrix::translate(Length::ZERO - panel_rect.x0, Length::ZERO - panel_rect.y0);
    let full = to_origin.then(placement.transform);

    let content_bytes = cover_doc.get_page_content(cover_page_id);
    let resources_dict = cover_doc
        .get_page_resources(cover_page_id)
        .ok()
        .and_then(|(r, _)| r)
        .cloned()
        .unwrap_or_default();
    let mut memo = std::collections::HashMap::new();
    let copied_resources = crate::pdf::deep_copy_object(
        dest,
        cover_doc,
        &lopdf::Object::Dictionary(resources_dict),
        &mut memo,
    );
    let form_dict = dictionary! {
        "Type" => "XObject",
        "Subtype" => "Form",
        "BBox" => rect_to_array(panel_rect),
        "Resources" => copied_resources,
    };
    let form_id = dest.add_object(lopdf::Object::Stream(lopdf::Stream::new(
        form_dict,
        content_bytes,
    )));

    let font_id = dest.add_object(
        dictionary! { "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Helvetica" },
    );

    let cm = full.as_cm_operands();
    let stamp_y = required_size.height.as_points() - 12.0;
    let page_content = format!(
        "q {:.4} {:.4} {:.4} {:.4} {:.4} {:.4} cm /Pn Do Q\n0.6 0 0 rg BT /Fp 8 Tf 1 0 0 1 6 {stamp_y:.2} Tm ({PREVIEW_NOTICE}) Tj ET",
        cm[0], cm[1], cm[2], cm[3], cm[4], cm[5]
    );
    let page_content_id = dest.add_object(lopdf::Stream::new(
        dictionary! {},
        page_content.into_bytes(),
    ));

    let page_resources = dictionary! {
        "XObject" => dictionary! { "Pn" => lopdf::Object::Reference(form_id) },
        "Font" => dictionary! { "Fp" => lopdf::Object::Reference(font_id) },
    };
    let boxes = crate::normalize::page_boxes(required_size);
    let page_id = dest.add_object(dictionary! {
        "Type" => "Page",
        "MediaBox" => rect_to_array(boxes.media_bleed_box),
        "CropBox" => rect_to_array(boxes.media_bleed_box),
        "TrimBox" => rect_to_array(boxes.trim_art_box),
        "Contents" => lopdf::Object::Reference(page_content_id),
        "Resources" => page_resources,
    });
    Ok(page_id)
}

/// Builds a combined preview PDF for human review only: the front cover
/// panel as page one, the normalized interior's pages in order, and the
/// back cover panel as the last page. Front and back preview pages are
/// sized to the product's page-with-bleed geometry (matching the interior
/// pages around them), not the full wrap-cover canvas — the spine panel is
/// never included as its own page.
///
/// `cover_doc`/`cover_page_id` and `interior_doc` are read-only: this
/// function copies what it needs and never mutates either input, so
/// generating a preview alongside a normal `book` run cannot affect the
/// separate interior file or wrap-format cover file Lulu's Print API
/// requires. The result is marked, in both a per-page stamp and its
/// document `Subject` metadata, as a proof and not a submittable file.
pub fn build_combined_preview(
    cover_doc: &lopdf::Document,
    cover_page_id: lopdf::ObjectId,
    geo: &CoverGeometry,
    interior_doc: &lopdf::Document,
    trim_size: Size,
) -> Result<lopdf::Document, crate::pdf::PageGeometryError> {
    let required_size = crate::geometry::required_page_size(trim_size);
    let mut dest = lopdf::Document::with_version("1.7");

    let front_page_id = extract_panel_as_preview_page(
        &mut dest,
        cover_doc,
        cover_page_id,
        geo.front_panel,
        required_size,
    )?;
    let mut kids = vec![lopdf::Object::Reference(front_page_id)];

    for interior_page_id in interior_doc.page_iter() {
        let copied = crate::pdf::copy_page(&mut dest, interior_doc, interior_page_id)?;
        kids.push(lopdf::Object::Reference(copied));
    }

    let back_page_id = extract_panel_as_preview_page(
        &mut dest,
        cover_doc,
        cover_page_id,
        geo.back_panel,
        required_size,
    )?;
    kids.push(lopdf::Object::Reference(back_page_id));

    let pages_id = dest.new_object_id();
    for kid in &kids {
        let lopdf::Object::Reference(id) = kid else {
            continue;
        };
        if let Ok(dict) = dest.get_dictionary_mut(*id) {
            dict.set("Parent", lopdf::Object::Reference(pages_id));
            dict.set("Type", "Page");
        }
    }
    let count = kids.len() as i64;
    let pages = dictionary! { "Type" => "Pages", "Kids" => kids, "Count" => count };
    dest.objects
        .insert(pages_id, lopdf::Object::Dictionary(pages));
    let catalog_id = dest.add_object(dictionary! {
        "Type" => "Catalog",
        "Pages" => lopdf::Object::Reference(pages_id),
        "PageLayout" => "SinglePage",
    });
    dest.trailer
        .set("Root", lopdf::Object::Reference(catalog_id));

    let info_id = dest.add_object(dictionary! {
        "Subject" => lopdf::Object::String(PREVIEW_NOTICE.as_bytes().to_vec(), lopdf::StringFormat::Literal),
        "Title" => lopdf::Object::String(b"Lulu book preview (proof, not for submission)".to_vec(), lopdf::StringFormat::Literal),
    });
    dest.trailer.set("Info", lopdf::Object::Reference(info_id));

    Ok(dest)
}

#[cfg(test)]
mod tests {
    use super::*;
    use lopdf::Object;

    fn sku() -> &'static CatalogEntry {
        crate::catalog::lookup("0600X0900.BW.STD.PB.060UW444.MXX").unwrap()
    }

    #[test]
    fn perfect_bound_geometry_matches_lulus_published_example() {
        // Lulu's own `cover-dimensions` worked example uses 210 pages -> 920x666pt,
        // but 210 is not itself a conformant final page count for this SKU (not a
        // multiple of 4) — the raw formula is cross-checked directly against that
        // exact example in geometry.rs; here we use 212, the nearest count this
        // module's own conformance gate accepts, and expect the same canvas within
        // the ~0.7pt the extra 2 pages of spine add.
        let geo = cover_geometry(sku(), 212).unwrap();
        assert!(
            (geo.canvas.width.as_points() - 920.0).abs() < 1.0,
            "{}",
            geo.canvas.width.as_points()
        );
        assert!((geo.canvas.height.as_points() - 666.0).abs() < 1e-6);
        // Spine width: 212/444 + 0.06in = 0.5375in = 38.7pt.
        let spine_width = geo.fold_positions.1 - geo.fold_positions.0;
        assert!(
            (spine_width.as_points() - 38.7).abs() < 0.1,
            "{}",
            spine_width.as_points()
        );
        // Back and front panels span from the canvas's outer (bleed) edge to
        // their fold, so each is trim_width + one bleed = 432 + 9 = 441pt wide.
        assert!((geo.back_panel.width().as_points() - 441.0).abs() < 1.0);
        assert!((geo.front_panel.width().as_points() - 441.0).abs() < 1.0);
        assert_eq!(geo.back_panel.height().as_points(), 666.0);
        assert_eq!(geo.page_count, 212);
        assert!(geo.hinge_zones.is_none());
    }

    #[test]
    fn non_conformant_page_count_is_refused_naming_the_next_valid_count() {
        let err = cover_geometry(sku(), 205).unwrap_err();
        assert_eq!(
            err,
            CoverGeometryError::NonConformantPageCount {
                page_count: 205,
                next_valid: 208
            }
        );
    }

    #[test]
    fn conformant_page_count_is_accepted() {
        assert!(cover_geometry(sku(), 208).is_ok());
    }

    #[test]
    fn geometry_from_interior_reads_the_page_count_from_the_document() {
        let mut doc = lopdf::Document::with_version("1.7");
        let pages_id = doc.new_object_id();
        let mut kids = Vec::new();
        for _ in 0..208 {
            let page_id = doc.add_object(
                dictionary! { "Type" => "Page", "Parent" => lopdf::Object::Reference(pages_id) },
            );
            kids.push(lopdf::Object::Reference(page_id));
        }
        let pages = dictionary! { "Type" => "Pages", "Kids" => kids, "Count" => 208 };
        doc.objects
            .insert(pages_id, lopdf::Object::Dictionary(pages));
        let catalog_id = doc.add_object(
            dictionary! { "Type" => "Catalog", "Pages" => lopdf::Object::Reference(pages_id) },
        );
        doc.trailer
            .set("Root", lopdf::Object::Reference(catalog_id));

        let geo = cover_geometry_from_interior(sku(), &doc).unwrap();
        assert_eq!(geo.page_count, 208);
    }

    #[test]
    fn hardcover_geometry_is_never_invented_when_the_table_has_no_entry() {
        // Case wrap now has a verified formula (see `case_wrap_geometry`);
        // linen wrap (a dust jacket, a different panel layout entirely) is
        // the one still correctly refusing rather than guessing.
        let linen = crate::catalog::search(|e| e.binding == Binding::LinenWrap)
            .first()
            .copied()
            .expect("at least one linen wrap product");
        let err = cover_geometry(linen, linen.min_page.max(24)).unwrap_err();
        assert!(matches!(
            err,
            CoverGeometryError::HardcoverGeometryUnavailable { .. }
        ));
    }

    #[test]
    fn hardcover_lookup_mechanism_works_when_the_table_has_an_entry() {
        // The production table is intentionally empty (see its doc comment);
        // this exercises the real hardcover_geometry code path — fold
        // positions, spine centring, hinge zones — against a synthetic row,
        // proving the mechanism is correct independent of data population.
        // hardcover_geometry_from_table is exercised generically here — it now
        // backs only linen wrap, but the lookup mechanism itself is binding-agnostic.
        let entry = crate::catalog::search(|e| e.binding == Binding::LinenWrap)
            .first()
            .copied()
            .expect("a linen wrap product");
        let table = [HardcoverTemplateEntry {
            sku: entry.sku.as_str(),
            page_count: 210,
            canvas: Size::new(Length::from_inches(13.0), Length::from_inches(9.25)),
            hinge_width: Length::from_inches(0.25),
        }];

        let geo = hardcover_geometry_from_table(entry, 210, &table).unwrap();
        assert!((geo.canvas.width.as_inches() - 13.0).abs() < 1e-9);
        let spine_width = geo.fold_positions.1 - geo.fold_positions.0;
        // Spine at 210 pages, case wrap: 0.75in per the hardcover table (195-222 band).
        assert!((spine_width.as_inches() - 0.75).abs() < 1e-6);
        // Spine is centred on the canvas.
        assert!(
            (geo.fold_positions.0.as_points() - (geo.canvas.width - spine_width).as_points() / 2.0)
                .abs()
                < 1e-6
        );
        let (left_hinge, right_hinge) = geo.hinge_zones.expect("hardcover must report hinge zones");
        assert!((left_hinge.width().as_inches() - 0.25).abs() < 1e-9);
        assert!((right_hinge.width().as_inches() - 0.25).abs() < 1e-9);
        assert_eq!(left_hinge.x0, geo.fold_positions.0);
        assert_eq!(right_hinge.x1, geo.fold_positions.1);
    }

    #[test]
    fn hardcover_lookup_misses_when_page_count_does_not_match_the_table() {
        // hardcover_geometry_from_table is exercised generically here — it now
        // backs only linen wrap, but the lookup mechanism itself is binding-agnostic.
        let entry = crate::catalog::search(|e| e.binding == Binding::LinenWrap)
            .first()
            .copied()
            .expect("a linen wrap product");
        let table = [HardcoverTemplateEntry {
            sku: entry.sku.as_str(),
            page_count: 210,
            canvas: Size::new(Length::from_inches(13.0), Length::from_inches(9.25)),
            hinge_width: Length::from_inches(0.25),
        }];
        let err = hardcover_geometry_from_table(entry, 96, &table).unwrap_err();
        assert!(matches!(
            err,
            CoverGeometryError::HardcoverGeometryUnavailable { page_count: 96 }
        ));
    }

    #[test]
    fn spineless_bindings_get_a_zero_width_spine_not_a_panic() {
        // Regression test: spine_width returns SpineWidth::None (not Perfect)
        // for these bindings, which must not panic the geometry computation.
        for binding in [Binding::SaddleStitch, Binding::Coil, Binding::WireO] {
            let entry = crate::catalog::search(|e| e.binding == binding)
                .first()
                .copied()
                .unwrap_or_else(|| panic!("no {binding:?} product in catalog"));
            let rules = PageCountRules::from_catalog_entry(entry);
            let page_count = rules.next_conformant(entry.min_page).unwrap();
            let geo = cover_geometry(entry, page_count).unwrap();
            let spine_width = geo.fold_positions.1 - geo.fold_positions.0;
            assert_eq!(spine_width.as_points(), 0.0, "{binding:?}");
        }
    }

    // --- template generation ---

    fn sample_metadata() -> CoverMetadata<'static> {
        CoverMetadata {
            product_sku: "0600X0900.BW.STD.PB.060UW444.MXX",
            page_count: 212,
            spine_width: Length::from_inches(0.537),
            canvas: Size::new(Length::from_points(920.7), Length::from_points(666.0)),
        }
    }

    #[test]
    fn template_carries_the_right_canvas_and_trim_box() {
        let geo = cover_geometry(sku(), 212).unwrap();
        let doc = generate_template(&geo, &sample_metadata());
        assert_eq!(doc.get_pages().len(), 1);
        let page_id = *doc.get_pages().values().next().unwrap();
        let page = doc.get_dictionary(page_id).unwrap();

        let media: Vec<f64> = page
            .get(b"MediaBox")
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .map(|o| o.as_float().unwrap() as f64)
            .collect();
        assert!((media[2] - geo.canvas.width.as_points()).abs() < 0.01);
        assert!((media[3] - geo.canvas.height.as_points()).abs() < 0.01);

        let trim: Vec<f64> = page
            .get(b"TrimBox")
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .map(|o| o.as_float().unwrap() as f64)
            .collect();
        let bleed_pt = geometry::bleed().as_points();
        assert!((trim[0] - bleed_pt).abs() < 0.01);
        assert!((trim[1] - bleed_pt).abs() < 0.01);
    }

    #[test]
    fn template_guides_sit_in_one_named_removable_optional_content_group() {
        let geo = cover_geometry(sku(), 212).unwrap();
        let doc = generate_template(&geo, &sample_metadata());
        let catalog = doc.catalog().unwrap();
        let oc_props = catalog.get(b"OCProperties").unwrap().as_dict().unwrap();
        let ocgs = oc_props.get(b"OCGs").unwrap().as_array().unwrap();
        assert_eq!(ocgs.len(), 1, "exactly one named OCG for all guides");
        let ocg_ref = ocgs[0].as_reference().unwrap();
        let ocg_dict = doc.get_dictionary(ocg_ref).unwrap();
        assert_eq!(ocg_dict.get(b"Type").unwrap().as_name().unwrap(), b"OCG");
        assert!(ocg_dict.get(b"Name").is_ok());

        let page_id = *doc.get_pages().values().next().unwrap();
        let content_ref = doc
            .get_dictionary(page_id)
            .unwrap()
            .get(b"Contents")
            .unwrap()
            .as_reference()
            .unwrap();
        let lopdf::Object::Stream(stream) = doc.get_object(content_ref).unwrap() else {
            panic!()
        };
        let text = String::from_utf8_lossy(&stream.get_plain_content().unwrap()).to_string();
        assert!(text.contains("/OC /MC0 BDC"), "{text}");
        assert!(text.contains("EMC"), "{text}");
    }

    #[test]
    fn template_is_marked_as_a_design_aid_in_legend_and_metadata() {
        let geo = cover_geometry(sku(), 212).unwrap();
        let doc = generate_template(&geo, &sample_metadata());

        let page_id = *doc.get_pages().values().next().unwrap();
        let content_ref = doc
            .get_dictionary(page_id)
            .unwrap()
            .get(b"Contents")
            .unwrap()
            .as_reference()
            .unwrap();
        let lopdf::Object::Stream(stream) = doc.get_object(content_ref).unwrap() else {
            panic!()
        };
        let text = String::from_utf8_lossy(&stream.get_plain_content().unwrap()).to_string();
        assert!(text.contains("NOT FOR SUBMISSION"), "{text}");
        assert!(
            text.contains("212"),
            "page count should appear in the legend: {text}"
        );

        let info_ref = doc.trailer.get(b"Info").unwrap().as_reference().unwrap();
        let info = doc.get_dictionary(info_ref).unwrap();
        let subject = info.get(b"Subject").unwrap().as_str().unwrap();
        assert!(String::from_utf8_lossy(subject).contains("NOT FOR SUBMISSION"));
    }

    // --- fitting supplied artwork ---

    fn single_page_doc(
        width_pt: f64,
        height_pt: f64,
        content: &[u8],
    ) -> (lopdf::Document, lopdf::ObjectId) {
        let mut doc = lopdf::Document::with_version("1.7");
        let content_id = doc.add_object(lopdf::Stream::new(dictionary! {}, content.to_vec()));
        let pages_id = doc.new_object_id();
        let page_id = doc.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => Object::Reference(pages_id),
            "MediaBox" => Object::Array(vec![0.0.into(), 0.0.into(), width_pt.into(), height_pt.into()]),
            "Contents" => Object::Reference(content_id),
        });
        let pages = dictionary! { "Type" => "Pages", "Kids" => vec![Object::Reference(page_id)], "Count" => 1 };
        doc.objects.insert(pages_id, Object::Dictionary(pages));
        let catalog_id = doc.add_object(
            dictionary! { "Type" => "Catalog", "Pages" => Object::Reference(pages_id) },
        );
        doc.trailer.set("Root", Object::Reference(catalog_id));
        (doc, page_id)
    }

    #[test]
    fn correctly_sized_cover_passes_through_unscaled() {
        let geo = cover_geometry(sku(), 212).unwrap();
        let (mut doc, page_id) = single_page_doc(
            geo.canvas.width.as_points(),
            geo.canvas.height.as_points(),
            b"",
        );
        let findings =
            fit_supplied_cover(&mut doc, page_id, &geo, crate::normalize::FitMode::Center).unwrap();
        assert!(findings.is_empty(), "{findings:?}");
        let page = doc.get_dictionary(page_id).unwrap();
        let media: Vec<f64> = page
            .get(b"MediaBox")
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .map(|o| o.as_float().unwrap() as f64)
            .collect();
        assert!((media[2] - geo.canvas.width.as_points()).abs() < 0.01);
    }

    #[test]
    fn wrong_spine_artwork_is_caught_and_not_stretched() {
        let geo = cover_geometry(sku(), 212).unwrap();
        // 20pt narrower than required — a plausible wrong-page-count spine.
        let wrong_width = geo.canvas.width.as_points() - 20.0;
        let (mut doc, page_id) = single_page_doc(wrong_width, geo.canvas.height.as_points(), b"");
        let findings =
            fit_supplied_cover(&mut doc, page_id, &geo, crate::normalize::FitMode::Center).unwrap();
        let f = findings
            .iter()
            .find(|f| f.code == "cover.spine-mismatch")
            .expect("mismatch finding");
        assert_eq!(f.severity, crate::report::Severity::Blocking);
        assert!(f.message.contains("20.0"), "{}", f.message);
        assert!(!f.fixable);

        // Center mode must not have stretched the content — scale stays 1.0,
        // confirmed indirectly: the page's own box is now the *required*
        // canvas (nest_page always resizes the page box), but content was
        // centred, not scaled, so this is really testing that no panic/
        // scaling error occurred and the finding fired instead of silently
        // resizing without comment.
        let page = doc.get_dictionary(page_id).unwrap();
        let media: Vec<f64> = page
            .get(b"MediaBox")
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .map(|o| o.as_float().unwrap() as f64)
            .collect();
        assert!((media[2] - geo.canvas.width.as_points()).abs() < 0.01);
    }

    #[test]
    fn three_files_are_assembled_at_their_computed_panels() {
        let geo = cover_geometry(sku(), 212).unwrap();
        let (back_doc, back_page) = single_page_doc(
            geo.back_panel.width().as_points(),
            geo.back_panel.height().as_points(),
            b"1 0 0 rg 0 0 1 1 re f",
        );
        let (spine_doc, spine_page) = single_page_doc(
            geo.spine.width().as_points(),
            geo.spine.height().as_points(),
            b"0 1 0 rg 0 0 1 1 re f",
        );
        let (front_doc, front_page) = single_page_doc(
            geo.front_panel.width().as_points(),
            geo.front_panel.height().as_points(),
            b"0 0 1 rg 0 0 1 1 re f",
        );

        let assembled = assemble_three_panel_cover(
            (&back_doc, back_page),
            (&spine_doc, spine_page),
            (&front_doc, front_page),
            &geo,
        )
        .unwrap();

        assert_eq!(assembled.get_pages().len(), 1);
        let page_id = *assembled.get_pages().values().next().unwrap();
        let page = assembled.get_dictionary(page_id).unwrap();
        let media: Vec<f64> = page
            .get(b"MediaBox")
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .map(|o| o.as_float().unwrap() as f64)
            .collect();
        assert!((media[2] - geo.canvas.width.as_points()).abs() < 0.01);

        let resources = page.get(b"Resources").unwrap().as_dict().unwrap();
        let xobjects = resources.get(b"XObject").unwrap().as_dict().unwrap();
        assert!(xobjects.get(b"Bk").is_ok());
        assert!(xobjects.get(b"Sp").is_ok());
        assert!(xobjects.get(b"Fr").is_ok());

        let content_ref = page.get(b"Contents").unwrap().as_reference().unwrap();
        let Object::Stream(stream) = assembled.get_object(content_ref).unwrap() else {
            panic!()
        };
        let text = String::from_utf8_lossy(&stream.get_plain_content().unwrap()).to_string();
        // Left-to-right order: back, then spine, then front.
        let bk_pos = text.find("/Bk Do").unwrap();
        let sp_pos = text.find("/Sp Do").unwrap();
        let fr_pos = text.find("/Fr Do").unwrap();
        assert!(bk_pos < sp_pos && sp_pos < fr_pos, "{text}");
    }

    #[test]
    fn cover_structural_rules_strip_annotations_and_refuse_encryption() {
        let (mut doc, _) = single_page_doc(920.0, 666.0, b"");
        let summary = apply_cover_structural_rules(&mut doc).unwrap();
        assert!(summary.page_layout_forced);
    }

    // --- combined preview PDF ---

    fn interior_doc_with_n_pages(n: usize, width_pt: f64, height_pt: f64) -> lopdf::Document {
        let mut doc = lopdf::Document::with_version("1.7");
        let pages_id = doc.new_object_id();
        let mut kids = Vec::new();
        for i in 0..n {
            let content_id = doc.add_object(lopdf::Stream::new(
                dictionary! {},
                format!("BT ({i}) Tj ET").into_bytes(),
            ));
            let page_id = doc.add_object(dictionary! {
                "Type" => "Page",
                "Parent" => Object::Reference(pages_id),
                "MediaBox" => Object::Array(vec![0.0.into(), 0.0.into(), width_pt.into(), height_pt.into()]),
                "Contents" => Object::Reference(content_id),
            });
            kids.push(Object::Reference(page_id));
        }
        let pages = dictionary! { "Type" => "Pages", "Kids" => kids, "Count" => n as i64 };
        doc.objects.insert(pages_id, Object::Dictionary(pages));
        let catalog_id = doc.add_object(
            dictionary! { "Type" => "Catalog", "Pages" => Object::Reference(pages_id) },
        );
        doc.trailer.set("Root", Object::Reference(catalog_id));
        doc
    }

    #[test]
    fn preview_page_order_is_front_then_interior_then_back() {
        let geo = cover_geometry(sku(), 32).unwrap();
        let (cover_doc, cover_page_id) = single_page_doc(
            geo.canvas.width.as_points(),
            geo.canvas.height.as_points(),
            b"",
        );
        let interior = interior_doc_with_n_pages(32, 450.0, 666.0);

        let preview =
            build_combined_preview(&cover_doc, cover_page_id, &geo, &interior, sku().trim_size)
                .unwrap();
        assert_eq!(preview.get_pages().len(), 34);
    }

    #[test]
    fn preview_front_and_back_pages_are_trim_sized_not_wrap_sized() {
        let geo = cover_geometry(sku(), 32).unwrap();
        let (cover_doc, cover_page_id) = single_page_doc(
            geo.canvas.width.as_points(),
            geo.canvas.height.as_points(),
            b"",
        );
        let interior = interior_doc_with_n_pages(32, 450.0, 666.0);

        let preview =
            build_combined_preview(&cover_doc, cover_page_id, &geo, &interior, sku().trim_size)
                .unwrap();
        let required_size = geometry::required_page_size(sku().trim_size);

        let first_id = *preview.get_pages().get(&1).unwrap();
        let last_id = *preview.get_pages().get(&34).unwrap();
        for page_id in [first_id, last_id] {
            let page = preview.get_dictionary(page_id).unwrap();
            let media: Vec<f64> = page
                .get(b"MediaBox")
                .unwrap()
                .as_array()
                .unwrap()
                .iter()
                .map(|o| o.as_float().unwrap() as f64)
                .collect();
            assert!(
                (media[2] - required_size.width.as_points()).abs() < 0.01,
                "{media:?}"
            );
            assert!(
                (media[3] - required_size.height.as_points()).abs() < 0.01,
                "{media:?}"
            );
            // Must not be the full wrap-canvas width, which includes the spine.
            assert!((media[2] - geo.canvas.width.as_points()).abs() > 1.0);
        }
    }

    #[test]
    fn preview_is_marked_as_a_proof_in_stamp_and_metadata() {
        let geo = cover_geometry(sku(), 32).unwrap();
        let (cover_doc, cover_page_id) = single_page_doc(
            geo.canvas.width.as_points(),
            geo.canvas.height.as_points(),
            b"",
        );
        let interior = interior_doc_with_n_pages(32, 450.0, 666.0);

        let preview =
            build_combined_preview(&cover_doc, cover_page_id, &geo, &interior, sku().trim_size)
                .unwrap();

        let info_ref = preview
            .trailer
            .get(b"Info")
            .unwrap()
            .as_reference()
            .unwrap();
        let subject = preview
            .get_dictionary(info_ref)
            .unwrap()
            .get(b"Subject")
            .unwrap()
            .as_str()
            .unwrap();
        assert!(String::from_utf8_lossy(subject).contains("NOT FOR SUBMISSION"));

        let first_id = *preview.get_pages().get(&1).unwrap();
        let content_ref = preview
            .get_dictionary(first_id)
            .unwrap()
            .get(b"Contents")
            .unwrap()
            .as_reference()
            .unwrap();
        let Object::Stream(stream) = preview.get_object(content_ref).unwrap() else {
            panic!()
        };
        let text = String::from_utf8_lossy(&stream.get_plain_content().unwrap()).to_string();
        assert!(text.contains("PROOF FOR REVIEW"), "{text}");
    }

    #[test]
    fn preview_generation_does_not_mutate_the_cover_or_interior_inputs() {
        let geo = cover_geometry(sku(), 32).unwrap();
        let (cover_doc, cover_page_id) = single_page_doc(
            geo.canvas.width.as_points(),
            geo.canvas.height.as_points(),
            b"1 0 0 rg 0 0 1 1 re f",
        );
        let interior = interior_doc_with_n_pages(32, 450.0, 666.0);

        let mut cover_bytes_before = Vec::new();
        cover_doc.clone().save_to(&mut cover_bytes_before).unwrap();
        let mut interior_bytes_before = Vec::new();
        interior
            .clone()
            .save_to(&mut interior_bytes_before)
            .unwrap();

        let _preview =
            build_combined_preview(&cover_doc, cover_page_id, &geo, &interior, sku().trim_size)
                .unwrap();

        let mut cover_bytes_after = Vec::new();
        cover_doc.clone().save_to(&mut cover_bytes_after).unwrap();
        let mut interior_bytes_after = Vec::new();
        interior.clone().save_to(&mut interior_bytes_after).unwrap();

        assert_eq!(cover_bytes_before, cover_bytes_after);
        assert_eq!(interior_bytes_before, interior_bytes_after);
    }

    // --- case wrap: verified against Lulu's live production API, 2026-09-03 ---
    // Each case asserts within 0.6pt, since two probes carried that much
    // rounding noise (the A4 trim's own catalog values are pre-rounded to 2
    // decimal places, and one 6x9in probe landed 0.5pt off at a spine-table
    // band boundary) — see `CASE_WRAP_OVERHANG_IN`'s doc comment.

    fn assert_close(actual: Length, expected_pt: f64, label: &str) {
        assert!(
            (actual.as_points() - expected_pt).abs() < 0.6,
            "{label}: got {:.3}pt, expected {expected_pt}pt",
            actual.as_points()
        );
    }

    #[test]
    fn case_wrap_matches_live_lulu_data_6x9in() {
        let entry = crate::catalog::lookup("0600X0900.BW.STD.CW.060UW444.MXX").unwrap();
        let cases: &[(u32, f64, f64)] = &[
            (24, 1008.0, 774.0),
            (100, 1026.0, 774.0),
            (212, 1044.0, 774.0),
            (400, 1076.0, 774.0),
            (800, 1143.0, 774.0),
        ];
        for &(pages, expected_w, expected_h) in cases {
            let geo = case_wrap_geometry(entry, pages).unwrap();
            assert_close(
                geo.canvas.width,
                expected_w,
                &format!("{pages} pages width"),
            );
            assert_close(
                geo.canvas.height,
                expected_h,
                &format!("{pages} pages height"),
            );
        }
    }

    #[test]
    fn case_wrap_matches_live_lulu_data_a4() {
        let entry = crate::catalog::lookup("0827X1169.BW.STD.CW.060UC444.GXX").unwrap();
        let cases: &[(u32, f64, f64)] = &[
            (24, 1335.0, 968.0),
            (212, 1371.0, 968.0),
            (800, 1470.0, 968.0),
        ];
        for &(pages, expected_w, expected_h) in cases {
            let geo = case_wrap_geometry(entry, pages).unwrap();
            assert_close(
                geo.canvas.width,
                expected_w,
                &format!("{pages} pages width"),
            );
            assert_close(
                geo.canvas.height,
                expected_h,
                &format!("{pages} pages height"),
            );
        }
    }

    #[test]
    fn case_wrap_height_is_independent_of_page_count() {
        let entry = crate::catalog::lookup("0600X0900.BW.STD.CW.060UW444.MXX").unwrap();
        let h24 = case_wrap_geometry(entry, 24).unwrap().canvas.height;
        let h800 = case_wrap_geometry(entry, 800).unwrap().canvas.height;
        assert_eq!(h24, h800);
    }

    #[test]
    fn case_wrap_reports_hinge_zones() {
        let entry = crate::catalog::lookup("0600X0900.BW.STD.CW.060UW444.MXX").unwrap();
        let geo = case_wrap_geometry(entry, 212).unwrap();
        let (left, right) = geo.hinge_zones.expect("case wrap must report hinge zones");
        assert!((left.width().as_inches() - 0.25).abs() < 1e-9);
        assert!((right.width().as_inches() - 0.25).abs() < 1e-9);
    }

    #[test]
    fn case_wrap_goes_through_cover_geometry_not_the_table() {
        let entry = crate::catalog::lookup("0600X0900.BW.STD.CW.060UW444.MXX").unwrap();
        // 212 is conformant (multiple of 4, matching the binding's rule).
        let geo = cover_geometry(entry, 212).unwrap();
        assert_close(geo.canvas.width, 1044.0, "212 pages width");
    }
}
