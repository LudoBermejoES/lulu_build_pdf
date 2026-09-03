//! Read-only inspection of a PDF against a target Lulu product: page geometry,
//! font embedding, page count, and (later) image resolution, colour, and
//! structural checks — all producing a [`crate::report::Report`], never
//! modifying the input.

use crate::catalog::CatalogEntry;
use crate::ctm_walk;
use crate::geometry::PageCountRules;
use crate::pdf;
use crate::report::{
    codes, DetectedTool, Finding, Report, Severity, StageLogEntry, SCHEMA_VERSION,
};
use crate::units::{Length, Rect, Size};
use lopdf::content::Operation;
use lopdf::{Dictionary, Document, Object, ObjectId};
use std::collections::BTreeMap;

/// `object`, dereferenced one level if it is an [`Object::Reference`], as a
/// dictionary — the same "direct dict or indirect reference to one" shape
/// legal for a page's `/Resources`, a form's own `/Resources`, and the
/// sub-dictionaries (`/Font`, `/XObject`, ...) within either.
fn resolve_dict_local<'a>(doc: &'a Document, object: &'a Object) -> Option<&'a Dictionary> {
    match object {
        Object::Reference(id) => doc.get_dictionary(*id).ok(),
        Object::Dictionary(d) => Some(d),
        _ => None,
    }
}

/// PDF page-size comparisons tolerate up to this much slack — Lulu's own
/// stated tolerance for "close enough" geometry.
const SIZE_TOLERANCE: Length = Length::from_points(0.5);

fn size_key(size: Size) -> (i64, i64) {
    // Round to hundredths of a point for stable grouping/display.
    (
        (size.width.as_points() * 100.0).round() as i64,
        (size.height.as_points() * 100.0).round() as i64,
    )
}

fn format_size(size: Size) -> String {
    format!(
        "{:.3} x {:.3} in",
        size.width.as_inches(),
        size.height.as_inches()
    )
}

/// One page's effective size, keyed by its 1-based page number in document order.
fn page_sizes(
    doc: &Document,
    page_ids: &[ObjectId],
) -> Vec<(u32, Result<Size, pdf::PageGeometryError>)> {
    page_ids
        .iter()
        .enumerate()
        .map(|(i, &id)| ((i + 1) as u32, pdf::effective_page_size(doc, id)))
        .collect()
}

/// Every page's effective size must equal `required`, within [`SIZE_TOLERANCE`].
/// Pages that don't match are grouped by their (wrong) size into one finding
/// per distinct wrong size, so a uniformly-wrong file gets a single finding.
pub fn check_page_size_matches_target(
    doc: &Document,
    page_ids: &[ObjectId],
    required: Size,
) -> Vec<Finding> {
    let mut wrong: BTreeMap<(i64, i64), (Size, Vec<u32>)> = BTreeMap::new();
    for (page_number, size) in page_sizes(doc, page_ids) {
        // A page whose geometry could not be resolved at all has nothing to
        // compare here — it is not silently dropped, though: it is always
        // reported separately by `check_page_geometry_resolution`, which
        // every caller of this function also calls.
        let Ok(size) = size else { continue };
        if !size.approx_eq(required, SIZE_TOLERANCE) {
            wrong
                .entry(size_key(size))
                .or_insert_with(|| (size, Vec::new()))
                .1
                .push(page_number);
        }
    }
    wrong
        .into_values()
        .map(|(observed, pages)| {
            Finding::new(
                codes::GEOMETRY_PAGE_SIZE_MISMATCH,
                Severity::Blocking,
                format!(
                    "{} page(s) measure {} but this product requires {} (0.125 in bleed per side)",
                    pages.len(),
                    format_size(observed),
                    format_size(required)
                ),
            )
            .with_pages(pages)
            .with_observed(format_size(observed))
            .with_expected(format_size(required))
            .fixable(true)
        })
        .collect()
}

/// All pages in the document must share one size — Lulu's own validation
/// rejects a mixed-size interior outright.
pub fn check_mixed_page_sizes(doc: &Document, page_ids: &[ObjectId]) -> Vec<Finding> {
    let mut groups: BTreeMap<(i64, i64), (Size, Vec<u32>)> = BTreeMap::new();
    for (page_number, size) in page_sizes(doc, page_ids) {
        // See the identical comment in `check_page_size_matches_target`:
        // reported separately, not silently dropped.
        let Ok(size) = size else { continue };
        groups
            .entry(size_key(size))
            .or_insert_with(|| (size, Vec::new()))
            .1
            .push(page_number);
    }
    if groups.len() <= 1 {
        return Vec::new();
    }
    let mut pages_all: Vec<u32> = Vec::new();
    let mut parts = Vec::new();
    for (size, pages) in groups.values() {
        parts.push(format!("{} on page(s) {:?}", format_size(*size), pages));
        pages_all.extend(pages.iter().copied());
    }
    pages_all.sort_unstable();
    vec![Finding::new(
        codes::GEOMETRY_MIXED_PAGE_SIZES,
        Severity::Blocking,
        format!(
            "interior pages are not a uniform size: {}",
            parts.join("; ")
        ),
    )
    .with_pages(pages_all)
    .fixable(true)]
}

/// A page whose geometry cannot be resolved at all (an indirect box entry
/// that does not resolve, or no box anywhere in its `BleedBox -> CropBox ->
/// MediaBox` fallback chain, including inherited) has nothing for
/// [`check_page_size_matches_target`] or [`check_mixed_page_sizes`] to
/// compare — both key on a resolved [`Size`] and skip a page they can't
/// measure. This is what makes that skip not a silent omission: every
/// caller of those two functions also calls this one, so the page is always
/// named in a blocking finding rather than simply missing from the report.
pub fn check_page_geometry_resolution(doc: &Document, page_ids: &[ObjectId]) -> Vec<Finding> {
    let mut unreadable: Vec<u32> = Vec::new();
    for (page_number, size) in page_sizes(doc, page_ids) {
        if size.is_err() {
            unreadable.push(page_number);
        }
    }
    if unreadable.is_empty() {
        return Vec::new();
    }
    vec![Finding::new(
        codes::GEOMETRY_UNREADABLE_PAGE_BOX,
        Severity::Blocking,
        format!(
            "{} page(s) have no resolvable page box (an indirect TrimBox/BleedBox/CropBox/MediaBox reference that does not resolve, or no box at all)",
            unreadable.len()
        ),
    )
    .with_pages(unreadable)
    .fixable(false)]
}

/// A `/Rotate` that is present but unreadable (an indirect reference that
/// does not resolve, or a non-numeric value), or that resolves to a number
/// which is not a multiple of 90 within tolerance, must not be silently
/// treated as "no rotation" the way [`pdf::rotation_degrees`] treats it for
/// callers that only need the geometric effect — that silent default is
/// exactly how a whole book can print sideways with no finding at all. This
/// check uses [`pdf::rotation_outcome`] directly so it can tell those cases
/// apart from a genuinely unrotated page and report them.
pub fn check_page_rotation(doc: &Document, page_ids: &[ObjectId]) -> Vec<Finding> {
    let mut unreadable: Vec<u32> = Vec::new();
    let mut not_multiple_pages: Vec<u32> = Vec::new();
    let mut not_multiple_degrees: Vec<i64> = Vec::new();

    for (i, &page_id) in page_ids.iter().enumerate() {
        let page_number = (i + 1) as u32;
        match pdf::rotation_outcome(doc, page_id) {
            Ok(pdf::RotationOutcome::Unreadable) => unreadable.push(page_number),
            Ok(pdf::RotationOutcome::NotAMultipleOf90(degrees)) => {
                not_multiple_pages.push(page_number);
                not_multiple_degrees.push(degrees);
            }
            Ok(pdf::RotationOutcome::Normalized(_)) | Err(_) => {}
        }
    }

    let mut findings = Vec::new();
    if !unreadable.is_empty() {
        findings.push(
            Finding::new(
                codes::GEOMETRY_UNREADABLE_ROTATION,
                Severity::Blocking,
                format!(
                    "{} page(s) carry a /Rotate entry that could not be resolved to a number (e.g. a broken indirect reference); the page's actual orientation is unknown",
                    unreadable.len()
                ),
            )
            .with_pages(unreadable)
            .fixable(false),
        );
    }
    if !not_multiple_pages.is_empty() {
        not_multiple_degrees.sort_unstable();
        not_multiple_degrees.dedup();
        let degrees_list = not_multiple_degrees
            .iter()
            .map(i64::to_string)
            .collect::<Vec<_>>()
            .join(", ");
        findings.push(
            Finding::new(
                codes::GEOMETRY_ROTATION_NOT_MULTIPLE_OF_90,
                Severity::Blocking,
                format!(
                    "{} page(s) carry a /Rotate value ({degrees_list}) that is not a multiple of 90 degrees",
                    not_multiple_pages.len()
                ),
            )
            .with_pages(not_multiple_pages)
            .fixable(false),
        );
    }
    findings
}

fn font_descriptor_is_embedded(doc: &Document, descriptor: &lopdf::Dictionary) -> bool {
    for key in [&b"FontFile"[..], b"FontFile2", b"FontFile3"] {
        if descriptor.get(key).is_ok() {
            return true;
        }
    }
    let _ = doc; // reserved for a future indirect-reference resolution if needed
    false
}

/// Checks one content layer's `/Font` resources (if any) for embedding,
/// recording every unembedded font's base name against `page_number`. Used
/// for both a page's own resources and every nested form XObject's — see
/// [`check_font_embedding`].
fn check_fonts_in_resources(
    doc: &Document,
    resources: &Dictionary,
    page_number: u32,
    not_embedded: &mut BTreeMap<String, Vec<u32>>,
) {
    let Some(fonts) = resources
        .get(b"Font")
        .ok()
        .and_then(|o| resolve_dict_local(doc, o))
    else {
        return;
    };
    for (_, font_obj) in fonts.iter() {
        let Some(font_dict) = resolve_dict_local(doc, font_obj) else {
            continue;
        };
        let base_font = font_dict
            .get(b"BaseFont")
            .and_then(|o| o.as_name())
            .map(|n| String::from_utf8_lossy(n).to_string())
            .unwrap_or_else(|_| "(unnamed font)".to_string());

        let embedded = if font_dict.get(b"Subtype").and_then(|o| o.as_name()).ok() == Some(b"Type0")
        {
            font_dict
                .get(b"DescendantFonts")
                .ok()
                .and_then(|o| o.as_array().ok())
                .and_then(|arr| arr.first())
                .and_then(|o| resolve_dict_local(doc, o))
                .and_then(|descendant| descendant.get(b"FontDescriptor").ok())
                .and_then(|o| resolve_dict_local(doc, o))
                .is_some_and(|descriptor| font_descriptor_is_embedded(doc, descriptor))
        } else {
            font_dict
                .get(b"FontDescriptor")
                .ok()
                .and_then(|o| resolve_dict_local(doc, o))
                .is_some_and(|descriptor| font_descriptor_is_embedded(doc, descriptor))
        };

        if !embedded {
            not_embedded.entry(base_font).or_default().push(page_number);
        }
    }
}

/// Every font referenced by the document must be fully embedded — Lulu's
/// file validation rejects an interior with any unembedded font, including
/// the standard 14 base fonts (which have no embedded file by definition).
///
/// Fonts are discovered through the page's effective resources (direct,
/// indirect, or inherited) and through the resources of every form XObject
/// the page draws, to whatever depth [`ctm_walk`]'s traversal budget allows
/// — reusing [`ctm_walk::collect_page_layers`] rather than a second descent,
/// so a font referenced only from inside a form XObject is found exactly as
/// one referenced directly by the page. A page whose traversal exceeds the
/// operation budget is reported separately, by
/// [`check_resource_references`], which shares this same walk's outcome.
pub fn check_font_embedding(doc: &Document, page_ids: &[ObjectId]) -> Vec<Finding> {
    let mut not_embedded: BTreeMap<String, Vec<u32>> = BTreeMap::new();

    for (i, &page_id) in page_ids.iter().enumerate() {
        let page_number = (i + 1) as u32;
        let (layers, _outcome) = ctm_walk::collect_page_layers(doc, page_id);
        for layer in &layers {
            check_fonts_in_resources(doc, &layer.resources, page_number, &mut not_embedded);
        }
    }

    not_embedded
        .into_iter()
        .map(|(name, mut pages)| {
            pages.sort_unstable();
            pages.dedup();
            Finding::new(
                codes::FONTS_NOT_EMBEDDED,
                Severity::Blocking,
                format!("font '{name}' is not embedded"),
            )
            .with_pages(pages)
            .fixable(false)
        })
        .collect()
}

/// Compares an observed interior page count against a product's rules,
/// stating the padding normalization would apply, or refusing above the
/// maximum.
pub fn check_page_count(observed: u32, rules: &PageCountRules) -> Vec<Finding> {
    match rules.next_conformant(observed) {
        Ok(target) if target == observed => Vec::new(),
        Ok(target) if observed < rules.min => {
            vec![Finding::new(
                codes::PAGE_COUNT_BELOW_MINIMUM,
                Severity::Blocking,
                format!("{observed} pages is below this product's {}-page minimum; {} blank page(s) would be appended to reach {target}", rules.min, target - observed),
            )
            .with_observed(observed.to_string())
            .with_expected(format!(">= {}", rules.min))
            .fixable(true)]
        }
        Ok(target) => {
            vec![Finding::new(
                codes::PAGE_COUNT_NOT_DIVISIBLE,
                Severity::Blocking,
                format!("{observed} pages is not a multiple of {}; {} blank page(s) would be appended to reach {target}", rules.multiple, target - observed),
            )
            .with_observed(observed.to_string())
            .with_expected(format!("multiple of {}", rules.multiple))
            .fixable(true)]
        }
        Err(crate::geometry::PageCountError::AboveMaximum { requested, max }) => {
            vec![Finding::new(
                codes::PAGE_COUNT_ABOVE_MAXIMUM,
                Severity::Blocking,
                format!("{requested} pages exceeds this product's {max}-page maximum; split the content or choose a different product"),
            )
            .with_observed(requested.to_string())
            .with_expected(format!("<= {max}"))
            .fixable(false)]
        }
        Err(crate::geometry::PageCountError::InvalidRules) => {
            vec![
                Finding::new(
                    "page-count.invalid-rules",
                    Severity::Blocking,
                    "this product's page-count rules have no valid divisibility multiple, so no conformant page count could be determined".to_string(),
                )
                .fixable(false),
            ]
        }
    }
}

/// Names dictionary entries that carry no print meaning and that Lulu's
/// pipeline has no use for, keyed by the stable finding code and the
/// human-readable label used in the finding message.
const REPORTABLE_NAME_TREES: &[(&[u8], &str, &str)] = &[
    (
        b"JavaScript",
        codes::STRUCTURE_JAVASCRIPT,
        "document-level JavaScript",
    ),
    (
        b"EmbeddedFiles",
        codes::STRUCTURE_EMBEDDED_FILES,
        "embedded file(s)",
    ),
];

/// Structural checks that don't need content-stream parsing: annotations
/// (including `AcroForm`/form fields and multimedia annotations), document-
/// or annotation-level JavaScript, embedded files, and a spread page layout.
/// Encryption is checked separately in [`preflight`], since it gates whether
/// the rest of the document can even be read.
pub fn check_structure(doc: &Document, page_ids: &[ObjectId]) -> Vec<Finding> {
    let mut findings = Vec::new();

    // Annotations (including multimedia annotations, which are just another
    // annotation subtype) and AcroForm/form fields.
    let mut annotation_pages: Vec<u32> = Vec::new();
    let mut subtypes: Vec<String> = Vec::new();
    for (i, &page_id) in page_ids.iter().enumerate() {
        if let Ok(annots) = doc.get_page_annotations(page_id) {
            if !annots.is_empty() {
                annotation_pages.push((i + 1) as u32);
                for annot in annots {
                    if let Ok(subtype) = annot.get(b"Subtype").and_then(|o| o.as_name()) {
                        subtypes.push(String::from_utf8_lossy(subtype).to_string());
                    }
                }
            }
        }
    }
    if !annotation_pages.is_empty() {
        subtypes.sort();
        subtypes.dedup();
        findings.push(
            Finding::new(
                codes::STRUCTURE_ANNOTATIONS,
                Severity::Warning,
                format!(
                    "annotations present ({}); Lulu's file has no use for interactive annotations",
                    subtypes.join(", ")
                ),
            )
            .with_pages(annotation_pages)
            .fixable(true),
        );
    }

    if let Ok(catalog) = doc.catalog() {
        if catalog.get(b"AcroForm").is_ok() {
            findings.push(
                Finding::new(codes::STRUCTURE_ANNOTATIONS, Severity::Warning, "an AcroForm with form fields is present; Lulu's file has no use for interactive form fields".to_string())
                    .fixable(true),
            );
        }

        // Document-level JavaScript and embedded files live under /Names in
        // the catalog, resolved whether /Names is a direct dictionary or an
        // indirect reference to one (`pdf::catalog_names`), so both
        // encodings are found identically.
        if let Some(names) = pdf::catalog_names(doc) {
            for (key, code, label) in REPORTABLE_NAME_TREES {
                if names.get(key).is_ok() {
                    findings.push(
                        Finding::new(
                            *code,
                            Severity::Warning,
                            format!(
                                "{label} present; not meaningful in print and will be stripped"
                            ),
                        )
                        .fixable(true),
                    );
                }
            }
        }

        let layout = catalog.get(b"PageLayout").and_then(|o| o.as_name()).ok();
        if let Some(layout) = layout {
            if layout != b"SinglePage" {
                findings.push(
                    Finding::new(
                        codes::STRUCTURE_SPREAD_LAYOUT,
                        Severity::Warning,
                        format!(
                            "page layout is '{}'; Lulu requires a single-page layout",
                            String::from_utf8_lossy(layout)
                        ),
                    )
                    .fixable(true),
                );
            }
        }
    }

    findings
}

const MIN_IMAGE_PPI: f64 = 300.0;
const MAX_IMAGE_PPI: f64 = 600.0;

struct ImageResolutionRecord {
    page: u32,
    image_id: lopdf::ObjectId,
    ppi: f64,
}

struct ImageResolutionCollector {
    records: Vec<ImageResolutionRecord>,
    current_page: u32,
}

impl crate::ctm_walk::ImageVisitor for ImageResolutionCollector {
    fn visit_image(
        &mut self,
        ctm: crate::units::Matrix,
        image_dict: &lopdf::Dictionary,
        image_id: lopdf::ObjectId,
    ) {
        let (Ok(pixel_w), Ok(pixel_h)) = (
            image_dict.get(b"Width").and_then(|o| o.as_i64()),
            image_dict.get(b"Height").and_then(|o| o.as_i64()),
        ) else {
            return;
        };
        let (drawn_w_pt, drawn_h_pt) = crate::ctm_walk::drawn_size_points(ctm);
        if drawn_w_pt <= 0.0 || drawn_h_pt <= 0.0 {
            return;
        }
        let ppi_x = pixel_w as f64 / (drawn_w_pt / 72.0);
        let ppi_y = pixel_h as f64 / (drawn_h_pt / 72.0);
        // Effective resolution is the lower of the two axes — a non-uniformly
        // scaled image is only as sharp as its blurriest dimension.
        let ppi = ppi_x.min(ppi_y);
        self.records.push(ImageResolutionRecord {
            page: self.current_page,
            image_id,
            ppi,
        });
    }
}

/// Effective resolution of every placed raster image, combining pixel
/// dimensions with the CTM at each draw site ([`crate::ctm_walk`]). Images
/// below 300 ppi or above 600 ppi are warnings; when several qualify, they
/// are folded into one finding per direction naming the worst offender, so
/// a document with many low-resolution images doesn't produce a wall of
/// near-duplicate findings.
pub fn check_image_resolution(doc: &Document, page_ids: &[ObjectId]) -> Vec<Finding> {
    let mut collector = ImageResolutionCollector {
        records: Vec::new(),
        current_page: 0,
    };
    for (i, &page_id) in page_ids.iter().enumerate() {
        collector.current_page = (i + 1) as u32;
        crate::ctm_walk::walk_page_images(doc, page_id, &mut collector);
    }

    let mut findings = Vec::new();

    let mut low: Vec<&ImageResolutionRecord> = collector
        .records
        .iter()
        .filter(|r| r.ppi < MIN_IMAGE_PPI)
        .collect();
    if !low.is_empty() {
        low.sort_by(|a, b| a.ppi.partial_cmp(&b.ppi).unwrap());
        let worst = &low[0];
        let mut pages: Vec<u32> = low.iter().map(|r| r.page).collect();
        pages.sort_unstable();
        pages.dedup();
        findings.push(
            Finding::new(
                codes::IMAGE_LOW_RESOLUTION,
                Severity::Warning,
                format!(
                    "{} image(s) below the 300 ppi target; the lowest is {:.0} ppi on page {} (object {:?})",
                    low.len(),
                    worst.ppi,
                    worst.page,
                    worst.image_id
                ),
            )
            .with_pages(pages)
            .with_observed(format!("{:.0} ppi", worst.ppi))
            .with_expected("300 ppi")
            .fixable(false),
        );
    }

    let mut high: Vec<&ImageResolutionRecord> = collector
        .records
        .iter()
        .filter(|r| r.ppi > MAX_IMAGE_PPI)
        .collect();
    if !high.is_empty() {
        high.sort_by(|a, b| b.ppi.partial_cmp(&a.ppi).unwrap());
        let worst = &high[0];
        let mut pages: Vec<u32> = high.iter().map(|r| r.page).collect();
        pages.sort_unstable();
        pages.dedup();
        findings.push(
            Finding::new(
                codes::IMAGE_EXCESSIVE_RESOLUTION,
                Severity::Warning,
                format!(
                    "{} image(s) exceed Lulu's 600 ppi maximum; the highest is {:.0} ppi on page {} (object {:?}) — this adds file size without improving print quality",
                    high.len(),
                    worst.ppi,
                    worst.page,
                    worst.image_id
                ),
            )
            .with_pages(pages)
            .with_observed(format!("{:.0} ppi", worst.ppi))
            .with_expected("<= 600 ppi")
            .fixable(false),
        );
    }

    findings
}

const MAX_TOTAL_AREA_COVERAGE_PERCENT: f64 = 270.0;
const MIN_TINT_PERCENT: f64 = 20.0;

fn operand_f64(operands: &[Object], i: usize) -> Option<f64> {
    match operands.get(i)? {
        Object::Integer(n) => Some(*n as f64),
        Object::Real(n) => Some(*n as f64),
        _ => None,
    }
}

/// Colour and ink checks over a page's own content stream operators: CMYK
/// total area coverage and low tints from `k`/`K` fill/stroke colour, and
/// non-DeviceGray/RGB/CMYK colour spaces, live transparency, and optional
/// content from the page's resources and the document catalog.
///
/// Evaluated over the page's own content stream and resources *and* over
/// the content and resources of every form XObject the page draws, to
/// whatever depth [`ctm_walk`]'s traversal budget allows — reusing
/// [`ctm_walk::collect_page_layers`], so colour set inside a nested form
/// XObject is not invisible to these checks. `OCProperties` remains a
/// document-catalog-level check, since it is not per-page content.
pub fn check_colour_and_ink(doc: &Document, page_ids: &[ObjectId]) -> Vec<Finding> {
    let mut findings = Vec::new();
    let mut tac_pages: Vec<u32> = Vec::new();
    let mut tac_worst: f64 = 0.0;
    let mut tint_pages: Vec<u32> = Vec::new();
    let mut tint_worst: f64 = 100.0;
    let mut transparency_pages: Vec<u32> = Vec::new();
    let mut space_pages: Vec<(String, Vec<u32>)> = Vec::new();

    for (i, &page_id) in page_ids.iter().enumerate() {
        let page_number = (i + 1) as u32;
        let (layers, _outcome) = ctm_walk::collect_page_layers(doc, page_id);

        for layer in &layers {
            let Ok(content) = lopdf::content::Content::decode(&layer.content) else {
                continue;
            };

            for op in content.operations.iter() {
                if (op.operator == "k" || op.operator == "K") && op.operands.len() == 4 {
                    let Some(vals) = (0..4)
                        .map(|i| operand_f64(&op.operands, i))
                        .collect::<Option<Vec<_>>>()
                    else {
                        continue;
                    };
                    let tac = vals.iter().sum::<f64>() * 100.0;
                    if tac > MAX_TOTAL_AREA_COVERAGE_PERCENT {
                        tac_pages.push(page_number);
                        tac_worst = tac_worst.max(tac);
                    }
                    for &v in &vals {
                        let pct = v * 100.0;
                        if pct > 0.0 && pct < MIN_TINT_PERCENT {
                            tint_pages.push(page_number);
                            tint_worst = tint_worst.min(pct);
                        }
                    }
                }
            }

            if let Ok(ext_g_states) = layer.resources.get(b"ExtGState").and_then(|o| o.as_dict()) {
                for (_, gs) in ext_g_states.as_hashmap() {
                    let Ok(gs) = gs.as_dict() else { continue };
                    let has_soft_mask = gs
                        .get(b"SMask")
                        .ok()
                        .is_some_and(|o| o.as_name().ok() != Some(b"None"));
                    let has_blend = gs
                        .get(b"BM")
                        .ok()
                        .and_then(|o| o.as_name().ok())
                        .is_some_and(|n| n != b"Normal" && n != b"Compatible");
                    if has_soft_mask || has_blend {
                        transparency_pages.push(page_number);
                    }
                }
            }
            if let Ok(color_spaces) = layer.resources.get(b"ColorSpace").and_then(|o| o.as_dict()) {
                for (_, cs) in color_spaces.as_hashmap() {
                    let name = match cs {
                        Object::Name(n) => Some(String::from_utf8_lossy(n).to_string()),
                        Object::Array(arr) => arr
                            .first()
                            .and_then(|o| o.as_name().ok())
                            .map(|n| String::from_utf8_lossy(n).to_string()),
                        _ => None,
                    };
                    if let Some(name) = name {
                        if !matches!(
                            name.as_str(),
                            "DeviceGray"
                                | "DeviceRGB"
                                | "DeviceCMYK"
                                | "CalRGB"
                                | "CalGray"
                                | "ICCBased"
                        ) {
                            space_pages.push((name, vec![page_number]));
                        }
                    }
                }
            }
        }
    }

    if !tac_pages.is_empty() {
        tac_pages.sort_unstable();
        tac_pages.dedup();
        findings.push(
            Finding::new(
                codes::COLOUR_TOTAL_AREA_COVERAGE,
                Severity::Warning,
                format!("total ink coverage reaches {tac_worst:.0}%, above Lulu's 270% ceiling"),
            )
            .with_pages(tac_pages)
            .with_observed(format!("{tac_worst:.0}%"))
            .with_expected("<= 270%")
            .fixable(false),
        );
    }

    if !tint_pages.is_empty() {
        tint_pages.sort_unstable();
        tint_pages.dedup();
        findings.push(
            Finding::new(
                codes::COLOUR_LOW_TINT,
                Severity::Warning,
                format!("a tint as low as {tint_worst:.0}% is used; Lulu advises against tints below 20%"),
            )
            .with_pages(tint_pages)
            .with_expected(">= 20%")
            .fixable(false),
        );
    }

    if !transparency_pages.is_empty() {
        transparency_pages.sort_unstable();
        transparency_pages.dedup();
        findings.push(
            Finding::new(
                codes::STRUCTURE_LIVE_TRANSPARENCY,
                Severity::Warning,
                "live transparency (a soft mask or non-Normal blend mode) is present and unflattened; flattening is the remedy".to_string(),
            )
            .with_pages(transparency_pages)
            .fixable(true),
        );
    }

    for (name, pages) in space_pages {
        findings.push(
            Finding::new(
                codes::COLOUR_UNSUPPORTED_SPACE,
                Severity::Warning,
                format!(
                    "colour space '{name}' is used; Lulu prefers DeviceGray, sRGB, or DeviceCMYK"
                ),
            )
            .with_pages(pages)
            .fixable(false),
        );
    }

    if let Ok(catalog) = doc.catalog() {
        if catalog.get(b"OCProperties").is_ok() {
            findings.push(
                Finding::new(codes::STRUCTURE_OPTIONAL_CONTENT, Severity::Warning, "the document declares optional content (layers), which must be flattened before printing".to_string()).fixable(true),
            );
        }
    }

    findings
}

/// Resource category dictionary keys a content-stream operator can name —
/// matching [`ctm_walk`]'s own `XObject` descent and
/// [`pdf::effective_page_resources`]'s merge categories, plus `Shading` (the
/// `sh` operator) and `Pattern` (a trailing `Name` operand to `scn`/`SCN`).
const BUILTIN_COLOR_SPACE_NAMES: &[&[u8]] =
    &[b"DeviceGray", b"DeviceRGB", b"DeviceCMYK", b"Pattern"];

/// One resource name a content-stream operator referenced, and the resource
/// category (`/Font`, `/XObject`, ...) it must resolve in.
struct ResourceRef {
    category: &'static [u8],
    category_label: &'static str,
    name: Vec<u8>,
}

fn name_operand(operands: &[Object], i: usize) -> Option<Vec<u8>> {
    match operands.get(i)? {
        Object::Name(n) => Some(n.clone()),
        _ => None,
    }
}

/// Every resource a content stream's operators name — `/F1 Tf`, `/Im0 Do`,
/// `/GS0 gs`, a non-device `cs`/`CS` colour space, a pattern name trailing
/// `scn`/`SCN`, and `/Sh0 sh` — regardless of whether that name actually
/// resolves; [`check_resource_references`] is what checks resolution.
fn referenced_resource_names(content_bytes: &[u8]) -> Vec<ResourceRef> {
    let Ok(content) = lopdf::content::Content::decode(content_bytes) else {
        return Vec::new();
    };
    let mut refs = Vec::new();
    for Operation { operator, operands } in content.operations.iter() {
        match operator.as_str() {
            "Tf" => {
                if let Some(name) = name_operand(operands, 0) {
                    refs.push(ResourceRef {
                        category: b"Font",
                        category_label: "font",
                        name,
                    });
                }
            }
            "Do" => {
                if let Some(name) = name_operand(operands, 0) {
                    refs.push(ResourceRef {
                        category: b"XObject",
                        category_label: "XObject",
                        name,
                    });
                }
            }
            "gs" => {
                if let Some(name) = name_operand(operands, 0) {
                    refs.push(ResourceRef {
                        category: b"ExtGState",
                        category_label: "ExtGState",
                        name,
                    });
                }
            }
            "cs" | "CS" => {
                if let Some(name) = name_operand(operands, 0) {
                    if !BUILTIN_COLOR_SPACE_NAMES.contains(&name.as_slice()) {
                        refs.push(ResourceRef {
                            category: b"ColorSpace",
                            category_label: "colour space",
                            name,
                        });
                    }
                }
            }
            "scn" | "SCN" => {
                // A pattern name is the operator's trailing operand only
                // when the current colour space is /Pattern; operands.last()
                // being a Name is otherwise vanishingly unlikely for scn/SCN
                // (its numeric-component operands are never Names), so this
                // is a safe, budget-cheap approximation rather than tracking
                // the active colour space through the whole stream.
                if let Some(Object::Name(name)) = operands.last() {
                    refs.push(ResourceRef {
                        category: b"Pattern",
                        category_label: "pattern",
                        name: name.clone(),
                    });
                }
            }
            "sh" => {
                if let Some(name) = name_operand(operands, 0) {
                    refs.push(ResourceRef {
                        category: b"Shading",
                        category_label: "shading",
                        name,
                    });
                }
            }
            _ => {}
        }
    }
    refs
}

/// A blocking finding for every resource name a page's or a nested form
/// XObject's content stream references but that does not resolve in its
/// effective resources — the "blank page" bug this crate exists to catch:
/// the content still says `/F1 Tf` or `/Im0 Do`, but nothing by that name
/// exists to draw. Keyed on the specific named operand, not on an empty
/// resource dictionary, so a page that legitimately draws nothing (an empty
/// content stream, no resources) is never flagged.
///
/// Also reports a page whose own effective `/Resources` cannot be resolved
/// at all (a broken indirect reference — distinct from "resolves, but is
/// missing a name the content asks for"), and a page whose traversal
/// exceeded [`ctm_walk::MAX_WALK_OPERATIONS`] — this is the one check that
/// surfaces that budget's exhaustion, since it already walks every layer for
/// its own purpose; [`check_font_embedding`] and [`check_colour_and_ink`]
/// share the same walk but do not also report it, to avoid a triple finding
/// for the same page.
pub fn check_resource_references(doc: &Document, page_ids: &[ObjectId]) -> Vec<Finding> {
    let mut missing: BTreeMap<(&'static str, String), Vec<u32>> = BTreeMap::new();
    let mut unresolved_resources_pages: Vec<u32> = Vec::new();
    let mut budget_pages: Vec<u32> = Vec::new();

    for (i, &page_id) in page_ids.iter().enumerate() {
        let page_number = (i + 1) as u32;
        if pdf::effective_page_resources(doc, page_id).is_err() {
            unresolved_resources_pages.push(page_number);
            continue;
        }
        let (layers, outcome) = ctm_walk::collect_page_layers(doc, page_id);
        if outcome == ctm_walk::WalkOutcome::BudgetExceeded {
            budget_pages.push(page_number);
        }
        for layer in &layers {
            for r in referenced_resource_names(&layer.content) {
                let resolved = layer
                    .resources
                    .get(r.category)
                    .ok()
                    .and_then(|o| resolve_dict_local(doc, o))
                    .is_some_and(|dict| dict.get(&r.name).is_ok());
                if !resolved {
                    let name_str = format!("/{}", String::from_utf8_lossy(&r.name));
                    missing
                        .entry((r.category_label, name_str))
                        .or_default()
                        .push(page_number);
                }
            }
        }
    }

    let mut findings = Vec::new();

    if !unresolved_resources_pages.is_empty() {
        unresolved_resources_pages.sort_unstable();
        unresolved_resources_pages.dedup();
        findings.push(
            Finding::new(
                codes::GEOMETRY_UNRESOLVABLE_RESOURCES,
                Severity::Blocking,
                format!(
                    "{} page(s) carry a /Resources entry (their own, or one inherited from a Pages ancestor) that is a reference which does not resolve to a dictionary",
                    unresolved_resources_pages.len()
                ),
            )
            .with_pages(unresolved_resources_pages)
            .fixable(false),
        );
    }

    for ((label, name), mut pages) in missing {
        pages.sort_unstable();
        pages.dedup();
        findings.push(
            Finding::new(
                codes::RESOURCES_MISSING_REFERENCE,
                Severity::Blocking,
                format!(
                    "content references {label} '{name}', which does not resolve in the effective resources — this content will not appear"
                ),
            )
            .with_pages(pages)
            .fixable(false),
        );
    }

    if !budget_pages.is_empty() {
        budget_pages.sort_unstable();
        budget_pages.dedup();
        findings.push(
            Finding::new(
                codes::STRUCTURE_TRAVERSAL_BUDGET_EXCEEDED,
                Severity::Blocking,
                format!(
                    "{} page(s) have nested form XObjects deep or numerous enough that traversal exceeded its {}-operation budget; checks on these pages are incomplete",
                    budget_pages.len(),
                    ctm_walk::MAX_WALK_OPERATIONS
                ),
            )
            .with_pages(budget_pages)
            .fixable(false),
        );
    }

    findings
}

struct SafetyMarginCollector {
    safe_rect: Rect,
    trim_rect: Rect,
    violated: bool,
}

impl ctm_walk::ImageVisitor for SafetyMarginCollector {
    fn visit_image(
        &mut self,
        ctm: crate::units::Matrix,
        _image_dict: &Dictionary,
        _image_id: ObjectId,
    ) {
        let corners = [(0.0, 0.0), (1.0, 0.0), (0.0, 1.0), (1.0, 1.0)];
        let mut min_x = f64::INFINITY;
        let mut max_x = f64::NEG_INFINITY;
        let mut min_y = f64::INFINITY;
        let mut max_y = f64::NEG_INFINITY;
        for (ux, uy) in corners {
            let (x, y) = ctm.apply_to_point(Length::from_points(ux), Length::from_points(uy));
            min_x = min_x.min(x.as_points());
            max_x = max_x.max(x.as_points());
            min_y = min_y.min(y.as_points());
            max_y = max_y.max(y.as_points());
        }

        // An image whose footprint extends past the trim edge is bleeding
        // intentionally — that's the expected way to fill the margin band
        // and beyond, not a violation of it.
        let bleeds_past_trim = min_x < self.trim_rect.x0.as_points()
            || max_x > self.trim_rect.x1.as_points()
            || min_y < self.trim_rect.y0.as_points()
            || max_y > self.trim_rect.y1.as_points();
        if bleeds_past_trim {
            return;
        }

        let inside_margin = min_x < self.safe_rect.x0.as_points()
            || max_x > self.safe_rect.x1.as_points()
            || min_y < self.safe_rect.y0.as_points()
            || max_y > self.safe_rect.y1.as_points();
        if inside_margin {
            self.violated = true;
        }
    }
}

/// Warns when a raster image is placed inside Lulu's recommended interior
/// safety margin ([`crate::geometry::interior_safety_margin`], 0.5 in from
/// the trim edge) without bleeding past the trim edge itself — content this
/// close to where the page is actually cut risks being trimmed off by
/// ordinary press tolerance.
///
/// Scoped to raster images placed via [`ctm_walk::walk_page_images`] (which
/// already tracks the CTM at each `Do`, so no new traversal is needed here);
/// text and vector-path marks are not tracked, since knowing where those are
/// actually drawn needs full glyph/path geometry analysis that this pass
/// does not attempt — see
/// `openspec/changes/harden-pdf-correctness/tasks.md` task 2.8.
pub fn check_interior_safety_margin(doc: &Document, page_ids: &[ObjectId]) -> Vec<Finding> {
    let bleed = crate::geometry::bleed();
    let safety = crate::geometry::interior_safety_margin();
    let mut pages: Vec<u32> = Vec::new();

    for (i, &page_id) in page_ids.iter().enumerate() {
        let page_number = (i + 1) as u32;
        let Ok(own_rect) = pdf::own_box_rect(doc, page_id) else {
            continue;
        };
        let trim_rect = own_rect.inset(bleed);
        let safe_rect = trim_rect.inset(safety);
        let mut collector = SafetyMarginCollector {
            safe_rect,
            trim_rect,
            violated: false,
        };
        ctm_walk::walk_page_images(doc, page_id, &mut collector);
        if collector.violated {
            pages.push(page_number);
        }
    }

    if pages.is_empty() {
        return Vec::new();
    }
    vec![Finding::new(
        codes::GEOMETRY_CONTENT_INSIDE_SAFETY_MARGIN,
        Severity::Warning,
        format!(
            "{} page(s) place a non-bleeding image inside Lulu's recommended {:.2} in safety margin from the trim edge",
            pages.len(),
            safety.as_inches()
        ),
    )
    .with_pages(pages)
    .fixable(false)]
}

/// Preflight a PDF, given as raw bytes, against an optional target product.
/// Always returns a [`Report`] — even a file that fails to parse gets one,
/// with a single blocking finding describing the failure.
pub fn preflight(bytes: &[u8], product: Option<&CatalogEntry>) -> Report {
    let mut findings = Vec::new();

    let doc = match pdf::load_from_bytes(bytes) {
        Ok(doc) => doc,
        Err(e) => {
            findings.push(
                Finding::new(
                    codes::DOCUMENT_PARSE_FAILED,
                    Severity::Blocking,
                    format!("could not parse this file as a PDF: {e}"),
                )
                .fixable(false),
            );
            return finish_report(product, None, findings, Vec::new(), Vec::new());
        }
    };

    if pdf::was_ever_encrypted(&doc) {
        findings.push(
            Finding::new(
                codes::STRUCTURE_ENCRYPTED,
                Severity::Blocking,
                "the file carries an encryption dictionary; Lulu does not accept security or password protection on any file, even with an empty user password".to_string(),
            )
            .fixable(true),
        );
    }

    let page_ids: Vec<ObjectId> = doc.page_iter().collect();
    let page_count = page_ids.len() as u32;

    findings.extend(check_mixed_page_sizes(&doc, &page_ids));
    findings.extend(check_page_geometry_resolution(&doc, &page_ids));
    findings.extend(check_page_rotation(&doc, &page_ids));
    findings.extend(check_font_embedding(&doc, &page_ids));
    findings.extend(check_structure(&doc, &page_ids));
    findings.extend(check_image_resolution(&doc, &page_ids));
    findings.extend(check_colour_and_ink(&doc, &page_ids));
    findings.extend(check_resource_references(&doc, &page_ids));
    findings.extend(check_interior_safety_margin(&doc, &page_ids));

    if let Some(product) = product {
        let required = crate::geometry::required_page_size(product.trim_size);
        findings.extend(check_page_size_matches_target(&doc, &page_ids, required));
        let rules = PageCountRules::from_catalog_entry(product);
        findings.extend(check_page_count(page_count, &rules));
    }

    finish_report(product, Some(page_count), findings, Vec::new(), Vec::new())
}

/// Preflights a cover file: the checks that make sense for a single page
/// sized to a cover canvas rather than an interior sized to a product's trim.
/// Unlike [`preflight`], this never applies interior-only rules — page-count
/// minimums/maximums and mixed-page-size comparisons are meaningless for a
/// cover, which is always exactly one page.
pub fn preflight_cover(bytes: &[u8], product: &CatalogEntry, expected_canvas: Size) -> Report {
    let mut findings = Vec::new();

    let doc = match pdf::load_from_bytes(bytes) {
        Ok(doc) => doc,
        Err(e) => {
            findings.push(
                Finding::new(
                    codes::DOCUMENT_PARSE_FAILED,
                    Severity::Blocking,
                    format!("could not parse this file as a PDF: {e}"),
                )
                .fixable(false),
            );
            return finish_report(Some(product), None, findings, Vec::new(), Vec::new());
        }
    };

    if pdf::was_ever_encrypted(&doc) {
        findings.push(
            Finding::new(
                codes::STRUCTURE_ENCRYPTED,
                Severity::Blocking,
                "the file carries an encryption dictionary; Lulu does not accept security or password protection on any file, even with an empty user password".to_string(),
            )
            .fixable(true),
        );
    }

    let page_ids: Vec<ObjectId> = doc.page_iter().collect();
    let page_count = page_ids.len() as u32;

    if page_count != 1 {
        findings.push(Finding::new(
            codes::COVER_WRONG_PAGE_COUNT,
            Severity::Blocking,
            format!("a cover file must be exactly one page; this file has {page_count}"),
        ));
    }

    findings.extend(check_page_size_matches_target(
        &doc,
        &page_ids,
        expected_canvas,
    ));
    findings.extend(check_page_geometry_resolution(&doc, &page_ids));
    findings.extend(check_page_rotation(&doc, &page_ids));
    findings.extend(check_font_embedding(&doc, &page_ids));
    findings.extend(check_structure(&doc, &page_ids));
    findings.extend(check_image_resolution(&doc, &page_ids));
    findings.extend(check_colour_and_ink(&doc, &page_ids));
    findings.extend(check_resource_references(&doc, &page_ids));

    finish_report(
        Some(product),
        Some(page_count),
        findings,
        Vec::new(),
        Vec::new(),
    )
}

fn finish_report(
    product: Option<&CatalogEntry>,
    page_count: Option<u32>,
    findings: Vec<Finding>,
    detected_tools: Vec<DetectedTool>,
    stages: Vec<StageLogEntry>,
) -> Report {
    Report {
        schema_version: SCHEMA_VERSION,
        input_digest: None,
        product_sku: product.map(|p| p.sku.clone()),
        page_count,
        catalog_fetch_date: product.map(|_| crate::catalog::metadata().fetch_date.clone()),
        tool_version: env!("CARGO_PKG_VERSION").to_string(),
        detected_tools,
        stages,
        findings,
        generated_at: crate::report::now_rfc3339(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog;
    use crate::report::{codes, Severity};
    use crate::units::{Length, Size};
    use lopdf::{dictionary, Object};
    use std::fs;
    use std::time::SystemTime;

    fn mediabox(w: f64, h: f64) -> Object {
        Object::Array(vec![0.0.into(), 0.0.into(), w.into(), h.into()])
    }

    /// A minimal N-page document, each page's dictionary built by `page_entries`.
    fn doc_with_pages(
        count: usize,
        mut page_entries: impl FnMut(usize) -> lopdf::Dictionary,
    ) -> lopdf::Document {
        let mut doc = lopdf::Document::with_version("1.7");
        let pages_id = doc.new_object_id();
        let mut kids = Vec::new();
        for i in 0..count {
            let mut dict = dictionary! {
                "Type" => "Page",
                "Parent" => Object::Reference(pages_id),
            };
            dict.extend(&page_entries(i));
            let page_id = doc.add_object(Object::Dictionary(dict));
            kids.push(Object::Reference(page_id));
        }
        let pages = dictionary! {
            "Type" => "Pages",
            "Kids" => kids,
            "Count" => count as i64,
        };
        doc.objects.insert(pages_id, Object::Dictionary(pages));
        let catalog_id = doc.add_object(dictionary! {
            "Type" => "Catalog",
            "Pages" => Object::Reference(pages_id),
        });
        doc.trailer.set("Root", Object::Reference(catalog_id));
        doc
    }

    fn required_6x9() -> Size {
        Size::new(Length::from_points(450.0), Length::from_points(666.0))
    }

    #[test]
    fn uniform_wrong_size_is_one_blocking_fixable_finding() {
        let doc = doc_with_pages(3, |_| dictionary! { "MediaBox" => mediabox(432.0, 648.0) }); // 6x9in, no bleed
        let page_ids: Vec<_> = doc.page_iter().collect();
        let findings = check_page_size_matches_target(&doc, &page_ids, required_6x9());
        assert_eq!(findings.len(), 1);
        let f = &findings[0];
        assert_eq!(f.code, codes::GEOMETRY_PAGE_SIZE_MISMATCH);
        assert_eq!(f.severity, Severity::Blocking);
        assert!(f.fixable);
        assert_eq!(f.pages.len(), 3);
        assert!(f.message.contains("0.125"), "{}", f.message);
    }

    #[test]
    fn page_within_tolerance_is_not_flagged() {
        let doc = doc_with_pages(
            1,
            |_| dictionary! { "MediaBox" => mediabox(450.02, 665.98) },
        );
        let page_ids: Vec<_> = doc.page_iter().collect();
        let findings = check_page_size_matches_target(&doc, &page_ids, required_6x9());
        assert!(findings.is_empty());
    }

    #[test]
    fn correctly_bled_pages_are_not_flagged() {
        let doc = doc_with_pages(2, |_| dictionary! { "MediaBox" => mediabox(450.0, 666.0) });
        let page_ids: Vec<_> = doc.page_iter().collect();
        let findings = check_page_size_matches_target(&doc, &page_ids, required_6x9());
        assert!(findings.is_empty());
    }

    #[test]
    fn rotated_page_is_checked_by_visible_size() {
        // Correctly-bled box (450x666), but rotated 90 degrees so the visible
        // size is 666x450 — which does not match a 6x9in target.
        let doc = doc_with_pages(
            1,
            |_| dictionary! { "MediaBox" => mediabox(450.0, 666.0), "Rotate" => 90 },
        );
        let page_ids: Vec<_> = doc.page_iter().collect();
        let findings = check_page_size_matches_target(&doc, &page_ids, required_6x9());
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].code, codes::GEOMETRY_PAGE_SIZE_MISMATCH);
    }

    #[test]
    fn mixed_page_sizes_lists_each_distinct_size() {
        let doc = doc_with_pages(3, |i| {
            if i == 1 {
                dictionary! { "MediaBox" => mediabox(500.0, 700.0) }
            } else {
                dictionary! { "MediaBox" => mediabox(450.0, 666.0) }
            }
        });
        let page_ids: Vec<_> = doc.page_iter().collect();
        let findings = check_mixed_page_sizes(&doc, &page_ids);
        assert_eq!(findings.len(), 1);
        let f = &findings[0];
        assert_eq!(f.code, codes::GEOMETRY_MIXED_PAGE_SIZES);
        assert_eq!(f.severity, Severity::Blocking);
        // page 2 (index 1, 1-based page 2) is the odd one out
        assert!(f.pages.contains(&2));
    }

    #[test]
    fn uniform_sizes_are_not_flagged_as_mixed() {
        let doc = doc_with_pages(3, |_| dictionary! { "MediaBox" => mediabox(450.0, 666.0) });
        let page_ids: Vec<_> = doc.page_iter().collect();
        assert!(check_mixed_page_sizes(&doc, &page_ids).is_empty());
    }

    // --- font embedding ---

    fn doc_with_unembedded_font() -> lopdf::Document {
        let mut doc = lopdf::Document::with_version("1.7");
        let pages_id = doc.new_object_id();
        let font_id = doc.add_object(dictionary! {
            "Type" => "Font",
            "Subtype" => "Type1",
            "BaseFont" => "Helvetica",
        });
        let resources =
            dictionary! { "Font" => dictionary! { "F1" => Object::Reference(font_id) } };
        let page_id = doc.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => Object::Reference(pages_id),
            "MediaBox" => mediabox(450.0, 666.0),
            "Resources" => resources,
        });
        let pages = dictionary! { "Type" => "Pages", "Kids" => vec![Object::Reference(page_id)], "Count" => 1 };
        doc.objects.insert(pages_id, Object::Dictionary(pages));
        let catalog_id = doc.add_object(
            dictionary! { "Type" => "Catalog", "Pages" => Object::Reference(pages_id) },
        );
        doc.trailer.set("Root", Object::Reference(catalog_id));
        doc
    }

    fn doc_with_embedded_font() -> lopdf::Document {
        let mut doc = lopdf::Document::with_version("1.7");
        let pages_id = doc.new_object_id();
        let font_file_id = doc.add_object(lopdf::Stream::new(dictionary! {}, vec![0u8; 4]));
        let descriptor_id = doc.add_object(dictionary! {
            "Type" => "FontDescriptor",
            "FontName" => "ABCDEF+Minion-Regular",
            "FontFile2" => Object::Reference(font_file_id),
        });
        let font_id = doc.add_object(dictionary! {
            "Type" => "Font",
            "Subtype" => "TrueType",
            "BaseFont" => "ABCDEF+Minion-Regular",
            "FontDescriptor" => Object::Reference(descriptor_id),
        });
        let resources =
            dictionary! { "Font" => dictionary! { "F1" => Object::Reference(font_id) } };
        let page_id = doc.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => Object::Reference(pages_id),
            "MediaBox" => mediabox(450.0, 666.0),
            "Resources" => resources,
        });
        let pages = dictionary! { "Type" => "Pages", "Kids" => vec![Object::Reference(page_id)], "Count" => 1 };
        doc.objects.insert(pages_id, Object::Dictionary(pages));
        let catalog_id = doc.add_object(
            dictionary! { "Type" => "Catalog", "Pages" => Object::Reference(pages_id) },
        );
        doc.trailer.set("Root", Object::Reference(catalog_id));
        doc
    }

    fn doc_with_composite_font(embedded: bool) -> lopdf::Document {
        let mut doc = lopdf::Document::with_version("1.7");
        let pages_id = doc.new_object_id();

        let mut descriptor_entries = dictionary! {
            "Type" => "FontDescriptor",
            "FontName" => "ABCDEF+NotoSansCJK",
        };
        if embedded {
            let font_file_id = doc.add_object(lopdf::Stream::new(dictionary! {}, vec![0u8; 4]));
            descriptor_entries.set("FontFile2", Object::Reference(font_file_id));
        }
        let descriptor_id = doc.add_object(descriptor_entries);
        let descendant_id = doc.add_object(dictionary! {
            "Type" => "Font",
            "Subtype" => "CIDFontType2",
            "FontDescriptor" => Object::Reference(descriptor_id),
        });
        let type0_id = doc.add_object(dictionary! {
            "Type" => "Font",
            "Subtype" => "Type0",
            "BaseFont" => "ABCDEF+NotoSansCJK",
            "DescendantFonts" => vec![Object::Reference(descendant_id)],
        });
        let resources =
            dictionary! { "Font" => dictionary! { "F1" => Object::Reference(type0_id) } };
        let page_id = doc.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => Object::Reference(pages_id),
            "MediaBox" => mediabox(450.0, 666.0),
            "Resources" => resources,
        });
        let pages = dictionary! { "Type" => "Pages", "Kids" => vec![Object::Reference(page_id)], "Count" => 1 };
        doc.objects.insert(pages_id, Object::Dictionary(pages));
        let catalog_id = doc.add_object(
            dictionary! { "Type" => "Catalog", "Pages" => Object::Reference(pages_id) },
        );
        doc.trailer.set("Root", Object::Reference(catalog_id));
        doc
    }

    #[test]
    fn unembedded_standard_font_is_blocking() {
        let doc = doc_with_unembedded_font();
        let page_ids: Vec<_> = doc.page_iter().collect();
        let findings = check_font_embedding(&doc, &page_ids);
        assert_eq!(findings.len(), 1);
        let f = &findings[0];
        assert_eq!(f.code, codes::FONTS_NOT_EMBEDDED);
        assert_eq!(f.severity, Severity::Blocking);
        assert!(f.message.contains("Helvetica"), "{}", f.message);
        assert_eq!(f.pages, vec![1]);
    }

    #[test]
    fn subset_embedded_font_passes() {
        let doc = doc_with_embedded_font();
        let page_ids: Vec<_> = doc.page_iter().collect();
        assert!(check_font_embedding(&doc, &page_ids).is_empty());
    }

    #[test]
    fn composite_font_descendant_embedding_is_checked() {
        let embedded = doc_with_composite_font(true);
        let page_ids: Vec<_> = embedded.page_iter().collect();
        assert!(check_font_embedding(&embedded, &page_ids).is_empty());

        let not_embedded = doc_with_composite_font(false);
        let page_ids: Vec<_> = not_embedded.page_iter().collect();
        let findings = check_font_embedding(&not_embedded, &page_ids);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].code, codes::FONTS_NOT_EMBEDDED);
    }

    // --- page count ---

    #[test]
    fn page_count_below_minimum_states_the_padding() {
        let rules = crate::geometry::PageCountRules {
            min: 32,
            max: 800,
            multiple: 4,
        };
        let findings = check_page_count(18, &rules);
        assert_eq!(findings.len(), 1);
        let f = &findings[0];
        assert_eq!(f.code, codes::PAGE_COUNT_BELOW_MINIMUM);
        assert_eq!(f.severity, Severity::Blocking);
        assert!(f.message.contains("14"), "{}", f.message); // 32 - 18 = 14 blank pages
    }

    #[test]
    fn page_count_not_divisible_states_the_padding() {
        let rules = crate::geometry::PageCountRules {
            min: 32,
            max: 800,
            multiple: 4,
        };
        let findings = check_page_count(205, &rules);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].code, codes::PAGE_COUNT_NOT_DIVISIBLE);
        assert!(findings[0].message.contains('3'), "{}", findings[0].message); // 208 - 205 = 3
    }

    #[test]
    fn page_count_above_maximum_is_blocking_and_unfixable() {
        let rules = crate::geometry::PageCountRules {
            min: 32,
            max: 800,
            multiple: 4,
        };
        let findings = check_page_count(812, &rules);
        assert_eq!(findings.len(), 1);
        let f = &findings[0];
        assert_eq!(f.code, codes::PAGE_COUNT_ABOVE_MAXIMUM);
        assert!(!f.fixable);
    }

    #[test]
    fn conformant_page_count_has_no_finding() {
        let rules = crate::geometry::PageCountRules {
            min: 32,
            max: 800,
            multiple: 4,
        };
        assert!(check_page_count(208, &rules).is_empty());
    }

    // --- top-level orchestration ---

    #[test]
    fn preflight_reports_readiness_for_a_clean_file() {
        let sku = "0600X0900.BW.STD.PB.060UW444.MXX";
        let entry = catalog::lookup(sku).unwrap();
        let mut doc = doc_with_embedded_font();
        // doc_with_embedded_font already has one correctly-sized page; pad it
        // to the product's 32-page minimum for a fully clean run.
        while doc.page_iter().count() < 32 {
            let page_id = doc.add_object(dictionary! {
                "Type" => "Page",
                "MediaBox" => mediabox(450.0, 666.0),
            });
            let pages_id = doc
                .catalog()
                .unwrap()
                .get(b"Pages")
                .unwrap()
                .as_reference()
                .unwrap();
            let pages = doc.get_dictionary_mut(pages_id).unwrap();
            pages
                .get_mut(b"Kids")
                .unwrap()
                .as_array_mut()
                .unwrap()
                .push(Object::Reference(page_id));
            let count = pages.get(b"Count").unwrap().as_i64().unwrap();
            pages.set("Count", count + 1);
        }
        let mut buf = Vec::new();
        doc.save_to(&mut buf).unwrap();

        let report = preflight(&buf, Some(entry));
        assert!(report.is_print_ready(), "{}", report.to_text());
        assert_eq!(report.page_count, Some(32));
    }

    #[test]
    fn preflight_on_unparseable_bytes_still_returns_a_report() {
        let report = preflight(b"not a pdf at all", None);
        assert!(!report.is_print_ready());
        assert!(report
            .findings
            .iter()
            .any(|f| f.severity == Severity::Blocking));
    }

    #[test]
    fn preflight_never_modifies_the_input_file() {
        let dir =
            std::env::temp_dir().join(format!("lulu-prep-preflight-test-{:?}", SystemTime::now()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("input.pdf");
        let doc = doc_with_pages(1, |_| dictionary! { "MediaBox" => mediabox(450.0, 666.0) });
        let mut buf = Vec::new();
        {
            let mut d = doc;
            d.save_to(&mut buf).unwrap();
        }
        fs::write(&path, &buf).unwrap();
        let before_bytes = fs::read(&path).unwrap();
        let before_mtime = fs::metadata(&path).unwrap().modified().unwrap();

        let _report = preflight(&before_bytes, None);

        let after_bytes = fs::read(&path).unwrap();
        let after_mtime = fs::metadata(&path).unwrap().modified().unwrap();
        assert_eq!(before_bytes, after_bytes);
        assert_eq!(before_mtime, after_mtime);
        fs::remove_dir_all(&dir).ok();
    }

    // --- structural checks ---

    fn doc_with_catalog_entries(
        catalog_entries: lopdf::Dictionary,
        page_entries: lopdf::Dictionary,
    ) -> lopdf::Document {
        let mut doc = lopdf::Document::with_version("1.7");
        let pages_id = doc.new_object_id();
        let mut page_dict = dictionary! {
            "Type" => "Page",
            "Parent" => Object::Reference(pages_id),
            "MediaBox" => mediabox(450.0, 666.0),
        };
        page_dict.extend(&page_entries);
        let page_id = doc.add_object(Object::Dictionary(page_dict));
        let pages = dictionary! { "Type" => "Pages", "Kids" => vec![Object::Reference(page_id)], "Count" => 1 };
        doc.objects.insert(pages_id, Object::Dictionary(pages));
        let mut catalog_dict = dictionary! {
            "Type" => "Catalog",
            "Pages" => Object::Reference(pages_id),
        };
        catalog_dict.extend(&catalog_entries);
        let catalog_id = doc.add_object(Object::Dictionary(catalog_dict));
        doc.trailer.set("Root", Object::Reference(catalog_id));
        doc
    }

    #[test]
    fn annotations_are_reported_with_subtypes_and_pages() {
        let mut doc = lopdf::Document::with_version("1.7");
        let pages_id = doc.new_object_id();
        let annot_id = doc.add_object(dictionary! { "Type" => "Annot", "Subtype" => "Link" });
        let page_id = doc.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => Object::Reference(pages_id),
            "MediaBox" => mediabox(450.0, 666.0),
            "Annots" => vec![Object::Reference(annot_id)],
        });
        let pages = dictionary! { "Type" => "Pages", "Kids" => vec![Object::Reference(page_id)], "Count" => 1 };
        doc.objects.insert(pages_id, Object::Dictionary(pages));
        let catalog_id = doc.add_object(
            dictionary! { "Type" => "Catalog", "Pages" => Object::Reference(pages_id) },
        );
        doc.trailer.set("Root", Object::Reference(catalog_id));

        let page_ids: Vec<_> = doc.page_iter().collect();
        let findings = check_structure(&doc, &page_ids);
        let f = findings
            .iter()
            .find(|f| f.code == codes::STRUCTURE_ANNOTATIONS)
            .expect("annotation finding");
        assert_eq!(f.severity, Severity::Warning);
        assert!(f.message.contains("Link"), "{}", f.message);
        assert_eq!(f.pages, vec![1]);
        assert!(f.fixable);
    }

    #[test]
    fn no_annotations_means_no_finding() {
        let doc = doc_with_pages(1, |_| dictionary! { "MediaBox" => mediabox(450.0, 666.0) });
        let page_ids: Vec<_> = doc.page_iter().collect();
        assert!(!check_structure(&doc, &page_ids)
            .iter()
            .any(|f| f.code == codes::STRUCTURE_ANNOTATIONS));
    }

    #[test]
    fn acroform_and_fields_are_reported() {
        let doc = doc_with_catalog_entries(
            dictionary! { "AcroForm" => dictionary! { "Fields" => Vec::<Object>::new() } },
            dictionary! {},
        );
        let page_ids: Vec<_> = doc.page_iter().collect();
        let findings = check_structure(&doc, &page_ids);
        assert!(findings
            .iter()
            .any(|f| f.code == codes::STRUCTURE_ANNOTATIONS
                && f.message.to_lowercase().contains("form")));
    }

    #[test]
    fn document_level_javascript_is_reported() {
        let doc = doc_with_catalog_entries(
            dictionary! { "Names" => dictionary! { "JavaScript" => dictionary! { "Names" => Vec::<Object>::new() } } },
            dictionary! {},
        );
        let page_ids: Vec<_> = doc.page_iter().collect();
        let findings = check_structure(&doc, &page_ids);
        assert!(findings
            .iter()
            .any(|f| f.message.to_lowercase().contains("javascript")));
    }

    #[test]
    fn embedded_files_are_reported() {
        let doc = doc_with_catalog_entries(
            dictionary! { "Names" => dictionary! { "EmbeddedFiles" => dictionary! { "Names" => Vec::<Object>::new() } } },
            dictionary! {},
        );
        let page_ids: Vec<_> = doc.page_iter().collect();
        let findings = check_structure(&doc, &page_ids);
        assert!(findings
            .iter()
            .any(|f| f.message.to_lowercase().contains("embedded file")));
    }

    #[test]
    fn spread_layout_is_reported_and_forced_to_single_page() {
        let doc = doc_with_catalog_entries(
            dictionary! { "PageLayout" => "TwoPageLeft" },
            dictionary! {},
        );
        let page_ids: Vec<_> = doc.page_iter().collect();
        let findings = check_structure(&doc, &page_ids);
        let f = findings
            .iter()
            .find(|f| f.code == codes::STRUCTURE_SPREAD_LAYOUT)
            .expect("spread finding");
        assert_eq!(f.severity, Severity::Warning);
        assert!(f.fixable);
    }

    #[test]
    fn single_page_layout_has_no_spread_finding() {
        let doc =
            doc_with_catalog_entries(dictionary! { "PageLayout" => "SinglePage" }, dictionary! {});
        let page_ids: Vec<_> = doc.page_iter().collect();
        assert!(!check_structure(&doc, &page_ids)
            .iter()
            .any(|f| f.code == codes::STRUCTURE_SPREAD_LAYOUT));
    }

    #[test]
    fn multimedia_annotation_is_reported() {
        let mut doc = lopdf::Document::with_version("1.7");
        let pages_id = doc.new_object_id();
        let annot_id = doc.add_object(dictionary! { "Type" => "Annot", "Subtype" => "Screen" });
        let page_id = doc.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => Object::Reference(pages_id),
            "MediaBox" => mediabox(450.0, 666.0),
            "Annots" => vec![Object::Reference(annot_id)],
        });
        let pages = dictionary! { "Type" => "Pages", "Kids" => vec![Object::Reference(page_id)], "Count" => 1 };
        doc.objects.insert(pages_id, Object::Dictionary(pages));
        let catalog_id = doc.add_object(
            dictionary! { "Type" => "Catalog", "Pages" => Object::Reference(pages_id) },
        );
        doc.trailer.set("Root", Object::Reference(catalog_id));

        let page_ids: Vec<_> = doc.page_iter().collect();
        let findings = check_structure(&doc, &page_ids);
        assert!(findings
            .iter()
            .any(|f| f.message.to_lowercase().contains("multimedia")
                || f.message.to_lowercase().contains("screen")));
    }

    #[test]
    fn clean_structure_has_no_findings() {
        let doc = doc_with_pages(1, |_| dictionary! { "MediaBox" => mediabox(450.0, 666.0) });
        let page_ids: Vec<_> = doc.page_iter().collect();
        assert!(check_structure(&doc, &page_ids).is_empty());
    }

    // --- image resolution ---

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

    /// A one-page document whose content stream draws one image XObject
    /// under the given `cm` operands (a full six-value matrix).
    fn doc_with_one_image(pixel_w: i64, pixel_h: i64, cm: [f64; 6]) -> lopdf::Document {
        let mut doc = lopdf::Document::with_version("1.7");
        let pages_id = doc.new_object_id();
        let image_id = doc.add_object(Object::Stream(lopdf::Stream::new(
            image_xobject(pixel_w, pixel_h),
            vec![0u8; 4],
        )));
        let resources =
            dictionary! { "XObject" => dictionary! { "Im0" => Object::Reference(image_id) } };
        let content = format!(
            "q {} {} {} {} {} {} cm /Im0 Do Q",
            cm[0], cm[1], cm[2], cm[3], cm[4], cm[5]
        );
        let content_id = doc.add_object(lopdf::Stream::new(dictionary! {}, content.into_bytes()));
        let page_id = doc.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => Object::Reference(pages_id),
            "MediaBox" => mediabox(450.0, 666.0),
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

    #[test]
    fn low_resolution_image_is_a_warning() {
        // 600x400px drawn across 6in (432pt) wide -> 100 ppi.
        let doc = doc_with_one_image(600, 400, [432.0, 0.0, 0.0, 288.0, 0.0, 0.0]);
        let page_ids: Vec<_> = doc.page_iter().collect();
        let findings = check_image_resolution(&doc, &page_ids);
        let f = findings
            .iter()
            .find(|f| f.code == codes::IMAGE_LOW_RESOLUTION)
            .expect("low-res finding");
        assert_eq!(f.severity, Severity::Warning);
        assert!(f.message.contains("100"), "{}", f.message);
        assert!(f.message.contains("300"), "{}", f.message);
        assert_eq!(f.pages, vec![1]);
    }

    #[test]
    fn excessive_resolution_image_is_a_warning() {
        // 1200x1200px drawn across 1in (72pt) -> 1200 ppi.
        let doc = doc_with_one_image(1200, 1200, [72.0, 0.0, 0.0, 72.0, 0.0, 0.0]);
        let page_ids: Vec<_> = doc.page_iter().collect();
        let findings = check_image_resolution(&doc, &page_ids);
        let f = findings
            .iter()
            .find(|f| f.code == codes::IMAGE_EXCESSIVE_RESOLUTION)
            .expect("excessive-res finding");
        assert_eq!(f.severity, Severity::Warning);
        assert!(f.message.contains("600"), "{}", f.message);
    }

    #[test]
    fn resolution_within_range_has_no_finding() {
        // 300x300px drawn across 1in (72pt) -> exactly 300 ppi.
        let doc = doc_with_one_image(300, 300, [72.0, 0.0, 0.0, 72.0, 0.0, 0.0]);
        let page_ids: Vec<_> = doc.page_iter().collect();
        let findings = check_image_resolution(&doc, &page_ids);
        assert!(findings.is_empty(), "{findings:?}");
    }

    #[test]
    fn vector_only_page_has_no_image_finding() {
        let doc = doc_with_pages(1, |_| dictionary! { "MediaBox" => mediabox(450.0, 666.0) });
        let page_ids: Vec<_> = doc.page_iter().collect();
        assert!(check_image_resolution(&doc, &page_ids).is_empty());
    }

    #[test]
    fn many_low_resolution_images_fold_into_one_finding_naming_the_worst() {
        let mut doc = lopdf::Document::with_version("1.7");
        let pages_id = doc.new_object_id();
        let mut kids = Vec::new();
        // Page 1: 600px / 6in = 100 ppi. Page 2: 600px / 12in = 50 ppi (the worst).
        for (pixel, cm_scale, page_no) in [(600i64, 432.0, 1u32), (600, 432.0 * 2.0, 2)] {
            let _ = page_no;
            let image_id = doc.add_object(Object::Stream(lopdf::Stream::new(
                image_xobject(pixel, pixel),
                vec![0u8; 4],
            )));
            let resources =
                dictionary! { "XObject" => dictionary! { "Im0" => Object::Reference(image_id) } };
            let content = format!("q {cm_scale} 0 0 {cm_scale} 0 0 cm /Im0 Do Q");
            let content_id =
                doc.add_object(lopdf::Stream::new(dictionary! {}, content.into_bytes()));
            let page_id = doc.add_object(dictionary! {
                "Type" => "Page",
                "Parent" => Object::Reference(pages_id),
                "MediaBox" => mediabox(450.0, 666.0),
                "Resources" => resources,
                "Contents" => Object::Reference(content_id),
            });
            kids.push(Object::Reference(page_id));
        }
        let pages = dictionary! { "Type" => "Pages", "Kids" => kids, "Count" => 2 };
        doc.objects.insert(pages_id, Object::Dictionary(pages));
        let catalog_id = doc.add_object(
            dictionary! { "Type" => "Catalog", "Pages" => Object::Reference(pages_id) },
        );
        doc.trailer.set("Root", Object::Reference(catalog_id));

        let page_ids: Vec<_> = doc.page_iter().collect();
        let findings = check_image_resolution(&doc, &page_ids);
        let low: Vec<_> = findings
            .iter()
            .filter(|f| f.code == codes::IMAGE_LOW_RESOLUTION)
            .collect();
        assert_eq!(
            low.len(),
            1,
            "multiple low-res images should fold into one finding"
        );
        assert!(low[0].message.contains("50"), "{}", low[0].message); // the worst of the two
        assert_eq!(low[0].pages, vec![1, 2]);
    }

    // --- colour and ink ---

    fn doc_with_page_content_stream(
        content: &[u8],
        resources: lopdf::Dictionary,
    ) -> lopdf::Document {
        let mut doc = lopdf::Document::with_version("1.7");
        let pages_id = doc.new_object_id();
        let content_id = doc.add_object(lopdf::Stream::new(dictionary! {}, content.to_vec()));
        let page_id = doc.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => Object::Reference(pages_id),
            "MediaBox" => mediabox(450.0, 666.0),
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

    #[test]
    fn total_area_coverage_over_270_percent_is_warning() {
        // C=0.9 M=0.9 Y=0.9 K=0.5 -> 320% TAC
        let doc =
            doc_with_page_content_stream(b"0.9 0.9 0.9 0.5 k 0 0 100 100 re f", dictionary! {});
        let page_ids: Vec<_> = doc.page_iter().collect();
        let findings = check_colour_and_ink(&doc, &page_ids);
        let f = findings
            .iter()
            .find(|f| f.code == codes::COLOUR_TOTAL_AREA_COVERAGE)
            .expect("TAC finding");
        assert_eq!(f.severity, Severity::Warning);
        assert!(f.message.contains("320"), "{}", f.message);
        assert_eq!(f.pages, vec![1]);
    }

    #[test]
    fn total_area_coverage_within_limit_has_no_finding() {
        let doc =
            doc_with_page_content_stream(b"0.5 0.5 0.5 0.2 k 0 0 100 100 re f", dictionary! {});
        let page_ids: Vec<_> = doc.page_iter().collect();
        assert!(!check_colour_and_ink(&doc, &page_ids)
            .iter()
            .any(|f| f.code == codes::COLOUR_TOTAL_AREA_COVERAGE));
    }

    #[test]
    fn low_tint_is_reported() {
        let doc = doc_with_page_content_stream(b"0.1 0 0 0 k 0 0 100 100 re f", dictionary! {});
        let page_ids: Vec<_> = doc.page_iter().collect();
        let findings = check_colour_and_ink(&doc, &page_ids);
        assert!(
            findings.iter().any(|f| f.message.contains("tint")),
            "{findings:?}"
        );
    }

    #[test]
    fn spot_colour_space_is_reported() {
        let resources = dictionary! { "ColorSpace" => dictionary! { "CS0" => Object::Array(vec!["Separation".into()]) } };
        let doc = doc_with_page_content_stream(b"", resources);
        let page_ids: Vec<_> = doc.page_iter().collect();
        let findings = check_colour_and_ink(&doc, &page_ids);
        assert!(
            findings.iter().any(|f| f.message.contains("Separation")),
            "{findings:?}"
        );
    }

    #[test]
    fn devicecmyk_is_not_reported_as_unsupported() {
        let resources = dictionary! { "ColorSpace" => dictionary! { "CS0" => Object::Name(b"DeviceCMYK".to_vec()) } };
        let doc = doc_with_page_content_stream(b"", resources);
        let page_ids: Vec<_> = doc.page_iter().collect();
        assert!(check_colour_and_ink(&doc, &page_ids).is_empty());
    }

    #[test]
    fn soft_mask_is_reported_as_live_transparency() {
        let resources = dictionary! { "ExtGState" => dictionary! { "GS0" => dictionary! { "SMask" => dictionary! { "Type" => "Mask" } } } };
        let doc = doc_with_page_content_stream(b"", resources);
        let page_ids: Vec<_> = doc.page_iter().collect();
        let findings = check_colour_and_ink(&doc, &page_ids);
        assert!(
            findings.iter().any(|f| f.message.contains("transparency")),
            "{findings:?}"
        );
    }

    #[test]
    fn non_normal_blend_mode_is_reported() {
        let resources = dictionary! { "ExtGState" => dictionary! { "GS0" => dictionary! { "BM" => "Multiply" } } };
        let doc = doc_with_page_content_stream(b"", resources);
        let page_ids: Vec<_> = doc.page_iter().collect();
        let findings = check_colour_and_ink(&doc, &page_ids);
        assert!(
            findings.iter().any(|f| f.message.contains("transparency")),
            "{findings:?}"
        );
    }

    #[test]
    fn optional_content_properties_are_reported() {
        let doc = doc_with_catalog_entries(
            dictionary! { "OCProperties" => dictionary! { "OCGs" => Vec::<Object>::new() } },
            dictionary! {},
        );
        let page_ids: Vec<_> = doc.page_iter().collect();
        let findings = check_colour_and_ink(&doc, &page_ids);
        assert!(
            findings
                .iter()
                .any(|f| f.message.to_lowercase().contains("layer")),
            "{findings:?}"
        );
    }

    #[test]
    fn clean_colour_has_no_findings() {
        // All channels >= 20% (no tint warning) and total < 270% (no TAC warning).
        let doc =
            doc_with_page_content_stream(b"0.3 0.3 0.3 0.3 k 0 0 100 100 re f", dictionary! {});
        let page_ids: Vec<_> = doc.page_iter().collect();
        assert!(check_colour_and_ink(&doc, &page_ids).is_empty());
    }

    fn sku() -> &'static CatalogEntry {
        catalog::lookup("0600X0900.BW.STD.PB.060UW444.MXX").unwrap()
    }

    #[test]
    fn preflight_cover_rejects_wrong_canvas_size_but_not_low_page_count() {
        let doc = doc_with_pages(1, |_| dictionary! { "MediaBox" => mediabox(432.0, 648.0) });
        let mut bytes = Vec::new();
        doc.clone().save_to(&mut bytes).unwrap();

        let expected_canvas = Size::new(Length::from_inches(12.0), Length::from_inches(9.25));
        let report = preflight_cover(&bytes, sku(), expected_canvas);

        assert!(
            report
                .findings
                .iter()
                .any(|f| f.code == codes::GEOMETRY_PAGE_SIZE_MISMATCH),
            "{:?}",
            report.findings
        );
        assert!(
            !report
                .findings
                .iter()
                .any(|f| f.code.contains("page-count")),
            "a cover has no interior page-count minimum: {:?}",
            report.findings
        );
    }

    #[test]
    fn preflight_cover_rejects_more_than_one_page() {
        let doc = doc_with_pages(2, |_| dictionary! { "MediaBox" => mediabox(432.0, 648.0) });
        let mut bytes = Vec::new();
        doc.clone().save_to(&mut bytes).unwrap();

        let report = preflight_cover(
            &bytes,
            sku(),
            Size::new(Length::from_inches(12.0), Length::from_inches(9.25)),
        );
        assert!(report
            .findings
            .iter()
            .any(|f| f.code == codes::COVER_WRONG_PAGE_COUNT));
    }

    #[test]
    fn preflight_cover_accepts_matching_canvas_size() {
        let canvas = Size::new(Length::from_inches(12.787), Length::from_inches(9.25));
        let doc = doc_with_pages(1, |_| {
            dictionary! { "MediaBox" => mediabox(canvas.width.as_points(), canvas.height.as_points()) }
        });
        let mut bytes = Vec::new();
        doc.clone().save_to(&mut bytes).unwrap();

        let report = preflight_cover(&bytes, sku(), canvas);
        assert!(
            !report
                .findings
                .iter()
                .any(|f| f.code == codes::GEOMETRY_PAGE_SIZE_MISMATCH),
            "{:?}",
            report.findings
        );
    }

    // --- checks seeing through form XObjects (openspec harden-pdf-correctness, group 2) ---

    /// A one-page document whose content is just `/Fm0 Do`, invoking a form
    /// XObject built from `form_content` and `build_form_resources` — the
    /// page's own resources reference nothing but the form itself, so any
    /// finding a check produces can only have come from looking inside the
    /// form.
    fn doc_with_page_drawing_a_form(
        form_content: &[u8],
        build_form_resources: impl FnOnce(&mut lopdf::Document) -> lopdf::Dictionary,
    ) -> lopdf::Document {
        let mut doc = lopdf::Document::with_version("1.7");
        let form_resources = build_form_resources(&mut doc);
        let pages_id = doc.new_object_id();
        let form_id = doc.add_object(lopdf::Stream::new(
            dictionary! { "Type" => "XObject", "Subtype" => "Form", "Resources" => form_resources },
            form_content.to_vec(),
        ));
        let page_resources =
            dictionary! { "XObject" => dictionary! { "Fm0" => Object::Reference(form_id) } };
        let content_id = doc.add_object(lopdf::Stream::new(dictionary! {}, b"/Fm0 Do".to_vec()));
        let page_id = doc.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => Object::Reference(pages_id),
            "MediaBox" => mediabox(450.0, 666.0),
            "Resources" => page_resources,
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

    #[test]
    fn font_inside_a_form_xobject_is_found() {
        let doc = doc_with_page_drawing_a_form(b"BT /F1 12 Tf ET", |doc| {
            let font_id = doc.add_object(dictionary! {
                "Type" => "Font",
                "Subtype" => "Type1",
                "BaseFont" => "Helvetica",
            });
            dictionary! { "Font" => dictionary! { "F1" => Object::Reference(font_id) } }
        });
        let page_ids: Vec<_> = doc.page_iter().collect();
        let findings = check_font_embedding(&doc, &page_ids);
        assert_eq!(findings.len(), 1, "{findings:?}");
        assert_eq!(findings[0].code, codes::FONTS_NOT_EMBEDDED);
        assert!(
            findings[0].message.contains("Helvetica"),
            "{}",
            findings[0].message
        );
        assert_eq!(findings[0].pages, vec![1]);
    }

    #[test]
    fn embedded_font_inside_a_form_xobject_is_not_flagged() {
        let doc = doc_with_page_drawing_a_form(b"BT /F1 12 Tf ET", |doc| {
            let font_file_id = doc.add_object(lopdf::Stream::new(dictionary! {}, vec![0u8; 4]));
            let descriptor_id = doc.add_object(dictionary! {
                "Type" => "FontDescriptor",
                "FontFile2" => Object::Reference(font_file_id),
            });
            let font_id = doc.add_object(dictionary! {
                "Type" => "Font",
                "Subtype" => "TrueType",
                "BaseFont" => "ABCDEF+Minion-Regular",
                "FontDescriptor" => Object::Reference(descriptor_id),
            });
            dictionary! { "Font" => dictionary! { "F1" => Object::Reference(font_id) } }
        });
        let page_ids: Vec<_> = doc.page_iter().collect();
        assert!(check_font_embedding(&doc, &page_ids).is_empty());
    }

    #[test]
    fn colour_inside_a_form_xobject_is_reported() {
        // C=0.9 M=0.9 Y=0.9 K=0.5 -> 320% TAC, set only inside the form.
        let doc =
            doc_with_page_drawing_a_form(b"0.9 0.9 0.9 0.5 k 0 0 100 100 re f", |_| dictionary! {});
        let page_ids: Vec<_> = doc.page_iter().collect();
        let findings = check_colour_and_ink(&doc, &page_ids);
        let f = findings
            .iter()
            .find(|f| f.code == codes::COLOUR_TOTAL_AREA_COVERAGE)
            .expect("TAC finding from inside the form");
        assert_eq!(f.pages, vec![1]);
    }

    #[test]
    fn low_tint_uses_its_own_code_not_unsupported_space() {
        let doc = doc_with_page_content_stream(b"0.1 0 0 0 k 0 0 100 100 re f", dictionary! {});
        let page_ids: Vec<_> = doc.page_iter().collect();
        let findings = check_colour_and_ink(&doc, &page_ids);
        assert!(
            findings.iter().any(|f| f.code == codes::COLOUR_LOW_TINT),
            "{findings:?}"
        );
        assert!(
            !findings
                .iter()
                .any(|f| f.code == codes::COLOUR_UNSUPPORTED_SPACE),
            "{findings:?}"
        );
    }

    // --- resource-name resolution (task 2.4) ---

    #[test]
    fn content_naming_an_unresolvable_font_is_a_blocking_finding() {
        let doc = doc_with_page_content_stream(b"BT /F1 12 Tf ET", dictionary! {});
        let page_ids: Vec<_> = doc.page_iter().collect();
        let findings = check_resource_references(&doc, &page_ids);
        let f = findings
            .iter()
            .find(|f| f.code == codes::RESOURCES_MISSING_REFERENCE)
            .expect("missing-reference finding");
        assert_eq!(f.severity, Severity::Blocking);
        assert!(f.message.contains("/F1"), "{}", f.message);
        assert_eq!(f.pages, vec![1]);
    }

    #[test]
    fn content_naming_an_unresolvable_image_inside_a_form_is_reported() {
        let doc = doc_with_page_drawing_a_form(b"/Im0 Do", |_| dictionary! {});
        let page_ids: Vec<_> = doc.page_iter().collect();
        let findings = check_resource_references(&doc, &page_ids);
        assert!(
            findings
                .iter()
                .any(|f| f.code == codes::RESOURCES_MISSING_REFERENCE
                    && f.message.contains("/Im0")),
            "{findings:?}"
        );
    }

    #[test]
    fn a_genuinely_blank_page_is_not_flagged_by_resource_reference_check() {
        let doc = doc_with_page_content_stream(b"", dictionary! {});
        let page_ids: Vec<_> = doc.page_iter().collect();
        assert!(check_resource_references(&doc, &page_ids).is_empty());
    }

    #[test]
    fn resolvable_font_reference_has_no_finding() {
        let doc = doc_with_embedded_font();
        let page_ids: Vec<_> = doc.page_iter().collect();
        assert!(check_resource_references(&doc, &page_ids).is_empty());
    }

    #[test]
    fn unresolved_resources_dictionary_is_a_blocking_finding() {
        let mut doc = lopdf::Document::with_version("1.7");
        let pages_id = doc.new_object_id();
        let bogus_ref = (9999, 0);
        let page_id = doc.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => Object::Reference(pages_id),
            "MediaBox" => mediabox(450.0, 666.0),
            "Resources" => Object::Reference(bogus_ref),
        });
        let pages = dictionary! { "Type" => "Pages", "Kids" => vec![Object::Reference(page_id)], "Count" => 1 };
        doc.objects.insert(pages_id, Object::Dictionary(pages));
        let catalog_id = doc.add_object(
            dictionary! { "Type" => "Catalog", "Pages" => Object::Reference(pages_id) },
        );
        doc.trailer.set("Root", Object::Reference(catalog_id));

        let page_ids: Vec<_> = doc.page_iter().collect();
        let findings = check_resource_references(&doc, &page_ids);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].code, codes::GEOMETRY_UNRESOLVABLE_RESOURCES);
        assert_eq!(findings[0].severity, Severity::Blocking);
    }

    #[test]
    fn traversal_budget_exceeded_is_reported_as_a_blocking_finding() {
        // Exercises the real MAX_WALK_OPERATIONS constant end-to-end;
        // ctm_walk's own tests exercise the mechanism itself with a tiny
        // budget so this one doesn't need to (kept here to prove the
        // finding is actually wired up in preflight, not just in ctm_walk).
        let op_count = crate::ctm_walk::MAX_WALK_OPERATIONS + 10;
        let content = "0 0 m ".repeat(op_count);
        let doc = doc_with_page_content_stream(content.as_bytes(), dictionary! {});
        let page_ids: Vec<_> = doc.page_iter().collect();
        let findings = check_resource_references(&doc, &page_ids);
        assert!(
            findings
                .iter()
                .any(|f| f.code == codes::STRUCTURE_TRAVERSAL_BUDGET_EXCEEDED
                    && f.severity == Severity::Blocking),
            "{findings:?}"
        );
    }

    // --- unresolvable page geometry (task 2.5) ---

    #[test]
    fn unresolvable_page_box_is_reported_not_silently_skipped() {
        let mut doc = lopdf::Document::with_version("1.7");
        let pages_id = doc.new_object_id();
        let bogus_ref = (9999, 0);
        let page_id = doc.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => Object::Reference(pages_id),
            "MediaBox" => Object::Reference(bogus_ref),
        });
        let pages = dictionary! { "Type" => "Pages", "Kids" => vec![Object::Reference(page_id)], "Count" => 1 };
        doc.objects.insert(pages_id, Object::Dictionary(pages));
        let catalog_id = doc.add_object(
            dictionary! { "Type" => "Catalog", "Pages" => Object::Reference(pages_id) },
        );
        doc.trailer.set("Root", Object::Reference(catalog_id));

        let page_ids: Vec<_> = doc.page_iter().collect();
        let findings = check_page_geometry_resolution(&doc, &page_ids);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].code, codes::GEOMETRY_UNREADABLE_PAGE_BOX);
        assert_eq!(findings[0].severity, Severity::Blocking);
        assert_eq!(findings[0].pages, vec![1]);

        // The page must not silently vanish from the other geometry checks —
        // they don't crash and don't claim it matches anything, but the
        // finding above is what makes its absence from these not silent.
        assert!(check_page_size_matches_target(&doc, &page_ids, required_6x9()).is_empty());
        assert!(check_mixed_page_sizes(&doc, &page_ids).is_empty());
    }

    #[test]
    fn resolvable_page_box_has_no_geometry_resolution_finding() {
        let doc = doc_with_pages(1, |_| dictionary! { "MediaBox" => mediabox(450.0, 666.0) });
        let page_ids: Vec<_> = doc.page_iter().collect();
        assert!(check_page_geometry_resolution(&doc, &page_ids).is_empty());
    }

    // --- unreadable / non-90-multiple rotation (task 2.6) ---

    #[test]
    fn unreadable_rotation_is_a_blocking_finding() {
        let mut doc = lopdf::Document::with_version("1.7");
        let pages_id = doc.new_object_id();
        let bogus_ref = (9999, 0);
        let page_id = doc.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => Object::Reference(pages_id),
            "MediaBox" => mediabox(450.0, 666.0),
            "Rotate" => Object::Reference(bogus_ref),
        });
        let pages = dictionary! { "Type" => "Pages", "Kids" => vec![Object::Reference(page_id)], "Count" => 1 };
        doc.objects.insert(pages_id, Object::Dictionary(pages));
        let catalog_id = doc.add_object(
            dictionary! { "Type" => "Catalog", "Pages" => Object::Reference(pages_id) },
        );
        doc.trailer.set("Root", Object::Reference(catalog_id));

        let page_ids: Vec<_> = doc.page_iter().collect();
        let findings = check_page_rotation(&doc, &page_ids);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].code, codes::GEOMETRY_UNREADABLE_ROTATION);
        assert_eq!(findings[0].severity, Severity::Blocking);
    }

    #[test]
    fn rotation_not_a_multiple_of_90_is_a_blocking_finding() {
        let doc = doc_with_pages(
            1,
            |_| dictionary! { "MediaBox" => mediabox(450.0, 666.0), "Rotate" => 45 },
        );
        let page_ids: Vec<_> = doc.page_iter().collect();
        let findings = check_page_rotation(&doc, &page_ids);
        assert_eq!(findings.len(), 1);
        assert_eq!(
            findings[0].code,
            codes::GEOMETRY_ROTATION_NOT_MULTIPLE_OF_90
        );
        assert!(findings[0].message.contains('4'), "{}", findings[0].message);
    }

    #[test]
    fn normal_rotation_has_no_rotation_finding() {
        let doc = doc_with_pages(
            1,
            |_| dictionary! { "MediaBox" => mediabox(450.0, 666.0), "Rotate" => 90 },
        );
        let page_ids: Vec<_> = doc.page_iter().collect();
        assert!(check_page_rotation(&doc, &page_ids).is_empty());
    }

    #[test]
    fn absent_rotation_has_no_rotation_finding() {
        let doc = doc_with_pages(1, |_| dictionary! { "MediaBox" => mediabox(450.0, 666.0) });
        let page_ids: Vec<_> = doc.page_iter().collect();
        assert!(check_page_rotation(&doc, &page_ids).is_empty());
    }

    // --- interior safety margin (task 2.8) ---
    //
    // These cover raster-image placement only, via ctm_walk's existing CTM
    // tracking — see `check_interior_safety_margin`'s doc comment for why
    // text and vector-path marks are out of scope for this pass.

    #[test]
    fn image_stopping_short_inside_the_safety_margin_is_a_warning() {
        // Page own box [0,0,450,666]; trim (bleed 9pt in) = [9,9,441,657];
        // safe area (further 36pt in) = [45,45,405,621]. Place a 20x20pt
        // image at (15,15): inside the trim rect (doesn't bleed) but its
        // corner at (15,15) falls inside the safety-margin band.
        let doc = doc_with_one_image(300, 300, [20.0, 0.0, 0.0, 20.0, 15.0, 15.0]);
        let page_ids: Vec<_> = doc.page_iter().collect();
        let findings = check_interior_safety_margin(&doc, &page_ids);
        assert_eq!(findings.len(), 1, "{findings:?}");
        assert_eq!(
            findings[0].code,
            codes::GEOMETRY_CONTENT_INSIDE_SAFETY_MARGIN
        );
        assert_eq!(findings[0].severity, Severity::Warning);
        assert_eq!(findings[0].pages, vec![1]);
    }

    #[test]
    fn image_bleeding_past_the_trim_edge_is_not_flagged() {
        // Starts at the page's own edge (0,0) and crosses the trim edge (9pt
        // in) — an intentional bleed, not a safety-margin violation.
        let doc = doc_with_one_image(300, 300, [60.0, 0.0, 0.0, 60.0, 0.0, 0.0]);
        let page_ids: Vec<_> = doc.page_iter().collect();
        assert!(check_interior_safety_margin(&doc, &page_ids).is_empty());
    }

    #[test]
    fn image_fully_inside_the_safe_area_is_not_flagged() {
        // Occupies [100,150]x[100,150], well inside the [45,405]x[45,621] safe rect.
        let doc = doc_with_one_image(300, 300, [50.0, 0.0, 0.0, 50.0, 100.0, 100.0]);
        let page_ids: Vec<_> = doc.page_iter().collect();
        assert!(check_interior_safety_margin(&doc, &page_ids).is_empty());
    }

    #[test]
    fn interior_safety_margin_matches_geometrys_published_value() {
        // Pins `check_interior_safety_margin` to the same 0.5in value
        // `geometry::interior_safety_margin` documents and is itself tested
        // against, so the two can't silently drift apart.
        assert_eq!(crate::geometry::interior_safety_margin().as_inches(), 0.5);
    }
}
