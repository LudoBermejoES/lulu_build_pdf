//! Lulu's embedded product catalog: `pod_package_id` resolution against the
//! spec sheet Lulu publishes, with no network access.

use crate::units::{Length, Size};
use std::sync::OnceLock;

const CATALOG_CSV: &str = include_str!("../data/pod-packages.csv");

/// The six binding types Lulu offers, decoded from the SKU's binding segment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Binding {
    Perfect,
    Coil,
    SaddleStitch,
    CaseWrap,
    LinenWrap,
    WireO,
}

impl Binding {
    pub fn from_sku_code(code: &str) -> Option<Binding> {
        match code {
            "PB" => Some(Binding::Perfect),
            "CO" => Some(Binding::Coil),
            "SS" => Some(Binding::SaddleStitch),
            "CW" => Some(Binding::CaseWrap),
            "LW" => Some(Binding::LinenWrap),
            "WO" => Some(Binding::WireO),
            _ => None,
        }
    }

    fn from_catalog_string(s: &str) -> Option<Binding> {
        match s {
            "Perfect" => Some(Binding::Perfect),
            "Coil" => Some(Binding::Coil),
            "Saddle Stitch" => Some(Binding::SaddleStitch),
            "Case Wrap" => Some(Binding::CaseWrap),
            "Linen Wrap" => Some(Binding::LinenWrap),
            "Wire O" => Some(Binding::WireO),
            _ => None,
        }
    }

    /// Whether this binding produces a printable spine (perfect and hardcover bindings do;
    /// saddle stitch, coil, and Wire-O do not).
    pub fn has_spine(self) -> bool {
        matches!(
            self,
            Binding::Perfect | Binding::CaseWrap | Binding::LinenWrap
        )
    }

    /// Divisibility rule Lulu applies to this binding's page count.
    pub fn page_count_multiple(self) -> u32 {
        match self {
            Binding::Coil | Binding::WireO => 2,
            _ => 4,
        }
    }
}

/// One product from Lulu's catalog: everything needed to derive trim, bleed,
/// page-count limits, spine, and cover geometry for a `pod_package_id`.
#[derive(Debug, Clone)]
pub struct CatalogEntry {
    pub legacy_sku: String,
    pub sku: String,
    pub book_type: String,
    pub min_page: u32,
    pub max_page: u32,
    pub trim_size: Size,
    /// Size with bleed, as published directly by Lulu — authoritative over any
    /// value derived from `trim_size` plus the bleed formula (see [`crate::geometry`]).
    pub bleed_size: Size,
    pub interior_color: String,
    pub print_quality: String,
    pub binding: Binding,
    pub paper_type: String,
    /// Pages-per-inch bulk for this paper. `None` for the one product in Lulu's own
    /// catalog (a Wire-O calendar) whose spec sheet row carries `#N/A` here — a binding
    /// with no spine (see [`Binding::has_spine`]) never needs this value.
    pub interior_ppi: Option<f64>,
    pub lamination: String,
    pub linen_color: String,
    pub foil_color: String,
}

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum CatalogError {
    #[error("SKU '{sku}' is not in the catalog (catalog fetched {fetch_date}, {product_count} products) — regenerate via crates/lulu-prep/data/regenerate.py if Lulu has since added it")]
    UnknownSku {
        sku: String,
        fetch_date: String,
        product_count: usize,
    },
}

/// Provenance of the embedded catalog, so a report can state which revision
/// of Lulu's data a decision was based on.
#[derive(Debug, Clone, PartialEq)]
pub struct CatalogMetadata {
    pub source_url: String,
    pub fetch_date: String,
    pub product_count: usize,
}

struct RawCatalog {
    entries: Vec<CatalogEntry>,
    metadata: CatalogMetadata,
}

fn parse_csv_line(line: &str) -> Vec<String> {
    // The catalog has no quoted or embedded-comma fields (verified at generation time),
    // so a plain split is sufficient and avoids pulling in a CSV crate for one file.
    line.split(',').map(|s| s.to_string()).collect()
}

fn parse_catalog(csv: &str) -> RawCatalog {
    let mut lines = csv.lines();

    let mut source_url = String::new();
    let mut fetch_date = String::new();
    let mut header_line = None;

    for line in &mut lines {
        if let Some(rest) = line.strip_prefix("# source: ") {
            source_url = rest.trim().to_string();
        } else if let Some(rest) = line.strip_prefix("# fetched: ") {
            fetch_date = rest.trim().to_string();
        } else if line.starts_with('#') {
            continue;
        } else {
            header_line = Some(line);
            break;
        }
    }

    let header = parse_csv_line(header_line.expect("catalog CSV must have a header row"));
    let col = |name: &str| {
        header
            .iter()
            .position(|h| h == name)
            .unwrap_or_else(|| panic!("catalog CSV missing column '{name}'"))
    };

    let idx_legacy = col("legacy_sku");
    let idx_sku = col("sku");
    let idx_book_type = col("book_type");
    let idx_min_page = col("min_page");
    let idx_max_page = col("max_page");
    let idx_trim_w_in = col("trim_width_in");
    let idx_trim_h_in = col("trim_height_in");
    let idx_bleed_w_in = col("bleed_width_in");
    let idx_bleed_h_in = col("bleed_height_in");
    let idx_interior_color = col("interior_color");
    let idx_print_quality = col("print_quality");
    let idx_bind = col("bind");
    let idx_paper_type = col("paper_type");
    let idx_interior_ppi = col("interior_ppi");
    let idx_lamination = col("lamination");
    let idx_linen_color = col("linen_color");
    let idx_foil_color = col("foil_color");

    let mut entries = Vec::new();
    for line in lines {
        if line.trim().is_empty() {
            continue;
        }
        let f = parse_csv_line(line);
        let binding = Binding::from_catalog_string(&f[idx_bind]).unwrap_or_else(|| {
            panic!(
                "unrecognised binding '{}' in catalog row: {line}",
                f[idx_bind]
            )
        });

        entries.push(CatalogEntry {
            legacy_sku: f[idx_legacy].clone(),
            sku: f[idx_sku].clone(),
            book_type: f[idx_book_type].clone(),
            min_page: f[idx_min_page].parse::<f64>().unwrap() as u32,
            max_page: f[idx_max_page].parse::<f64>().unwrap() as u32,
            trim_size: Size::new(
                Length::from_inches(f[idx_trim_w_in].parse().unwrap()),
                Length::from_inches(f[idx_trim_h_in].parse().unwrap()),
            ),
            bleed_size: Size::new(
                Length::from_inches(f[idx_bleed_w_in].parse().unwrap()),
                Length::from_inches(f[idx_bleed_h_in].parse().unwrap()),
            ),
            interior_color: f[idx_interior_color].clone(),
            print_quality: f[idx_print_quality].clone(),
            binding,
            paper_type: f[idx_paper_type].clone(),
            interior_ppi: f[idx_interior_ppi].parse().ok(),
            lamination: f[idx_lamination].clone(),
            linen_color: f[idx_linen_color].clone(),
            foil_color: f[idx_foil_color].clone(),
        });
    }

    let product_count = entries.len();
    RawCatalog {
        entries,
        metadata: CatalogMetadata {
            source_url,
            fetch_date,
            product_count,
        },
    }
}

static CATALOG: OnceLock<RawCatalog> = OnceLock::new();

fn catalog() -> &'static RawCatalog {
    CATALOG.get_or_init(|| parse_catalog(CATALOG_CSV))
}

/// Look up a product by either SKU form (dotted or legacy 27-character).
pub fn lookup(sku: &str) -> Result<&'static CatalogEntry, CatalogError> {
    let c = catalog();
    c.entries
        .iter()
        .find(|e| e.sku == sku || e.legacy_sku == sku)
        .ok_or_else(|| CatalogError::UnknownSku {
            sku: sku.to_string(),
            fetch_date: c.metadata.fetch_date.clone(),
            product_count: c.metadata.product_count,
        })
}

/// Search the catalog with a predicate, e.g. filtering by trim size or binding.
pub fn search(predicate: impl Fn(&CatalogEntry) -> bool) -> Vec<&'static CatalogEntry> {
    catalog().entries.iter().filter(|e| predicate(e)).collect()
}

pub fn metadata() -> &'static CatalogMetadata {
    &catalog().metadata
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_loads_all_products() {
        assert_eq!(metadata().product_count, 3277);
        assert_eq!(catalog().entries.len(), 3277);
    }

    #[test]
    fn metadata_reports_provenance() {
        let m = metadata();
        assert_eq!(
            m.source_url,
            "https://assets.lulu.com/media/specs/lulu-print-api-spec-sheet.xlsx"
        );
        assert!(!m.fetch_date.is_empty());
    }

    #[test]
    fn lookup_by_dotted_sku_resolves_worked_example() {
        let e = lookup("0600X0900.BW.STD.PB.060UW444.MXX").expect("known SKU");
        assert!((e.trim_size.width.as_inches() - 6.0).abs() < 1e-9);
        assert!((e.trim_size.height.as_inches() - 9.0).abs() < 1e-9);
        assert!((e.bleed_size.width.as_inches() - 6.25).abs() < 1e-9);
        assert!((e.bleed_size.height.as_inches() - 9.25).abs() < 1e-9);
        assert_eq!(e.binding, Binding::Perfect);
        assert_eq!(e.min_page, 32);
        assert_eq!(e.max_page, 800);
        assert_eq!(e.interior_ppi, Some(444.0));
    }

    #[test]
    fn lookup_by_legacy_sku_resolves_the_same_entry() {
        let by_dotted = lookup("0600X0900.BW.STD.PB.060UW444.MXX").unwrap();
        let by_legacy = lookup("0600X0900BWSTDPB060UW444MXX").unwrap();
        assert_eq!(by_dotted.sku, by_legacy.sku);
        assert_eq!(by_dotted.legacy_sku, by_legacy.legacy_sku);
    }

    #[test]
    fn lookup_unknown_sku_names_the_catalog_fetch_date() {
        let err = lookup("0000X0000.XX.XX.XX.XXXXXXXXX.XXX").unwrap_err();
        match err {
            CatalogError::UnknownSku {
                fetch_date,
                product_count,
                ..
            } => {
                assert!(!fetch_date.is_empty());
                assert_eq!(product_count, 3277);
            }
        }
    }

    #[test]
    fn search_filters_by_binding() {
        let coil = search(|e| e.binding == Binding::Coil);
        assert!(!coil.is_empty());
        assert!(coil.iter().all(|e| e.binding == Binding::Coil));
    }

    #[test]
    fn binding_page_count_multiples_match_lulu_rules() {
        assert_eq!(Binding::Coil.page_count_multiple(), 2);
        assert_eq!(Binding::WireO.page_count_multiple(), 2);
        assert_eq!(Binding::Perfect.page_count_multiple(), 4);
        assert_eq!(Binding::SaddleStitch.page_count_multiple(), 4);
        assert_eq!(Binding::CaseWrap.page_count_multiple(), 4);
        assert_eq!(Binding::LinenWrap.page_count_multiple(), 4);
    }

    #[test]
    fn comic_paper_row_with_blank_paper_type_is_not_column_shifted() {
        // Regression test for a sparse-XLSX extraction bug: this row's "Paper Type"
        // cell is blank in Lulu's own spreadsheet, which — if columns are read
        // positionally instead of by cell reference — shifts every later column
        // left by one and lands "Gloss" in interior_ppi instead of 460.0.
        let e = lookup("0663X1025.BW.PRE.PB.070CW460.GIX").expect("known SKU");
        assert_eq!(e.paper_type, "");
        assert_eq!(e.interior_ppi, Some(460.0));
        assert_eq!(e.lamination, "Gloss");
    }

    #[test]
    fn missing_ppi_on_a_spineless_product_does_not_panic() {
        let e = lookup("1100X0850.FC.PRE.WO.100CW200.GXX").expect("known SKU");
        assert_eq!(e.interior_ppi, None);
        assert!(!e.binding.has_spine());
    }

    #[test]
    fn binding_has_spine_matches_lulu_rules() {
        assert!(Binding::Perfect.has_spine());
        assert!(Binding::CaseWrap.has_spine());
        assert!(Binding::LinenWrap.has_spine());
        assert!(!Binding::Coil.has_spine());
        assert!(!Binding::SaddleStitch.has_spine());
        assert!(!Binding::WireO.has_spine());
    }
}
