//! A read-only walk of a page's content stream, tracking the current
//! transformation matrix (CTM) through `cm`/`q`/`Q` and descending into form
//! XObjects, so callers can learn where on the page each image XObject is
//! actually drawn. Confined to preflight: a bug here degrades a warning, it
//! never corrupts output, since nothing here writes to the document.

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

/// Maximum form-XObject recursion depth, guarding against a (malformed or
/// adversarial) form that references itself.
const MAX_FORM_DEPTH: u32 = 32;

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

#[allow(clippy::too_many_arguments)]
fn walk(
    doc: &Document,
    content_bytes: &[u8],
    resources: Option<&Dictionary>,
    ctm: Matrix,
    depth: u32,
    visitor: &mut dyn ImageVisitor,
) {
    if depth > MAX_FORM_DEPTH {
        return;
    }
    let Ok(content) = Content::decode(content_bytes) else {
        return;
    };
    let Some(resources) = resources else { return };

    let mut stack: Vec<Matrix> = Vec::new();
    let mut current = ctm;

    for Operation { operator, operands } in content.operations.iter() {
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
                let Some((xdict, xid)) = xobject_dict(doc, resources, name) else {
                    continue;
                };
                let subtype = xdict.get(b"Subtype").and_then(|o| o.as_name()).ok();
                match subtype {
                    Some(b"Image") => visitor.visit_image(current, xdict, xid),
                    Some(b"Form") => {
                        let form_matrix = xdict
                            .get(b"Matrix")
                            .ok()
                            .and_then(|o| o.as_array().ok())
                            .and_then(|arr| matrix_from_cm_operands(arr))
                            .unwrap_or(Matrix::IDENTITY);
                        let form_ctm = form_matrix.then(current);
                        let form_resources = xdict
                            .get(b"Resources")
                            .ok()
                            .and_then(|o| o.as_dict().ok())
                            .or(Some(resources));
                        if let Ok(Object::Stream(stream)) = doc.get_object(xid) {
                            if let Ok(bytes) = stream.get_plain_content() {
                                walk(doc, &bytes, form_resources, form_ctm, depth + 1, visitor);
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

/// Walk a page's content stream (and any form XObjects it invokes), calling
/// `visitor` for every image `Do` with the CTM at that draw site.
pub fn walk_page_images(doc: &Document, page_id: ObjectId, visitor: &mut dyn ImageVisitor) {
    let content_bytes = doc.get_page_content(page_id);
    let resources = doc.get_page_resources(page_id).ok().and_then(|(r, _)| r);
    walk(doc, &content_bytes, resources, Matrix::IDENTITY, 0, visitor);
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
}
