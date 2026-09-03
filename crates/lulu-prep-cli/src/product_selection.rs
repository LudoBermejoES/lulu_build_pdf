//! Resolving a Lulu product from either an explicit `pod_package_id` or a
//! set of component filters (trim size, binding, ink, quality, paper).

use lulu_prep::catalog::{Binding, CatalogEntry};

/// Filters used to narrow the catalog down to one product when the caller
/// didn't supply a SKU directly. Every field is optional; an absent field
/// matches anything.
#[derive(Debug, Clone, Default)]
pub struct ComponentFilter {
    /// Trim width and height in inches, matched within 0.01 in.
    pub trim_in: Option<(f64, f64)>,
    pub binding: Option<Binding>,
    /// "BW" or "FC" (case-insensitive), matched against the catalog's
    /// `interior_color` ("Black & White" / "Full Color").
    pub ink: Option<String>,
    /// "Standard" or "Premium" (case-insensitive, substring match).
    pub quality: Option<String>,
    /// Substring match against the catalog's `paper_type`.
    pub paper: Option<String>,
    /// Substring match against the catalog's `lamination` (e.g. "Gloss", "Matte").
    pub lamination: Option<String>,
}

impl ComponentFilter {
    fn matches(&self, entry: &CatalogEntry) -> bool {
        if let Some((w, h)) = self.trim_in {
            let ew = entry.trim_size.width.as_inches();
            let eh = entry.trim_size.height.as_inches();
            if (ew - w).abs() > 0.01 || (eh - h).abs() > 0.01 {
                return false;
            }
        }
        if let Some(binding) = self.binding {
            if entry.binding != binding {
                return false;
            }
        }
        if let Some(ink) = &self.ink {
            let ink_lower = ink.to_lowercase();
            let matches_ink = match ink_lower.as_str() {
                "bw" | "black & white" | "black and white" => {
                    entry.interior_color.eq_ignore_ascii_case("Black & White")
                }
                "fc" | "full color" | "full colour" => {
                    entry.interior_color.eq_ignore_ascii_case("Full Color")
                }
                other => entry.interior_color.to_lowercase().contains(other),
            };
            if !matches_ink {
                return false;
            }
        }
        if let Some(quality) = &self.quality {
            if !entry
                .print_quality
                .to_lowercase()
                .contains(&quality.to_lowercase())
            {
                return false;
            }
        }
        if let Some(paper) = &self.paper {
            if !entry
                .paper_type
                .to_lowercase()
                .contains(&paper.to_lowercase())
            {
                return false;
            }
        }
        if let Some(lamination) = &self.lamination {
            if !entry
                .lamination
                .to_lowercase()
                .contains(&lamination.to_lowercase())
            {
                return false;
            }
        }
        true
    }

    pub fn is_empty(&self) -> bool {
        self.trim_in.is_none()
            && self.binding.is_none()
            && self.ink.is_none()
            && self.quality.is_none()
            && self.paper.is_none()
            && self.lamination.is_none()
    }
}

#[derive(Debug, Clone)]
pub enum ProductSelector {
    Sku(String),
    Components(ComponentFilter),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SelectionError {
    UnknownSku(String),
    NoComponentsGiven,
    NoMatch,
    Ambiguous(Vec<String>),
}

impl std::fmt::Display for SelectionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SelectionError::UnknownSku(sku) => write!(f, "unknown pod_package_id '{sku}'"),
            SelectionError::NoComponentsGiven => write!(
                f,
                "no product selector given (pass --sku or at least one component flag)"
            ),
            SelectionError::NoMatch => write!(f, "no product matches the given component filters"),
            SelectionError::Ambiguous(candidates) => {
                writeln!(
                    f,
                    "{} products match the given filters; narrow the selection:",
                    candidates.len()
                )?;
                for sku in candidates.iter().take(20) {
                    writeln!(f, "  {sku}")?;
                }
                if candidates.len() > 20 {
                    write!(f, "  ... and {} more", candidates.len() - 20)?;
                }
                Ok(())
            }
        }
    }
}

/// Every catalog entry matching `filter` — an empty filter matches
/// everything, unlike [`resolve_product`], which refuses an empty filter.
pub fn search_catalog(filter: &ComponentFilter) -> Vec<&'static CatalogEntry> {
    lulu_prep::catalog::search(|e| filter.matches(e))
}

/// Resolves a product, either directly by SKU (dotted or legacy form) or by
/// narrowing the catalog with component filters. Ambiguous component
/// filters list every matching SKU rather than picking one.
pub fn resolve_product(
    selector: &ProductSelector,
) -> Result<&'static CatalogEntry, SelectionError> {
    match selector {
        ProductSelector::Sku(sku) => {
            lulu_prep::catalog::lookup(sku).map_err(|_| SelectionError::UnknownSku(sku.clone()))
        }
        ProductSelector::Components(filter) => {
            if filter.is_empty() {
                return Err(SelectionError::NoComponentsGiven);
            }
            let matches = lulu_prep::catalog::search(|e| filter.matches(e));
            match matches.len() {
                0 => Err(SelectionError::NoMatch),
                1 => Ok(matches[0]),
                _ => Err(SelectionError::Ambiguous(
                    matches.iter().map(|e| e.sku.clone()).collect(),
                )),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_by_exact_sku() {
        let selector = ProductSelector::Sku("0600X0900.BW.STD.PB.060UW444.MXX".to_string());
        let entry = resolve_product(&selector).unwrap();
        assert_eq!(entry.sku, "0600X0900.BW.STD.PB.060UW444.MXX");
    }

    #[test]
    fn resolves_by_legacy_sku() {
        let selector = ProductSelector::Sku("0600X0900BWSTDPB060UW444MXX".to_string());
        let entry = resolve_product(&selector).unwrap();
        assert_eq!(entry.sku, "0600X0900.BW.STD.PB.060UW444.MXX");
    }

    #[test]
    fn unknown_sku_is_reported() {
        let selector = ProductSelector::Sku("not-a-real-sku".to_string());
        let err = resolve_product(&selector).unwrap_err();
        assert!(matches!(err, SelectionError::UnknownSku(_)));
    }

    #[test]
    fn components_resolving_to_exactly_one_product_succeeds() {
        let filter = ComponentFilter {
            trim_in: Some((6.0, 9.0)),
            binding: Some(Binding::Perfect),
            ink: Some("BW".to_string()),
            quality: Some("Standard".to_string()),
            paper: Some("60# Uncoated White".to_string()),
            lamination: Some("Matte".to_string()),
        };
        let entry = resolve_product(&ProductSelector::Components(filter)).unwrap();
        assert_eq!(entry.sku, "0600X0900.BW.STD.PB.060UW444.MXX");
    }

    #[test]
    fn ambiguous_components_list_every_candidate() {
        let filter = ComponentFilter {
            trim_in: Some((6.0, 9.0)),
            binding: Some(Binding::Perfect),
            ..Default::default()
        };
        let err = resolve_product(&ProductSelector::Components(filter)).unwrap_err();
        match err {
            SelectionError::Ambiguous(candidates) => assert!(candidates.len() > 1),
            other => panic!("expected Ambiguous, got {other:?}"),
        }
    }

    #[test]
    fn no_components_given_is_an_error_not_matching_everything() {
        let err =
            resolve_product(&ProductSelector::Components(ComponentFilter::default())).unwrap_err();
        assert_eq!(err, SelectionError::NoComponentsGiven);
    }

    #[test]
    fn components_matching_nothing_is_reported() {
        let filter = ComponentFilter {
            trim_in: Some((99.0, 99.0)),
            ..Default::default()
        };
        let err = resolve_product(&ProductSelector::Components(filter)).unwrap_err();
        assert_eq!(err, SelectionError::NoMatch);
    }
}
