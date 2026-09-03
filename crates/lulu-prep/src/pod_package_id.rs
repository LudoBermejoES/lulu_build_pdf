//! Parsing of Lulu's `pod_package_id` in both the current dotted form and the
//! legacy 27-character form Lulu retires on 2027-02-01.
//!
//! The trim segment (e.g. `0600X0900`) encodes width and height as hundredths
//! of an inch, but Lulu's own catalog shows this is a *lossy* encoding for some
//! products (e.g. a 6.875 in trim height encodes as `0687`, i.e. 6.87 in) — so
//! [`PodPackageId::trim_size_in`] is a decoded, approximate value for display
//! and identification only. [`crate::catalog`] is the authoritative source of
//! exact trim and bleed geometry.

use crate::catalog::Binding;

const LEGACY_END_OF_SUPPORT: &str = "2027-02-01";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ink {
    BlackAndWhite,
    FullColor,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Quality {
    Standard,
    Premium,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaperCoating {
    Coated,
    Uncoated,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaperColor {
    White,
    Cream,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lamination {
    Gloss,
    Matte,
    /// No lamination code recognised (rare on interior-only SKUs); kept rather
    /// than rejecting the whole id over a cosmetic finish detail.
    Unspecified,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaperCode {
    pub raw: String,
    pub weight: u32,
    pub coating: PaperCoating,
    pub color: PaperColor,
    pub ppi: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FinishCode {
    pub raw: String,
    pub lamination: Lamination,
    /// `None` when the code is `X` (no linen).
    pub linen_color: Option<char>,
    /// `None` when the code is `X` (no foil).
    pub foil_color: Option<char>,
}

/// Carried by a descriptor parsed from the legacy 27-character form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeprecationNotice {
    pub dotted_equivalent: String,
    pub legacy_support_ends: &'static str,
}

/// A parsed `pod_package_id`, in either source form.
#[derive(Debug, Clone, PartialEq)]
pub struct PodPackageId {
    pub canonical: String,
    pub trim_code: String,
    /// Decoded from the trim segment — approximate, see the module docs.
    pub trim_size_in: (f64, f64),
    pub ink: Ink,
    pub quality: Quality,
    pub binding: Binding,
    pub paper: PaperCode,
    pub finish: FinishCode,
    pub deprecation: Option<DeprecationNotice>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PodPackageIdError {
    #[error("segment {segment_index} ('{segment_name}') is invalid: '{value}'")]
    InvalidSegment {
        segment_index: usize,
        segment_name: &'static str,
        value: String,
    },
    #[error("dotted pod_package_id must have 6 segments, found {found}")]
    WrongSegmentCount { found: usize },
    #[error("legacy pod_package_id must be exactly 27 characters, found {found}")]
    WrongLegacyLength { found: usize },
}

fn parse_trim(segment: &str, index: usize) -> Result<(String, (f64, f64)), PodPackageIdError> {
    let invalid = || PodPackageIdError::InvalidSegment {
        segment_index: index,
        segment_name: "trim",
        value: segment.to_string(),
    };
    if segment.len() != 9 {
        return Err(invalid());
    }
    let (w, rest) = segment.split_at(4);
    let rest = rest.strip_prefix('X').ok_or_else(invalid)?;
    if rest.len() != 4 {
        return Err(invalid());
    }
    let w: u32 = w.parse().map_err(|_| invalid())?;
    let h: u32 = rest.parse().map_err(|_| invalid())?;
    Ok((segment.to_string(), (w as f64 / 100.0, h as f64 / 100.0)))
}

fn parse_ink(segment: &str, index: usize) -> Result<Ink, PodPackageIdError> {
    match segment {
        "BW" => Ok(Ink::BlackAndWhite),
        "FC" => Ok(Ink::FullColor),
        _ => Err(PodPackageIdError::InvalidSegment {
            segment_index: index,
            segment_name: "ink",
            value: segment.to_string(),
        }),
    }
}

fn parse_quality(segment: &str, index: usize) -> Result<Quality, PodPackageIdError> {
    match segment {
        "STD" => Ok(Quality::Standard),
        "PRE" => Ok(Quality::Premium),
        _ => Err(PodPackageIdError::InvalidSegment {
            segment_index: index,
            segment_name: "quality",
            value: segment.to_string(),
        }),
    }
}

fn parse_binding(segment: &str, index: usize) -> Result<Binding, PodPackageIdError> {
    Binding::from_sku_code(segment).ok_or_else(|| PodPackageIdError::InvalidSegment {
        segment_index: index,
        segment_name: "binding",
        value: segment.to_string(),
    })
}

fn parse_paper(segment: &str, index: usize) -> Result<PaperCode, PodPackageIdError> {
    let invalid = || PodPackageIdError::InvalidSegment {
        segment_index: index,
        segment_name: "paper",
        value: segment.to_string(),
    };
    if segment.len() != 8 {
        return Err(invalid());
    }
    let bytes = segment.as_bytes();
    let weight: u32 = segment[0..3].parse().map_err(|_| invalid())?;
    let coating = match bytes[3] {
        b'C' => PaperCoating::Coated,
        b'U' => PaperCoating::Uncoated,
        _ => return Err(invalid()),
    };
    let color = match bytes[4] {
        b'W' => PaperColor::White,
        b'C' => PaperColor::Cream,
        _ => return Err(invalid()),
    };
    let ppi: u32 = segment[5..8].parse().map_err(|_| invalid())?;
    Ok(PaperCode {
        raw: segment.to_string(),
        weight,
        coating,
        color,
        ppi,
    })
}

fn parse_finish(segment: &str, index: usize) -> Result<FinishCode, PodPackageIdError> {
    let invalid = || PodPackageIdError::InvalidSegment {
        segment_index: index,
        segment_name: "finish",
        value: segment.to_string(),
    };
    if segment.len() != 3 {
        return Err(invalid());
    }
    let chars: Vec<char> = segment.chars().collect();
    let lamination = match chars[0] {
        'G' => Lamination::Gloss,
        'M' => Lamination::Matte,
        _ => Lamination::Unspecified,
    };
    let linen_color = if chars[1] == 'X' {
        None
    } else {
        Some(chars[1])
    };
    let foil_color = if chars[2] == 'X' {
        None
    } else {
        Some(chars[2])
    };
    Ok(FinishCode {
        raw: segment.to_string(),
        lamination,
        linen_color,
        foil_color,
    })
}

/// The parsed segments of a `pod_package_id`, before the canonical dotted form
/// and any deprecation notice are attached.
struct ParsedSegments {
    trim_code: String,
    trim_size_in: (f64, f64),
    ink: Ink,
    quality: Quality,
    binding: Binding,
    paper: PaperCode,
    finish: FinishCode,
}

fn assemble(segments: ParsedSegments, deprecation: Option<DeprecationNotice>) -> PodPackageId {
    let ParsedSegments {
        trim_code,
        trim_size_in,
        ink,
        quality,
        binding,
        paper,
        finish,
    } = segments;
    let ink_code = match ink {
        Ink::BlackAndWhite => "BW",
        Ink::FullColor => "FC",
    };
    let quality_code = match quality {
        Quality::Standard => "STD",
        Quality::Premium => "PRE",
    };
    let binding_code = match binding {
        Binding::Perfect => "PB",
        Binding::Coil => "CO",
        Binding::SaddleStitch => "SS",
        Binding::CaseWrap => "CW",
        Binding::LinenWrap => "LW",
        Binding::WireO => "WO",
    };
    let canonical = format!(
        "{}.{}.{}.{}.{}.{}",
        trim_code, ink_code, quality_code, binding_code, paper.raw, finish.raw
    );
    PodPackageId {
        canonical,
        trim_code,
        trim_size_in,
        ink,
        quality,
        binding,
        paper,
        finish,
        deprecation,
    }
}

impl PodPackageId {
    /// Parse either the dotted form (`[Trim].[Ink].[Quality].[Binding].[Paper].[Finish]`)
    /// or the legacy 27-character undotted form.
    pub fn parse(id: &str) -> Result<PodPackageId, PodPackageIdError> {
        if id.contains('.') {
            Self::parse_dotted(id)
        } else {
            Self::parse_legacy(id)
        }
    }

    fn parse_dotted(id: &str) -> Result<PodPackageId, PodPackageIdError> {
        let segments: Vec<&str> = id.split('.').collect();
        if segments.len() != 6 {
            return Err(PodPackageIdError::WrongSegmentCount {
                found: segments.len(),
            });
        }
        let (trim_code, trim_size_in) = parse_trim(segments[0], 0)?;
        let ink = parse_ink(segments[1], 1)?;
        let quality = parse_quality(segments[2], 2)?;
        let binding = parse_binding(segments[3], 3)?;
        let paper = parse_paper(segments[4], 4)?;
        let finish = parse_finish(segments[5], 5)?;
        Ok(assemble(
            ParsedSegments {
                trim_code,
                trim_size_in,
                ink,
                quality,
                binding,
                paper,
                finish,
            },
            None,
        ))
    }

    fn parse_legacy(id: &str) -> Result<PodPackageId, PodPackageIdError> {
        if id.len() != 27 {
            return Err(PodPackageIdError::WrongLegacyLength { found: id.len() });
        }
        let trim_segment = &id[0..9];
        let ink_segment = &id[9..11];
        let quality_segment = &id[11..14];
        let binding_segment = &id[14..16];
        let paper_segment = &id[16..24];
        let finish_segment = &id[24..27];

        let (trim_code, trim_size_in) = parse_trim(trim_segment, 0)?;
        let ink = parse_ink(ink_segment, 1)?;
        let quality = parse_quality(quality_segment, 2)?;
        let binding = parse_binding(binding_segment, 3)?;
        let paper = parse_paper(paper_segment, 4)?;
        let finish = parse_finish(finish_segment, 5)?;

        let segments = ParsedSegments {
            trim_code,
            trim_size_in,
            ink,
            quality,
            binding,
            paper,
            finish,
        };
        let dotted_canonical = assemble(
            ParsedSegments {
                trim_code: segments.trim_code.clone(),
                trim_size_in: segments.trim_size_in,
                ink: segments.ink,
                quality: segments.quality,
                binding: segments.binding,
                paper: segments.paper.clone(),
                finish: segments.finish.clone(),
            },
            None,
        )
        .canonical;
        let notice = DeprecationNotice {
            dotted_equivalent: dotted_canonical,
            legacy_support_ends: LEGACY_END_OF_SUPPORT,
        };
        Ok(assemble(segments, Some(notice)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dotted_id_is_parsed() {
        let d = PodPackageId::parse("0600X0900.FC.STD.PB.080CW444.GXX").expect("valid dotted id");
        assert!((d.trim_size_in.0 - 6.0).abs() < 1e-9);
        assert!((d.trim_size_in.1 - 9.0).abs() < 1e-9);
        assert_eq!(d.ink, Ink::FullColor);
        assert_eq!(d.quality, Quality::Standard);
        assert_eq!(d.binding, crate::catalog::Binding::Perfect);
        assert_eq!(d.paper.weight, 80);
        assert_eq!(d.paper.coating, PaperCoating::Coated);
        assert_eq!(d.paper.color, PaperColor::White);
        assert_eq!(d.paper.ppi, 444);
        assert_eq!(d.finish.lamination, Lamination::Gloss);
        assert_eq!(d.finish.linen_color, None);
        assert_eq!(d.finish.foil_color, None);
        assert!(d.deprecation.is_none());
    }

    #[test]
    fn legacy_id_parses_to_the_same_descriptor_and_is_flagged() {
        let dotted = PodPackageId::parse("0600X0900.BW.STD.PB.060UW444.MXX").unwrap();
        let legacy = PodPackageId::parse("0600X0900BWSTDPB060UW444MXX").unwrap();

        assert_eq!(dotted.trim_size_in, legacy.trim_size_in);
        assert_eq!(dotted.ink, legacy.ink);
        assert_eq!(dotted.quality, legacy.quality);
        assert_eq!(dotted.binding, legacy.binding);
        assert_eq!(dotted.paper, legacy.paper);
        assert_eq!(dotted.finish, legacy.finish);

        let notice = legacy
            .deprecation
            .expect("legacy form must carry a deprecation notice");
        assert_eq!(notice.dotted_equivalent, "0600X0900.BW.STD.PB.060UW444.MXX");
        assert_eq!(notice.legacy_support_ends, "2027-02-01");
        assert!(dotted.deprecation.is_none());
    }

    #[test]
    fn malformed_trim_segment_is_rejected_with_position() {
        let err = PodPackageId::parse("06X0900.BW.STD.PB.060UW444.MXX").unwrap_err();
        match err {
            PodPackageIdError::InvalidSegment {
                segment_index,
                segment_name,
                ..
            } => {
                assert_eq!(segment_index, 0);
                assert_eq!(segment_name, "trim");
            }
            other => panic!("expected InvalidSegment, got {other:?}"),
        }
    }

    #[test]
    fn unrecognised_binding_code_is_rejected_with_position() {
        let err = PodPackageId::parse("0600X0900.BW.STD.ZZ.060UW444.MXX").unwrap_err();
        match err {
            PodPackageIdError::InvalidSegment {
                segment_index,
                segment_name,
                ..
            } => {
                assert_eq!(segment_index, 3);
                assert_eq!(segment_name, "binding");
            }
            other => panic!("expected InvalidSegment, got {other:?}"),
        }
    }

    #[test]
    fn wrong_segment_count_is_rejected() {
        let err = PodPackageId::parse("0600X0900.BW.STD.PB.060UW444").unwrap_err();
        assert!(matches!(err, PodPackageIdError::WrongSegmentCount { .. }));
    }

    #[test]
    fn wrong_legacy_length_is_rejected() {
        let err = PodPackageId::parse("0600X0900BWSTDPB060UW444M").unwrap_err();
        assert!(matches!(err, PodPackageIdError::WrongLegacyLength { .. }));
    }
}
