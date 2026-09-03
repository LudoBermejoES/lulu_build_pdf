//! `lulu-prep products` — catalog search output
//! (`specs/cli/spec.md`, "Product selection", "Catalog search" scenario).

use lulu_prep::catalog::CatalogEntry;

/// One line per matching product: SKU, book type, trim size, size with
/// bleed, binding, paper, and page-count range — the exact column set the
/// spec's "Catalog search" scenario requires.
pub fn format_products_table(entries: &[&CatalogEntry]) -> String {
    if entries.is_empty() {
        return "No products match.".to_string();
    }
    let mut out = String::new();
    out.push_str("SKU\tBook type\tTrim (in)\tWith bleed (in)\tBinding\tPaper\tPages\n");
    for entry in entries {
        out.push_str(&format!(
            "{}\t{}\t{:.3}x{:.3}\t{:.3}x{:.3}\t{:?}\t{}\t{}-{}\n",
            entry.sku,
            entry.book_type,
            entry.trim_size.width.as_inches(),
            entry.trim_size.height.as_inches(),
            entry.bleed_size.width.as_inches(),
            entry.bleed_size.height.as_inches(),
            entry.binding,
            entry.paper_type,
            entry.min_page,
            entry.max_page,
        ));
    }
    out.trim_end().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_results_say_so() {
        assert_eq!(format_products_table(&[]), "No products match.");
    }

    #[test]
    fn lists_every_required_column() {
        let entry = lulu_prep::catalog::lookup("0600X0900.BW.STD.PB.060UW444.MXX").unwrap();
        let table = format_products_table(&[entry]);
        assert!(table.contains("0600X0900.BW.STD.PB.060UW444.MXX"));
        assert!(table.contains("6.000x9.000"));
        assert!(table.contains(&format!("{}-{}", entry.min_page, entry.max_page)));
        assert!(
            table.starts_with("SKU\tBook type\tTrim (in)\tWith bleed (in)\tBinding\tPaper\tPages")
        );
    }
}
