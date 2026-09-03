//! A read-only walk of a page's content stream, tracking the current
//! transformation matrix (CTM) through `cm`/`q`/`Q` and descending into form
//! XObjects, so callers can learn where on the page each image XObject is
//! actually drawn — and, via [`LayerVisitor`]/[`collect_page_layers`], what
//! resources and content every nested form carries, so a font, colour
//! operator, or resource reference set only inside a form is visible to a
//! caller exactly as if it were on the page itself. Confined to preflight: a
//! bug here degrades a finding, it never corrupts output, since nothing here
//! writes to the document.
//!
//! Both entry points share one traversal (`walk`, below) rather than two
//! that could disagree about which forms exist or how deep they nest — see
//! `design.md`'s "Preflight gains a form-XObject-aware mode rather than a
//! second implementation".

use crate::pdf::effective_page_resources;
use crate::units::Matrix;
use lopdf::content::{Content, Operation};
use lopdf::{Dictionary, Document, Object, ObjectId};

fn as_f64(o: &Object) -> Option<f64> {
    match o {
        Object::Integer(n) => Some(*n as f64),
        Object::Real(n) => Some(*n as f64),
        _ => None,
    }
}

fn matrix_from_cm_operands(operands: &[Object]) -> Option<Matrix> {
    if operands.len() != 6 {
        return None;
    }
    let v: Vec<f64> = operands.iter().map(as_f64).collect::<Option<Vec<_>>>()?;
    Some(Matrix {
        a: v[0],
        b: v[1],
        c: v[2],
        d: v[3],
        e: v[4],
        f: v[5],
    })
}

/// Called once for every image XObject `Do` invocation encountered, with the
/// CTM in effect at that draw site and the image XObject's own dictionary.
pub trait ImageVisitor {
    fn visit_image(&mut self, ctm: Matrix, image_dict: &Dictionary, image_id: ObjectId);
}

impl<F: FnMut(Matrix, &Dictionary, ObjectId)> ImageVisitor for F {
    fn visit_image(&mut self, ctm: Matrix, image_dict: &Dictionary, image_id: ObjectId) {
        self(ctm, image_dict, image_id)
    }
}

/// Called once for the page's own content stream (`form_id: None`), and once
/// more for every form XObject the page draws — directly, or transitively
/// through another form — up to the same depth and operation budget as image
/// discovery. Given the resources and (already-decoded) content bytes in
/// effect for that layer.
///
/// This is what lets preflight's font-embedding, colour, and resource-name
/// checks see through nesting: they implement this trait once and get the
/// page's own content plus every nested form's, from a single walk, rather
/// than needing their own descent into form XObjects.
pub trait LayerVisitor {
    fn visit_layer(&mut self, resources: &Dictionary, content: &[u8], form_id: Option<ObjectId>);
}

impl<F: FnMut(&Dictionary, &[u8], Option<ObjectId>)> LayerVisitor for F {
    fn visit_layer(&mut self, resources: &Dictionary, content: &[u8], form_id: Option<ObjectId>) {
        self(resources, content, form_id)
    }
}

/// Maximum form-XObject recursion depth, guarding against a (malformed or
/// adversarial) form that references itself.
const MAX_FORM_DEPTH: u32 = 32;

/// Maximum total content-stream operations (summed across the page's own
/// content and every nested form, at every depth) a single walk will
/// examine before giving up. `MAX_FORM_DEPTH` alone bounds recursion depth
/// but not total work: a file with N levels of forms, each invoking two more
/// forms, is 2^N walks from a handful of bytes, which is a CPU-exhaustion
/// risk from a small, legally-shaped input.
///
/// 300,000 is chosen to be generous for any real book while still bounding
/// that adversarial case: a heavily illustrated interior plausibly reaches a
/// few hundred operations per page for vector art plus one `Do` per image,
/// so a walk covering even a 1,000+ page interior in a single pass (this
/// crate walks one page's tree at a time, so in practice the operative
/// bound is per-page, not per-document) stays well under this ceiling,
/// while an adversarial file trying to multiply a small input into billions
/// of apparent operations hits the budget almost immediately relative to
/// its claimed structure. Tune this in light of what a large real-world
/// fixture in the test corpus actually needs once one exists (see
/// `openspec/changes/harden-pdf-correctness/tasks.md` group 9).
pub const MAX_WALK_OPERATIONS: usize = 300_000;

/// How a walk over a page's content and nested form XObjects finished.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WalkOutcome {
    /// Every reachable form XObject (bounded by [`MAX_FORM_DEPTH`]) was
    /// examined without exhausting [`MAX_WALK_OPERATIONS`].
    Completed,
    /// The walk stopped early because it examined
    /// [`MAX_WALK_OPERATIONS`] content-stream operations without finishing —
    /// the file's nested form XObjects are large or deep enough that
    /// finishing would risk denial-of-service. Whatever a visitor already
    /// saw is genuine, but the walk is incomplete: a caller MUST treat this
    /// as a blocking finding ("checks are incomplete"), never as a silent
    /// truncation that a clean report could follow.
    BudgetExceeded,
}

/// One examined content stream and its effective resources: either a page's
/// own content and resources (`form_id: None`), or one form XObject it draws
/// — directly, or transitively through another form (`form_id: Some`, the
/// form's own object id). Owned, so a caller can run several checks (font
/// embedding, colour, resource-name resolution) against the same walk
/// without invoking [`collect_page_layers`] once per check.
#[derive(Debug, Clone)]
pub struct ContentLayer {
    pub resources: Dictionary,
    pub content: Vec<u8>,
    pub form_id: Option<ObjectId>,
}

/// `object`, dereferenced one level if it is an [`Object::Reference`], as a
/// dictionary — used for a form XObject's own `/Resources`, which (like a
/// page's) is legally either a direct dictionary or an indirect reference to
/// one.
fn resolve_dict<'a>(doc: &'a Document, object: &'a Object) -> Option<&'a Dictionary> {
    match object {
        Object::Reference(id) => doc.get_dictionary(*id).ok(),
        Object::Dictionary(d) => Some(d),
        _ => None,
    }
}

fn xobject_dict<'a>(
    doc: &'a Document,
    resources: &Dictionary,
    name: &[u8],
) -> Option<(&'a Dictionary, ObjectId)> {
    let xobjects = resources.get(b"XObject").ok()?.as_dict().ok().or_else(|| {
        resources
            .get(b"XObject")
            .ok()
            .and_then(|o| o.as_reference().ok())
            .and_then(|id| doc.get_dictionary(id).ok())
    })?;
    let object_ref = xobjects.get(name).ok()?.as_reference().ok()?;
    let dict = match doc.get_object(object_ref).ok()? {
        Object::Stream(s) => &s.dict,
        Object::Dictionary(d) => d,
        _ => return None,
    };
    Some((dict, object_ref))
}

/// Shared, mutable state threaded through every recursive [`walk`] call: the
/// document being walked (read-only), and the remaining operation budget —
/// see [`MAX_WALK_OPERATIONS`].
struct WalkCtx<'a> {
    doc: &'a Document,
    budget: usize,
    exceeded: bool,
}

/// Explicit, fresh reborrow of an `Option<&mut dyn ImageVisitor>` — written
/// out by hand rather than via `Option::as_deref_mut`, because that generic
/// combinator (going through `DerefMut`'s associated type) does not give the
/// borrow checker a short-enough-lived reborrow across a loop's iterations
/// or into a recursive call in this shape, and reports a spurious "borrowed
/// more than once" error. A plain function reborrowing manually does not
/// have that limitation.
fn reborrow_image<'s>(
    visitor: &'s mut Option<&mut dyn ImageVisitor>,
) -> Option<&'s mut dyn ImageVisitor> {
    match visitor {
        Some(v) => Some(&mut **v),
        None => None,
    }
}

/// [`reborrow_image`]'s counterpart for [`LayerVisitor`].
fn reborrow_layer<'s>(
    visitor: &'s mut Option<&mut dyn LayerVisitor>,
) -> Option<&'s mut dyn LayerVisitor> {
    match visitor {
        Some(v) => Some(&mut **v),
        None => None,
    }
}

#[allow(clippy::too_many_arguments)]
fn walk(
    ctx: &mut WalkCtx,
    content_bytes: &[u8],
    resources: &Dictionary,
    ctm: Matrix,
    depth: u32,
    form_id: Option<ObjectId>,
    mut image_visitor: Option<&mut dyn ImageVisitor>,
    mut layer_visitor: Option<&mut dyn LayerVisitor>,
) {
    if depth > MAX_FORM_DEPTH || ctx.exceeded {
        return;
    }
    let Ok(content) = Content::decode(content_bytes) else {
        return;
    };

    if let Some(lv) = reborrow_layer(&mut layer_visitor) {
        lv.visit_layer(resources, content_bytes, form_id);
    }

    let mut stack: Vec<Matrix> = Vec::new();
    let mut current = ctm;

    for Operation { operator, operands } in content.operations.iter() {
        if ctx.budget == 0 {
            ctx.exceeded = true;
            return;
        }
        ctx.budget -= 1;

        match operator.as_str() {
            "q" => stack.push(current),
            "Q" => {
                if let Some(m) = stack.pop() {
                    current = m;
                }
            }
            "cm" => {
                if let Some(m) = matrix_from_cm_operands(operands) {
                    current = m.then(current);
                }
            }
            "Do" => {
                let Some(Object::Name(name)) = operands.first() else {
                    continue;
                };
                let Some((xdict, xid)) = xobject_dict(ctx.doc, resources, name) else {
                    continue;
                };
                let subtype = xdict.get(b"Subtype").and_then(|o| o.as_name()).ok();
                match subtype {
                    Some(b"Image") => {
                        if let Some(iv) = reborrow_image(&mut image_visitor) {
                            iv.visit_image(current, xdict, xid);
                        }
                    }
                    Some(b"Form") => {
                        let form_matrix = xdict
                            .get(b"Matrix")
                            .ok()
                            .and_then(|o| o.as_array().ok())
                            .and_then(|arr| matrix_from_cm_operands(arr))
                            .unwrap_or(Matrix::IDENTITY);
                        let form_ctm = form_matrix.then(current);
                        // A form with no /Resources of its own uses the
                        // resources in effect where it is invoked (the
                        // page's, or an enclosing form's) — PDF spec,
                        // "Form Dictionaries". Resolved through one level of
                        // indirection either way, since /Resources is
                        // legally a direct dict or a reference to one.
                        let form_resources = xdict
                            .get(b"Resources")
                            .ok()
                            .and_then(|o| resolve_dict(ctx.doc, o))
                            .unwrap_or(resources);
                        if let Ok(Object::Stream(stream)) = ctx.doc.get_object(xid) {
                            if let Ok(bytes) = stream.get_plain_content() {
                                walk(
                                    ctx,
                                    &bytes,
                                    form_resources,
                                    form_ctm,
                                    depth + 1,
                                    Some(xid),
                                    reborrow_image(&mut image_visitor),
                                    reborrow_layer(&mut layer_visitor),
                                );
                                if ctx.exceeded {
                                    return;
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }
}

/// Core entry point both public functions below delegate to, parameterized
/// on the operation budget so tests can exercise [`WalkOutcome::BudgetExceeded`]
/// without generating a multi-hundred-thousand-operation fixture.
fn run_walk(
    doc: &Document,
    page_id: ObjectId,
    budget: usize,
    image_visitor: Option<&mut dyn ImageVisitor>,
    layer_visitor: Option<&mut dyn LayerVisitor>,
) -> WalkOutcome {
    // A page whose effective resources can't be resolved at all (a broken
    // /Resources reference) is a distinct defect that a caller needing to
    // report it should detect directly via `pdf::effective_page_resources`
    // — there is nothing for a content walk to see in that case, so this
    // just walks nothing rather than guessing at a fallback.
    let Ok(resources) = effective_page_resources(doc, page_id) else {
        return WalkOutcome::Completed;
    };
    let content_bytes = doc.get_page_content(page_id);
    let mut ctx = WalkCtx {
        doc,
        budget,
        exceeded: false,
    };
    walk(
        &mut ctx,
        &content_bytes,
        &resources,
        Matrix::IDENTITY,
        0,
        None,
        image_visitor,
        layer_visitor,
    );
    if ctx.exceeded {
        WalkOutcome::BudgetExceeded
    } else {
        WalkOutcome::Completed
    }
}

/// Walk a page's content stream (and any form XObjects it invokes), calling
/// `visitor` for every image `Do` with the CTM at that draw site.
pub fn walk_page_images(
    doc: &Document,
    page_id: ObjectId,
    visitor: &mut dyn ImageVisitor,
) -> WalkOutcome {
    run_walk(doc, page_id, MAX_WALK_OPERATIONS, Some(visitor), None)
}

struct LayerCollector {
    layers: Vec<ContentLayer>,
}

impl LayerVisitor for LayerCollector {
    fn visit_layer(&mut self, resources: &Dictionary, content: &[u8], form_id: Option<ObjectId>) {
        self.layers.push(ContentLayer {
            resources: resources.clone(),
            content: content.to_vec(),
            form_id,
        });
    }
}

/// Collects every [`ContentLayer`] for a page — its own, plus every form
/// XObject it draws to whatever depth and operation budget the walk allows
/// — in one pass. This is the traversal preflight's font-embedding, colour,
/// and resource-name checks share, so a font or colour operator set only
/// inside a nested form XObject is examined exactly as if it were on the
/// page itself, and all three checks agree on what "the page's content"
/// means.
pub fn collect_page_layers(doc: &Document, page_id: ObjectId) -> (Vec<ContentLayer>, WalkOutcome) {
    let mut collector = LayerCollector { layers: Vec::new() };
    let outcome = run_walk(
        doc,
        page_id,
        MAX_WALK_OPERATIONS,
        None,
        Some(&mut collector),
    );
    (collector.layers, outcome)
}

/// The length, in points, of the two edges of the unit square as transformed
/// by `ctm` — i.e. the size an image `Do`'d under this CTM is actually drawn
/// at. Correct for axis-aligned and rotated placement; an approximation
/// (edge length rather than true parallelogram area) under shear.
pub fn drawn_size_points(ctm: Matrix) -> (f64, f64) {
    let width = (ctm.a * ctm.a + ctm.b * ctm.b).sqrt();
    let height = (ctm.c * ctm.c + ctm.d * ctm.d).sqrt();
    (width, height)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::units::Length;
    use lopdf::{dictionary, Stream};

    /// Builds a one-page document whose content stream and resources come
    /// from `build`, which receives the `Document` so any XObjects it needs
    /// (images, forms) are allocated in the *same* id space as the page —
    /// two separately-constructed `Document`s both number objects from
    /// `(1, 0)`, so merging their object maps silently collides ids.
    fn doc_with_page_content(
        content: &[u8],
        build_resources: impl FnOnce(&mut Document) -> Dictionary,
    ) -> (Document, ObjectId) {
        let mut doc = Document::with_version("1.7");
        let resources = build_resources(&mut doc);
        let pages_id = doc.new_object_id();
        let content_id = doc.add_object(Stream::new(dictionary! {}, content.to_vec()));
        let page_id = doc.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => Object::Reference(pages_id),
            "MediaBox" => Object::Array(vec![0.into(), 0.into(), 450.into(), 666.into()]),
            "Resources" => resources,
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

    fn image_xobject(width: i64, height: i64) -> Dictionary {
        dictionary! {
            "Type" => "XObject",
            "Subtype" => "Image",
            "Width" => width,
            "Height" => height,
            "BitsPerComponent" => 8,
            "ColorSpace" => "DeviceRGB",
        }
    }

    #[test]
    fn simple_do_reports_the_ctm_in_effect() {
        // 6in wide placement: cm 432 0 0 288 0 0  (432pt = 6in)
        let content = b"q 432 0 0 288 0 0 cm /Im0 Do Q";
        let (doc, page_id) = doc_with_page_content(content, |doc| {
            let image_id = doc.add_object(Object::Stream(Stream::new(
                image_xobject(600, 400),
                vec![0u8; 4],
            )));
            dictionary! { "XObject" => dictionary! { "Im0" => Object::Reference(image_id) } }
        });

        let mut seen = Vec::new();
        walk_page_images(&doc, page_id, &mut |ctm: Matrix, dict: &Dictionary, _id| {
            let (w, h) = drawn_size_points(ctm);
            let pixel_w = dict.get(b"Width").unwrap().as_i64().unwrap();
            let pixel_h = dict.get(b"Height").unwrap().as_i64().unwrap();
            seen.push((w, h, pixel_w, pixel_h));
        });

        assert_eq!(seen.len(), 1);
        let (w, h, pixel_w, pixel_h) = seen[0];
        assert!((w - 432.0).abs() < 1e-6);
        assert!((h - 288.0).abs() < 1e-6);
        assert_eq!(pixel_w, 600);
        assert_eq!(pixel_h, 400);

        let effective_ppi_x = pixel_w as f64 / (w / 72.0);
        assert!((effective_ppi_x - 100.0).abs() < 1e-6); // 600px / 6in = 100 ppi
    }

    #[test]
    fn nested_q_restores_prior_ctm() {
        // Scale by 2x inside q/Q, then draw at identity scale outside it.
        let content = b"q 2 0 0 2 0 0 cm Q /Im0 Do";
        let (doc, page_id) = doc_with_page_content(content, |doc| {
            let image_id = doc.add_object(Object::Stream(Stream::new(
                image_xobject(300, 300),
                vec![0u8; 4],
            )));
            dictionary! { "XObject" => dictionary! { "Im0" => Object::Reference(image_id) } }
        });

        let mut sizes = Vec::new();
        walk_page_images(&doc, page_id, &mut |ctm: Matrix, _: &Dictionary, _id| {
            sizes.push(drawn_size_points(ctm));
        });
        assert_eq!(sizes.len(), 1);
        // Unit square under identity CTM: 1x1 point — the q/Q scale must not leak out.
        assert!((sizes[0].0 - 1.0).abs() < 1e-9);
        assert!((sizes[0].1 - 1.0).abs() < 1e-9);
    }

    #[test]
    fn form_xobject_composes_its_matrix_with_the_callers_ctm() {
        // Page-level cm scales by 2; the form's own Matrix scales by 10 further.
        let page_content = b"q 2 0 0 2 0 0 cm /Fm0 Do Q";
        let (doc, page_id) = doc_with_page_content(page_content, |doc| {
            let image_id = doc.add_object(Object::Stream(Stream::new(
                image_xobject(300, 300),
                vec![0u8; 4],
            )));
            let form_resources =
                dictionary! { "XObject" => dictionary! { "Im0" => Object::Reference(image_id) } };
            let form_content = b"/Im0 Do";
            let form_id = doc.add_object(Object::Stream(Stream::new(
                dictionary! {
                    "Type" => "XObject",
                    "Subtype" => "Form",
                    "Matrix" => Object::Array(vec![10.0.into(), 0.0.into(), 0.0.into(), 10.0.into(), 0.0.into(), 0.0.into()]),
                    "Resources" => form_resources,
                },
                form_content.to_vec(),
            )));
            dictionary! { "XObject" => dictionary! { "Fm0" => Object::Reference(form_id) } }
        });

        let mut sizes = Vec::new();
        walk_page_images(&doc, page_id, &mut |ctm: Matrix, _: &Dictionary, _id| {
            sizes.push(drawn_size_points(ctm));
        });
        assert_eq!(sizes.len(), 1);
        // 1 (unit) * 10 (form matrix) * 2 (page cm) = 20
        assert!((sizes[0].0 - 20.0).abs() < 1e-9);
        assert!((sizes[0].1 - 20.0).abs() < 1e-9);
    }

    #[test]
    fn vector_only_page_yields_no_images() {
        let (doc, page_id) =
            doc_with_page_content(b"1 0 0 RG 0 0 100 100 re S", |_| dictionary! {});
        let mut count = 0;
        walk_page_images(&doc, page_id, &mut |_: Matrix, _: &Dictionary, _id| {
            count += 1
        });
        assert_eq!(count, 0);
    }

    #[test]
    fn deeply_nested_forms_do_not_infinite_loop() {
        // A form that (illegally, but a real-world adversarial/malformed file
        // might do this) invokes itself must not hang or overflow the stack.
        let (doc, page_id) = doc_with_page_content(b"/Fm0 Do", |doc| {
            let form_id = doc.new_object_id();
            let form_resources =
                dictionary! { "XObject" => dictionary! { "Self" => Object::Reference(form_id) } };
            doc.objects.insert(
                form_id,
                Object::Stream(Stream::new(
                    dictionary! { "Type" => "XObject", "Subtype" => "Form", "Resources" => form_resources },
                    b"/Self Do".to_vec(),
                )),
            );
            dictionary! { "XObject" => dictionary! { "Fm0" => Object::Reference(form_id) } }
        });
        let mut count = 0;
        walk_page_images(&doc, page_id, &mut |_: Matrix, _: &Dictionary, _id| {
            count += 1
        });
        // Just needs to terminate; no images to find.
        assert_eq!(count, 0);
        let _ = Length::ZERO; // silence unused-import in case of future edits
    }

    #[test]
    fn indirect_page_resources_are_resolved() {
        // /Resources on the page written as an indirect reference — the
        // shape that `effective_page_resources` exists to handle, and that
        // `doc.get_page_resources` alone used to silently drop.
        let mut doc = Document::with_version("1.7");
        let image_id = doc.add_object(Object::Stream(Stream::new(
            image_xobject(300, 300),
            vec![0u8; 4],
        )));
        let resources_id = doc.add_object(Object::Dictionary(
            dictionary! { "XObject" => dictionary! { "Im0" => Object::Reference(image_id) } },
        ));
        let pages_id = doc.new_object_id();
        let content_id = doc.add_object(Stream::new(dictionary! {}, b"/Im0 Do".to_vec()));
        let page_id = doc.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => Object::Reference(pages_id),
            "MediaBox" => Object::Array(vec![0.into(), 0.into(), 450.into(), 666.into()]),
            "Resources" => Object::Reference(resources_id),
            "Contents" => Object::Reference(content_id),
        });
        let pages = dictionary! { "Type" => "Pages", "Kids" => vec![Object::Reference(page_id)], "Count" => 1 };
        doc.objects.insert(pages_id, Object::Dictionary(pages));
        let catalog_id = doc.add_object(
            dictionary! { "Type" => "Catalog", "Pages" => Object::Reference(pages_id) },
        );
        doc.trailer.set("Root", Object::Reference(catalog_id));

        let mut count = 0;
        let outcome = walk_page_images(&doc, page_id, &mut |_: Matrix, _: &Dictionary, _id| {
            count += 1
        });
        assert_eq!(count, 1);
        assert_eq!(outcome, WalkOutcome::Completed);
    }

    #[test]
    fn indirect_form_resources_are_resolved() {
        // The form's own /Resources written as an indirect reference — the
        // shape task 1.2 calls out as still falling back to the page's
        // resources instead of resolving the indirection.
        let (doc, page_id) = doc_with_page_content(b"/Fm0 Do", |doc| {
            let image_id = doc.add_object(Object::Stream(Stream::new(
                image_xobject(300, 300),
                vec![0u8; 4],
            )));
            let form_resources_id = doc.add_object(Object::Dictionary(
                dictionary! { "XObject" => dictionary! { "Im0" => Object::Reference(image_id) } },
            ));
            let form_id = doc.add_object(Object::Stream(Stream::new(
                dictionary! {
                    "Type" => "XObject",
                    "Subtype" => "Form",
                    "Resources" => Object::Reference(form_resources_id),
                },
                b"/Im0 Do".to_vec(),
            )));
            dictionary! { "XObject" => dictionary! { "Fm0" => Object::Reference(form_id) } }
        });

        let mut count = 0;
        walk_page_images(&doc, page_id, &mut |_: Matrix, _: &Dictionary, _id| {
            count += 1
        });
        assert_eq!(
            count, 1,
            "an image findable only through the form's indirect /Resources must be found"
        );
    }

    #[test]
    fn collect_page_layers_sees_the_page_and_every_nested_form() {
        let (doc, page_id) = doc_with_page_content(b"BT /F1 12 Tf ET /Fm0 Do", |doc| {
            let form_content = b"BT /F2 12 Tf ET";
            let form_resources = dictionary! { "Font" => dictionary! { "F2" => Object::Reference(doc.new_object_id()) } };
            let form_id = doc.add_object(Object::Stream(Stream::new(
                dictionary! { "Type" => "XObject", "Subtype" => "Form", "Resources" => form_resources },
                form_content.to_vec(),
            )));
            dictionary! {
                "Font" => dictionary! { "F1" => Object::Reference(doc.new_object_id()) },
                "XObject" => dictionary! { "Fm0" => Object::Reference(form_id) },
            }
        });

        let (layers, outcome) = collect_page_layers(&doc, page_id);
        assert_eq!(outcome, WalkOutcome::Completed);
        assert_eq!(layers.len(), 2, "the page's own layer plus one form layer");

        let page_layer = layers
            .iter()
            .find(|l| l.form_id.is_none())
            .expect("page layer");
        assert!(page_layer.content.starts_with(b"BT /F1"));
        assert!(page_layer.resources.get(b"Font").is_ok());

        let form_layer = layers
            .iter()
            .find(|l| l.form_id.is_some())
            .expect("form layer");
        assert!(form_layer.content.starts_with(b"BT /F2"));
        assert!(form_layer
            .resources
            .get(b"Font")
            .and_then(|o| o.as_dict())
            .unwrap()
            .get(b"F2")
            .is_ok());
    }

    #[test]
    fn operation_budget_exceeded_is_reported_not_silently_truncated() {
        // A tiny budget of 3 operations against a content stream with far
        // more than that must stop early and report BudgetExceeded, rather
        // than silently examining only the first 3 and calling it done.
        let (doc, page_id) =
            doc_with_page_content(b"q Q q Q q Q q Q q Q q Q q Q", |_| dictionary! {});
        let outcome = run_walk(&doc, page_id, 3, None, None);
        assert_eq!(outcome, WalkOutcome::BudgetExceeded);
    }

    #[test]
    fn operation_budget_not_exceeded_when_content_fits() {
        let (doc, page_id) = doc_with_page_content(b"q Q", |_| dictionary! {});
        let outcome = run_walk(&doc, page_id, 1000, None, None);
        assert_eq!(outcome, WalkOutcome::Completed);
    }

    #[test]
    fn budget_is_shared_across_nested_forms_not_reset_per_form() {
        // Each form burns 2 operations (q Q); with a budget of 3, the first
        // form's own two operations plus one more from the second form's
        // entry must exhaust it — proving the budget is a single, walk-wide
        // total rather than being reset at each recursion level.
        let (doc, page_id) = doc_with_page_content(b"/Fm0 Do /Fm1 Do", |doc| {
            let fm0 = doc.add_object(Object::Stream(Stream::new(
                dictionary! { "Type" => "XObject", "Subtype" => "Form" },
                b"q Q".to_vec(),
            )));
            let fm1 = doc.add_object(Object::Stream(Stream::new(
                dictionary! { "Type" => "XObject", "Subtype" => "Form" },
                b"q Q".to_vec(),
            )));
            dictionary! { "XObject" => dictionary! { "Fm0" => Object::Reference(fm0), "Fm1" => Object::Reference(fm1) } }
        });
        // Top-level content "/Fm0 Do /Fm1 Do" is itself 2 operations; budget
        // of 3 must run out partway through, well short of completing both
        // forms plus both Do calls.
        let outcome = run_walk(&doc, page_id, 3, None, None);
        assert_eq!(outcome, WalkOutcome::BudgetExceeded);
    }
}
