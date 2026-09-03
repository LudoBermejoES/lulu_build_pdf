## Why

Lulu rejects or silently degrades print jobs whose PDFs don't match its product specification: page size must be the trim size plus exactly 0.125 in of bleed per side, every page must be the same size, fonts must be embedded, page count must fall inside the binding's min/max, the interior and the cover must be separate files, and the cover's width depends on a spine width derived from the final page count. Today that preparation is manual — a designer reads the help pages, computes the spine by hand, exports from Acrobat with the right job options, and finds out whether it worked only after uploading. There is no tool that takes an arbitrary PDF plus a target Lulu product and deterministically produces the two print-ready files, with a report of what was changed and what could not be fixed.

## What Changes

- New Rust workspace: a `lulu-prep` library crate holding all the domain logic, plus a thin `lulu-prep` CLI binary over it.
- Embed Lulu's official product catalog (3,277 POD packages from Lulu's published spec sheet) so trim size, size-with-bleed, binding, paper PPI, and page-count limits are resolved offline from a `pod_package_id`.
- Parse and validate both POD package ID formats: the current dotted `[Trim].[Ink].[Quality].[Binding].[Paper].[Finish]` form and the legacy 27-character form (Lulu retires the legacy form on 2027-02-01).
- Preflight an input PDF against a chosen product and emit a findings report (human-readable and JSON) covering page-size consistency, trim/bleed geometry, font embedding, effective image PPI, colour spaces, transparency, encryption, annotations, and page count.
- Normalize the interior: rescale/reposition every page onto a uniform trim+bleed canvas, apply the page-count-dependent gutter shift, pad with blank pages to satisfy the binding's minimum and its multiple-of rule, strip encryption, annotations, embedded JavaScript, embedded files, and any page-box configuration that would print as trim marks.
- Prepare the cover: compute spine width (perfect-bound formula per paper PPI; hardcover/linen from Lulu's stepped table), compute the full wrap canvas, generate a blank cover template with guide layers, or fit a supplied cover artwork onto the correct wrap geometry.
- Optionally delegate the operations Rust cannot do faithfully — transparency flattening, sRGB→CMYK conversion against a GRACoL profile, structural repair, linearization — to Ghostscript and qpdf when present, degrading to a reported warning when absent.
- Optionally verify the finished files against Lulu's own Print API (`validate-interior`, `validate-cover`, `cover-dimensions`) when credentials and a reachable file URL are supplied.

## Capabilities

### New Capabilities
- `lulu-product-catalog`: POD package ID parsing (dotted and legacy), the embedded product catalog, and the derived geometry — trim size, size with bleed, page-count limits, paper PPI, spine width, and full cover wrap dimensions.
- `pdf-preflight`: read-only inspection of a PDF against a target product, producing a structured, severity-ranked findings report in human and JSON form.
- `interior-normalization`: transforming an arbitrary interior PDF into a Lulu-conformant interior file — uniform trim+bleed page geometry, gutter, blank-page padding, and structural sanitation.
- `cover-preparation`: spine and wrap geometry for a given product and page count, blank cover template generation, and fitting supplied cover artwork onto that geometry.
- `external-tool-pipeline`: detection, invocation, and graceful degradation of optional Ghostscript and qpdf stages for flattening, colour conversion, and repair.
- `lulu-api-verification`: opt-in verification of prepared files against Lulu's Print API, including OAuth client-credentials handling and the asynchronous validation poll.
- `cli`: the `lulu-prep` command surface — subcommands, product selection, configuration precedence, report output, and exit codes.

### Modified Capabilities

None — this is the first change in a new project.

## Impact

- **New code**: a Cargo workspace with `crates/lulu-prep` (library) and `crates/lulu-prep-cli` (binary). No existing code to modify.
- **Vendored data**: a catalog file generated from `https://assets.lulu.com/media/specs/lulu-print-api-spec-sheet.xlsx`, checked in as CSV with a recorded fetch date and a regeneration script. Lulu changes products and prices, so this is versioned data with a documented refresh path, not a constant.
- **Dependencies**: `lopdf` for the PDF object model, `pdf-writer`/`printpdf` for generated pages, `lcms2` for ICC transforms on images, `clap` for the CLI, `serde`/`serde_json` for reports, and `reqwest`/`oauth2` behind an off-by-default `lulu-api` feature.
- **Runtime dependencies**: Ghostscript and qpdf are optional external binaries. Ghostscript is AGPL — invoking it as a subprocess keeps it out of the crate's link graph, but it must stay optional and be documented as such, and the tool must be fully useful without it.
- **Non-goals for this change**: no GUI, no order placement or payment through the Print API, no editorial layout work (typesetting, reflow, hyphenation), and no attempt to repair genuinely corrupt PDFs beyond what qpdf can do.
- **Correctness risk to manage**: Lulu's published formulas and tables are the contract. Spine width, bleed, and page-count rules must live in one place, be covered by tests using the worked examples from Lulu's own documentation (e.g. a 6×9 in perfect-bound book with 210 interior pages must yield a 920×666 pt cover), and never be duplicated inline.
