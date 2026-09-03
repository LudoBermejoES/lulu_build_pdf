//! Native, image-only ICC colour conversion via `lcms2` — behind the `icc`
//! Cargo feature. For a caller who wants CMYK images without letting
//! Ghostscript rewrite the whole document ([`crate::external_tools`]'s
//! flatten stage). Converts only raster image samples; vector colour
//! operators are left untouched, and this module never claims otherwise.

use lopdf::{Document, Object, ObjectId};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SampleColorSpace {
    Gray,
    Rgb,
    Cmyk,
}

impl SampleColorSpace {
    pub fn channels(self) -> usize {
        match self {
            SampleColorSpace::Gray => 1,
            SampleColorSpace::Rgb => 3,
            SampleColorSpace::Cmyk => 4,
        }
    }

    fn pixel_format(self) -> lcms2::PixelFormat {
        match self {
            SampleColorSpace::Gray => lcms2::PixelFormat::GRAY_8,
            SampleColorSpace::Rgb => lcms2::PixelFormat::RGB_8,
            SampleColorSpace::Cmyk => lcms2::PixelFormat::CMYK_8,
        }
    }

    fn device_space_name(self) -> &'static [u8] {
        match self {
            SampleColorSpace::Gray => b"DeviceGray",
            SampleColorSpace::Rgb => b"DeviceRGB",
            SampleColorSpace::Cmyk => b"DeviceCMYK",
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum IccError {
    #[error("could not parse the ICC profile: {0}")]
    InvalidProfile(String),
    #[error("sample buffer length {len} is not a multiple of {channels} channels")]
    MalformedSamples { len: usize, channels: usize },
    #[error("could not build the colour transform: {0}")]
    TransformFailed(String),
}

/// Converts one buffer of interleaved 8-bit-per-channel samples from
/// `source_profile`/`source_space` to `dest_profile`/`dest_space`, using
/// lcms2's perceptual rendering intent. Both profiles are required — there
/// is no unspecified default source or destination.
pub fn convert_samples(
    samples: &[u8],
    source_profile: &lcms2::Profile,
    source_space: SampleColorSpace,
    dest_profile: &lcms2::Profile,
    dest_space: SampleColorSpace,
) -> Result<Vec<u8>, IccError> {
    let in_channels = source_space.channels();
    if !samples.len().is_multiple_of(in_channels) {
        return Err(IccError::MalformedSamples {
            len: samples.len(),
            channels: in_channels,
        });
    }
    let transform: lcms2::Transform<u8, u8> = lcms2::Transform::new(
        source_profile,
        source_space.pixel_format(),
        dest_profile,
        dest_space.pixel_format(),
        lcms2::Intent::Perceptual,
    )
    .map_err(|e| IccError::TransformFailed(e.to_string()))?;

    let pixel_count = samples.len() / in_channels;
    let mut out = vec![0u8; pixel_count * dest_space.channels()];
    transform.transform_pixels(samples, &mut out);
    Ok(out)
}

/// Parses a destination ICC profile from raw bytes (e.g. a GRACoL profile
/// read from disk by the caller).
pub fn load_icc_profile(bytes: &[u8]) -> Result<lcms2::Profile, IccError> {
    lcms2::Profile::new_icc(bytes).map_err(|e| IccError::InvalidProfile(e.to_string()))
}

/// One image XObject's conversion outcome, for the run report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImageConversionOutcome {
    Converted,
    /// Left unchanged — not a failure. Anything other than an 8-bit-per-
    /// component `DeviceRGB` image with no filter or a plain `FlateDecode`
    /// filter is outside this native path's scope (JPEG/DCTDecode, indexed
    /// palettes, 16-bit samples, CMYK-already, etc.) and is reported as
    /// skipped rather than attempted.
    Skipped {
        reason: String,
    },
}

fn plain_content_and_filter(
    doc: &Document,
    image_id: ObjectId,
) -> Option<(Vec<u8>, Option<String>)> {
    let Object::Stream(stream) = doc.get_object(image_id).ok()? else {
        return None;
    };
    let filter = stream
        .dict
        .get(b"Filter")
        .ok()
        .and_then(|o| o.as_name().ok())
        .map(|n| String::from_utf8_lossy(n).to_string());
    match filter.as_deref() {
        None | Some("FlateDecode") => Some((stream.get_plain_content().ok()?, filter)),
        _ => None,
    }
}

/// Converts every eligible `DeviceRGB` image in the document to
/// `dest_space` using `dest_profile`, in place. Eligible means 8 bits per
/// component and either no filter or a plain `FlateDecode` filter;
/// anything else — JPEG/DCTDecode, JPX, indexed colour, 16-bit samples —
/// is left untouched and reported as skipped, never failed. The converted
/// image is written as raw, uncompressed samples with its `/Filter`
/// removed rather than re-applied — this can make a converted image
/// substantially larger than its original, since nothing recompresses it
/// here; `/Decode`, `/SMask`, and `/ImageMask` are also left as found on
/// the (now wrong colour space) image dictionary rather than reconciled.
/// Returns one [`ImageConversionOutcome`] per image found, in document
/// order.
///
/// `dest_space` is exposed (rather than hardcoding CMYK) so the mechanism
/// can be exercised in tests against destination profiles lcms2 can build
/// without an external ICC file (e.g. RGB via [`lcms2::Profile::new_srgb`]);
/// [`convert_document_images_to_cmyk`] is the real entry point and always
/// passes [`SampleColorSpace::Cmyk`].
pub fn convert_document_images(
    doc: &mut Document,
    dest_profile: &lcms2::Profile,
    dest_space: SampleColorSpace,
) -> Vec<(ObjectId, ImageConversionOutcome)> {
    let source_profile = lcms2::Profile::new_srgb();
    let mut results = Vec::new();
    let mut conversions: Vec<(ObjectId, Vec<u8>)> = Vec::new();

    for page_id in doc.page_iter().collect::<Vec<_>>() {
        let Ok(images) = doc.get_page_images(page_id) else {
            continue;
        };
        for image in images {
            let outcome = 'outcome: {
                if image.bits_per_component != Some(8) {
                    break 'outcome ImageConversionOutcome::Skipped {
                        reason: format!(
                            "{}-bit samples are not supported by native conversion",
                            image.bits_per_component.unwrap_or(-1)
                        ),
                    };
                }
                if image.color_space.as_deref() != Some("DeviceRGB") {
                    break 'outcome ImageConversionOutcome::Skipped {
                        reason: format!(
                            "colour space '{}' is not DeviceRGB",
                            image.color_space.as_deref().unwrap_or("(none)")
                        ),
                    };
                }
                let Some((raw, _filter)) = plain_content_and_filter(doc, image.id) else {
                    break 'outcome ImageConversionOutcome::Skipped {
                        reason: "filter is not natively decodable (e.g. DCTDecode/JPX)".to_string(),
                    };
                };
                match convert_samples(
                    &raw,
                    &source_profile,
                    SampleColorSpace::Rgb,
                    dest_profile,
                    dest_space,
                ) {
                    Ok(converted) => {
                        conversions.push((image.id, converted));
                        ImageConversionOutcome::Converted
                    }
                    Err(e) => ImageConversionOutcome::Skipped {
                        reason: e.to_string(),
                    },
                }
            };
            results.push((image.id, outcome));
        }
    }

    for (image_id, converted) in conversions {
        if let Ok(Object::Stream(stream)) = doc.get_object_mut(image_id) {
            stream.dict.set(
                "ColorSpace",
                Object::Name(dest_space.device_space_name().to_vec()),
            );
            stream.dict.remove(b"Filter");
            stream.dict.remove(b"DecodeParms");
            stream.set_content(converted);
        }
    }

    results
}

/// Converts every eligible `DeviceRGB` image in the document to CMYK using
/// `dest_profile` (a real CMYK ICC profile, e.g. GRACoL, supplied by the
/// caller). See [`convert_document_images`] for the mechanism.
pub fn convert_document_images_to_cmyk(
    doc: &mut Document,
    dest_profile: &lcms2::Profile,
) -> Vec<(ObjectId, ImageConversionOutcome)> {
    convert_document_images(doc, dest_profile, SampleColorSpace::Cmyk)
}

/// Builds a [`crate::report::Finding`] summarising the skipped images (the
/// converted ones need no finding — they succeeded silently, same as any
/// other normalization step). Also states, once, that vector colour
/// operators were never touched — the honesty this module's doc comment
/// promises.
pub fn summarize_conversions(
    results: &[(ObjectId, ImageConversionOutcome)],
) -> Vec<crate::report::Finding> {
    let mut findings = Vec::new();
    let converted = results
        .iter()
        .filter(|(_, o)| matches!(o, ImageConversionOutcome::Converted))
        .count();
    let skipped: Vec<&String> = results
        .iter()
        .filter_map(|(_, o)| match o {
            ImageConversionOutcome::Skipped { reason } => Some(reason),
            _ => None,
        })
        .collect();

    if converted > 0 {
        findings.push(crate::report::Finding::new(
            "icc.images-converted",
            crate::report::Severity::Info,
            format!("converted {converted} image(s) to CMYK natively; vector colour operators were left unchanged (Ghostscript or Lulu's own normalizer handle those)"),
        ));
    }
    if !skipped.is_empty() {
        findings.push(crate::report::Finding::new(
            "icc.images-skipped",
            crate::report::Severity::Warning,
            format!(
                "{} image(s) could not be converted natively and were left unchanged: {}",
                skipped.len(),
                skipped
                    .iter()
                    .take(3)
                    .cloned()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join("; ")
            ),
        ));
    }
    findings
}

#[cfg(test)]
mod tests {
    use super::*;
    use lopdf::{dictionary, Stream};

    #[test]
    fn converts_rgb_to_a_three_channel_destination_and_preserves_pixel_count() {
        // No real CMYK ICC profile is bundled with the test suite, so this
        // validates the actual lcms2 transform plumbing (buffer sizing,
        // channel handling) using two built-in profiles.
        let source = lcms2::Profile::new_srgb();
        let dest = lcms2::Profile::new_srgb();
        let rgb = [255u8, 0, 0, 0, 255, 0, 0, 0, 255]; // red, green, blue pixels
        let out = convert_samples(
            &rgb,
            &source,
            SampleColorSpace::Rgb,
            &dest,
            SampleColorSpace::Rgb,
        )
        .unwrap();
        assert_eq!(out.len(), rgb.len());
    }

    #[test]
    fn malformed_sample_length_is_rejected() {
        let source = lcms2::Profile::new_srgb();
        let dest = lcms2::Profile::new_srgb();
        let bad = [255u8, 0]; // 2 bytes, not a multiple of 3 (RGB)
        let err = convert_samples(
            &bad,
            &source,
            SampleColorSpace::Rgb,
            &dest,
            SampleColorSpace::Rgb,
        )
        .unwrap_err();
        assert!(matches!(
            err,
            IccError::MalformedSamples {
                len: 2,
                channels: 3
            }
        ));
    }

    #[test]
    fn invalid_icc_bytes_are_rejected_not_panicked() {
        let err = load_icc_profile(b"not a real icc profile").unwrap_err();
        assert!(matches!(err, IccError::InvalidProfile(_)));
    }

    fn doc_with_rgb_image(width: i64, height: i64, filter: Option<&str>) -> (Document, ObjectId) {
        let mut doc = Document::with_version("1.7");
        let pixel_count = (width * height) as usize;
        let raw = vec![128u8; pixel_count * 3];
        let mut image_dict = dictionary! {
            "Type" => "XObject",
            "Subtype" => "Image",
            "Width" => width,
            "Height" => height,
            "BitsPerComponent" => 8,
            "ColorSpace" => "DeviceRGB",
        };
        let content = if let Some(f) = filter {
            image_dict.set("Filter", Object::Name(f.as_bytes().to_vec()));
            if f == "FlateDecode" {
                use std::io::Write;
                let mut encoder =
                    flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
                encoder.write_all(&raw).unwrap();
                encoder.finish().unwrap()
            } else {
                raw.clone()
            }
        } else {
            raw.clone()
        };
        let image_id = doc.add_object(Object::Stream(Stream::new(image_dict, content)));
        let resources =
            dictionary! { "XObject" => dictionary! { "Im0" => Object::Reference(image_id) } };
        let pages_id = doc.new_object_id();
        let page_id = doc.add_object(dictionary! {
            "Type" => "Page",
            "Parent" => Object::Reference(pages_id),
            "MediaBox" => Object::Array(vec![0.into(), 0.into(), 450.into(), 666.into()]),
            "Resources" => resources,
        });
        let pages = dictionary! { "Type" => "Pages", "Kids" => vec![Object::Reference(page_id)], "Count" => 1 };
        doc.objects.insert(pages_id, Object::Dictionary(pages));
        let catalog_id = doc.add_object(
            dictionary! { "Type" => "Catalog", "Pages" => Object::Reference(pages_id) },
        );
        doc.trailer.set("Root", Object::Reference(catalog_id));
        (doc, image_id)
    }

    #[test]
    fn converts_an_uncompressed_rgb_image_in_place() {
        // Destination space is RGB here (a real lcms2 sRGB->sRGB pairing) to
        // exercise the walk/filter/mutation mechanism without needing an
        // external CMYK ICC file; convert_document_images_to_cmyk is the
        // real CMYK entry point and shares this same mechanism.
        let (mut doc, image_id) = doc_with_rgb_image(2, 2, None);
        let dest_profile = lcms2::Profile::new_srgb();
        let results = convert_document_images(&mut doc, &dest_profile, SampleColorSpace::Rgb);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].1, ImageConversionOutcome::Converted);

        let Object::Stream(stream) = doc.get_object(image_id).unwrap() else {
            panic!()
        };
        assert_eq!(
            stream.dict.get(b"ColorSpace").unwrap().as_name().unwrap(),
            b"DeviceRGB"
        );
        assert!(
            stream.dict.get(b"Filter").is_err(),
            "output is raw, no stale Filter"
        );
        assert_eq!(stream.content.len(), 4 * 3); // 4 pixels * 3 channels (RGB)
    }

    #[test]
    fn converts_a_flate_decoded_rgb_image_in_place() {
        let (mut doc, image_id) = doc_with_rgb_image(2, 2, Some("FlateDecode"));
        let dest_profile = lcms2::Profile::new_srgb();
        let results = convert_document_images(&mut doc, &dest_profile, SampleColorSpace::Rgb);
        assert_eq!(results[0].1, ImageConversionOutcome::Converted);
        let Object::Stream(stream) = doc.get_object(image_id).unwrap() else {
            panic!()
        };
        assert_eq!(stream.content.len(), 4 * 3);
    }

    #[test]
    fn dct_decode_image_is_skipped_not_failed() {
        let (mut doc, _) = doc_with_rgb_image(2, 2, Some("DCTDecode"));
        let dest_profile = lcms2::Profile::new_srgb();
        let results = convert_document_images_to_cmyk(&mut doc, &dest_profile);
        assert_eq!(results.len(), 1);
        assert!(matches!(
            &results[0].1,
            ImageConversionOutcome::Skipped { .. }
        ));
    }

    #[test]
    fn summary_reports_both_converted_and_skipped_counts() {
        let results: Vec<(ObjectId, ImageConversionOutcome)> = vec![
            ((1, 0), ImageConversionOutcome::Converted),
            (
                (2, 0),
                ImageConversionOutcome::Skipped {
                    reason: "DCTDecode".to_string(),
                },
            ),
        ];
        let findings = summarize_conversions(&results);
        assert!(findings.iter().any(|f| f.code == "icc.images-converted"));
        assert!(findings
            .iter()
            .any(|f| f.code == "icc.images-skipped"
                && f.severity == crate::report::Severity::Warning));
        assert!(findings.iter().any(|f| f
            .message
            .contains("vector colour operators were left unchanged")));
    }

    #[test]
    fn no_images_produces_no_findings() {
        assert!(summarize_conversions(&[]).is_empty());
    }
}
