//! Read-only inspection of a PDF against a target Lulu product: page geometry,
//! font embedding, page count, and (later) image resolution, colour, and
//! structural checks — all producing a [`crate::report::Report`], never
//! modifying the input.

use crate::catalog::CatalogEntry;
use crate::geometry::PageCountRules;
use crate::pdf;
use crate::report::{
    codes, DetectedTool, Finding, Report, Severity, StageLogEntry, SCHEMA_VERSION,
};
use crate::units::{Length, Size};
use lopdf::{Document, Object, ObjectId};
use std::collections::BTreeMap;

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

fn font_descriptor_is_embedded(doc: &Document, descriptor: &lopdf::Dictionary) -> bool {
    for key in [&b"FontFile"[..], b"FontFile2", b"FontFile3"] {
        if descriptor.get(key).is_ok() {
            return true;
        }
    }
    let _ = doc; // reserved for a future indirect-reference resolution if needed
    false
}

/// Every font referenced by the document must be fully embedded — Lulu's
/// file validation rejects an interior with any unembedded font, including
/// the standard 14 base fonts (which have no embedded file by definition).
pub fn check_font_embedding(doc: &Document, page_ids: &[ObjectId]) -> Vec<Finding> {
    let mut not_embedded: BTreeMap<String, Vec<u32>> = BTreeMap::new();

    for (i, &page_id) in page_ids.iter().enumerate() {
        let page_number = (i + 1) as u32;
        let Ok(fonts) = doc.get_page_fonts(page_id) else {
            continue;
        };
        for font_dict in fonts.values() {
            let base_font = font_dict
                .get(b"BaseFont")
                .and_then(|o| o.as_name())
                .map(|n| String::from_utf8_lossy(n).to_string())
                .unwrap_or_else(|_| "(unnamed font)".to_string());

            let embedded =
                if font_dict.get(b"Subtype").and_then(|o| o.as_name()).ok() == Some(b"Type0") {
                    font_dict
                        .get(b"DescendantFonts")
                        .ok()
                        .and_then(|o| o.as_array().ok())
                        .and_then(|arr| arr.first())
                        .and_then(|o| o.as_reference().ok())
                        .and_then(|id| doc.get_dictionary(id).ok())
                        .and_then(|descendant| descendant.get(b"FontDescriptor").ok())
                        .and_then(|o| o.as_reference().ok())
                        .and_then(|id| doc.get_dictionary(id).ok())
                        .is_some_and(|descriptor| font_descriptor_is_embedded(doc, descriptor))
                } else {
                    font_dict
                        .get(b"FontDescriptor")
                        .ok()
                        .and_then(|o| o.as_reference().ok())
                        .and_then(|id| doc.get_dictionary(id).ok())
                        .is_some_and(|descriptor| font_descriptor_is_embedded(doc, descriptor))
                };

            if !embedded {
                not_embedded.entry(base_font).or_default().push(page_number);
            }
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
    }
}

/// Names dictionary entries that carry no print meaning and that Lulu's
/// pipeline has no use for, keyed by the human-readable label used in the
/// finding message.
const REPORTABLE_NAME_TREES: &[(&[u8], &str)] = &[
    (b"JavaScript", "document-level JavaScript"),
    (b"EmbeddedFiles", "embedded file(s)"),
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

        // Document-level JavaScript and embedded files live under /Names in the catalog.
        if let Ok(names) = catalog.get(b"Names").and_then(|o| o.as_dict()) {
            for (key, label) in REPORTABLE_NAME_TREES {
                if names.get(key).is_ok() {
                    findings.push(
                        Finding::new(
                            format!("structure.{}", String::from_utf8_lossy(key).to_lowercase()),
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
/// Scoped to the page's own content stream — colour set *inside* a form
/// XObject's content is not inspected. Lulu's own normalizer is the
/// authoritative check for colour and ink; this exists to catch the common
/// cases before upload, not to replace it.
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
        let content_bytes = doc.get_page_content(page_id);
        let Ok(content) = lopdf::content::Content::decode(&content_bytes) else {
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

        if let Ok((Some(resources), _)) = doc.get_page_resources(page_id) {
            if let Ok(ext_g_states) = resources.get(b"ExtGState").and_then(|o| o.as_dict()) {
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
            if let Ok(color_spaces) = resources.get(b"ColorSpace").and_then(|o| o.as_dict()) {
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
                codes::COLOUR_UNSUPPORTED_SPACE,
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
                "structure.live-transparency",
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
                Finding::new("structure.optional-content", Severity::Warning, "the document declares optional content (layers), which must be flattened before printing".to_string()).fixable(true),
            );
        }
    }

    findings
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
                    "document.parse-failed",
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
    findings.extend(check_font_embedding(&doc, &page_ids));
    findings.extend(check_structure(&doc, &page_ids));
    findings.extend(check_image_resolution(&doc, &page_ids));
    findings.extend(check_colour_and_ink(&doc, &page_ids));

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
                    "document.parse-failed",
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
            "cover.wrong-page-count",
            Severity::Blocking,
            format!("a cover file must be exactly one page; this file has {page_count}"),
        ));
    }

    findings.extend(check_page_size_matches_target(
        &doc,
        &page_ids,
        expected_canvas,
    ));
    findings.extend(check_font_embedding(&doc, &page_ids));
    findings.extend(check_structure(&doc, &page_ids));
    findings.extend(check_image_resolution(&doc, &page_ids));
    findings.extend(check_colour_and_ink(&doc, &page_ids));

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
            .any(|f| f.code == "cover.wrong-page-count"));
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
}
