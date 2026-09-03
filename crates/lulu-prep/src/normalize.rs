//! Turning an arbitrary interior PDF into a Lulu-conformant one: page
//! geometry (nesting each source page as a form XObject, baking rotation,
//! fitting it to the required bleed size), blank-page padding, and
//! structural sanitation.

use crate::catalog::CatalogEntry;
use crate::geometry::PageCountRules;
use crate::report::{codes, Finding, Report, Severity, SCHEMA_VERSION};
use crate::units::{Length, Matrix, Rect, Size};
use lopdf::dictionary;

/// A matrix that reproduces, in an unrotated page of the returned size,
/// exactly what a viewer would display for a page of `old_size` carrying
/// PDF `/Rotate rotation_degrees` (clockwise). `rotation_degrees` must
/// already be normalized into `{0, 90, 180, 270}` (see
/// [`crate::pdf::effective_page_size`]'s rotation handling).
///
/// Returns the matrix and the size of the page it must be drawn onto.
pub fn rotation_bake(rotation_degrees: i64, old_size: Size) -> (Matrix, Size) {
    let w = old_size.width;
    let h = old_size.height;
    match rotation_degrees {
        90 => (
            Matrix {
                a: 0.0,
                b: -1.0,
                c: 1.0,
                d: 0.0,
                e: 0.0,
                f: w.as_points(),
            },
            Size::new(h, w),
        ),
        180 => (
            Matrix {
                a: -1.0,
                b: 0.0,
                c: 0.0,
                d: -1.0,
                e: w.as_points(),
                f: h.as_points(),
            },
            Size::new(w, h),
        ),
        270 => (
            Matrix {
                a: 0.0,
                b: 1.0,
                c: -1.0,
                d: 0.0,
                e: h.as_points(),
                f: 0.0,
            },
            Size::new(h, w),
        ),
        _ => (Matrix::IDENTITY, Size::new(w, h)),
    }
}

/// How a rotation-baked page (of `content_size`) is placed onto the
/// product's required page (of `required_size`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FitMode {
    /// Centre the content at its original scale — the default. Leaves an
    /// unprinted border where the source has no bleed, but never moves
    /// content relative to the trim edge.
    #[default]
    Center,
    /// Scale the content uniformly so it fully covers the bleed area,
    /// cropping equally on all sides.
    ScaleToBleed,
    /// Keep the content at original scale, centred as [`FitMode::Center`]
    /// does, and fill the surrounding bleed area with a flat colour rather
    /// than leaving it blank (a documented simplification of Lulu's "extend
    /// the outermost edge pixels **or** fill colour" allowance — true edge
    /// extension would require decoding and resampling raster content).
    StretchMargins,
}

/// The placement computed for one page: the transform to apply to the
/// rotation-baked content, and — for [`FitMode::ScaleToBleed`] — the
/// enlargement factor actually used, for reporting.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Placement {
    pub transform: Matrix,
    pub scale: f64,
}

/// Computes where rotation-baked content of `content_size` should be drawn
/// on a page of `required_size`, under `mode`.
pub fn fit_placement(content_size: Size, required_size: Size, mode: FitMode) -> Placement {
    match mode {
        FitMode::Center | FitMode::StretchMargins => {
            let dx = (required_size.width - content_size.width) / 2.0;
            let dy = (required_size.height - content_size.height) / 2.0;
            Placement {
                transform: Matrix::translate(dx, dy),
                scale: 1.0,
            }
        }
        FitMode::ScaleToBleed => {
            let scale_x = required_size.width.as_points() / content_size.width.as_points();
            let scale_y = required_size.height.as_points() / content_size.height.as_points();
            let scale = scale_x.max(scale_y);
            let scaled = Size::new(content_size.width * scale, content_size.height * scale);
            let dx = (required_size.width - scaled.width) / 2.0;
            let dy = (required_size.height - scaled.height) / 2.0;
            Placement {
                transform: Matrix::scale_uniform(scale).then(Matrix::translate(dx, dy)),
                scale,
            }
        }
    }
}

/// The output page's box entries for a product's required page size:
/// `MediaBox`/`CropBox`/`BleedBox` cover the full bleed page; `TrimBox`/`ArtBox`
/// are inset by [`crate::geometry::bleed`] on every side.
pub struct PageBoxes {
    pub media_bleed_box: Rect,
    pub trim_art_box: Rect,
}

pub fn page_boxes(required_size: Size) -> PageBoxes {
    let full = Rect::from_origin_size(required_size);
    PageBoxes {
        media_bleed_box: full,
        trim_art_box: full.inset(crate::geometry::bleed()),
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SpreadSplitError {
    #[error(transparent)]
    Pdf(#[from] lopdf::Error),
    #[error(transparent)]
    Geometry(#[from] crate::pdf::PageGeometryError),
}

/// Splits every page down its vertical centre into two single pages, left
/// half first, replacing the original spread pages in the document's page
/// order. Opt-in only (see [`NormalizeOptions::split_spreads`]) — this is
/// never called based on an aspect-ratio guess; see
/// [`landscape_pages_finding`] for the non-splitting alternative.
///
/// Each half wraps the *original* page's content as a Form XObject (so
/// vector and image data are preserved exactly, matching [`nest_page`]'s
/// approach), drawn under a translation that brings its half into the new
/// page's own origin, and an explicit clip to that half's rectangle — the
/// clip is what actually crops the other half away, since content spilling
/// outside a page's own box is not something the PDF spec itself guarantees
/// a viewer or RIP will clip.
pub fn split_spread_pages(doc: &mut lopdf::Document) -> Result<u32, SpreadSplitError> {
    let page_ids: Vec<lopdf::ObjectId> = doc.page_iter().collect();
    let mut new_kids = Vec::with_capacity(page_ids.len() * 2);
    let mut split_count = 0u32;

    for page_id in page_ids {
        let own_rect = crate::pdf::own_box_rect(doc, page_id)?;
        let half_width = own_rect.width() * 0.5;
        let height = own_rect.height();

        let content_bytes = doc.get_page_content(page_id);
        let resources = doc
            .get_page_resources(page_id)
            .ok()
            .and_then(|(r, _)| r)
            .cloned()
            .unwrap_or_default();
        let form_dict = dictionary! {
            "Type" => "XObject",
            "Subtype" => "Form",
            "BBox" => rect_to_array(own_rect),
            "Resources" => resources,
        };
        let form_id = doc.add_object(lopdf::Object::Stream(lopdf::Stream::new(
            form_dict,
            content_bytes,
        )));

        let left_width = half_width;
        let right_width = own_rect.width() - half_width;
        let to_left_origin =
            Matrix::translate(Length::ZERO - own_rect.x0, Length::ZERO - own_rect.y0);
        let to_right_origin = Matrix::translate(
            Length::ZERO - (own_rect.x0 + half_width),
            Length::ZERO - own_rect.y0,
        );

        let half_page = |doc: &mut lopdf::Document, width: Length, cm: Matrix| -> lopdf::ObjectId {
            let cm = cm.as_cm_operands();
            let content = format!(
                "q 0 0 {} {} re W n {} {} {} {} {} {} cm /Fx0 Do Q",
                width.as_points(),
                height.as_points(),
                cm[0],
                cm[1],
                cm[2],
                cm[3],
                cm[4],
                cm[5],
            );
            let content_id =
                doc.add_object(lopdf::Stream::new(dictionary! {}, content.into_bytes()));
            doc.add_object(dictionary! {
                "Type" => "Page",
                "MediaBox" => rect_to_array(Rect::from_origin_size(Size::new(width, height))),
                "Contents" => lopdf::Object::Reference(content_id),
                "Resources" => dictionary! { "XObject" => dictionary! { "Fx0" => lopdf::Object::Reference(form_id) } },
            })
        };

        let left_id = half_page(doc, left_width, to_left_origin);
        let right_id = half_page(doc, right_width, to_right_origin);
        new_kids.push(lopdf::Object::Reference(left_id));
        new_kids.push(lopdf::Object::Reference(right_id));
        split_count += 1;
    }

    let pages_id = doc.catalog()?.get(b"Pages")?.as_reference()?;
    let count = new_kids.len() as i64;
    let pages_dict = doc.get_dictionary_mut(pages_id)?;
    pages_dict.set("Kids", new_kids.clone());
    pages_dict.set("Count", count);
    for kid in &new_kids {
        if let lopdf::Object::Reference(id) = kid {
            if let Ok(page_dict) = doc.get_dictionary_mut(*id) {
                page_dict.set("Parent", lopdf::Object::Reference(pages_id));
            }
        }
    }

    Ok(split_count)
}

/// An informational finding suggesting `split_spreads` when pages are wider
/// than they are tall — but only ever a *suggestion*: aspect ratio alone
/// cannot distinguish a genuine landscape product from an unsplit spread,
/// so this never triggers a split on its own (`specs/interior-normalization/spec.md`,
/// "Splitting is never automatic").
fn landscape_pages_finding(doc: &lopdf::Document, page_ids: &[lopdf::ObjectId]) -> Option<Finding> {
    let mut landscape_pages = Vec::new();
    for (i, &page_id) in page_ids.iter().enumerate() {
        if let Ok(rect) = crate::pdf::own_box_rect(doc, page_id) {
            if rect.width() > rect.height() {
                landscape_pages.push((i + 1) as u32);
            }
        }
    }
    if landscape_pages.is_empty() {
        return None;
    }
    Some(
        Finding::new(
            "normalize.landscape-pages-observed",
            Severity::Info,
            format!(
                "{} page(s) are wider than tall; if this source is imposed as two-up spreads, normalize with spread splitting enabled",
                landscape_pages.len()
            ),
        )
        .with_pages(landscape_pages),
    )
}

#[derive(Debug, thiserror::Error)]
pub enum PadError {
    #[error("{requested} pages exceeds this product's maximum of {max}; split the content or choose a different product")]
    AboveMaximum { requested: u32, max: u32 },
    #[error("could not locate the document's page tree: {0}")]
    Pdf(#[from] lopdf::Error),
}

/// Appends blank pages (of `required_size`, with no content stream) until the
/// document's page count satisfies `rules` — the product's minimum and its
/// binding's divisibility rule — appended at the end, matching Lulu's own
/// behaviour of adding white pages to the back of the book. Returns the
/// 1-based page numbers of the pages added. Refuses (adding nothing) when no
/// conformant count exists below the product's maximum.
pub fn pad_pages(
    doc: &mut lopdf::Document,
    required_size: Size,
    rules: &crate::geometry::PageCountRules,
) -> Result<Vec<u32>, PadError> {
    let current = doc.get_pages().len() as u32;
    let target = rules.next_conformant(current).map_err(
        |crate::geometry::PageCountError::AboveMaximum { requested, max }| PadError::AboveMaximum {
            requested,
            max,
        },
    )?;
    if target == current {
        return Ok(Vec::new());
    }

    let pages_id = doc.catalog()?.get(b"Pages")?.as_reference()?;
    let boxes = page_boxes(required_size);
    let mut new_numbers = Vec::new();

    for i in 0..(target - current) {
        let page_id = doc.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => lopdf::Object::Reference(pages_id),
            "MediaBox" => rect_to_array(boxes.media_bleed_box),
            "CropBox" => rect_to_array(boxes.media_bleed_box),
            "BleedBox" => rect_to_array(boxes.media_bleed_box),
            "TrimBox" => rect_to_array(boxes.trim_art_box),
            "ArtBox" => rect_to_array(boxes.trim_art_box),
        });
        let pages_dict = doc.get_dictionary_mut(pages_id)?;
        pages_dict
            .get_mut(b"Kids")?
            .as_array_mut()?
            .push(lopdf::Object::Reference(page_id));
        let count = pages_dict.get(b"Count")?.as_i64()?;
        pages_dict.set("Count", count + 1);
        new_numbers.push(current + i + 1);
    }

    Ok(new_numbers)
}

/// What [`sanitize_structure`] removed, for reporting.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SanitizeSummary {
    pub pages_with_annotations_cleared: Vec<u32>,
    pub acroform_removed: bool,
    pub javascript_removed: bool,
    pub embedded_files_removed: bool,
    pub page_layout_forced: bool,
}

/// Turns what [`sanitize_structure`] removed into the info-level findings
/// both `normalize_interior` and the CLI's cover command report, so the two
/// callers never describe the same summary differently.
pub fn sanitize_summary_findings(summary: &SanitizeSummary) -> Vec<Finding> {
    let mut findings = Vec::new();
    if !summary.pages_with_annotations_cleared.is_empty() {
        findings.push(
            Finding::new(
                "normalize.annotations-removed",
                Severity::Info,
                format!(
                    "removed annotations from {} page(s)",
                    summary.pages_with_annotations_cleared.len()
                ),
            )
            .with_pages(summary.pages_with_annotations_cleared.clone()),
        );
    }
    if summary.acroform_removed {
        findings.push(Finding::new(
            "normalize.acroform-removed",
            Severity::Info,
            "removed the AcroForm and its fields".to_string(),
        ));
    }
    if summary.javascript_removed {
        findings.push(Finding::new(
            "normalize.javascript-removed",
            Severity::Info,
            "removed document-level JavaScript".to_string(),
        ));
    }
    if summary.embedded_files_removed {
        findings.push(Finding::new(
            "normalize.embedded-files-removed",
            Severity::Info,
            "removed embedded file(s)".to_string(),
        ));
    }
    findings
}

/// Strips every structure Lulu prohibits or that carries no print meaning:
/// all page annotations (which removes any annotation-level JavaScript and
/// multimedia annotations along with them, since those are just other
/// annotation subtypes), the catalog's `AcroForm` and its fields,
/// document-level JavaScript and embedded files (both live under the
/// catalog's `/Names` tree), and forces a single-page `/PageLayout`.
/// Encryption is not handled here: an empty-password-encrypted file is
/// already fully decrypted with its `/Encrypt` trailer entry removed by the
/// time [`crate::pdf::load_from_bytes`] returns it (see that function's
/// docs) — a file that still needs a real password is refused before
/// normalization runs at all (see [`normalize_interior`]).
pub fn sanitize_structure(doc: &mut lopdf::Document) -> SanitizeSummary {
    let mut summary = SanitizeSummary::default();

    let page_ids: Vec<(u32, lopdf::ObjectId)> = doc
        .page_iter()
        .enumerate()
        .map(|(i, id)| ((i + 1) as u32, id))
        .collect();
    for (page_number, page_id) in page_ids {
        if let Ok(page_dict) = doc.get_dictionary_mut(page_id) {
            if page_dict.remove(b"Annots").is_some() {
                summary.pages_with_annotations_cleared.push(page_number);
            }
        }
    }

    if let Ok(catalog) = doc.catalog_mut() {
        if catalog.remove(b"AcroForm").is_some() {
            summary.acroform_removed = true;
        }
        if let Ok(names) = catalog.get_mut(b"Names").and_then(|o| o.as_dict_mut()) {
            if names.remove(b"JavaScript").is_some() {
                summary.javascript_removed = true;
            }
            if names.remove(b"EmbeddedFiles").is_some() {
                summary.embedded_files_removed = true;
            }
        }
        catalog.set("PageLayout", lopdf::Object::Name(b"SinglePage".to_vec()));
        summary.page_layout_forced = true;
    }

    summary
}

/// The gutter-compensation shift for one page: odd pages move toward
/// increasing x (the right), even pages toward decreasing x (the left), so
/// inner-edge content clears the binding. Off by default — pass
/// [`Matrix::IDENTITY`] to [`nest_page`] when the caller hasn't opted in;
/// this only computes what the shift *would* be.
pub fn gutter_shift(page_number: u32, gutter: Length) -> Matrix {
    if page_number % 2 == 1 {
        Matrix::translate(gutter, Length::ZERO)
    } else {
        Matrix::translate(Length::ZERO - gutter, Length::ZERO)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum NestError {
    #[error("could not read source page geometry: {0}")]
    Geometry(#[from] crate::pdf::PageGeometryError),
}

fn rect_to_array(r: Rect) -> lopdf::Object {
    lopdf::Object::Array(
        r.as_pdf_array_points()
            .into_iter()
            .map(|v| lopdf::Object::Real(v as f32))
            .collect(),
    )
}

/// Embeds a source page's content as a form XObject on a fresh page sized
/// `required_size`, under a transform that bakes in the page's `/Rotate`
/// (if any), applies `fit_mode`, and then applies `extra_transform` (pass
/// [`Matrix::IDENTITY`] when there is none — e.g. the gutter shift, computed
/// by the caller from the final page count and this page's parity). The
/// source page's content, images, and fonts are reused unchanged — nothing
/// here resamples or rasterizes. Mutates `doc` in place: the same object
/// graph, just a rewritten page.
pub fn nest_page(
    doc: &mut lopdf::Document,
    page_id: lopdf::ObjectId,
    required_size: Size,
    fit_mode: FitMode,
    extra_transform: Matrix,
) -> Result<Placement, NestError> {
    let own_rect = crate::pdf::own_box_rect(doc, page_id)?;
    let rotation = crate::pdf::rotation_degrees(doc, page_id)?;
    let own_size = Size::new(own_rect.width(), own_rect.height());

    let (rotate_matrix, rotated_size) = rotation_bake(rotation, own_size);
    let placement = fit_placement(rotated_size, required_size, fit_mode);

    // The form's BBox is the source page's own (absolute) box, so its content
    // — untouched — is already correctly positioned in form space; this
    // transform carries it from there through the rotation bake to its
    // final placement on the new page.
    let to_origin = Matrix::translate(Length::ZERO - own_rect.x0, Length::ZERO - own_rect.y0);
    let full_transform = to_origin
        .then(rotate_matrix)
        .then(placement.transform)
        .then(extra_transform);

    let content_bytes = doc.get_page_content(page_id);
    let resources = doc
        .get_page_resources(page_id)
        .ok()
        .and_then(|(r, _)| r)
        .cloned()
        .unwrap_or_default();

    let form_dict = dictionary! {
        "Type" => "XObject",
        "Subtype" => "Form",
        "BBox" => rect_to_array(own_rect),
        "Resources" => resources,
    };
    let form_id = doc.add_object(lopdf::Object::Stream(lopdf::Stream::new(
        form_dict,
        content_bytes,
    )));

    let cm = full_transform.as_cm_operands();
    let new_content = format!(
        "q {} {} {} {} {} {} cm /Fx0 Do Q",
        cm[0], cm[1], cm[2], cm[3], cm[4], cm[5]
    );
    let new_content_id =
        doc.add_object(lopdf::Stream::new(dictionary! {}, new_content.into_bytes()));

    let boxes = page_boxes(required_size);
    let page_dict = doc
        .get_dictionary_mut(page_id)
        .map_err(crate::pdf::PageGeometryError::from)?;
    page_dict.set("Contents", lopdf::Object::Reference(new_content_id));
    page_dict.set(
        "Resources",
        dictionary! { "XObject" => dictionary! { "Fx0" => lopdf::Object::Reference(form_id) } },
    );
    page_dict.set("MediaBox", rect_to_array(boxes.media_bleed_box));
    page_dict.set("CropBox", rect_to_array(boxes.media_bleed_box));
    page_dict.set("BleedBox", rect_to_array(boxes.media_bleed_box));
    page_dict.set("TrimBox", rect_to_array(boxes.trim_art_box));
    page_dict.set("ArtBox", rect_to_array(boxes.trim_art_box));
    page_dict.remove(b"Rotate");

    Ok(placement)
}

/// Options controlling how [`normalize_interior`] fits and repositions content.
#[derive(Debug, Clone, Copy, Default)]
pub struct NormalizeOptions {
    pub fit_mode: FitMode,
    /// Off by default — a source already laid out with its own gutter would
    /// otherwise be double-shifted.
    pub apply_gutter: bool,
    /// Opt-in only, and never inferred from aspect ratio — a legitimately
    /// landscape product is indistinguishable from a two-up spread by
    /// geometry alone. See [`split_spread_pages`].
    pub split_spreads: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum NormalizeInteriorError {
    #[error("could not parse the input PDF: {0}")]
    Load(#[from] crate::pdf::LoadError),
    #[error(
        "this file is encrypted with a password; supply it and decrypt the file before normalizing"
    )]
    PasswordRequired,
    #[error("{requested} pages exceeds this product's maximum of {max}; split the content or choose a different product")]
    AboveMaximum { requested: u32, max: u32 },
    #[error(transparent)]
    Nest(#[from] NestError),
    #[error(transparent)]
    Pad(#[from] PadError),
    #[error(transparent)]
    SpreadSplit(#[from] SpreadSplitError),
    #[error("could not write the normalized PDF: {0}")]
    Save(#[from] std::io::Error),
}

/// The result of a successful [`normalize_interior`] run.
#[derive(Debug)]
pub struct NormalizeOutcome {
    pub output_bytes: Vec<u8>,
    pub final_page_count: u32,
    pub padded_pages: Vec<u32>,
    pub gutter_applied: Option<Length>,
    pub sanitize_summary: SanitizeSummary,
    /// The full run report: this function's own findings (what it changed)
    /// followed by a preflight of its own output, so any finding it could
    /// not fix is repeated here rather than silently dropped.
    pub report: Report,
}

/// Turns an arbitrary interior PDF into a Lulu-conformant one for `product`:
/// nests every page's content at the required bleed size (baking in any
/// `/Rotate` and applying `options.fit_mode`), optionally applies the gutter
/// shift, pads with blank pages to the product's minimum and divisibility
/// rule, and strips every structure Lulu prohibits. Refuses — writing no
/// output — when the input still needs a real password or when no
/// conformant page count exists below the product's maximum.
pub fn normalize_interior(
    bytes: &[u8],
    product: &CatalogEntry,
    options: NormalizeOptions,
) -> Result<NormalizeOutcome, NormalizeInteriorError> {
    let mut doc = crate::pdf::load_from_bytes(bytes)?;
    if doc.is_encrypted() {
        return Err(NormalizeInteriorError::PasswordRequired);
    }

    let mut findings = Vec::new();
    if options.split_spreads {
        split_spread_pages(&mut doc)?;
    } else {
        let page_ids: Vec<lopdf::ObjectId> = doc.page_iter().collect();
        findings.extend(landscape_pages_finding(&doc, &page_ids));
    }

    let required_size = crate::geometry::required_page_size(product.trim_size);
    let rules = PageCountRules::from_catalog_entry(product);

    let original_count = doc.get_pages().len() as u32;
    let final_count = rules.next_conformant(original_count).map_err(
        |crate::geometry::PageCountError::AboveMaximum { requested, max }| {
            NormalizeInteriorError::AboveMaximum { requested, max }
        },
    )?;

    let gutter_allowance = crate::geometry::gutter_allowance(final_count);
    let gutter_applied = options.apply_gutter.then_some(gutter_allowance.gutter);

    if gutter_allowance.below_advisory_floor {
        findings.push(Finding::new(
            codes::GUTTER_BELOW_ADVISORY_FLOOR,
            Severity::Warning,
            format!(
                "at {final_count} pages, Lulu's page-count-banded gutter table gives {:.3} in, below the 0.200 in minimum Lulu's own PDF creation settings advise; this is a warning only — the banded table, not the advisory floor, is what this tool applies",
                gutter_allowance.gutter.as_inches()
            ),
        ));
    }
    let page_ids: Vec<lopdf::ObjectId> = doc.page_iter().collect();
    for (i, page_id) in page_ids.iter().enumerate() {
        let page_number = (i + 1) as u32;
        let extra = gutter_applied
            .map(|g| gutter_shift(page_number, g))
            .unwrap_or(Matrix::IDENTITY);
        let placement = nest_page(&mut doc, *page_id, required_size, options.fit_mode, extra)?;
        if options.fit_mode == FitMode::ScaleToBleed && (placement.scale - 1.0).abs() > 1e-9 {
            findings.push(Finding::new(
                "normalize.scaled-to-bleed",
                Severity::Info,
                format!(
                    "page {page_number} scaled by {:.1}% to cover the full bleed area",
                    (placement.scale - 1.0) * 100.0
                ),
            ));
        }
    }

    let padded_pages = pad_pages(&mut doc, required_size, &rules)?;
    if !padded_pages.is_empty() {
        findings.push(
            Finding::new(
                "normalize.pages-padded",
                Severity::Info,
                format!(
                    "appended {} blank page(s) to satisfy this product's page-count rules: {:?}",
                    padded_pages.len(),
                    padded_pages
                ),
            )
            .with_pages(padded_pages.clone()),
        );
    }

    let sanitize_summary = sanitize_structure(&mut doc);
    findings.extend(sanitize_summary_findings(&sanitize_summary));

    let mut output_bytes = Vec::new();
    doc.save_to(&mut output_bytes)?;

    let mut preflight_report = crate::preflight::preflight(&output_bytes, Some(product));
    findings.append(&mut preflight_report.findings);

    let report = Report {
        schema_version: SCHEMA_VERSION,
        input_digest: None,
        product_sku: Some(product.sku.clone()),
        page_count: Some(final_count),
        catalog_fetch_date: Some(crate::catalog::metadata().fetch_date.clone()),
        tool_version: env!("CARGO_PKG_VERSION").to_string(),
        detected_tools: Vec::new(),
        stages: Vec::new(),
        findings,
        generated_at: crate::report::now_rfc3339(),
    };

    Ok(NormalizeOutcome {
        output_bytes,
        final_page_count: final_count,
        padded_pages,
        gutter_applied,
        sanitize_summary,
        report,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pt(p: f64) -> Length {
        Length::from_points(p)
    }

    fn corners(size: Size) -> [(Length, Length); 4] {
        [
            (Length::ZERO, Length::ZERO),
            (size.width, Length::ZERO),
            (size.width, size.height),
            (Length::ZERO, size.height),
        ]
    }

    fn assert_pt_eq(a: Length, b: Length) {
        assert!(
            (a.as_points() - b.as_points()).abs() < 1e-9,
            "{} != {}",
            a.as_points(),
            b.as_points()
        );
    }

    #[test]
    fn rotation_zero_is_identity() {
        let old = Size::new(pt(432.0), pt(648.0));
        let (m, new_size) = rotation_bake(0, old);
        assert_eq!(m, Matrix::IDENTITY);
        assert_eq!(new_size, old);
    }

    #[test]
    fn rotation_90_maps_corners_into_the_swapped_bounding_box() {
        let old = Size::new(pt(432.0), pt(648.0)); // W=432, H=648
        let (m, new_size) = rotation_bake(90, old);
        assert_eq!(new_size, Size::new(pt(648.0), pt(432.0)));

        let mapped: Vec<(Length, Length)> = corners(old)
            .iter()
            .map(|&(x, y)| m.apply_to_point(x, y))
            .collect();
        // Every mapped corner must land within the new page's bounding box.
        for (x, y) in &mapped {
            assert!(x.as_points() >= -1e-6 && x.as_points() <= new_size.width.as_points() + 1e-6);
            assert!(y.as_points() >= -1e-6 && y.as_points() <= new_size.height.as_points() + 1e-6);
        }
        // Old bottom-left (0,0) -> (0, W): the corner the viewer sees rotated
        // 90 degrees clockwise into the new page's top-left region.
        assert_pt_eq(mapped[0].0, Length::ZERO);
        assert_pt_eq(mapped[0].1, old.width);
        // Old bottom-right (W,0) -> (0,0).
        assert_pt_eq(mapped[1].0, Length::ZERO);
        assert_pt_eq(mapped[1].1, Length::ZERO);
    }

    #[test]
    fn rotation_180_maps_corners_into_the_same_sized_box() {
        let old = Size::new(pt(432.0), pt(648.0));
        let (m, new_size) = rotation_bake(180, old);
        assert_eq!(new_size, old);
        let (x, y) = m.apply_to_point(Length::ZERO, Length::ZERO);
        assert_pt_eq(x, old.width);
        assert_pt_eq(y, old.height);
        let (x, y) = m.apply_to_point(old.width, old.height);
        assert_pt_eq(x, Length::ZERO);
        assert_pt_eq(y, Length::ZERO);
    }

    #[test]
    fn rotation_270_maps_corners_into_the_swapped_bounding_box() {
        let old = Size::new(pt(432.0), pt(648.0));
        let (m, new_size) = rotation_bake(270, old);
        assert_eq!(new_size, Size::new(pt(648.0), pt(432.0)));
        let mapped: Vec<(Length, Length)> = corners(old)
            .iter()
            .map(|&(x, y)| m.apply_to_point(x, y))
            .collect();
        for (x, y) in &mapped {
            assert!(x.as_points() >= -1e-6 && x.as_points() <= new_size.width.as_points() + 1e-6);
            assert!(y.as_points() >= -1e-6 && y.as_points() <= new_size.height.as_points() + 1e-6);
        }
    }

    #[test]
    fn rotation_90_then_270_is_the_original_orientation() {
        // Baking 90 then baking 270 on the result should recompose to identity
        // placement (up to the two swaps cancelling), confirming the two are
        // inverses of one another rather than both being "some rotation".
        let old = Size::new(pt(432.0), pt(648.0));
        let (m90, mid_size) = rotation_bake(90, old);
        let (m270, back_size) = rotation_bake(270, mid_size);
        assert_eq!(back_size, old);
        let composed = m90.then(m270);
        // A point should return to its original position.
        let (x, y) = composed.apply_to_point(pt(100.0), pt(200.0));
        assert_pt_eq(x, pt(100.0));
        assert_pt_eq(y, pt(200.0));
    }

    #[test]
    fn center_fit_on_6x9_gives_9pt_offset_at_unit_scale() {
        let content = Size::new(pt(432.0), pt(648.0)); // 6x9in, no bleed
        let required = Size::new(pt(450.0), pt(666.0)); // 6.25x9.25in
        let p = fit_placement(content, required, FitMode::Center);
        assert_eq!(p.scale, 1.0);
        let (x, y) = p.transform.apply_to_point(Length::ZERO, Length::ZERO);
        assert_pt_eq(x, pt(9.0));
        assert_pt_eq(y, pt(9.0));
    }

    #[test]
    fn already_bled_content_is_passed_through_unscaled_with_zero_offset() {
        let size = Size::new(pt(450.0), pt(666.0));
        let p = fit_placement(size, size, FitMode::Center);
        assert_eq!(p.scale, 1.0);
        let (x, y) = p.transform.apply_to_point(Length::ZERO, Length::ZERO);
        assert_pt_eq(x, Length::ZERO);
        assert_pt_eq(y, Length::ZERO);
    }

    #[test]
    fn scale_to_bleed_scales_uniformly_and_reports_the_enlargement() {
        let content = Size::new(pt(432.0), pt(648.0)); // 6x9in
        let required = Size::new(pt(450.0), pt(666.0)); // 6.25x9.25in
        let p = fit_placement(content, required, FitMode::ScaleToBleed);
        let expected_scale = 450.0 / 432.0; // 6.25/6.0
        assert!((p.scale - expected_scale).abs() < 1e-9);
        // The scaled content must fully cover the required page (no visible border).
        let scaled_w = content.width.as_points() * p.scale;
        let scaled_h = content.height.as_points() * p.scale;
        assert!(scaled_w >= required.width.as_points() - 1e-6);
        assert!(scaled_h >= required.height.as_points() - 1e-6);
    }

    #[test]
    fn page_boxes_match_lulus_geometry_for_6x9() {
        let required = Size::new(pt(450.0), pt(666.0));
        let boxes = page_boxes(required);
        assert_eq!(
            boxes.media_bleed_box.as_pdf_array_points(),
            [0.0, 0.0, 450.0, 666.0]
        );
        assert_eq!(
            boxes.trim_art_box.as_pdf_array_points(),
            [9.0, 9.0, 441.0, 657.0]
        );
    }

    // --- nest_page ---

    use lopdf::{dictionary, Object};

    fn doc_with_one_page(
        mediabox: [f64; 4],
        rotate: Option<i64>,
        content: &[u8],
    ) -> (lopdf::Document, lopdf::ObjectId) {
        let mut doc = lopdf::Document::with_version("1.7");
        let pages_id = doc.new_object_id();
        let content_id = doc.add_object(lopdf::Stream::new(dictionary! {}, content.to_vec()));
        let mut page_dict = dictionary! {
            "Type" => "Page",
            "Parent" => Object::Reference(pages_id),
            "MediaBox" => Object::Array(mediabox.into_iter().map(Object::from).collect()),
            "Contents" => Object::Reference(content_id),
        };
        if let Some(r) = rotate {
            page_dict.set("Rotate", r);
        }
        let page_id = doc.add_object(Object::Dictionary(page_dict));
        let pages = dictionary! { "Type" => "Pages", "Kids" => vec![Object::Reference(page_id)], "Count" => 1 };
        doc.objects.insert(pages_id, Object::Dictionary(pages));
        let catalog_id = doc.add_object(
            dictionary! { "Type" => "Catalog", "Pages" => Object::Reference(pages_id) },
        );
        doc.trailer.set("Root", Object::Reference(catalog_id));
        (doc, page_id)
    }

    fn form_dict_and_content(
        doc: &lopdf::Document,
        page_id: lopdf::ObjectId,
    ) -> (&lopdf::Dictionary, Vec<u8>) {
        let page = doc.get_dictionary(page_id).unwrap();
        let resources = page.get(b"Resources").unwrap().as_dict().unwrap();
        let xobjects = resources.get(b"XObject").unwrap().as_dict().unwrap();
        let form_ref = xobjects.get(b"Fx0").unwrap().as_reference().unwrap();
        let Object::Stream(stream) = doc.get_object(form_ref).unwrap() else {
            panic!("expected a stream")
        };
        (&stream.dict, stream.get_plain_content().unwrap())
    }

    #[test]
    fn nest_page_sets_required_boxes_and_removes_rotate() {
        let (mut doc, page_id) =
            doc_with_one_page([0.0, 0.0, 432.0, 648.0], None, b"1 0 0 RG 0 0 10 10 re S");
        let required = Size::new(pt(450.0), pt(666.0));
        nest_page(
            &mut doc,
            page_id,
            required,
            FitMode::Center,
            Matrix::IDENTITY,
        )
        .unwrap();

        let page = doc.get_dictionary(page_id).unwrap();
        for key in [&b"MediaBox"[..], b"CropBox", b"BleedBox"] {
            let arr = page.get(key).unwrap().as_array().unwrap();
            let vals: Vec<f64> = arr.iter().map(|o| o.as_float().unwrap() as f64).collect();
            assert_eq!(
                vals,
                vec![0.0, 0.0, 450.0, 666.0],
                "{}",
                String::from_utf8_lossy(key)
            );
        }
        for key in [&b"TrimBox"[..], b"ArtBox"] {
            let arr = page.get(key).unwrap().as_array().unwrap();
            let vals: Vec<f64> = arr.iter().map(|o| o.as_float().unwrap() as f64).collect();
            assert_eq!(
                vals,
                vec![9.0, 9.0, 441.0, 657.0],
                "{}",
                String::from_utf8_lossy(key)
            );
        }
        assert!(
            page.get(b"Rotate").is_err(),
            "Rotate must be removed from the output"
        );
    }

    #[test]
    fn nest_page_preserves_original_content_bytes_unchanged() {
        let original_content = b"1 0 0 RG 0 0 10 10 re S";
        let (mut doc, page_id) =
            doc_with_one_page([0.0, 0.0, 432.0, 648.0], None, original_content);
        nest_page(
            &mut doc,
            page_id,
            Size::new(pt(450.0), pt(666.0)),
            FitMode::Center,
            Matrix::IDENTITY,
        )
        .unwrap();

        let (form_dict, form_content) = form_dict_and_content(&doc, page_id);
        assert_eq!(
            form_dict.get(b"Subtype").unwrap().as_name().unwrap(),
            b"Form"
        );
        // `get_page_content` appends a trailing newline when concatenating a
        // page's content streams (harmless whitespace) — compare trimmed.
        assert_eq!(
            std::str::from_utf8(&form_content).unwrap().trim_end(),
            std::str::from_utf8(original_content).unwrap()
        );
        let bbox: Vec<f64> = form_dict
            .get(b"BBox")
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .map(|o| o.as_float().unwrap() as f64)
            .collect();
        assert_eq!(bbox, vec![0.0, 0.0, 432.0, 648.0]);
    }

    #[test]
    fn nest_page_center_fit_places_content_at_the_bleed_offset() {
        let (mut doc, page_id) = doc_with_one_page([0.0, 0.0, 432.0, 648.0], None, b"");
        let placement = nest_page(
            &mut doc,
            page_id,
            Size::new(pt(450.0), pt(666.0)),
            FitMode::Center,
            Matrix::IDENTITY,
        )
        .unwrap();
        assert_eq!(placement.scale, 1.0);

        let content_ref = doc
            .get_dictionary(page_id)
            .unwrap()
            .get(b"Contents")
            .unwrap()
            .as_reference()
            .unwrap();
        let Object::Stream(stream) = doc.get_object(content_ref).unwrap() else {
            panic!()
        };
        let bytes = stream.get_plain_content().unwrap();
        let text = String::from_utf8_lossy(&bytes);
        assert!(text.contains(" cm"), "{text}");
        assert!(text.contains("/Fx0 Do"), "{text}");

        // Parse the cm operands directly and confirm they map (0,0) -> (9,9).
        let content = lopdf::content::Content::decode(&bytes).unwrap();
        let cm_op = content
            .operations
            .iter()
            .find(|op| op.operator == "cm")
            .unwrap();
        let vals: Vec<f64> = cm_op
            .operands
            .iter()
            .map(|o| o.as_float().unwrap() as f64)
            .collect();
        let m = Matrix {
            a: vals[0],
            b: vals[1],
            c: vals[2],
            d: vals[3],
            e: vals[4],
            f: vals[5],
        };
        let (x, y) = m.apply_to_point(Length::ZERO, Length::ZERO);
        assert_pt_eq(x, pt(9.0));
        assert_pt_eq(y, pt(9.0));
    }

    #[test]
    fn nest_page_bakes_rotation_so_original_bottom_left_lands_correctly() {
        // A 432x648 (6x9in) page rotated 90 degrees displays as 648x432 (9x6in).
        let (mut doc, page_id) = doc_with_one_page([0.0, 0.0, 432.0, 648.0], Some(90), b"");
        // Required size matches the *rotated* (displayed) dimensions plus bleed.
        let required = Size::new(pt(666.0), pt(450.0));
        nest_page(
            &mut doc,
            page_id,
            required,
            FitMode::Center,
            Matrix::IDENTITY,
        )
        .unwrap();

        let page = doc.get_dictionary(page_id).unwrap();
        let media: Vec<f64> = page
            .get(b"MediaBox")
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .map(|o| o.as_float().unwrap() as f64)
            .collect();
        assert_eq!(media, vec![0.0, 0.0, 666.0, 450.0]);
        assert!(page.get(b"Rotate").is_err());

        let content_ref = page.get(b"Contents").unwrap().as_reference().unwrap();
        let Object::Stream(stream) = doc.get_object(content_ref).unwrap() else {
            panic!()
        };
        let bytes = stream.get_plain_content().unwrap();
        let content = lopdf::content::Content::decode(&bytes).unwrap();
        let cm_op = content
            .operations
            .iter()
            .find(|op| op.operator == "cm")
            .unwrap();
        let vals: Vec<f64> = cm_op
            .operands
            .iter()
            .map(|o| o.as_float().unwrap() as f64)
            .collect();
        let m = Matrix {
            a: vals[0],
            b: vals[1],
            c: vals[2],
            d: vals[3],
            e: vals[4],
            f: vals[5],
        };

        // Original bottom-left (0,0) rotates 90deg CW to the rotated page's
        // top-left, i.e. (0, 432) in the 648x432 rotated frame, then centre
        // offset (9,9) is added for the bleed.
        let (x, y) = m.apply_to_point(Length::ZERO, Length::ZERO);
        assert_pt_eq(x, pt(9.0));
        assert_pt_eq(y, pt(432.0 + 9.0));
    }

    #[test]
    fn nest_page_handles_a_nonzero_origin_mediabox() {
        // MediaBox not starting at (0,0) — content is still authored relative
        // to this box, so the transform must shift it to the origin first.
        let (mut doc, page_id) = doc_with_one_page([10.0, 20.0, 442.0, 668.0], None, b"");
        nest_page(
            &mut doc,
            page_id,
            Size::new(pt(450.0), pt(666.0)),
            FitMode::Center,
            Matrix::IDENTITY,
        )
        .unwrap();

        let content_ref = doc
            .get_dictionary(page_id)
            .unwrap()
            .get(b"Contents")
            .unwrap()
            .as_reference()
            .unwrap();
        let Object::Stream(stream) = doc.get_object(content_ref).unwrap() else {
            panic!()
        };
        let bytes = stream.get_plain_content().unwrap();
        let content = lopdf::content::Content::decode(&bytes).unwrap();
        let cm_op = content
            .operations
            .iter()
            .find(|op| op.operator == "cm")
            .unwrap();
        let vals: Vec<f64> = cm_op
            .operands
            .iter()
            .map(|o| o.as_float().unwrap() as f64)
            .collect();
        let m = Matrix {
            a: vals[0],
            b: vals[1],
            c: vals[2],
            d: vals[3],
            e: vals[4],
            f: vals[5],
        };

        // The box's own bottom-left corner (10,20) must map to the same
        // (9,9) bleed offset as a zero-origin box would.
        let (x, y) = m.apply_to_point(pt(10.0), pt(20.0));
        assert_pt_eq(x, pt(9.0));
        assert_pt_eq(y, pt(9.0));
    }

    #[test]
    fn nest_page_scale_to_bleed_reports_the_scale_used() {
        let (mut doc, page_id) = doc_with_one_page([0.0, 0.0, 432.0, 648.0], None, b"");
        let placement = nest_page(
            &mut doc,
            page_id,
            Size::new(pt(450.0), pt(666.0)),
            FitMode::ScaleToBleed,
            Matrix::IDENTITY,
        )
        .unwrap();
        assert!((placement.scale - 450.0 / 432.0).abs() < 1e-9);
    }

    // --- gutter shift ---

    #[test]
    fn odd_pages_shift_toward_increasing_x() {
        let m = gutter_shift(1, pt(36.0));
        let (x, y) = m.apply_to_point(Length::ZERO, Length::ZERO);
        assert_pt_eq(x, pt(36.0));
        assert_pt_eq(y, Length::ZERO);
    }

    #[test]
    fn even_pages_shift_toward_decreasing_x() {
        let m = gutter_shift(2, pt(36.0));
        let (x, _) = m.apply_to_point(Length::ZERO, Length::ZERO);
        assert_pt_eq(x, pt(-36.0));
    }

    #[test]
    fn gutter_shift_composes_as_the_extra_transform_in_nest_page() {
        let (mut doc, page_id) = doc_with_one_page([0.0, 0.0, 432.0, 648.0], None, b"");
        let required = Size::new(pt(450.0), pt(666.0));
        nest_page(
            &mut doc,
            page_id,
            required,
            FitMode::Center,
            gutter_shift(1, pt(36.0)),
        )
        .unwrap();

        let content_ref = doc
            .get_dictionary(page_id)
            .unwrap()
            .get(b"Contents")
            .unwrap()
            .as_reference()
            .unwrap();
        let Object::Stream(stream) = doc.get_object(content_ref).unwrap() else {
            panic!()
        };
        let bytes = stream.get_plain_content().unwrap();
        let content = lopdf::content::Content::decode(&bytes).unwrap();
        let cm_op = content
            .operations
            .iter()
            .find(|op| op.operator == "cm")
            .unwrap();
        let vals: Vec<f64> = cm_op
            .operands
            .iter()
            .map(|o| o.as_float().unwrap() as f64)
            .collect();
        let m = Matrix {
            a: vals[0],
            b: vals[1],
            c: vals[2],
            d: vals[3],
            e: vals[4],
            f: vals[5],
        };
        // Centre offset (9,9) plus the 36pt odd-page gutter shift = (45, 9).
        let (x, y) = m.apply_to_point(Length::ZERO, Length::ZERO);
        assert_pt_eq(x, pt(45.0));
        assert_pt_eq(y, pt(9.0));
    }

    // --- blank-page padding ---

    fn doc_with_n_pages(n: usize) -> lopdf::Document {
        let mut doc = lopdf::Document::with_version("1.7");
        let pages_id = doc.new_object_id();
        let mut kids = Vec::new();
        for _ in 0..n {
            let page_id = doc.add_object(dictionary! {
                "Type" => "Page",
                "Parent" => Object::Reference(pages_id),
                "MediaBox" => Object::Array(vec![0.into(), 0.into(), 450.into(), 666.into()]),
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
    fn pads_to_the_binding_multiple() {
        let mut doc = doc_with_n_pages(205);
        let rules = crate::geometry::PageCountRules {
            min: 32,
            max: 800,
            multiple: 4,
        };
        let added = pad_pages(&mut doc, Size::new(pt(450.0), pt(666.0)), &rules).unwrap();
        assert_eq!(added, vec![206, 207, 208]);
        assert_eq!(doc.get_pages().len(), 208);
    }

    #[test]
    fn pads_to_the_product_minimum() {
        let mut doc = doc_with_n_pages(18);
        let rules = crate::geometry::PageCountRules {
            min: 32,
            max: 800,
            multiple: 4,
        };
        let added = pad_pages(&mut doc, Size::new(pt(450.0), pt(666.0)), &rules).unwrap();
        assert_eq!(added.len(), 14);
        assert_eq!(doc.get_pages().len(), 32);
    }

    #[test]
    fn already_conformant_count_adds_nothing() {
        let mut doc = doc_with_n_pages(208);
        let rules = crate::geometry::PageCountRules {
            min: 32,
            max: 800,
            multiple: 4,
        };
        let added = pad_pages(&mut doc, Size::new(pt(450.0), pt(666.0)), &rules).unwrap();
        assert!(added.is_empty());
        assert_eq!(doc.get_pages().len(), 208);
    }

    #[test]
    fn over_the_maximum_is_refused_and_adds_no_pages() {
        let mut doc = doc_with_n_pages(812);
        let rules = crate::geometry::PageCountRules {
            min: 32,
            max: 800,
            multiple: 4,
        };
        let err = pad_pages(&mut doc, Size::new(pt(450.0), pt(666.0)), &rules).unwrap_err();
        assert!(matches!(
            err,
            PadError::AboveMaximum {
                requested: 812,
                max: 800
            }
        ));
        assert_eq!(doc.get_pages().len(), 812);
    }

    #[test]
    fn added_pages_have_the_required_boxes_and_no_content() {
        let mut doc = doc_with_n_pages(30);
        let rules = crate::geometry::PageCountRules {
            min: 4,
            max: 48,
            multiple: 4,
        };
        let required = Size::new(pt(450.0), pt(666.0));
        pad_pages(&mut doc, required, &rules).unwrap();
        let last_page_id = *doc.get_pages().get(&32).unwrap();
        let page = doc.get_dictionary(last_page_id).unwrap();
        assert!(
            page.get(b"Contents").is_err(),
            "blank pages must carry no content stream"
        );
        let media: Vec<f64> = page
            .get(b"MediaBox")
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .map(|o| o.as_float().unwrap() as f64)
            .collect();
        assert_eq!(media, vec![0.0, 0.0, 450.0, 666.0]);
    }

    // --- structural sanitation ---

    fn doc_with_structure(
        catalog_entries: lopdf::Dictionary,
        page_entries: lopdf::Dictionary,
    ) -> lopdf::Document {
        let mut doc = lopdf::Document::with_version("1.7");
        let pages_id = doc.new_object_id();
        let mut page_dict = dictionary! {
            "Type" => "Page",
            "Parent" => Object::Reference(pages_id),
            "MediaBox" => Object::Array(vec![0.into(), 0.into(), 450.into(), 666.into()]),
        };
        page_dict.extend(&page_entries);
        let page_id = doc.add_object(Object::Dictionary(page_dict));
        let pages = dictionary! { "Type" => "Pages", "Kids" => vec![Object::Reference(page_id)], "Count" => 1 };
        doc.objects.insert(pages_id, Object::Dictionary(pages));
        let mut catalog_dict =
            dictionary! { "Type" => "Catalog", "Pages" => Object::Reference(pages_id) };
        catalog_dict.extend(&catalog_entries);
        let catalog_id = doc.add_object(Object::Dictionary(catalog_dict));
        doc.trailer.set("Root", Object::Reference(catalog_id));
        doc
    }

    #[test]
    fn annotations_are_stripped_and_reported() {
        let mut doc = lopdf::Document::with_version("1.7");
        let pages_id = doc.new_object_id();
        let annot_id = doc.add_object(dictionary! { "Type" => "Annot", "Subtype" => "Link" });
        let page_id = doc.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => Object::Reference(pages_id),
            "MediaBox" => Object::Array(vec![0.into(), 0.into(), 450.into(), 666.into()]),
            "Annots" => vec![Object::Reference(annot_id)],
        });
        let pages = dictionary! { "Type" => "Pages", "Kids" => vec![Object::Reference(page_id)], "Count" => 1 };
        doc.objects.insert(pages_id, Object::Dictionary(pages));
        let catalog_id = doc.add_object(
            dictionary! { "Type" => "Catalog", "Pages" => Object::Reference(pages_id) },
        );
        doc.trailer.set("Root", Object::Reference(catalog_id));

        let summary = sanitize_structure(&mut doc);
        assert_eq!(summary.pages_with_annotations_cleared, vec![1]);
        assert!(doc.get_dictionary(page_id).unwrap().get(b"Annots").is_err());
    }

    #[test]
    fn acroform_is_removed_and_reported() {
        let mut doc = doc_with_structure(
            dictionary! { "AcroForm" => dictionary! { "Fields" => Vec::<Object>::new() } },
            dictionary! {},
        );
        let summary = sanitize_structure(&mut doc);
        assert!(summary.acroform_removed);
        assert!(doc.catalog().unwrap().get(b"AcroForm").is_err());
    }

    #[test]
    fn document_javascript_and_embedded_files_are_removed_and_reported() {
        let mut doc = doc_with_structure(
            dictionary! { "Names" => dictionary! {
                "JavaScript" => dictionary! { "Names" => Vec::<Object>::new() },
                "EmbeddedFiles" => dictionary! { "Names" => Vec::<Object>::new() },
            } },
            dictionary! {},
        );
        let summary = sanitize_structure(&mut doc);
        assert!(summary.javascript_removed);
        assert!(summary.embedded_files_removed);
        let names = doc
            .catalog()
            .unwrap()
            .get(b"Names")
            .unwrap()
            .as_dict()
            .unwrap();
        assert!(names.get(b"JavaScript").is_err());
        assert!(names.get(b"EmbeddedFiles").is_err());
    }

    #[test]
    fn page_layout_is_always_forced_to_single_page() {
        let mut doc = doc_with_structure(
            dictionary! { "PageLayout" => "TwoPageLeft" },
            dictionary! {},
        );
        sanitize_structure(&mut doc);
        assert_eq!(
            doc.catalog()
                .unwrap()
                .get(b"PageLayout")
                .unwrap()
                .as_name()
                .unwrap(),
            b"SinglePage"
        );
    }

    #[test]
    fn clean_document_reports_no_changes_except_forced_layout() {
        let mut doc = doc_with_structure(dictionary! {}, dictionary! {});
        let summary = sanitize_structure(&mut doc);
        assert!(summary.pages_with_annotations_cleared.is_empty());
        assert!(!summary.acroform_removed);
        assert!(!summary.javascript_removed);
        assert!(!summary.embedded_files_removed);
        assert!(summary.page_layout_forced);
    }

    // --- normalize_interior (top-level orchestration) ---

    fn bytes_for(doc: &mut lopdf::Document) -> Vec<u8> {
        let mut buf = Vec::new();
        doc.save_to(&mut buf).unwrap();
        buf
    }

    fn doc_with_n_unbled_pages(n: usize) -> lopdf::Document {
        let mut doc = lopdf::Document::with_version("1.7");
        let pages_id = doc.new_object_id();
        let mut kids = Vec::new();
        for _ in 0..n {
            let content_id = doc.add_object(lopdf::Stream::new(dictionary! {}, Vec::new()));
            let page_id = doc.add_object(dictionary! {
                "Type" => "Page",
                "Parent" => Object::Reference(pages_id),
                "MediaBox" => Object::Array(vec![0.into(), 0.into(), 432.into(), 648.into()]),
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

    fn sku_entry() -> &'static crate::catalog::CatalogEntry {
        crate::catalog::lookup("0600X0900.BW.STD.PB.060UW444.MXX").unwrap()
    }

    #[test]
    fn normalize_produces_a_print_ready_conformant_output() {
        let mut doc = doc_with_n_unbled_pages(18);
        let input = bytes_for(&mut doc);
        let outcome = normalize_interior(&input, sku_entry(), NormalizeOptions::default()).unwrap();

        assert_eq!(outcome.final_page_count, 32);
        assert_eq!(outcome.padded_pages.len(), 14);
        assert!(
            outcome.report.is_print_ready(),
            "{}",
            outcome.report.to_text()
        );
        assert_eq!(outcome.report.page_count, Some(32));

        // Re-parse the output and check the geometry actually landed.
        let reloaded = crate::pdf::load_from_bytes(&outcome.output_bytes).unwrap();
        assert_eq!(reloaded.get_pages().len(), 32);
        let first_page = *reloaded.get_pages().get(&1).unwrap();
        let size = crate::pdf::effective_page_size(&reloaded, first_page).unwrap();
        assert_eq!(size.width.as_points(), 450.0);
        assert_eq!(size.height.as_points(), 666.0);
    }

    #[test]
    fn thin_book_below_advisory_gutter_floor_gets_a_warning_not_a_blocker() {
        // 32 pages -> 0.000in banded gutter, below Lulu's 0.200in advisory floor.
        let mut doc = doc_with_n_unbled_pages(32);
        let input = bytes_for(&mut doc);
        let outcome = normalize_interior(&input, sku_entry(), NormalizeOptions::default()).unwrap();

        let finding = outcome
            .report
            .findings
            .iter()
            .find(|f| f.code == crate::report::codes::GUTTER_BELOW_ADVISORY_FLOOR)
            .expect("expected a gutter-below-advisory-floor finding");
        assert_eq!(finding.severity, crate::report::Severity::Warning);
        assert!(
            outcome.report.is_print_ready(),
            "a warning must not block print-readiness"
        );
    }

    #[test]
    fn thick_book_above_advisory_gutter_floor_has_no_warning() {
        // 210 pages -> 0.500in banded gutter, comfortably above the 0.200in floor.
        let mut doc = doc_with_n_unbled_pages(210);
        let input = bytes_for(&mut doc);
        let outcome = normalize_interior(&input, sku_entry(), NormalizeOptions::default()).unwrap();
        assert!(!outcome
            .report
            .findings
            .iter()
            .any(|f| f.code == crate::report::codes::GUTTER_BELOW_ADVISORY_FLOOR));
    }

    #[test]
    fn a_file_needing_a_real_password_is_refused() {
        const REAL_PASSWORD_PDF: &[u8] =
            include_bytes!("../tests/fixtures/encrypted_real_password.pdf");
        let err = normalize_interior(REAL_PASSWORD_PDF, sku_entry(), NormalizeOptions::default())
            .unwrap_err();
        assert!(matches!(err, NormalizeInteriorError::PasswordRequired));
    }

    #[test]
    fn above_maximum_page_count_is_refused_before_writing_anything() {
        let mut doc = doc_with_n_unbled_pages(812);
        let input = bytes_for(&mut doc);
        let err = normalize_interior(&input, sku_entry(), NormalizeOptions::default()).unwrap_err();
        assert!(matches!(
            err,
            NormalizeInteriorError::AboveMaximum {
                requested: 812,
                max: 800
            }
        ));
    }

    #[test]
    fn normalizing_normalized_output_is_idempotent() {
        let mut doc = doc_with_n_unbled_pages(205);
        let input = bytes_for(&mut doc);
        let first = normalize_interior(&input, sku_entry(), NormalizeOptions::default()).unwrap();
        assert_eq!(first.final_page_count, 208);

        let second = normalize_interior(
            &first.output_bytes,
            sku_entry(),
            NormalizeOptions::default(),
        )
        .unwrap();
        assert_eq!(second.final_page_count, 208);
        assert!(
            second.padded_pages.is_empty(),
            "already-conformant count must add nothing"
        );
        assert!(
            second.report.is_print_ready(),
            "{}",
            second.report.to_text()
        );

        let doc1 = crate::pdf::load_from_bytes(&first.output_bytes).unwrap();
        let doc2 = crate::pdf::load_from_bytes(&second.output_bytes).unwrap();
        assert_eq!(doc1.get_pages().len(), doc2.get_pages().len());
        for page_number in [1u32, 100, 208] {
            let id1 = *doc1.get_pages().get(&page_number).unwrap();
            let id2 = *doc2.get_pages().get(&page_number).unwrap();
            let size1 = crate::pdf::effective_page_size(&doc1, id1).unwrap();
            let size2 = crate::pdf::effective_page_size(&doc2, id2).unwrap();
            assert_eq!(
                size1, size2,
                "page {page_number} size changed on re-normalization"
            );
        }
    }

    // --- spread splitting ---

    fn image_xobject(width: i64, height: i64) -> lopdf::Dictionary {
        dictionary! {
            "Type" => "XObject",
            "Subtype" => "Image",
            "Width" => width,
            "Height" => height,
            "BitsPerComponent" => 8,
            "ColorSpace" => "DeviceRGB",
        }
    }

    /// A spread page `spread_width x height`, with a `marker_width`-tall
    /// marker image drawn at `[x, x + 1]` (unit width, so its exact drawn
    /// position pins down the offset a splitter applied) via `cm`.
    fn doc_with_one_marked_spread(
        spread_width: f64,
        height: f64,
        marker_pixel_width: i64,
        image_x: f64,
    ) -> lopdf::Document {
        let mut doc = lopdf::Document::with_version("1.7");
        let image_id = doc.add_object(Object::Stream(lopdf::Stream::new(
            image_xobject(marker_pixel_width, 1),
            vec![0u8; 4],
        )));
        let resources =
            dictionary! { "XObject" => dictionary! { "Im0" => Object::Reference(image_id) } };
        let content = format!("q 1 0 0 1 {image_x} 0 cm /Im0 Do Q");
        let content_id = doc.add_object(lopdf::Stream::new(dictionary! {}, content.into_bytes()));
        let pages_id = doc.new_object_id();
        let page_id = doc.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => Object::Reference(pages_id),
            "MediaBox" => Object::Array(vec![0.into(), 0.into(), spread_width.into(), height.into()]),
            "Resources" => resources,
            "Contents" => Object::Reference(content_id),
        });
        let pages = dictionary! { "Type" => "Pages", "Kids" => vec![Object::Reference(page_id)], "Count" => 1 };
        doc.objects.insert(pages_id, Object::Dictionary(pages));
        let catalog_id = doc.add_object(
            dictionary! { "Type" => "Catalog", "Pages" => Object::Reference(pages_id) },
        );
        doc.trailer.set("Root", Object::Reference(catalog_id));
        doc
    }

    /// The x-position `walk_page_images` reports the (sole) image drawn at,
    /// via its CTM's translation component (a `cm` of `1 0 0 1 tx 0` has
    /// `drawn_size_points` unaffected, so read the CTM directly instead).
    fn drawn_x_position(doc: &lopdf::Document, page_id: lopdf::ObjectId) -> Option<f64> {
        let mut x = None;
        crate::ctm_walk::walk_page_images(
            doc,
            page_id,
            &mut |ctm: Matrix, _: &lopdf::Dictionary, _id| {
                x = Some(ctm.as_cm_operands()[4]);
            },
        );
        x
    }

    #[test]
    fn spread_split_doubles_page_count_in_reading_order() {
        // 3 spread pages, each carrying a distinct marker (its image's pixel
        // width == 100 + original page index) so the output order is traceable.
        let mut doc = lopdf::Document::with_version("1.7");
        let pages_id = doc.new_object_id();
        let mut kids = Vec::new();
        for i in 0..3i64 {
            let image_id = doc.add_object(Object::Stream(lopdf::Stream::new(
                image_xobject(100 + i, 1),
                vec![0u8; 4],
            )));
            let resources =
                dictionary! { "XObject" => dictionary! { "Im0" => Object::Reference(image_id) } };
            let content_id =
                doc.add_object(lopdf::Stream::new(dictionary! {}, b"/Im0 Do".to_vec()));
            let page_id = doc.add_object(dictionary! {
                "Type" => "Page",
                "Parent" => Object::Reference(pages_id),
                "MediaBox" => Object::Array(vec![0.into(), 0.into(), 900.into(), 666.into()]),
                "Resources" => resources,
                "Contents" => Object::Reference(content_id),
            });
            kids.push(Object::Reference(page_id));
        }
        doc.objects.insert(
            pages_id,
            Object::Dictionary(dictionary! { "Type" => "Pages", "Kids" => kids, "Count" => 3 }),
        );
        let catalog_id = doc.add_object(
            dictionary! { "Type" => "Catalog", "Pages" => Object::Reference(pages_id) },
        );
        doc.trailer.set("Root", Object::Reference(catalog_id));

        let split_count = split_spread_pages(&mut doc).unwrap();
        assert_eq!(split_count, 3);
        let page_ids: Vec<_> = doc.page_iter().collect();
        assert_eq!(page_ids.len(), 6, "3 spreads must become 6 pages");

        // Every pair of consecutive output pages carries the same marker
        // (both halves of the same original page), in reading order.
        for (original_index, pair) in page_ids.chunks(2).enumerate() {
            for &page_id in pair {
                let mut widths = Vec::new();
                crate::ctm_walk::walk_page_images(
                    &doc,
                    page_id,
                    &mut |_: Matrix, dict: &lopdf::Dictionary, _id| {
                        widths.push(dict.get(b"Width").unwrap().as_i64().unwrap());
                    },
                );
                assert_eq!(widths, vec![100 + original_index as i64]);
            }
        }
    }

    #[test]
    fn left_half_lands_at_the_original_offset_and_right_half_shifts_by_half_width() {
        // 900x666 spread; a 1pt-wide marker at x=100 is entirely in the left
        // half [0,450); a marker at x=500 is entirely in the right half.
        let mut left_marked = doc_with_one_marked_spread(900.0, 666.0, 1, 100.0);
        split_spread_pages(&mut left_marked).unwrap();
        let ids: Vec<_> = left_marked.page_iter().collect();
        assert_eq!(ids.len(), 2);
        assert_eq!(
            drawn_x_position(&left_marked, ids[0]),
            Some(100.0),
            "left page: no shift, since the spread's own x0 is 0"
        );
        assert_eq!(
            drawn_x_position(&left_marked, ids[1]),
            Some(100.0 - 450.0),
            "right page: shifted left by the half-width, landing off that page's own [0,450) box"
        );

        let mut right_marked = doc_with_one_marked_spread(900.0, 666.0, 1, 500.0);
        split_spread_pages(&mut right_marked).unwrap();
        let ids: Vec<_> = right_marked.page_iter().collect();
        assert_eq!(
            drawn_x_position(&right_marked, ids[0]),
            Some(500.0),
            "left page: unshifted, so this lands off its own [0,450) box"
        );
        assert_eq!(
            drawn_x_position(&right_marked, ids[1]),
            Some(500.0 - 450.0),
            "right page: shifted into its own [0,450) box"
        );
    }

    #[test]
    fn spread_halves_are_half_the_original_width_and_the_full_height() {
        let mut doc = doc_with_one_marked_spread(900.0, 666.0, 1, 0.0);
        split_spread_pages(&mut doc).unwrap();
        let ids: Vec<_> = doc.page_iter().collect();
        for &id in &ids {
            let size = crate::pdf::own_box_size(&doc, id).unwrap();
            assert_eq!(size.width.as_points(), 450.0);
            assert_eq!(size.height.as_points(), 666.0);
        }
    }

    #[test]
    fn landscape_pages_are_reported_but_never_split_without_the_option() {
        let mut doc = doc_with_one_marked_spread(900.0, 666.0, 1, 0.0);
        let bytes = bytes_for(&mut doc);
        let outcome = normalize_interior(&bytes, sku_entry(), NormalizeOptions::default()).unwrap();

        // Never split: the interior's page count reflects padding from 1
        // page, not from 2 (1 landscape page split into a left+right pair).
        let reloaded = crate::pdf::load_from_bytes(&outcome.output_bytes).unwrap();
        assert_eq!(reloaded.get_pages().len() as u32, outcome.final_page_count);
        assert!(
            outcome
                .report
                .findings
                .iter()
                .any(|f| f.code == "normalize.landscape-pages-observed"),
            "{:?}",
            outcome.report.findings
        );
    }

    #[test]
    fn split_spreads_option_produces_no_landscape_finding() {
        let mut doc = doc_with_one_marked_spread(900.0, 666.0, 1, 0.0);
        let bytes = bytes_for(&mut doc);
        let options = NormalizeOptions {
            split_spreads: true,
            ..NormalizeOptions::default()
        };
        let outcome = normalize_interior(&bytes, sku_entry(), options).unwrap();
        assert!(
            !outcome
                .report
                .findings
                .iter()
                .any(|f| f.code == "normalize.landscape-pages-observed"),
            "{:?}",
            outcome.report.findings
        );
    }
}
